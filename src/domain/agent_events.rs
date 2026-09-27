use serde::Serialize;
use ulid::Ulid;

use crate::domain::SideEffect;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Delta { turn_id: Ulid, content: String },
    ToolCall { turn_id: Ulid, name: String, args: serde_json::Value },
    ToolResult { turn_id: Ulid, name: String, ok: bool, preview: String },
    SideEffect { turn_id: Ulid, side_effect: SideEffect },
    Done { turn_id: Ulid, message_id: Ulid },
    Error { turn_id: Ulid, message: String },
}
