use serde::Serialize;
use ulid::Ulid;

use crate::domain::SideEffect;

/// Tool 执行结果的错误分类。前端据此渲染不同的提示（红 = 服务端故障、
/// 橙 = 业务拒绝、黄 = 参数错），未来 runner 也可据此决定是否 abort turn。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorKind {
    Ok,
    BadArgs,
    Rejected,
    ServerError,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Delta { turn_id: Ulid, content: String },
    ToolCall { turn_id: Ulid, name: String, args: serde_json::Value },
    ToolResult {
        turn_id: Ulid,
        name: String,
        ok: bool,
        kind: ToolErrorKind,
        preview: String,
    },
    SideEffect { turn_id: Ulid, side_effect: SideEffect },
    Done { turn_id: Ulid, message_id: Ulid },
    Error { turn_id: Ulid, message: String },
}
