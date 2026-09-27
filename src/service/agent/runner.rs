//! Agent 编排循环 + turn 注册表。

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use ulid::Ulid;

use crate::domain::agent_events::AgentEvent;
use crate::error::AppError;

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