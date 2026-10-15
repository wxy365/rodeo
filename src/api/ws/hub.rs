//! Per-user WebSocket 推送中枢。
//!
//! 拓扑：`account_id → broadcast::Sender<WsMessage>`。
//! 每个用户的所有活跃连接共用同一个 broadcast channel；同账号多 tab / 多设备
//! 自然支持。注册新连接给一个账号时第一次懒建 sender；后续复用同一 sender。
//!
//! 不引入 DashMap：`tokio::sync::Mutex<HashMap>` 足够，不增加依赖、不破坏
//! rustls 单 provider 编译约束（CLAUDE.md §"顶层 crate 与特性"）。
//!
//! 业务调用点收敛在 `MessageService::send`（spec §7.1）：4 个新触发点不需要
//! 持 hub 引用。

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::{broadcast, Mutex};
use ulid::Ulid;

use super::message::WsMessage;

/// 单用户的广播 channel 容量。单用户 1 分钟内 128 条新消息已是异常，溢出时
/// `send` 返回 `RecvError::Lagged(N)`，handler 选择丢弃旧消息并继续。
const USER_CHANNEL_CAPACITY: usize = 128;

pub struct WsHub {
    /// `account_id → 该用户当前所有活跃连接共用的 broadcast::Sender`
    users: Mutex<HashMap<Ulid, broadcast::Sender<WsMessage>>>,
}

impl WsHub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            users: Mutex::new(HashMap::new()),
        })
    }

    /// 给一个 account 注册一个新连接，返回 receiver。
    /// 第一次为某 user 调用此方法时新建 `broadcast::Sender`；后续调用复用
    /// 同一个 sender（多 tab / 多设备自然支持）。
    pub async fn subscribe(&self, user_id: Ulid) -> broadcast::Receiver<WsMessage> {
        let mut users = self.users.lock().await;
        let sender = users
            .entry(user_id)
            .or_insert_with(|| broadcast::channel(USER_CHANNEL_CAPACITY).0);
        sender.subscribe()
    }

    /// 连接关闭时调用：当 user 没有任何 receiver 时从 map 中移除 sender。
    /// 不存在时静默 no-op。
    pub async fn unsubscribe(&self, user_id: Ulid) {
        let mut users = self.users.lock().await;
        if let Some(sender) = users.get(&user_id) {
            if sender.receiver_count() == 0 {
                users.remove(&user_id);
            }
        }
    }

    /// 给单个用户的所有活跃连接广播。best-effort：`send` 失败（无 receiver
    /// 或 Lagged）时忽略——客户端会走 30s polling 兜底拉一次（见 spec §6.4）。
    pub async fn push_to(&self, user_id: Ulid, msg: WsMessage) {
        let users = self.users.lock().await;
        if let Some(sender) = users.get(&user_id) {
            let _ = sender.send(msg);
        }
    }
}
