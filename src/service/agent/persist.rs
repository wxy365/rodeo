use std::sync::Arc;

use chrono::{DateTime, Utc};
use ulid::Ulid;

use crate::domain::{AgentMessage, AgentMessageRecord, AgentSession};
use crate::error::AppError;
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct AgentService {
    store: Arc<DocStore>,
}

impl AgentService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    // ---- 会话：保留现有签名，让 graphql.rs / runner.rs 的 2-arg / 3-arg
    //      调用点继续编译；Task 5/11 才会按新形状统一收口。

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

    /// 删会话：先按 (user_id, session_id) 校验所有权，越权返回 false。
    /// 删除 session 行 + 级联删全部 message。
    pub fn delete_session(&self, user_id: Ulid, session_id: Ulid) -> Result<bool, AppError> {
        let session = match self.get_session(user_id, session_id)? {
            Some(s) => s,
            None => return Ok(false),
        };
        let mut ops = vec![BatchOp::delete(
            cf::AGENT_SESSIONS,
            keys::agent_session_key(user_id, session.workspace_id, session_id),
        )];
        // 级联删所有消息
        for (k, _) in self
            .store
            .scan_prefix(cf::AGENT_MESSAGES, &session_id.to_bytes())?
        {
            ops.push(BatchOp::delete(cf::AGENT_MESSAGES, k));
        }
        self.store.write_batch(ops)?;
        Ok(true)
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

    // ---- 消息：直接读写 `rig::message::Message`；`AgentMessageRecord` 只在
    //      落库时生成 / 取回时一并返回，避免把 rig 类型漏到外层调用方。

    /// 落 Rig Message：每条记录自己生成 ULID（rig Message 内部不带）。
    /// 返回带 row id 的 record，调用方要把它再喂给 LLM 时可以引用 record.id。
    pub fn append_message(
        &self,
        session_id: Ulid,
        msg: AgentMessage,
    ) -> Result<AgentMessageRecord, AppError> {
        let record = AgentMessageRecord {
            id: Ulid::new(),
            session_id,
            message: msg,
            created_at: Utc::now(),
        };
        let key = keys::agent_message_key(session_id, record.id);
        self.store.put(cf::AGENT_MESSAGES, &key, &record)?;
        Ok(record)
    }

    /// 列出会话内全部消息，按 ULID 时间升序（ULID 头部是时间戳），截到 limit 条。
    /// 返回 `Vec<AgentMessage>`（即 `Vec<rig::message::Message>`）——调用方
    /// 直接拿到 rig 类型，不需要再剥 record 壳。
    /// 不做归属校验：调用方需先 `get_session(user_id, session_id)` 校验所有权。
    pub fn list_messages(
        &self,
        session_id: Ulid,
        limit: usize,
    ) -> Result<Vec<AgentMessage>, AppError> {
        let mut records = self.collect_records(session_id)?;
        records.sort_by_key(|e| e.id);
        Ok(records
            .into_iter()
            .rev()
            .take(limit)
            .rev()
            .map(|r| r.message)
            .collect())
    }

    /// 落 `AgentMessageRecord`（row id 已由调用方生成，例如要把 SSE 流回来的
    /// assistant id 同步到 DB 行）。
    pub fn append_record(&self, record: AgentMessageRecord) -> Result<(), AppError> {
        let key = keys::agent_message_key(record.session_id, record.id);
        self.store.put(cf::AGENT_MESSAGES, &key, &record)?;
        Ok(())
    }

    /// 拿原始 record（含 id / session_id / created_at），给 GraphQL 层填充
    /// `GqlAgentMessage` 的非 rig 字段用。不做归属校验。
    pub fn list_message_records(
        &self,
        session_id: Ulid,
        limit: usize,
    ) -> Result<Vec<AgentMessageRecord>, AppError> {
        let mut records = self.collect_records(session_id)?;
        records.sort_by_key(|e| e.created_at);
        if records.len() > limit {
            // 保留最新的 limit 条；前端展示时翻转
            records = records.split_off(records.len() - limit);
        }
        Ok(records)
    }

    /// `list_messages` / `list_message_records` 共享：扫前缀 → 反序列化。
    /// 反序列化失败的 row 视为「历史遗留 / bincode 旧版本不兼容」，跳过并
    /// 打 warn——以前 `filter_map(|..| .ok())` 静默吞掉，让用户看到的是
    /// 「消息写进去了但 agentMessages 永远空」这种鬼故事，至少留个信号。
    fn collect_records(
        &self,
        session_id: Ulid,
    ) -> Result<Vec<AgentMessageRecord>, AppError> {
        let mut records = Vec::new();
        for (k, v) in self.store.scan_prefix(cf::AGENT_MESSAGES, &session_id.to_bytes())? {
            match bincode::deserialize::<AgentMessageRecord>(&v) {
                Ok(r) => records.push(r),
                Err(e) => {
                    tracing::warn!(
                        target: "rodeo.agent.persist",
                        session_id = %session_id,
                        key = %format!("{:02x?}", &k[..k.len().min(32)]),
                        "跳过无法解码的 agent 消息 row: {e}"
                    );
                }
            }
        }
        Ok(records)
    }

    /// 删掉会话内全部 message（不动 session 行；session 删走 `delete_session`）。
    /// 不做归属校验：调用方需先校验 session 归属。
    pub fn purge_session_messages(&self, session_id: Ulid) -> Result<(), AppError> {
        let mut ops = Vec::new();
        for (key, _) in self
            .store
            .scan_prefix(cf::AGENT_MESSAGES, &session_id.to_bytes())?
        {
            ops.push(BatchOp::delete(cf::AGENT_MESSAGES, key));
        }
        if !ops.is_empty() {
            self.store.write_batch(ops)?;
        }
        Ok(())
    }
}
