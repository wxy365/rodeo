//! Agent 编排循环 + turn 注册表。

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use futures_util::StreamExt;
use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use crate::domain::agent_events::AgentEvent;
use crate::domain::{AgentMessage, Role, ToolCall};
use crate::error::AppError;
use crate::service::agent::exec::execute_tool;
use crate::service::ai::{agent_system_prompt, to_chat_messages};
use crate::service::{AuthContext, Services};

/// 单 turn 的运行句柄：task + 它的 broadcast sender。
struct TurnHandle {
    #[allow(dead_code)]
    handle: tokio::task::JoinHandle<()>,
    sender: broadcast::Sender<AgentEvent>,
}

/// 注册 session/turn 的活跃状态。同 session 同时只跑一个 turn。
pub struct SessionTurns {
    turns: Mutex<HashMap<Ulid, TurnHandle>>,
    by_session: Mutex<HashMap<Ulid, Ulid>>,
    cancel_tokens: Mutex<HashMap<Ulid, CancellationToken>>,
}

impl Default for SessionTurns {
    fn default() -> Self {
        Self {
            turns: Mutex::new(HashMap::new()),
            by_session: Mutex::new(HashMap::new()),
            cancel_tokens: Mutex::new(HashMap::new()),
        }
    }
}

impl SessionTurns {
    /// 起一个新 turn；若同 session 已有 turn，先 cancel 旧。
    pub fn start_or_replace(
        self: &Arc<Self>,
        session_id: Ulid,
        turn_id: Ulid,
        runner: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), AppError> {
        if let Some(old) = self.by_session.lock().remove(&session_id) {
            self.turns.lock().remove(&old);
            if let Some(tok) = self.cancel_tokens.lock().remove(&old) {
                tok.cancel();
            }
        }
        let (tx, _) = broadcast::channel(128);
        let cancel = CancellationToken::new();
        let handle = tokio::spawn(runner);
        self.turns.lock().insert(turn_id, TurnHandle { handle, sender: tx.clone() });
        self.by_session.lock().insert(session_id, turn_id);
        self.cancel_tokens.lock().insert(turn_id, cancel);
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

    pub fn cancel_token(&self, turn_id: Ulid) -> CancellationToken {
        self.cancel_tokens
            .lock()
            .get(&turn_id)
            .cloned()
            .unwrap_or_else(CancellationToken::new)
    }

    /// 在 start_or_replace 之前调用，确保后续 cancel_token 不会被替换。
    pub fn cancel_token_for(&self, turn_id: Ulid) -> CancellationToken {
        self.cancel_tokens
            .lock()
            .entry(turn_id)
            .or_insert_with(CancellationToken::new)
            .clone()
    }

    pub fn session_of(&self, turn_id: Ulid) -> Option<Ulid> {
        let bs = self.by_session.lock();
        bs.iter().find_map(|(&sid, &tid)| (tid == turn_id).then_some(sid))
    }

    pub fn finish(&self, turn_id: Ulid) {
        self.turns.lock().remove(&turn_id);
        self.cancel_tokens.lock().remove(&turn_id);
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
pub async fn run_turn(
    services: Arc<Services>,
    auth: AuthContext,
    session_id: Ulid,
    turn_id: Ulid,
    cancel: CancellationToken,
) {
    let max_turns = services.config.agent.max_tool_calls_per_turn;
    let max_history = services.config.agent.max_history_turns;

    // runner 必须拿到 turn 的 broadcast sender 才能 emit event。
    // 拿不到（已被 finish 或 turn 被替换）就直接退出。
    let Some(tx) = services.agent_turns.turn_sender(turn_id) else {
        return;
    };

    let client = match services.ai_client.as_ref() {
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

    for _ in 0..max_turns {
        if cancel.is_cancelled() {
            break;
        }

        let history = match services
            .agent
            .list_messages(auth.account_id, session_id, max_history)
        {
            Ok(h) => h,
            Err(e) => {
                let _ = tx.send(AgentEvent::Error {
                    turn_id,
                    message: e.to_string(),
                });
                break;
            }
        };
        // 占位：用 workspace_id 字符串作为 ws 标题——取真实标题要查 Workspace 表，
        // 留到下一轮再做（不影响功能）。
        let ws_id_str = workspace_id.to_string();
        let mut messages = vec![agent_system_prompt(&ws_id_str, &ws_id_str)];
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
            tokio::select! {
                _ = cancel.cancelled() => break,
                chunk = stream.next() => match chunk {
                    Some(Ok(crate::service::ai::StreamChunk::Delta(s))) => {
                        content.push_str(&s);
                        let _ = tx.send(AgentEvent::Delta { turn_id, content: s });
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
        }

        if let Some(msg) = stream_error {
            let _ = tx.send(AgentEvent::Error {
                turn_id,
                message: msg,
            });
            services.agent_turns.finish(turn_id);
            return;
        }
        if cancel.is_cancelled() {
            break;
        }

        // 落库 assistant 消息。
        let assistant = AgentMessage {
            id: Ulid::new(),
            session_id,
            role: Role::Assistant,
            content: content.clone(),
            tool_calls: final_tool_calls
                .iter()
                .map(|tc| ToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    args: serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Null),
                    result_preview: String::new(),
                    ok: false,
                })
                .collect(),
            tool_call_id: None,
            created_at: Utc::now(),
        };
        if let Err(e) = services.agent.append_message(assistant.clone()) {
            let _ = tx.send(AgentEvent::Error {
                turn_id,
                message: e.to_string(),
            });
            break;
        }

        // 没有 tool_call：本轮就是答案。落库 last_message_at 然后发 Done。
        if final_tool_calls.is_empty() {
            let _ = services.agent.update_session_meta(
                auth.account_id,
                session_id,
                None,
                Some(assistant.created_at),
            );
            let _ = tx.send(AgentEvent::Done {
                turn_id,
                message_id: assistant.id,
            });
            services.agent_turns.finish(turn_id);
            return;
        }

        // 串行执行每个 tool_call：发 ToolCall → execute → 发 ToolResult/SideEffect → 落 tool 消息。
        for tc in final_tool_calls {
            if cancel.is_cancelled() {
                break;
            }
            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                .unwrap_or(serde_json::Value::Null);
            let _ = tx.send(AgentEvent::ToolCall {
                turn_id,
                name: tc.function.name.clone(),
                args: args.clone(),
            });
            let outcome = execute_tool(
                &services.schema,
                &services,
                &auth,
                workspace_id,
                &tc.function.name,
                args,
            )
            .await
            .unwrap_or(crate::service::agent::exec::ToolOutcome {
                ok: false,
                result_preview: "工具调用失败".to_string(),
                side_effect: None,
            });
            let _ = tx.send(AgentEvent::ToolResult {
                turn_id,
                name: tc.function.name.clone(),
                ok: outcome.ok,
                preview: outcome.result_preview.clone(),
            });
            if let Some(se) = &outcome.side_effect {
                let _ = tx.send(AgentEvent::SideEffect {
                    turn_id,
                    side_effect: se.clone(),
                });
            }
            let tool_msg = AgentMessage {
                id: Ulid::new(),
                session_id,
                role: Role::Tool,
                content: outcome.result_preview,
                tool_calls: vec![],
                tool_call_id: Some(tc.id.clone()),
                created_at: Utc::now(),
            };
            let _ = services.agent.append_message(tool_msg);
        }
    }

    if !cancel.is_cancelled() {
        let _ = tx.send(AgentEvent::Error {
            turn_id,
            message: format!("超过最大 turn 数 ({max_turns})"),
        });
    }
    services.agent_turns.finish(turn_id);
}
