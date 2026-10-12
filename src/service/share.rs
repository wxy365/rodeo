//! Entry 匿名只读分享。
//!
//! 设计见 `spec/.../gleaming-inventing-duckling.md`：share.id（ULID）即 token；
//! 公开 URL `/share/{id}` 是 capability；token 失效（不存在 / 撤销 / Entry 删了）
//! 一律 `NotFound` 兜底，**不**做存在性探测口。

use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{
    Attachment, AuditAction, AuditLog, Comment, Entry, EntryShare, LabelSchema, Workspace,
};
use crate::error::AppError;
use crate::storage::{cf, keys, BatchOp, DocStore};

use super::audit::audit_ops;

/// 评论的公开投影：仅 id / body / 作者名 / 时间，**不**带 author_id / email。
/// 给 `ShareService::resolve_full` 用，避免把评论作者字段暴露给匿名读者。
#[derive(Debug, Clone)]
pub struct PublicCommentData {
    pub id: Ulid,
    pub body: String,
    pub author_name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// `created_at != updated_at`：编辑过的评论，前端会展示「已编辑」标记。
    pub edited: bool,
}

/// 工作空间成员的公开投影：id + name。email / role / status 不暴露。
#[derive(Debug, Clone)]
pub struct PublicMemberData {
    pub id: Ulid,
    pub name: String,
}

/// `/share/{token}` 拉一次拿到的全量数据。`resolve_full` 用一次 token 校验
/// + 一次前缀扫描（评论列表）、若干 `get`（workspace / 附件 / 标签），
/// GraphQL 端就不必再分多次 round-trip；隐私上各字段都是经裁剪的，
/// 没有「workspace_id」等与匿名读者无关的内部字段。
#[derive(Debug, Clone)]
pub struct EntryFullData {
    pub entry: Entry,
    #[allow(dead_code)]
    pub share: EntryShare,
    pub workspace: Workspace,
    pub comments: Vec<PublicCommentData>,
    pub attachments: Vec<Attachment>,
    pub label_schemas: Vec<LabelSchema>,
    pub members: Vec<PublicMemberData>,
}

pub struct ShareService {
    store: Arc<DocStore>,
}

impl ShareService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    /// 创建分享。调用方应在 GraphQL 层完成 `require_member(workspace_id)` 守卫：
    /// 服务层不重复实现 workspace 成员检查，避免双重判定漂移；并靠
    /// `Entry.workspace_id == workspace_id` 兜底防调用方传错 ws。
    ///
    /// 校验顺序有意：先确认 Entry 存在 + 属于该 workspace + 没被软删，再写。
    /// 不存在 / 不属于 / 删了 三种情形均 `NotFound`——不暴露区分「不属于」与
    /// 「删了」。
    pub fn create(
        &self,
        entry_code: &str,
        workspace_id: Ulid,
        actor: Ulid,
    ) -> Result<EntryShare, AppError> {
        let entry = self
            .store
            .get::<Entry>(cf::ENTRIES, entry_code.as_bytes())?
            .ok_or(AppError::NotFound)?;
        if entry.workspace_id != workspace_id || entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        let share = EntryShare::new(workspace_id, entry_code.to_string(), actor);
        let id_bytes = keys::entry_share_key(share.id);
        let mut ops = vec![
            BatchOp::put(cf::ENTRY_SHARES, id_bytes.to_vec(), &share)?,
            BatchOp::put_raw(
                cf::ENTRY_SHARES_BY_ENTRY,
                keys::entry_share_by_entry_key(workspace_id, entry_code, share.id),
                Vec::new(),
            ),
        ];
        let audit = AuditLog::new(
            AuditAction::EntryShareCreated,
            actor,
            "entry_share",
            &share.id.to_string(),
            Some(workspace_id),
            None,
            Some(serde_json::to_string(&share).unwrap_or_default()),
        );
        ops.extend(audit_ops(&audit)?);
        self.store.write_batch(ops)?;
        Ok(share)
    }

    /// 列出某条目的全部 share（含已撤销的；UI 自行渲染「已撤销」标记）。
    /// ULID 后缀字节序即时间升序；share 数量小（同一 Entry 通常 < 10）就不
    /// 额外排了——依赖 ULID 自然的单调性。
    pub fn list(
        &self,
        workspace_id: Ulid,
        entry_code: &str,
    ) -> Result<Vec<EntryShare>, AppError> {
        let prefix = keys::entry_share_by_entry_prefix(workspace_id, entry_code);
        let mut out = Vec::new();
        for (k, _) in self.store.scan_prefix(cf::ENTRY_SHARES_BY_ENTRY, &prefix)? {
            let Some(id_bytes) = k.get(prefix.len()..).and_then(|s| s.get(..16)) else {
                continue;
            };
            let Ok(arr) = <[u8; 16]>::try_from(id_bytes) else {
                continue;
            };
            if let Some(s) = self.store.get::<EntryShare>(cf::ENTRY_SHARES, &arr)? {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// 软撤销。校验 share 属于该 workspace——防越权：账号 A 在 ws1 是成员
    /// 但拿到 ws2 的 share id 仍会被拒。**幂等**：重复撤销直接成功，不重写
    /// `revoked_by` / `revoked_at`，也不重复写审计。
    pub fn revoke(
        &self,
        share_id: Ulid,
        workspace_id: Ulid,
        actor: Ulid,
    ) -> Result<(), AppError> {
        let mut share = self
            .store
            .get::<EntryShare>(cf::ENTRY_SHARES, &share_id.to_bytes())?
            .ok_or(AppError::NotFound)?;
        if share.workspace_id != workspace_id {
            return Err(AppError::Forbidden);
        }
        if share.revoked {
            return Ok(());
        }
        share.revoked = true;
        share.revoked_by = Some(actor);
        share.revoked_at = Some(Utc::now());
        let mut ops = vec![BatchOp::put(
            cf::ENTRY_SHARES,
            keys::entry_share_key(share_id).to_vec(),
            &share,
        )?];
        let audit = AuditLog::new(
            AuditAction::EntryShareRevoked,
            actor,
            "entry_share",
            &share_id.to_string(),
            Some(workspace_id),
            Some(serde_json::to_string(&share).unwrap_or_default()),
            None,
        );
        ops.extend(audit_ops(&audit)?);
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 公开解析：token → (Entry, EntryShare)。所有失败（token 解析错 / 不存在 /
    /// 已撤销 / Entry 不存在 / Entry 软删）一律 `NotFound`——免得成存在性探测。
    pub fn resolve(&self, token: &str) -> Result<(Entry, EntryShare), AppError> {
        let Ok(share_id) = Ulid::from_string(token) else {
            return Err(AppError::NotFound);
        };
        let share = self
            .store
            .get::<EntryShare>(cf::ENTRY_SHARES, &share_id.to_bytes())?
            .ok_or(AppError::NotFound)?;
        if share.revoked {
            return Err(AppError::NotFound);
        }
        let entry = self
            .store
            .get::<Entry>(cf::ENTRIES, share.entry_code.as_bytes())?
            .ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        Ok((entry, share))
    }

    /// 公开解析 + 拉取全量读视图数据。任何一步失败统一 `NotFound`。一次调
    /// 用内做完所有取数，GraphQL 端不必再开多个 round-trip；评论作者名和成员
    /// 名也在这里一次性 resolve（按 `created_by` / `account_id` 单点 `get`，
    /// 不是邻接列表扫描），避免 GraphQL 解析器要挨个 `find_by_id` 再拼。
    pub fn resolve_full(&self, token: &str) -> Result<EntryFullData, AppError> {
        let (entry, share) = self.resolve(token)?;
        let workspace = self
            .store
            .get::<Workspace>(cf::WORKSPACES, &share.workspace_id.to_bytes())?
            .ok_or(AppError::NotFound)?;
        let comments = self.fetch_public_comments(&entry.code)?;
        let attachments = self.fetch_public_attachments(&entry.code)?;
        let label_schemas = self.fetch_public_label_schemas(share.workspace_id)?;
        let members = self.fetch_public_members(share.workspace_id)?;
        Ok(EntryFullData {
            entry,
            share,
            workspace,
            comments,
            attachments,
            label_schemas,
            members,
        })
    }

    /// 拉评论 + 公开投影（不含 author_id / email）。作者账号缺失时退回
    /// `—`（"—"emdash）——评论作者已注销 / 硬删时仍要让这条评论可见，
    /// 不能因为匿名性「我没有名字」而抹掉内容。
    fn fetch_public_comments(&self, entry_code: &str) -> Result<Vec<PublicCommentData>, AppError> {
        let raw = self
            .store
            .scan_prefix(cf::COMMENTS, entry_code.as_bytes())?;
        let mut out = Vec::with_capacity(raw.len());
        for (_, v) in raw {
            let c: Comment = bincode::deserialize(&v)?;
            // account_name 在缺失时回 None；前端按 "—" 兜底。
            let name = self
                .store
                .get::<crate::domain::Account>(cf::ACCOUNTS, &c.created_by.to_bytes())?
                .map(|a| a.name)
                .unwrap_or_else(|| "—".to_string());
            out.push(PublicCommentData {
                id: c.id,
                body: c.body,
                author_name: name,
                created_at: c.created_at,
                updated_at: c.updated_at,
                edited: c.created_at != c.updated_at,
            });
        }
        Ok(out)
    }

    /// 拉附件元数据。`AttachmentService::list` 内部已过滤掉 inline 图片
    /// ——分享页只要「附件」列表，不含正文里嵌入的图片（后者只有作者在
    /// 详情页能看到）。匿名读者拿到 `Attachment` 全文无所谓：id 是
    /// capability URL 的钥匙，作者字段不会泄露。
    fn fetch_public_attachments(
        &self,
        entry_code: &str,
    ) -> Result<Vec<Attachment>, AppError> {
        // 这里走底层 `scan_prefix` 而非 `AttachmentService::list`，因为后者
        // 是 service 方法、`share.rs` 还没有也不该有 `AttachmentService`
        // 依赖（单向：`AttachmentService` 也不该依赖 `ShareService`）。
        // 复制 8 行实现远胜引入循环依赖。
        let mut out = Vec::new();
        for (k, _) in self
            .store
            .scan_prefix(cf::ATTACHMENTS_BY_ENTRY, entry_code.as_bytes())?
        {
            let Some(id_bytes) = k.get(entry_code.len()..).and_then(|s| s.get(..16)) else {
                continue;
            };
            let Ok(arr) = <[u8; 16]>::try_from(id_bytes) else {
                continue;
            };
            if self.store.exists(cf::INLINE_ATTACHMENTS, &arr)? {
                continue;
            }
            if let Some(a) = self
                .store
                .get::<Attachment>(cf::ATTACHMENTS, &arr)?
            {
                out.push(a);
            }
        }
        Ok(out)
    }

    fn fetch_public_label_schemas(
        &self,
        ws_id: Ulid,
    ) -> Result<Vec<LabelSchema>, AppError> {
        let prefix = ws_id.to_bytes();
        let mut out = Vec::new();
        for (_, v) in self.store.scan_prefix(cf::LABEL_SCHEMAS, &prefix)? {
            if let Ok(s) = bincode::deserialize::<LabelSchema>(&v) {
                out.push(s);
            }
        }
        Ok(out)
    }

    /// 拉工作空间成员（公开投影：仅 id + name）。`role` / `status` / `email`
    /// 在 `PublicMemberData` 里已经被裁掉，与匿名读者无关。
    fn fetch_public_members(
        &self,
        ws_id: Ulid,
    ) -> Result<Vec<PublicMemberData>, AppError> {
        let prefix = ws_id.to_bytes();
        let mut out = Vec::new();
        for (k, _) in self.store.scan_prefix(cf::WORKSPACE_MEMBERS, &prefix)? {
            // 32 字节键：前 16 = workspace_id（与 prefix 一致，必中），
            // 后 16 = account_id。
            let Some(id_bytes) = k.get(16..).and_then(|s| s.get(..16)) else {
                continue;
            };
            let Ok(arr) = <[u8; 16]>::try_from(id_bytes) else {
                continue;
            };
            let id = Ulid::from_bytes(arr);
            // 成员行可能已被物理删除但反向索引还在——account 不存在就跳过。
            if let Some(account) = self
                .store
                .get::<crate::domain::Account>(cf::ACCOUNTS, &id.to_bytes())?
            {
                out.push(PublicMemberData {
                    id,
                    name: account.name,
                });
            }
        }
        Ok(out)
    }
}
