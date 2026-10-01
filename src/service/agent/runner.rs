//! Agent 编排循环 + turn 注册表。

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use parking_lot::Mutex;
use rig::agent::{AgentBuilder, MultiTurnStreamItem};
use rig::completion::message::AssistantContent;
use rig::message::{ToolCall as RigToolCall, ToolResultContent};
use rig::streaming::{StreamedAssistantContent, StreamedUserContent};
use tokio::sync::broadcast;
use ulid::Ulid;

use crate::domain::agent::{AgentMessage, AgentMessageRecord};
use crate::domain::agent_events::{AgentEvent, ToolErrorKind};
use crate::error::AppError;
use crate::service::agent::system_prompt::agent_system_prompt;
use crate::service::agent::tools::AgentTools;
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

/// Agent 单 turn 编排：交 Rig `AgentRunner::stream()` 驱动，把每条
/// `MultiTurnStreamItem` 适配到现有 `AgentEvent` 走 SSE。
///
/// 错误路径统一「推 `AgentEvent::Error` → `finish(turn_id)` → `return`」——
/// 不会有 `break` 落尾部跑到「超过最大 turn 数」的老 bug。
pub async fn run_turn(services: Arc<Services>, auth: AuthContext, session_id: Ulid, turn_id: Ulid) {
    // runner 必须拿到 turn 的 broadcast sender 才能 emit event。
    // 拿不到（已被 finish 或 turn 被替换）就直接退出。
    let Some(tx) = services.agent_turns.turn_sender(turn_id) else {
        return;
    };

    let rig = match services.rig_client.as_ref() {
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

    let ctx_md = services.agent_turns.take_context_md(turn_id);
    let history = match services
        .agent
        .list_messages(session_id, services.config.agent.max_history_turns)
    {
        Ok(h) => h,
        Err(e) => {
            let _ = tx.send(AgentEvent::Error {
                turn_id,
                message: e.to_string(),
            });
            services.agent_turns.finish(turn_id);
            return;
        }
    };

    let workspace_id = session.workspace_id;
    let ws_title = services
        .workspace
        .get_by_id(workspace_id)
        .ok()
        .flatten()
        .map(|w| w.name)
        .unwrap_or_else(|| workspace_id.to_string());
    let preamble = agent_system_prompt(&ws_title, &workspace_id.to_string());
    let completion_model = rig.completion_model();

    // 当前 user 输入：ctx_md 存在就用它，否则拿 history 最后一条；都没有就空 user。
    // history 是 Vec<AgentMessage> = Vec<rig::Message>，chat history 传过去让 Rig
    // 帮我们组装到 system 之后。
    //
    // history 已经包含刚 append 的 user message（graphql.rs:append_message 在
    // runner 启动前调用过）；.runner(user_msg) 把 user_msg 作为 prompt 再发一次，
    // .history(chat_history) 把 chat_history 拼在 prompt **之前**——所以
    // ctx_md=Some 时 user_msg != history.last()（一个是 ctx_md、一个是真实
    // user query），两个独立 user 消息不需要去重；ctx_md=None 时
    // user_msg == history.last()，不去重就会 [...past..., user_msg, user_msg]
    // 让 LLM 看到重复输入。这里按 ctx_md 是否存在分支去重。
    let user_msg = match ctx_md.as_deref() {
        Some(md) => AgentMessage::user(md.to_string()),
        None => history
            .last()
            .cloned()
            .unwrap_or_else(|| AgentMessage::user(String::new())),
    };
    let chat_history: Vec<AgentMessage> = if ctx_md.is_some() || history.is_empty() {
        history
    } else {
        history[..history.len() - 1].to_vec()
    };

    let agent = AgentTools::build(services.clone(), auth.clone(), workspace_id)
        .register_into(
            AgentBuilder::new(completion_model)
                .preamble(&preamble)
                .default_max_turns(services.config.agent.max_turns),
        )
        .build();

    // `AgentRunner::stream` 是 `async fn stream(self) -> StreamingResult`，把错误
    // 装进 stream 的 `Item` 而不是 `Result`——所以直接 `.await` 拿到 stream。
    let stream = agent
        .runner(user_msg)
        .history(chat_history)
        .max_turns(services.config.agent.max_turns)
        .stream()
        .await;
    futures_util::pin_mut!(stream);

    // 累积本次 assistant 输出，便于在 FinalResponse 时落库。
    let mut accumulated_text = String::new();
    let mut assistant_tool_calls: Vec<RigToolCall> = Vec::new();
    // assistant record 在 FinalResponse 时创建；先 hold ULID 给 Done 事件用。
    let assistant_id = Ulid::new();

    while let Some(item) = stream.next().await {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t))) => {
                accumulated_text.push_str(&t.text);
                let _ = tx.send(AgentEvent::Delta {
                    turn_id,
                    content: t.text,
                });
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) => {
                assistant_tool_calls.push(tool_call);
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Reasoning {
                reasoning,
                ..
            })) => {
                // 推理链不入库——只在流式期间推给前端做样式区分。
                let content = reasoning.display_text();
                if !content.is_empty() {
                    let _ = tx.send(AgentEvent::Reasoning { turn_id, content });
                }
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(_)) => {
                // ToolCallDelta / ReasoningDelta / Final / Unknown——暂不直接推 SSE；
                // 完整 ToolCall 在 StreamAssistantItem::ToolCall 路径，完整 Reasoning
                // 也在 Reasoning 路径。
            }
            Ok(MultiTurnStreamItem::ToolExecutionCommitted { tool_call, .. }) => {
                let name = tool_call.function.name.clone();
                let args = tool_call.function.arguments.clone();
                let _ = tx.send(AgentEvent::ToolCall {
                    turn_id,
                    name,
                    args,
                });
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            })) => {
                let preview = tool_result
                    .content
                    .iter()
                    .map(|trc| match trc {
                        ToolResultContent::Text(t) => t.text.clone(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                // rig 0.42 的 ToolResult 没有 is_err 字段——到达这里说明工具执行
                // 没失败（失败的执行不产 ToolResult，见 rig-agent MultiTurnStreamItem
                // 注释）。前端 ok=true 用 ToolErrorKind::Ok 表达即可。
                let _ = tx.send(AgentEvent::ToolResult {
                    turn_id,
                    name: tool_result.name.clone(),
                    ok: true,
                    kind: ToolErrorKind::Ok,
                    preview,
                });
            }
            Ok(MultiTurnStreamItem::CompletionCall(_)) => {
                // 内部使用统计，前端不关心。
            }
            Ok(MultiTurnStreamItem::ModelTurnRetried { turn }) => {
                // 短暂 banner，不中断流。
                let _ = tx.send(AgentEvent::Error {
                    turn_id,
                    message: format!("LLM 内部重试 turn {turn}"),
                });
            }
            Ok(MultiTurnStreamItem::FinalResponse(_resp)) => {
                // 落库 assistant 消息：Rig Message::Assistant 的 content 是
                // Vec<AssistantContent>，文本先放，再追加全部 tool_call。
                let mut content = Vec::new();
                if !accumulated_text.is_empty() {
                    content.push(AssistantContent::text(accumulated_text.clone()));
                }
                content.extend(
                    assistant_tool_calls
                        .iter()
                        .cloned()
                        .map(AssistantContent::ToolCall),
                );
                if content.is_empty() {
                    content.push(AssistantContent::text(String::new()));
                }
                let assistant_msg = AgentMessage::Assistant {
                    id: None,
                    content,
                };
                let record = AgentMessageRecord {
                    id: assistant_id,
                    session_id,
                    message: assistant_msg,
                    created_at: chrono::Utc::now(),
                };
                if let Err(e) = services.agent.append_record(record) {
                    let _ = tx.send(AgentEvent::Error {
                        turn_id,
                        message: format!("assistant 落库失败: {e}"),
                    });
                }
                let _ = tx.send(AgentEvent::Done {
                    turn_id,
                    message_id: assistant_id,
                });
            }
            Err(e) => {
                let _ = tx.send(AgentEvent::Error {
                    turn_id,
                    message: format!("stream error: {e}"),
                });
                services.agent_turns.finish(turn_id);
                return;
            }
        }
    }

    services.agent_turns.finish(turn_id);
}