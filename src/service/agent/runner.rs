//! Agent 编排循环 + turn 注册表。

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use ulid::Ulid;

use crate::domain::agent::AssistantContent;
use crate::domain::agent_events::{AgentEvent, ToolErrorKind};
use crate::domain::AgentMessage;
use crate::error::AppError;
use crate::service::agent::exec::execute_tool;
use crate::service::agent::system_prompt::{agent_system_prompt, to_chat_messages};
use crate::service::{AuthContext, Services};

/// 单 turn 的运行句柄：task + 它的 broadcast sender。
struct TurnHandle {
    #[allow(dead_code)]
    handle: tokio::task::JoinHandle<()>,
    sender: broadcast::Sender<AgentEvent>,
}

/// 注册 session/turn 的活跃状态。同 session 同时只跑一个 turn。
///
/// 历史上曾用 `CancellationToken` HashMap 给每个 turn 配 cancel hook，
/// 但 runner 自身没有任何路径会触发它——`start_or_replace` 用新 token
/// 覆盖注册表项，旧 runner 实际靠 broadcast Sender drop 自然结束；
/// SSE 端点的 Drop guard 调的 `.cancel()` 落在一个孤儿 token 上，纯
/// 无副作用。整张表已是死基础设施（参见 SDD plan 终评 parked follow-up
/// #7），不留以免误导后续修改。
pub struct SessionTurns {
    turns: Mutex<HashMap<Ulid, TurnHandle>>,
    by_session: Mutex<HashMap<Ulid, Ulid>>,
    /// turn_id → 注入到 system 之后的「任务上下文」markdown 文本。
    /// 一次性消费（runner 读完即删），避免长会话内存累积。
    context_md: Mutex<HashMap<Ulid, String>>,
}

impl Default for SessionTurns {
    fn default() -> Self {
        Self {
            turns: Mutex::new(HashMap::new()),
            by_session: Mutex::new(HashMap::new()),
            context_md: Mutex::new(HashMap::new()),
        }
    }
}

impl SessionTurns {
    /// 起一个新 turn；若同 session 已有 turn，旧 turn 的 broadcast sender
    /// 会被 HashMap::remove 一起 drop，SSE 订阅端读到 `RecvError::Closed`
    /// 后自动退出——这条路径才是取消语义的真实承担者。
    pub fn start_or_replace(
        self: &Arc<Self>,
        session_id: Ulid,
        turn_id: Ulid,
        runner: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), AppError> {
        if let Some(old) = self.by_session.lock().remove(&session_id) {
            self.turns.lock().remove(&old);
        }
        let (tx, _) = broadcast::channel(128);
        let handle = tokio::spawn(runner);
        self.turns.lock().insert(
            turn_id,
            TurnHandle {
                handle,
                sender: tx.clone(),
            },
        );
        self.by_session.lock().insert(session_id, turn_id);
        Ok(())
    }

    pub fn subscribe(&self, turn_id: Ulid) -> Result<broadcast::Receiver<AgentEvent>, AppError> {
        self.turns
            .lock()
            .get(&turn_id)
            .map(|h| h.sender.subscribe())
            .ok_or(AppError::NotFound)
    }

    pub fn turn_sender(&self, turn_id: Ulid) -> Option<broadcast::Sender<AgentEvent>> {
        self.turns.lock().get(&turn_id).map(|h| h.sender.clone())
    }

    /// 给 turn 投递「任务上下文」markdown，由 resolver 在 start_or_replace 之前
    /// 调用；runner 在第一轮历史组装时通过 take_context_md 取回。
    pub fn put_context_md(&self, turn_id: Ulid, md: String) {
        self.context_md.lock().insert(turn_id, md);
    }

    /// 一次性取出 turn 的上下文 markdown；runner 取完即删。
    pub fn take_context_md(&self, turn_id: Ulid) -> Option<String> {
        self.context_md.lock().remove(&turn_id)
    }

    pub fn session_of(&self, turn_id: Ulid) -> Option<Ulid> {
        let bs = self.by_session.lock();
        bs.iter()
            .find_map(|(&sid, &tid)| (tid == turn_id).then_some(sid))
    }

    pub fn finish(&self, turn_id: Ulid) {
        self.turns.lock().remove(&turn_id);
        let mut bs = self.by_session.lock();
        if let Some((&sid, _)) = bs.iter().find(|(_, &t)| t == turn_id) {
            bs.remove(&sid);
        }
    }
}

/// Agent 单 turn 编排循环：
/// 1) 取历史 → 组 messages（含 system + 全部历史 user/assistant/tool）
/// 2) 流式调 LLM，边收边 emit `Delta`，把 tool_call delta 累加成完整 `ToolCalls`
/// 3) 落库 assistant 消息
/// 4) 若无 tool_call，emit `Done` 退出；否则按顺序执行每个工具，落库 tool 消息，
///    回到 1) 进入下一轮（最多 `max_tool_calls_per_turn` 轮）
///
/// 错误与 cancel 通过 `AgentEvent::Error` 透传给订阅者，最后统一 `finish(turn_id)`
/// 收尾——订阅端（Task 7 的 SSE handler）据此断开连接。
pub async fn run_turn(services: Arc<Services>, auth: AuthContext, session_id: Ulid, turn_id: Ulid) {
    let max_turns = services.config.agent.max_tool_calls_per_turn;
    let max_history = services.config.agent.max_history_turns;

    // runner 必须拿到 turn 的 broadcast sender 才能 emit event。
    // 拿不到（已被 finish 或 turn 被替换）就直接退出。
    let Some(tx) = services.agent_turns.turn_sender(turn_id) else {
        return;
    };

    let client = match services.rig_client.as_ref() {
        Some(c) => c,
        None => {
            let _ = tx.send(AgentEvent::Error {
                turn_id,
                message: "AI 未配置".into(),
            });
            services.agent_turns.finish(turn_id);
            return;
        }
    };

    let session = match services.agent.get_session(auth.account_id, session_id) {
        Ok(Some(s)) => s,
        _ => {
            let _ = tx.send(AgentEvent::Error {
                turn_id,
                message: "session 不存在".into(),
            });
            services.agent_turns.finish(turn_id);
            return;
        }
    };

    let tools = services.agent_tools.clone();
    let workspace_id = session.workspace_id;
    // 任务上下文（视图 / 选中条目）：resolver 在 start_or_replace 前已经把
    // markdown 文本塞进 SessionTurns。Hoist 到循环外——`take_context_md` 是
    // HashMap::remove 的一次性消费，循环里再取第二轮起就 None，LLM 看见的
    // 消息形状从第二轮起就丢了 context，跟系统提示里「上下文在第二条 user
    // 消息」承诺不一致。每轮都复用同一份 markdown——text 不变，复用零成本。
    let ctx_md = services.agent_turns.take_context_md(turn_id);

    for _ in 0..max_turns {
        let history = match services.agent.list_messages(session_id, max_history) {
            Ok(h) => h,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error {
                    turn_id,
                    message: e.to_string(),
                });
                break;
            }
        };
        // 占位已填：runner 进 turn 前查一次 Workspace 拿真实 name 给 system prompt；
        // 查不到（罕见：session 引用的 ws 已删）就退回到用 id 串。Workspaces 表的读
        // 是同步 DocStore get，不阻塞 current_thread 运行时。
        let ws_title = services
            .workspace
            .get_by_id(workspace_id)
            .ok()
            .flatten()
            .map(|w| w.name)
            .unwrap_or_else(|| workspace_id.to_string());
        let ws_id_str = workspace_id.to_string();
        // 任务上下文 markdown 在循环外一次性取，跨轮复用——见 for-loop 上方的
        // ctx_md 取法说明。
        let mut messages = vec![agent_system_prompt(&ws_title, &ws_id_str)];
        if let Some(md) = ctx_md.as_deref() {
            messages.push(crate::service::ai::ChatMessage {
                role: "user".to_string(),
                content: md.to_string(),
                tool_call_id: None,
                tool_calls: None,
            });
        }
        messages.extend(to_chat_messages(&history));

        let stream = match client.complete_stream(&messages, &tools).await {
            Ok(s) => s,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error {
                    turn_id,
                    message: e.to_string(),
                });
                break;
            }
        };
        futures_util::pin_mut!(stream);

        let mut content = String::new();
        let mut pending_args: HashMap<u32, (String, String, String)> = HashMap::new();
        let mut final_tool_calls: Vec<crate::service::ai::ToolCallRequest> = Vec::new();
        let mut stream_error: Option<String> = None;

        loop {
            match stream.next().await {
                Some(Ok(crate::service::ai::StreamChunk::Delta(s))) => {
                    content.push_str(&s);
                    let _ = tx.send(AgentEvent::Delta {
                        turn_id,
                        content: s,
                    });
                }
                Some(Ok(crate::service::ai::StreamChunk::Reasoning(s))) => {
                    // 推理链不入库——只在流式期间推给前端做样式区分。
                    let _ = tx.send(AgentEvent::Reasoning {
                        turn_id,
                        content: s,
                    });
                }
                Some(Ok(crate::service::ai::StreamChunk::ToolCallsPartial(partials))) => {
                    for (idx, id, name, args) in partials {
                        let entry = pending_args
                            .entry(idx)
                            .or_insert_with(|| (String::new(), String::new(), String::new()));
                        if !id.is_empty() {
                            entry.0 = id;
                        }
                        if !name.is_empty() {
                            entry.1 = name;
                        }
                        entry.2.push_str(&args);
                    }
                }
                Some(Ok(crate::service::ai::StreamChunk::ToolCalls(tcs))) => {
                    final_tool_calls = tcs;
                }
                Some(Ok(crate::service::ai::StreamChunk::Done)) | None => break,
                Some(Err(e)) => {
                    stream_error = Some(e.to_string());
                    break;
                }
            }
        }

        if let Some(msg) = stream_error {
            let _ = tx.send(AgentEvent::Error {
                turn_id,
                message: msg,
            });
            services.agent_turns.finish(turn_id);
            return;
        }

        // 落库 assistant 消息。Rig `Message::Assistant` 的 `content` 是
        // `Vec<AssistantContent>`：先放文本，再追加 tool_call。ULID 由
        // persist 层在落库时分配，runner 不预生成。
        let mut assistant_content: Vec<AssistantContent> = Vec::new();
        if !content.is_empty() {
            assistant_content.push(AssistantContent::text(content.clone()));
        }
        for tc in &final_tool_calls {
            let args =
                serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
            assistant_content.push(AssistantContent::tool_call(
                tc.id.clone(),
                tc.function.name.clone(),
                args,
            ));
        }
        let assistant_msg = AgentMessage::Assistant {
            id: None,
            content: assistant_content,
        };
        let assistant_record = match services.agent.append_message(session_id, assistant_msg) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error {
                    turn_id,
                    message: e.to_string(),
                });
                break;
            }
        };

        // 没有 tool_call：本轮就是答案。落库 last_message_at 然后发 Done。
        if final_tool_calls.is_empty() {
            let _ = services.agent.update_session_meta(
                auth.account_id,
                session_id,
                None,
                Some(assistant_record.created_at),
            );
            let _ = tx.send(AgentEvent::Done {
                turn_id,
                message_id: assistant_record.id,
            });
            services.agent_turns.finish(turn_id);
            return;
        }

        // 串行执行每个 tool_call：发 ToolCall → execute → 发 ToolResult/SideEffect → 落 tool 消息。
        for tc in final_tool_calls {
            let args: serde_json::Value = match serde_json::from_str(&tc.function.arguments) {
                Ok(v) => v,
                Err(parse_err) => {
                    // LLM 回传的 arguments 不是合法 JSON——以前静默退化成
                    // `Value::Null` 继续 execute，让 LLM 在下一轮看不到反馈、
                    // 重发同样坏串。现在短路：照常发 ToolCall / ToolResult，
                    // preview + 落库的 tool 消息里写明解析失败原因，让它在下一轮
                    // 自己修正。
                    let _ = tx.send(AgentEvent::ToolCall {
                        turn_id,
                        name: tc.function.name.clone(),
                        args: serde_json::Value::Null,
                    });
                    let preview = format!("参数 JSON 解析失败: {parse_err}");
                    let _ = tx.send(AgentEvent::Error {
                        turn_id,
                        message: preview.clone(),
                    });
                    let _ = tx.send(AgentEvent::ToolResult {
                        turn_id,
                        name: tc.function.name.clone(),
                        ok: false,
                        kind: ToolErrorKind::BadArgs,
                        preview: preview.clone(),
                    });
                    let tool_msg =
                        AgentMessage::tool_result(tc.id.clone(), tc.function.name.clone(), preview);
                    let _ = services.agent.append_message(session_id, tool_msg);
                    continue;
                }
            };
            let _ = tx.send(AgentEvent::ToolCall {
                turn_id,
                name: tc.function.name.clone(),
                args: args.clone(),
            });
            let outcome = match execute_tool(
                &services.schema,
                &services,
                &auth,
                workspace_id,
                &tc.function.name,
                args,
            )
            .await
            {
                Ok(o) => o,
                Err(e) => crate::service::agent::exec::ToolOutcome {
                    // execute_tool 返回 Err：典型路径是「未知工具」（build 阶段
                    // 没过滤干净的 query 漂移）或罕见的序列化 / panic。归
                    // ServerError——LLM 重试参数无意义（不是它能改的）。preview
                    // 把真实原因带出来，不然下次再踩这个坑前端只看到一句"工具
                    // 调用失败"，又得走一轮 systematic-debugging 才能定位。
                    ok: false,
                    kind: ToolErrorKind::ServerError,
                    result_preview: format!("工具调用失败: {e}"),
                    side_effect: None,
                },
            };
            let _ = tx.send(AgentEvent::ToolResult {
                turn_id,
                name: tc.function.name.clone(),
                ok: outcome.ok,
                kind: outcome.kind,
                preview: outcome.result_preview.clone(),
            });
            if let Some(se) = &outcome.side_effect {
                let _ = tx.send(AgentEvent::SideEffect {
                    turn_id,
                    side_effect: se.clone(),
                });
            }
            let tool_msg = AgentMessage::tool_result(
                tc.id.clone(),
                tc.function.name.clone(),
                outcome.result_preview,
            );
            let _ = services.agent.append_message(session_id, tool_msg);
        }
    }

    let _ = tx.send(AgentEvent::Error {
        turn_id,
        message: format!("超过最大 turn 数 ({max_turns})"),
    });
    services.agent_turns.finish(turn_id);
}
