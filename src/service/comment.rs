use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, Comment};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::entry::EntryService;
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct CommentService {
    store: Arc<DocStore>,
    /// 单向依赖：评论变更要推进 Entry 的更新时间并重建检索文档。
    /// `EntryService` 不感知评论，因此不构成循环。
    entries: EntryService,
}

impl CommentService {
    pub fn new(store: Arc<DocStore>, entries: EntryService) -> Self {
        Self { store, entries }
    }

    /// 某条目的全部评论。`comment_key` 的 ULID 后缀保证扫描顺序即时间升序。
    pub fn list(&self, entry_code: &str) -> Result<Vec<Comment>, AppError> {
        self.store
            .scan_prefix(cf::COMMENTS, entry_code.as_bytes())?
            .into_iter()
            .map(|(_, v)| bincode::deserialize::<Comment>(&v).map_err(Into::into))
            .collect()
    }

    /// 只数条数，不反序列化正文——视图表格按页取计数时走这条。
    pub fn count(&self, entry_code: &str) -> Result<usize, AppError> {
        Ok(self
            .store
            .scan_prefix(cf::COMMENTS, entry_code.as_bytes())?
            .len())
    }

    pub fn get(&self, entry_code: &str, id: Ulid) -> Result<Option<Comment>, AppError> {
        self.store
            .get(cf::COMMENTS, &keys::comment_key(entry_code, id))
    }

    pub fn create(&self, actor: Ulid, entry_code: &str, body: &str) -> Result<Comment, AppError> {
        if is_blank_body(body) {
            return Err(AppError::InvalidQuery("评论内容不能为空".to_string()));
        }
        let mut entry = self.entries.get(entry_code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        let comment = Comment::new(
            entry_code.to_string(),
            entry.workspace_id,
            body.trim().to_string(),
            actor,
        );
        // 发言是条目上的活动：推进 updated_at，让条目回到「按更新时间倒序」的最前。
        // 由此带来的副作用是，同时正在编辑详情的人保存时会撞上乐观并发冲突——
        // 属低频场景，前端已有冲突后重载的处理路径。
        entry.updated_by = actor;
        entry.updated_at = Utc::now();

        let audit = AuditLog::new(
            AuditAction::CommentCreated,
            actor,
            "comment",
            entry_code,
            Some(entry.workspace_id),
            None,
            Some(serde_json::to_string(&comment).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::COMMENTS,
            keys::comment_key(entry_code, comment.id),
            &comment,
        )?);
        ops.push(BatchOp::put(cf::ENTRIES, entry_code.as_bytes().to_vec(), &entry)?);
        self.store.write_batch(ops)?;
        self.entries.reindex_by_code(entry_code)?;
        Ok(comment)
    }

    pub fn update(
        &self,
        actor: Ulid,
        entry_code: &str,
        id: Ulid,
        body: &str,
    ) -> Result<Comment, AppError> {
        if is_blank_body(body) {
            return Err(AppError::InvalidQuery("评论内容不能为空".to_string()));
        }
        let mut comment = self.get(entry_code, id)?.ok_or(AppError::NotFound)?;
        if comment.created_by != actor {
            return Err(AppError::Forbidden);
        }
        let before = serde_json::to_string(&comment).unwrap_or_default();
        comment.body = body.trim().to_string();
        comment.updated_by = actor;
        comment.updated_at = Utc::now();
        let audit = AuditLog::new(
            AuditAction::CommentUpdated,
            actor,
            "comment",
            entry_code,
            Some(comment.workspace_id),
            Some(before),
            Some(serde_json::to_string(&comment).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::COMMENTS,
            keys::comment_key(entry_code, id),
            &comment,
        )?);
        self.store.write_batch(ops)?;
        // 编辑不推进 Entry.updated_at：纠错不该把条目顶到列表最前；但检索要跟着更新。
        self.entries.reindex_by_code(entry_code)?;
        Ok(comment)
    }

    /// `can_moderate` 由调用方按工作空间角色算好：删他人评论需要 Maintainer+，
    /// 作者本人即使被降级为 Reader 也仍可撤回自己的内容。
    pub fn delete(
        &self,
        actor: Ulid,
        entry_code: &str,
        id: Ulid,
        can_moderate: bool,
    ) -> Result<(), AppError> {
        let comment = self.get(entry_code, id)?.ok_or(AppError::NotFound)?;
        if comment.created_by != actor && !can_moderate {
            return Err(AppError::Forbidden);
        }
        let audit = AuditLog::new(
            AuditAction::CommentDeleted,
            actor,
            "comment",
            entry_code,
            Some(comment.workspace_id),
            Some(serde_json::to_string(&comment).unwrap_or_default()),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(
            cf::COMMENTS,
            keys::comment_key(entry_code, id),
        ));
        self.store.write_batch(ops)?;
        self.entries.reindex_by_code(entry_code)?;
        Ok(())
    }
}

/// 只有空白内容的评论不算评论。直接用检索侧的 Delta 抽文本逻辑，
/// 这样「Delta 里只有空白片段」和「纯文本全是空格」两种情况一并覆盖。
/// 图片嵌入没有文本但显然不是「空内容」，单独认一下。
fn is_blank_body(body: &str) -> bool {
    if has_image_embed(body) {
        return false;
    }
    crate::service::search::strip_rich_text(body).trim().is_empty()
}

/// Delta 里是否存在 `{"insert": {"image": …}}` 形式的嵌入。
/// 只认 `image` 这一个格式：其他嵌入（如将来的分隔线）不该绕过「必须有内容」的判定。
fn has_image_embed(body: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let ops = v
        .get("ops")
        .and_then(|o| o.as_array())
        .cloned()
        .or_else(|| v.as_array().cloned());
    ops.map(|ops| {
        ops.iter().any(|op| {
            op.get("insert")
                .and_then(|i| i.get("image"))
                .is_some()
        })
    })
    .unwrap_or(false)
}
