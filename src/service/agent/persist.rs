use std::sync::Arc;

use chrono::{DateTime, Utc};
use ulid::Ulid;

use crate::config::Config;
use crate::domain::{AgentMessage, AgentSession};
use crate::error::AppError;
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct AgentService {
    store: Arc<DocStore>,
    #[allow(dead_code)]
    config: Arc<Config>,
}

impl AgentService {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Self {
        Self { store, config }
    }

    pub fn create_session(
        &self,
        user_id: Ulid,
        workspace_id: Ulid,
    ) -> Result<AgentSession, AppError> {
        let s = AgentSession::new(user_id, workspace_id);
        let op = BatchOp::put(
            cf::AGENT_SESSIONS,
            keys::agent_session_key(user_id, workspace_id, s.id),
            &s,
        )?;
        self.store.write_batch(vec![op])?;
        Ok(s)
    }

    pub fn get_session(
        &self,
        user_id: Ulid,
        session_id: Ulid,
    ) -> Result<Option<AgentSession>, AppError> {
        // 跨 user 读直接返回 None；不泄漏存在性。
        // 这里必须用 prefix 扫描来拒绝越权，因为 ws + session_id 都在 key 里。
        let prefix = keys::agent_session_prefix(user_id);
        for (_, v) in self.store.scan_prefix(cf::AGENT_SESSIONS, &prefix)? {
            let s: AgentSession = bincode::deserialize(&v)?;
            if s.id == session_id {
                return Ok(Some(s));
            }
        }
        Ok(None)
    }

    pub fn list_sessions(
        &self,
        user_id: Ulid,
        workspace_id: Ulid,
        limit: usize,
    ) -> Result<Vec<AgentSession>, AppError> {
        let prefix = keys::agent_session_prefix(user_id);
        let mut out = Vec::new();
        for (_, v) in self.store.scan_prefix(cf::AGENT_SESSIONS, &prefix)? {
            let s: AgentSession = bincode::deserialize(&v)?;
            if s.workspace_id != workspace_id {
                continue;
            }
            out.push(s);
            if out.len() >= limit {
                break;
            }
        }
        // 最新的在前：按 last_message_at / updated_at 降序
        out.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(out)
    }

    pub fn delete_session(&self, user_id: Ulid, session_id: Ulid) -> Result<bool, AppError> {
        let session = match self.get_session(user_id, session_id)? {
            Some(s) => s,
            None => return Ok(false),
        };
        // 先删 session
        let mut ops = vec![BatchOp::delete(
            cf::AGENT_SESSIONS,
            keys::agent_session_key(user_id, session.workspace_id, session_id),
        )];
        // 级联删所有消息
        let prefix = keys::agent_message_prefix(session_id);
        for (k, _) in self.store.scan_prefix(cf::AGENT_MESSAGES, &prefix)? {
            ops.push(BatchOp::delete(cf::AGENT_MESSAGES, k));
        }
        self.store.write_batch(ops)?;
        Ok(true)
    }

    pub fn append_message(&self, msg: AgentMessage) -> Result<(), AppError> {
        let op = BatchOp::put(
            cf::AGENT_MESSAGES,
            keys::agent_message_key(msg.session_id, msg.id).to_vec(),
            &msg,
        )?;
        self.store.write_batch(vec![op])?;
        Ok(())
    }

    pub fn list_messages(
        &self,
        user_id: Ulid,
        session_id: Ulid,
        limit: usize,
    ) -> Result<Vec<AgentMessage>, AppError> {
        // 先确认 session 归属
        let _ = self
            .get_session(user_id, session_id)?
            .ok_or(AppError::NotFound)?;
        let prefix = keys::agent_message_prefix(session_id);
        let mut out = Vec::new();
        for (_, v) in self.store.scan_prefix(cf::AGENT_MESSAGES, &prefix)? {
            let m: AgentMessage = bincode::deserialize(&v)?;
            out.push(m);
        }
        out.sort_by_key(|m| m.created_at);
        if out.len() > limit {
            // 保留最新的 limit 条；前端展示时翻转
            out = out.split_off(out.len() - limit);
        }
        Ok(out)
    }

    pub fn update_session_meta(
        &self,
        user_id: Ulid,
        session_id: Ulid,
        title: Option<String>,
        last_message_at: Option<DateTime<Utc>>,
    ) -> Result<(), AppError> {
        let mut s = self
            .get_session(user_id, session_id)?
            .ok_or(AppError::NotFound)?;
        if let Some(t) = title {
            s.title = t;
        }
        s.last_message_at = last_message_at.or(s.last_message_at);
        s.updated_at = Utc::now();
        let op = BatchOp::put(
            cf::AGENT_SESSIONS,
            keys::agent_session_key(user_id, s.workspace_id, s.id),
            &s,
        )?;
        self.store.write_batch(vec![op])?;
        Ok(())
    }
}
