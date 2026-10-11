//! WebSocket 协议消息。
//!
//! 服务端 → 客户端单向通信（v1）。后续在末尾追加变体即可（serde tag 兼容）：
//!   - `ViewInvalidated` / `PresenceUpdate` / `EditIndicator`（spec §11）

use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsMessage {
    /// 握手后服务端首推。客户端据此确认已连接，并可选地用来校时。
    #[serde(rename = "hello")]
    Hello {
        user_id: Ulid,
        /// unix seconds。客户端可以用来校时。
        server_time: i64,
    },

    /// 通知用户有新消息。客户端收到后调 GraphQL `unreadMessages` 拉清单。
    /// 只推消息 ID，不推完整 Message 体——避免在 WS 与 GraphQL 之间重复定义
    /// Message 形状与 PII 剥离逻辑。
    #[serde(rename = "new_message")]
    NewMessage { message_id: Ulid },

    /// 服务端单向心跳（每 30s）。客户端不需要回包；服务端判活靠「写 Pong
    /// 进 Sink 时是否 BrokenPipe」。详见 spec §5.3。
    #[serde(rename = "pong")]
    Pong,
}