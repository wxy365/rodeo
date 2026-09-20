use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{Attachment, AuditAction, AuditLog, ATTACHMENT_URL_PREFIX};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::entry::EntryService;
use crate::storage::{cf, keys, BatchOp, BlobStore, DocStore};

/// 单文件上限。与 UI 文案「附件（≤ 50MB）」一致。
pub const MAX_ATTACHMENT_SIZE: u64 = 50 * 1024 * 1024;

pub struct AttachmentService {
    store: Arc<DocStore>,
    /// 单向依赖：上传是条目上的活动，要推进 Entry.updated_at 并重建检索文档。
    /// `EntryService` 不感知附件，因此不构成循环。
    entries: EntryService,
    /// 附件本体。local 后端落 `{data_dir}/attachments`，rustfs 后端落桶。
    blobs: BlobStore,
}

impl AttachmentService {
    pub fn new(store: Arc<DocStore>, entries: EntryService, blobs: BlobStore) -> Self {
        Self { store, entries, blobs }
    }

    /// 启动期探活附件后端，见 [`BlobStore::health_check`]。
    pub async fn check_blob_store(&self) -> Result<(), AppError> {
        self.blobs.health_check().await
    }

    /// 对象键。前两段无需净化：`workspace_id` 是 ULID，`entry_code` 是 16 位 base62
    /// 字母数字。唯一由客户端控制的段是文件名，由 `safe_name` 兜住。
    /// 键里带上前两段，是为了让「一个条目下的附件」能被前缀列举，也让本地后端的
    /// 目录结构与历史版本逐字一致。
    fn blob_key(a: &Attachment) -> String {
        format!(
            "{}/{}/{}_{}",
            a.workspace_id,
            a.entry_code,
            a.id,
            safe_name(&a.filename)
        )
    }

    /// 落盘并写元数据。`content` 是 async-graphql 给的临时文件句柄，交给
    /// [`BlobStore::put`] 写入（本地后端整块读入后落盘，见其注释）。
    pub async fn save(
        &self,
        actor: Ulid,
        entry_code: &str,
        filename: &str,
        content_type: &str,
        content: std::fs::File,
        // 编辑器里粘贴/拖入的图片：仍写附件元数据，但不进附件列表。
        inline: bool,
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
        let key = Self::blob_key(&attachment);
        self.blobs.put(&key, content).await?;

        // 上传是条目上的活动：推进 updated_at，让条目回到「按更新时间倒序」最前。
        // 与 `CommentService::create` 同一取舍——正在编辑详情的人保存时会撞乐观并发冲突。
        entry.updated_by = actor;
        entry.updated_at = Utc::now();

        let audit = AuditLog::new(
            AuditAction::AttachmentUploaded,
            actor,
            "attachment",
            // 用 entry_code 而非附件 id：entry 页的「历史」按
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
        if inline {
            ops.push(BatchOp::put_raw(
                cf::INLINE_ATTACHMENTS,
                attachment.id.to_bytes().to_vec(),
                Vec::new(),
            ));
        }
        ops.push(BatchOp::put(
            cf::ENTRIES,
            entry_code.as_bytes().to_vec(),
            &entry,
        )?);
        if let Err(e) = self.store.write_batch(ops) {
            // 元数据没落库，存储里那份就是孤儿，尽力清掉再报错。
            let _ = self.blobs.delete(&key).await;
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
            // 编辑器贴进来的图片是正文的一部分，不是一条独立附件。
            if self.store.exists(cf::INLINE_ATTACHMENTS, &arr)? {
                continue;
            }
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
        self.remove(actor, &attachment).await
    }

    /// 删除一条附件的元数据、索引、内联标记与文件，不判权限。
    /// 用户主动删除（`delete`）与正文改写后的自动回收（`purge_unreferenced`）共用；
    /// 后者没有「附件上传者」这个概念可用，权限在工作空间层已经判过。
    async fn remove(&self, actor: Ulid, attachment: &Attachment) -> Result<(), AppError> {
        let audit = AuditLog::new(
            AuditAction::AttachmentDeleted,
            actor,
            "attachment",
            &attachment.entry_code,
            Some(attachment.workspace_id),
            Some(serde_json::to_string(attachment).unwrap_or_default()),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(
            cf::ATTACHMENTS,
            attachment.id.to_bytes().to_vec(),
        ));
        ops.push(BatchOp::delete(
            cf::ATTACHMENTS_BY_ENTRY,
            keys::attachment_by_entry_key(&attachment.entry_code, attachment.id),
        ));
        // 内联标记也要清掉：ULID 不会再被复用，但留着标记等于留一条只增不减的记录。
        ops.push(BatchOp::delete(
            cf::INLINE_ATTACHMENTS,
            attachment.id.to_bytes().to_vec(),
        ));
        self.store.write_batch(ops)?;
        // 元数据删成功之后才动文件。删文件失败只记日志不回滚——元数据是真相来源，
        // 宁可留孤儿对象，也不要「DB 说还在但文件已经没了」这种更难查的不一致。
        let key = Self::blob_key(attachment);
        if let Err(e) = self.blobs.delete(&key).await {
            tracing::warn!("删除附件对象失败 {key}: {e}");
        }
        Ok(())
    }

    /// 读取附件内容，供下载路由。元数据在但对象不在（被外部删掉）→ `NotFound`。
    pub async fn read(&self, attachment: &Attachment) -> Result<Vec<u8>, AppError> {
        self.blobs
            .get(&Self::blob_key(attachment))
            .await
            .map_err(|_| AppError::NotFound)
    }

    /// 正文重新保存后，把「原本被引用、现在不再被引用」的内联图片删掉。
    ///
    /// 只删带内联标记的附件：手动上传的附件即便正文里没有它的 URL 也不能动。
    /// 只比对 `before` / `after` 两个已落库的快照，所以「粘贴后没保存就离开」的图片
    /// 回收不到——这是刻意的取舍：改成「正文里没提到就删」会误删此刻还在编辑器里、
    /// 尚未保存的图片，那比留下几个看不见的文件严重得多。
    ///
    /// 引用扫描只看当前正在保存的这条正文：若某张图片的 URL 被手动复制进别的条目
    /// （或评论）正文，保存原正文去掉该图片时仍会回收文件，另一处正文就留下断链。
    /// 跨正文的引用不在追踪范围内，这是设计边界。
    pub async fn purge_unreferenced(
        &self,
        actor: Ulid,
        entry_code: &str,
        before: &str,
        after: &str,
    ) -> Result<usize, AppError> {
        let keep: std::collections::HashSet<Ulid> =
            referenced_attachment_ids(after).into_iter().collect();
        let mut removed = 0;
        for id in referenced_attachment_ids(before) {
            if keep.contains(&id) {
                continue;
            }
            if !self.store.exists(cf::INLINE_ATTACHMENTS, &id.to_bytes())? {
                continue;
            }
            let Some(attachment) = self.get(id)? else {
                continue;
            };
            // 图片上传到它被粘贴时所在的那个条目；对不上说明是别的条目的引用。
            if attachment.entry_code != entry_code {
                continue;
            }
            self.remove(actor, &attachment).await?;
            removed += 1;
        }
        Ok(removed)
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

/// 从 Delta 正文（或任意文本）里抽出内联图片引用的附件 id。
/// 只认服务端拼的那个 URL 前缀；认不出来的片段跳过，不影响后续扫描。
pub fn referenced_attachment_ids(body: &str) -> Vec<Ulid> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.find(ATTACHMENT_URL_PREFIX) {
        let tail = &rest[i + ATTACHMENT_URL_PREFIX.len()..];
        // ULID 的规范文本恒为 26 字符；`get` 落在非字符边界时返回 None，不会 panic。
        if let Some(id) = tail.get(..26).and_then(|s| Ulid::from_string(s).ok()) {
            out.push(id);
        }
        // `tail` 一定比 `rest` 短（前缀非空），循环必然收敛。
        rest = tail;
    }
    out
}
