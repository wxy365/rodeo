use std::collections::HashSet;
use std::sync::Arc;

use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, Message};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::workspace::WorkspaceService;
use crate::storage::{cf, keys, BatchOp, DocStore};

#[derive(Clone)]
pub struct MessageService {
    store: Arc<DocStore>,
    /// 用来按工作空间成员列表做 `@姓名` → account_id 反查。
    /// 字段标 `pub(crate)`：`EntryService::dispatch_entry_mentions` 会跨模块读它
    /// （`self.messages.workspaces.list_members(...)`）。
    pub(crate) workspaces: WorkspaceService,
}

impl MessageService {
    pub fn new(store: Arc<DocStore>, workspaces: WorkspaceService) -> Self {
        Self { store, workspaces }
    }

    /// 写一条消息。`actor_name` 由调用方传入——避免 MessageService 反向依赖 AuthService
    /// 引发 service 间的循环。内部用单条 `write_batch`：消息本体 + 审计。
    pub fn send(
        &self,
        actor: Ulid,
        actor_name: &str,
        workspace_id: Ulid,
        entry_code: &str,
        source_type: &str,
        source_id: Option<Ulid>,
        preview: &str,
        recipient_id: Ulid,
    ) -> Result<Message, AppError> {
        // 不给自己发：调用方通常已经过滤，这里再兜一次。
        if actor == recipient_id {
            return Err(AppError::InvalidQuery("不能给自己发消息".to_string()));
        }
        // 收件人必须是该工作空间成员。允许外人触发消息会让通知系统被人借名发垃圾。
        if self.workspaces.get_member(workspace_id, recipient_id)?.is_none() {
            return Err(AppError::InvalidQuery(
                "收件人不是该工作空间成员".to_string(),
            ));
        }
        let msg = Message::new(
            recipient_id,
            workspace_id,
            entry_code.to_string(),
            source_type,
            source_id,
            actor,
            actor_name.to_string(),
            preview.to_string(),
        );
        let audit = AuditLog::new(
            AuditAction::MessageCreated,
            actor,
            "message",
            &msg.id.to_string(),
            Some(workspace_id),
            None,
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::MESSAGES_BY_RECIPIENT,
            keys::message_key(recipient_id, msg.created_at, msg.id),
            &msg,
        )?);
        self.store.write_batch(ops)?;
        Ok(msg)
    }

    /// 拉某用户最近的消息（最新在前）。`limit = 0` 表示不限。
    pub fn list_for_recipient(
        &self,
        account_id: Ulid,
        limit: usize,
    ) -> Result<Vec<Message>, AppError> {
        let prefix = account_id.to_bytes().to_vec();
        let rows = self.store.scan_prefix(cf::MESSAGES_BY_RECIPIENT, &prefix)?;
        let mut out = Vec::with_capacity(rows.len());
        for (_k, v) in rows {
            let m: Message = bincode::deserialize(&v)?;
            out.push(m);
            if limit > 0 && out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// 未读消息数。前缀扫描整个分区然后在内存里数 read=false 的行；
    /// 账号级消息量远在阈值下，无须二级索引。
    pub fn unread_count(&self, account_id: Ulid) -> Result<u64, AppError> {
        let prefix = account_id.to_bytes().to_vec();
        let rows = self.store.scan_prefix(cf::MESSAGES_BY_RECIPIENT, &prefix)?;
        let mut n = 0u64;
        for (_k, v) in rows {
            if let Ok(m) = bincode::deserialize::<Message>(&v) {
                if !m.read {
                    n += 1;
                }
            }
        }
        Ok(n)
    }

    /// 单条标记已读。`recipient_id` 必须等于 `account_id`：防止传别人的 message_id
    /// 把别人的消息误标。前缀扫描拿到对应记录——单条 + 单用户，分区很小。
    pub fn mark_read(&self, account_id: Ulid, message_id: Ulid) -> Result<(), AppError> {
        let prefix = account_id.to_bytes().to_vec();
        let rows = self.store.scan_prefix(cf::MESSAGES_BY_RECIPIENT, &prefix)?;
        for (k, v) in rows {
            let mut m: Message = bincode::deserialize(&v)?;
            if m.id == message_id {
                m.read = true;
                self.store
                    .put_raw(cf::MESSAGES_BY_RECIPIENT, &k, &bincode::serialize(&m)?)?;
                return Ok(());
            }
        }
        Err(AppError::NotFound)
    }

    /// 全部标已读。前缀扫描 + 一次 `write_batch`。
    pub fn mark_all_read(&self, account_id: Ulid) -> Result<(), AppError> {
        let prefix = account_id.to_bytes().to_vec();
        let rows = self.store.scan_prefix(cf::MESSAGES_BY_RECIPIENT, &prefix)?;
        let mut ops = Vec::with_capacity(rows.len());
        for (k, v) in rows {
            let mut m: Message = bincode::deserialize(&v)?;
            if m.read {
                continue;
            }
            m.read = true;
            ops.push(BatchOp::put_raw(
                cf::MESSAGES_BY_RECIPIENT,
                k,
                bincode::serialize(&m)?,
            ));
        }
        if !ops.is_empty() {
            self.store.write_batch(ops)?;
        }
        Ok(())
    }

    /// 编辑时用：列出「针对这个 source 已经通知过的所有 recipient_id」。
    /// 用来求 `new_mentions - old_notified` 避免重复发消息。
    pub fn notified_recipients(
        &self,
        workspace_id: Ulid,
        entry_code: &str,
        source_type: &str,
        source_id: Option<Ulid>,
    ) -> Result<HashSet<Ulid>, AppError> {
        let mut out = HashSet::new();
        // 没有按 (workspace, entry) 的二级索引，只能扫全表。
        // 账号级消息分区里 source_type+source_id 是稀疏的，但消息量级仍小。
        let rows = self.store.scan_prefix(cf::MESSAGES_BY_RECIPIENT, b"")?;
        for (_k, v) in rows {
            let m: Message = match bincode::deserialize(&v) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if m.workspace_id != workspace_id || m.entry_code != entry_code {
                continue;
            }
            if m.source_type != source_type || m.source_id != source_id {
                continue;
            }
            out.insert(m.recipient_id);
        }
        Ok(out)
    }
}
