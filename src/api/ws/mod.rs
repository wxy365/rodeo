//! WebSocket 推送通道。
//!
//! 模块内文件：
//! - `message.rs` — `WsMessage` 协议枚举
//! - `hub.rs` — per-user 广播中枢
//! - `handler.rs` — axum upgrade handler + per-connection task

pub mod handler;
pub mod hub;
pub mod message;

pub use handler::ws_handler;
pub use hub::WsHub;
pub use message::WsMessage;
