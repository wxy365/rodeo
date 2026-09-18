use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use tokio::io::AsyncWriteExt;
use ulid::Ulid;

use crate::domain::{Attachment, AuditAction, AuditLog};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::entry::EntryService;
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 单文件上限。与 UI 文案「附件（≤ 50MB）」一致。
pub const MAX_ATTACHMENT_SIZE: u64 = 50 * 1024 * 1024;

pub struct AttachmentService {
    store: Arc<DocStore>,
    /// 单向依赖：上传是条目上的活动，要推进 Entry.updated_at 并重建检索文档。
    /// `EntryService` 不感知附件，因此不构成循环。
    entries: EntryService,
    /// 文件落盘根目录 `{data_dir}/attachments`。
    base_dir: PathBuf,
}

impl AttachmentService {
    pub fn new(store: Arc<DocStore>, entries: EntryService, data_dir: &str) -> Self {
        Self {
            store,
            entries,
            base_dir: Path::new(data_dir).join("attachments"),
        }
    }

    /// 相对 `base_dir` 的存储路径。前两段无需净化：`workspace_id` 是 ULID，
    /// `entry_code` 是 16 位 base62 字母数字。唯一由客户端控制的段是文件名。
    fn abs_path(&self, a: &Attachment) -> PathBuf {
        self.base_dir
            .join(a.workspace_id.to_string())
            .join(&a.entry_code)
            .join(format!("{}_{}", a.id, safe_name(&a.filename)))
    }

    /// 落盘并写元数据。`content` 是 async-graphql 给的临时文件句柄，
    /// 用 `tokio::io::copy` 流式写入，不整份读进内存。
    pub async fn save(
        &self,
        actor: Ulid,
        entry_code: &str,
        filename: &str,
        content_type: &str,
        content: std::fs::File,
    ) -> Result<Attachment, AppError> {
        let mut entry = self.entries.get(entry_code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        let size = content
            .metadata()
            .map_err(|e| AppError::Storage(e.to_string()))?
            .len();
        if size == 0 {
            return Err(AppError::InvalidQuery("附件内容为空".to_string()));
        }
        if size > MAX_ATTACHMENT_SIZE {
            return Err(AppError::InvalidQuery("附件超过 50MB".to_string()));
        }

        let attachment = Attachment::new(
            entry_code.to_string(),
            entry.workspace_id,
            filename.to_string(),
            content_type.to_string(),
            size,
            actor,
        );
        let abs = self.abs_path(&attachment);
        if let Some(parent) = abs.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Storage(e.to_string()))?;
        }
        let mut src = tokio::fs::File::from_std(content);
        let mut dst = tokio::fs::File::create(&abs)
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;
        tokio::io::copy(&mut src, &mut dst)
            .await
            .map_err(|e| AppError::Storage(e.to_string()))?;
        // `copy` 在读完源文件后就不再碰 writer，而 `tokio::fs::File` 的
        // 尾块写入由 spawn_blocking 承载，其错误只在下次 `poll_write`/`poll_flush`
        // 才浮现。因此必须显式 flush：它既等待在途写入完成，又返回其错误
        // （tokio::fs::file.rs 的 `last_write_err` / `Operation::Write` 路径）。
        // 少了这一步，ENOSPC/EIO 之类的尾块失败会被当成成功，元数据里的 size
        // 来自源文件、与实际落盘字节数不符，下游读到截断内容还以为是成功。
        dst.flush().await.map_err(|e| AppError::Storage(e.to_string()))?;

        // 上传是条目上的活动：推进 updated_at，让条目回到「按更新时间倒序」最前。
        // 与 `CommentService::create` 同一取舍——正在编辑详情的人保存时会撞乐观并发冲突。
        entry.updated_by = actor;
        entry.updated_at = Utc::now();

        let audit = AuditLog::new(
            AuditAction::AttachmentUploaded,
            actor,
            "attachment",
            // 用 entry_code 而非附件 id：entry 页的「审计历史」按
            // resource_id == 条目 code 过滤，用附件 id 的话记录不会出现在任何地方。
            entry_code,
            Some(attachment.workspace_id),
            None,
            Some(serde_json::to_string(&attachment).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::ATTACHMENTS,
            attachment.id.to_bytes().to_vec(),
            &attachment,
        )?);
        ops.push(BatchOp::put_raw(
            cf::ATTACHMENTS_BY_ENTRY,
            keys::attachment_by_entry_key(entry_code, attachment.id),
            Vec::new(),
        ));
        ops.push(BatchOp::put(
            cf::ENTRIES,
            entry_code.as_bytes().to_vec(),
            &entry,
        )?);
        if let Err(e) = self.store.write_batch(ops) {
            // 元数据没落库，磁盘上那份就是孤儿，尽力清掉再报错。
            let _ = tokio::fs::remove_file(&abs).await;
            return Err(e);
        }
        self.entries.reindex_by_code(entry_code)?;
        Ok(attachment)
    }

    pub fn get(&self, id: Ulid) -> Result<Option<Attachment>, AppError> {
        self.store.get(cf::ATTACHMENTS, &id.to_bytes())
    }

    /// 某条目的全部附件，按上传时间升序。索引命中但主键缺失（历史脏数据）时跳过。
    pub fn list(&self, entry_code: &str) -> Result<Vec<Attachment>, AppError> {
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
            if let Some(a) = self.get(Ulid::from_bytes(arr))? {
                out.push(a);
            }
        }
        Ok(out)
    }

    /// `can_moderate` 由调用方按工作空间角色算好：删他人附件需要 Maintainer+，
    /// 上传者本人即使被降级为 Reader 也仍可撤回自己的上传。
    pub async fn delete(&self, actor: Ulid, id: Ulid, can_moderate: bool) -> Result<(), AppError> {
        let Some(attachment) = self.get(id)? else {
            return Err(AppError::NotFound);
        };
        if attachment.created_by != actor && !can_moderate {
            return Err(AppError::Forbidden);
        }
        let audit = AuditLog::new(
            AuditAction::AttachmentDeleted,
            actor,
            "attachment",
            &attachment.entry_code,
            Some(attachment.workspace_id),
            Some(serde_json::to_string(&attachment).unwrap_or_default()),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::ATTACHMENTS, id.to_bytes().to_vec()));
        ops.push(BatchOp::delete(
            cf::ATTACHMENTS_BY_ENTRY,
            keys::attachment_by_entry_key(&attachment.entry_code, id),
        ));
        self.store.write_batch(ops)?;
        // 元数据删成功之后才动文件。文件删失败只记日志不回滚——元数据是真相来源，
        // 宁可留孤儿文件，也不要「DB 说还在但文件已经没了」这种更难查的不一致。
        let abs = self.abs_path(&attachment);
        if let Err(e) = tokio::fs::remove_file(&abs).await {
            tracing::warn!("删除附件文件失败 {}: {e}", abs.display());
        }
        Ok(())
    }

    /// 读取附件内容，供下载路由。元数据在但文件不在（被外部删掉）→ `NotFound`。
    pub async fn read(&self, attachment: &Attachment) -> Result<Vec<u8>, AppError> {
        tokio::fs::read(self.abs_path(attachment))
            .await
            .map_err(|_| AppError::NotFound)
    }
}

/// 文件名净化：客户端文件名不能参与路径解析。ASCII 侧只放行字母数字与 `-` `_` `.` 空格，
/// 其余（`/`、`\`、控制字符）一律换成 `_`；非 ASCII 字符保留，中文文件名要能原样落盘。
/// 再去掉首尾的点与空格（`..`、`.bashrc` 这类名字不该出现在路径里），空了回退 `file`。
fn safe_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        let ok = if ch.is_ascii() {
            ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ' ')
        } else {
            !ch.is_control()
        };
        out.push(if ok { ch } else { '_' });
    }
    let trimmed = out.trim_matches(|c: char| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed.to_string()
    }
}
