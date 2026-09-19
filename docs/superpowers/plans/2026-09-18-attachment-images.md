# Entry 附件与图片粘贴 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让编辑器里粘贴的图片走附件体系上传并以 URL 插入，同时把 entry 页「附件（≤ 50MB）即将上线」的占位落地为真列表（列表 + 删除 + 手动上传）。

**Architecture:** 元数据进 RocksDB 两个新列族（主键 `attachments`、按条目的索引 `attachments_by_entry`），文件本体落 `{data_dir}/attachments/{workspace_id}/{entry_code}/{id}_{safe_name}`。上传走既有 `/api/graphql` 端点的 multipart（`async-graphql` 的 `Upload` scalar，提取器已内建支持，无需改特性），下载走新增的免鉴权路由 `GET /api/attachments/{id}`。前端由 `glue.js` 在捕获阶段接住粘贴/拖入的图片文件，经 Rust 侧回调上传后 `insertEmbed` 插入 URL。

**Tech Stack:** Rust 1.85+、Leptos 0.8（SSR + wasm hydrate）、axum 0.8、async-graphql 7.2、RocksDB、Quill 2.0（`@opentiny/fluent-editor`，经 `public/tiny-editor/glue.js` 桥接）。

**Spec:** `docs/superpowers/specs/2026-09-18-attachment-images-design.md`

## Global Constraints

- **不新增后端 Rust 单测。** 每个后端任务的验证是编译门禁，不是测试。
- 编译门禁两条，**都必须 0 错误**：
  - `cargo check`（默认特性 = ssr）
  - `cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown`
  - 纯后端任务只需第一条；任何触及 `src/frontend/` 的任务两条都要跑。
- **不碰工作区里已有的未提交改动**：`.gitignore`、`Cargo.lock`、`Cargo.toml`（TLS / `hash-files` 部分）、`config.example.toml`、`src/app.rs`、`src/config.rs`、`src/main.rs`（TLS 分支）、`src/service/label.rs`、`src/service/view.rs`、`src/frontend/pages/settings.rs` 等。本计划对 `Cargo.toml` 与 `src/main.rs` 的改动都是**增量**，不要重排既有行。
- 提交时**逐个文件 `git add`**，不要 `git add -A`。
- 单文件上限 `50 * 1024 * 1024` 字节，与 UI 文案「附件（≤ 50MB）」一致。不引入配置项。
- 审计枚举 `AuditAction::AttachmentUploaded` / `AttachmentDeleted` 已存在（`src/domain/audit.rs:27-28`），**不要新增变体**。
- 下载路由免鉴权（用户已确认）；上传需 Worker+；删除需上传者本人或 Maintainer+。
- 不动用户 :3000 的 dev server。真机验证用 3099 隔离实例。

---

## 文件结构

| 文件 | 职责 | 动作 |
|---|---|---|
| `src/storage/rocksdb.rs` | 两个新列族常量 + `ALL_CFS` | 改 |
| `src/storage/keys.rs` | `attachment_by_entry_key` | 改 |
| `src/domain/attachment.rs` | `Attachment` 实体 | 建 |
| `src/domain/mod.rs` | 导出 `Attachment` | 改 |
| `src/service/attachment.rs` | 磁盘布局、`save` / `get` / `list` / `delete` / `read`、文件名净化 | 建 |
| `src/service/comment.rs` | 「纯图片评论不算空内容」 | 改 |
| `src/service/mod.rs` | 装配 `AttachmentService` | 改 |
| `src/api/attachments.rs` | 下载路由 handler + 内联白名单 | 建 |
| `src/api/mod.rs` | 导出 | 改 |
| `src/api/graphql.rs` | `GqlAttachment` + 1 查询 + 2 变更 | 改 |
| `src/main.rs` | 下载路由 + `/api/graphql` 的请求体上限 | 改 |
| `src/frontend/graphql_client.rs` | `Attachment` 结构 + 3 个调用 + multipart 上传 | 改 |
| `src/frontend/tiny_editor.rs` | `entry_code` prop + `upload` 回调 | 改 |
| `public/tiny-editor/glue.js` | 删 `blockImages()`，换 `attachImageUpload()` | 改 |
| `src/frontend/attachment_list.rs` | 附件区组件 | 建 |
| `src/frontend/components.rs` | `human_size` | 改 |
| `src/frontend/pages/entry.rs` | 挂载附件区；给编辑器传 code | 改 |
| `src/frontend/pages/workspace_main.rs` | 给编辑器传 code | 改 |
| `src/frontend/comment_list.rs` | 给两个编辑器传 code | 改 |
| `style/main.css` | 附件区少量补充 | 改 |

---

### Task 1: 数据层 —— 列族、复合键、Attachment 实体

**Files:**
- Modify: `src/storage/rocksdb.rs:47-76`
- Modify: `src/storage/keys.rs`（插在 `comment_key` 之后）
- Create: `src/domain/attachment.rs`
- Modify: `src/domain/mod.rs`
- Modify: `Cargo.toml:33`（tokio 特性）

**Interfaces:**
- Consumes: 无（第一个任务）
- Produces:
  - `cf::ATTACHMENTS: &str`、`cf::ATTACHMENTS_BY_ENTRY: &str`
  - `keys::attachment_by_entry_key(entry_code: &str, attachment_id: Ulid) -> Vec<u8>`
  - `domain::Attachment { id: Ulid, entry_code: String, workspace_id: Ulid, filename: String, content_type: String, size: u64, created_by: Ulid, created_at: DateTime<Utc> }`
  - `Attachment::new(entry_code: String, workspace_id: Ulid, filename: String, content_type: String, size: u64, actor: Ulid) -> Attachment`

- [ ] **Step 1: 加两个列族常量**

在 `src/storage/rocksdb.rs` 的 `pub mod cf` 内，`pub const COMMENTS: &str = "comments";` 之后追加：

```rust
    /// 附件主键：`attachment_id`（ULID 16 字节）→ `Attachment`（bincode）。
    /// 用 id 单键而非 (entry_code, id)，因为下载路由手里只有 id，必须能直取。
    pub const ATTACHMENTS: &str = "attachments";
    /// 附件索引：(entry_code, attachment_id) → 空值，供按条目前缀扫描。
    pub const ATTACHMENTS_BY_ENTRY: &str = "attachments_by_entry";
```

在 `const ALL_CFS: &[&str]` 里 `cf::COMMENTS,` 之后追加：

```rust
    cf::ATTACHMENTS,
    cf::ATTACHMENTS_BY_ENTRY,
```

`DocStore::open` 已开 `create_missing_column_families(true)`，存量库启动时会自动补建这两个列族，不需要迁移代码。

- [ ] **Step 2: 加复合键函数**

在 `src/storage/keys.rs` 的 `comment_key` 之后追加：

```rust
/// (entry_code, attachment_id) 复合键。`attachment_id` 是 ULID，字节序即时间序，
/// 因此按 entry_code 前缀扫描天然得到按上传时间升序的附件列表——与 `comment_key` 同构。
pub fn attachment_by_entry_key(entry_code: &str, attachment_id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(entry_code.len() + 16);
    key.extend_from_slice(entry_code.as_bytes());
    key.extend_from_slice(&attachment_id.to_bytes());
    key
}
```

- [ ] **Step 3: 建实体**

创建 `src/domain/attachment.rs`：

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Entry 附件。元数据进 RocksDB，文件本体落 `{data_dir}/attachments`。
///
/// 存储路径**不进实体**：由 workspace_id / entry_code / id / filename 现算，
/// 这样路径与文件名不会各自漂移。`filename` 是上传时的原始名，只用于展示与
/// 下载响应头；磁盘上的文件名是净化后的版本（见 `service::attachment::safe_name`）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub id: Ulid,
    pub entry_code: String,
    /// 冗余存一份：权限校验与「这个附件属于哪个空间」不必每次回查 Entry。
    pub workspace_id: Ulid,
    pub filename: String,
    /// 客户端上报的 MIME，只当提示看：下载响应绝不回声此值。
    pub content_type: String,
    pub size: u64,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
}

impl Attachment {
    pub fn new(
        entry_code: String,
        workspace_id: Ulid,
        filename: String,
        content_type: String,
        size: u64,
        actor: Ulid,
    ) -> Self {
        Self {
            id: Ulid::new(),
            entry_code,
            workspace_id,
            filename,
            content_type,
            size,
            created_by: actor,
            created_at: Utc::now(),
        }
    }
}
```

- [ ] **Step 4: 导出实体**

`src/domain/mod.rs`：在 `pub mod comment;` 之后加 `pub mod attachment;`（保持字母序，即放在 `pub mod audit;` 之前），并在 `pub use comment::Comment;` 之后加：

```rust
pub use attachment::Attachment;
```

- [ ] **Step 5: 给 tokio 补两个特性**

`Cargo.toml` 第 33 行，把 tokio 的特性表改成（只加 `"fs"` 与 `"io-util"`，其余原样，注释保留）：

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "signal", "fs", "io-util"], optional = true }
```

Task 2 的 `tokio::fs` 与 `tokio::io::copy` 需要它们。

- [ ] **Step 6: 编译门禁**

```bash
cargo check
```
Expected: 0 错误。`Attachment` 此时还没有任何使用点，会有 `dead_code` 警告——正常，Task 2 起就会被用上。

- [ ] **Step 7: 提交**

```bash
git add src/storage/rocksdb.rs src/storage/keys.rs src/domain/attachment.rs src/domain/mod.rs Cargo.toml
git commit -m "feat(storage): 附件列族、复合键与 Attachment 实体"
```

---

### Task 2: AttachmentService 与 Services 装配

**Files:**
- Create: `src/service/attachment.rs`
- Modify: `src/service/mod.rs`

**Interfaces:**
- Consumes: `cf::ATTACHMENTS`、`cf::ATTACHMENTS_BY_ENTRY`、`keys::attachment_by_entry_key`、`domain::Attachment`（Task 1）
- Produces:
  - `service::attachment::MAX_ATTACHMENT_SIZE: u64`
  - `AttachmentService::new(store: Arc<DocStore>, entries: EntryService, data_dir: &str) -> Self`
  - `AttachmentService::save(&self, actor: Ulid, entry_code: &str, filename: &str, content_type: &str, content: std::fs::File) -> Result<Attachment, AppError>`（async）
  - `AttachmentService::get(&self, id: Ulid) -> Result<Option<Attachment>, AppError>`
  - `AttachmentService::list(&self, entry_code: &str) -> Result<Vec<Attachment>, AppError>`
  - `AttachmentService::delete(&self, actor: Ulid, id: Ulid, can_moderate: bool) -> Result<(), AppError>`（async）
  - `AttachmentService::read(&self, attachment: &Attachment) -> Result<Vec<u8>, AppError>`（async）
  - `Services::attachment: AttachmentService`

- [ ] **Step 1: 写服务**

创建 `src/service/attachment.rs`：

```rust
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
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
        drop(dst);

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
```

说明两点实现取舍（与 spec 的对应关系）：
- spec §6.2 写「resolver 里先用 `size()` 判上限」，实际把上限判定放在服务里（写盘之前用文件 metadata 判）。规则只有一处，且一样是「超限不落盘」——resolver 不再重复判一遍。
- spec §5.2 没提「空文件」，实现里补了一个 0 字节拒绝：0 字节附件在任何 UI 里都不可见也不可下载，属于无意义记录。

- [ ] **Step 2: 装配进 Services**

`src/service/mod.rs`：

1. 模块声明处加 `pub mod attachment;`（放在 `pub mod audit;` 之前，保持字母序）。
2. `pub use` 区加 `pub use attachment::AttachmentService;`。
3. `pub struct Services` 里，`pub comment: CommentService,` 之后加 `pub attachment: AttachmentService,`。
4. 构造函数里，在已有那行 `let comment = CommentService::new(store.clone(), entry.clone());` 之后加：

```rust
        // 附件服务同样要与 EntryService 共用同一份实例：上传后要推进条目更新时间并重索引。
        let attachment = AttachmentService::new(store.clone(), entry.clone(), &config.data_dir());
```

5. `Self { ... }` 里 `comment,` 之后加 `attachment,`。

- [ ] **Step 3: 编译门禁**

```bash
cargo check
```
Expected: 0 错误。`AttachmentService` 的方法此时尚无调用点，`dead_code` 警告属正常。

- [ ] **Step 4: 提交**

```bash
git add src/service/attachment.rs src/service/mod.rs
git commit -m "feat(service): AttachmentService —— 元数据进 RocksDB、文件落盘"
```

---

### Task 3: 纯图片评论不算空内容

**Files:**
- Modify: `src/service/comment.rs:161-163`

**Interfaces:**
- Consumes: 无
- Produces: 无（内部行为变更）

**为什么需要这一步**：`strip_rich_text`（`src/service/search.rs:33`）只收集 Delta 里的字符串片段，图片嵌入 `{"insert":{"image":…}}` 贡献零文本。于是「只贴一张图、不打字」的评论会被 `is_blank_body` 判为空，得到 `评论内容不能为空` —— 与「评论也能贴图」直接冲突。条目正文没有同样的空白校验（只有标题有），所以只需处理评论这一处。**不要改 `strip_rich_text`**：它同时服务于检索索引，改它会让索引里多出图片占位文本。

- [ ] **Step 1: 放行图片嵌入**

`src/service/comment.rs` 末尾，把 `is_blank_body` 替换为下面两个函数：

```rust
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
```

- [ ] **Step 2: 编译门禁**

```bash
cargo check
```
Expected: 0 错误。

- [ ] **Step 3: 提交**

```bash
git add src/service/comment.rs
git commit -m "fix(comment): 纯图片评论不再被判为空内容"
```

---

### Task 4: GraphQL —— 类型、查询、变更、请求体上限

**Files:**
- Modify: `src/api/graphql.rs`（imports 区、`GqlComment` 附近加类型、`impl Query` 的 `comments` 之后加查询、`impl Mutation` 的 `delete_comment` 之后加两个变更）
- Modify: `src/main.rs:45`（`/api/graphql` 路由加请求体上限）

**Interfaces:**
- Consumes: `Services::attachment`、`AttachmentService::{save,get,list,delete}`（Task 2）
- Produces:
  - GraphQL `type GqlAttachment { id entryCode filename contentType size url createdAt createdBy createdByAccount }`
  - `query attachments(entryCode: String!): [GqlAttachment!]!`
  - `mutation uploadAttachment(entryCode: String!, file: Upload!): GqlAttachment!`
  - `mutation deleteAttachment(id: ID!): Boolean!`

- [ ] **Step 1: 引入 Upload 与 Attachment**

`src/api/graphql.rs` 顶部：在 `use async_graphql::{...}` 的引入列表里补上 `Upload`。该文件用的是分组引入，把 `Upload` 加进现有的 `async_graphql` 引入项即可（与 `Context`、`ID`、`Object`、`SimpleObject` 同组）。

`use crate::domain::{...}` 里补上 `Attachment`（与已有的 `Comment` 同组）。

- [ ] **Step 2: 加 GraphQL 类型与组装函数**

在 `gql_comment` 函数之后（`GqlAuthResult` 定义之前）插入：

```rust
#[derive(SimpleObject, Clone)]
pub struct GqlAttachment {
    id: ID,
    entry_code: String,
    filename: String,
    content_type: String,
    size: i32,
    /// 由服务端拼死：下载路径只有一处定义，客户端不自己拼。
    url: String,
    created_at: String,
    created_by: ID,
    created_by_account: Option<GqlAccount>,
}

/// 组装 GqlAttachment，顺带补上上传者账号。
fn gql_attachment(gql: &GraphqlContext, a: Attachment) -> GqlResult<GqlAttachment> {
    let created_by_account = gql.services.auth.find_by_id(a.created_by)?.map(Into::into);
    Ok(GqlAttachment {
        id: a.id.to_string().into(),
        entry_code: a.entry_code,
        filename: a.filename,
        content_type: a.content_type,
        // GraphQL Int 是 32 位；上限 50MB 远在范围内。
        size: a.size as i32,
        url: format!("/api/attachments/{}", a.id),
        created_at: a.created_at.to_rfc3339(),
        created_by: a.created_by.to_string().into(),
        created_by_account,
    })
}
```

- [ ] **Step 3: 加查询**

在 `impl Query` 的 `comments` 方法之后插入：

```rust
    /// 某条目的全部附件，按上传时间升序。成员即可读（与 `comments` 一致）。
    async fn attachments(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
    ) -> GqlResult<Vec<GqlAttachment>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_member(entry.workspace_id)?;
        gql.services
            .attachment
            .list(&entry_code)?
            .into_iter()
            .map(|a| gql_attachment(gql, a))
            .collect()
    }
```

- [ ] **Step 4: 加两个变更**

在 `impl Mutation` 的 `delete_comment` 方法之后插入：

```rust
    /// 上传附件（Worker+）。multipart 由 async-graphql 的 `Upload` scalar 承载。
    async fn upload_attachment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        file: Upload,
    ) -> GqlResult<GqlAttachment> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        // tempfile 特性开启时 content 是临时文件句柄，try_clone 取一份自有的，
        // 这样 filename / content_type 还能从原值上读。
        let value = file
            .value(ctx)?
            .try_clone()
            .map_err(|e| AppError::Storage(e.to_string()))?;
        let filename = value.filename.clone();
        let content_type = value
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let a = gql
            .services
            .attachment
            .save(
                auth.account_id,
                &entry_code,
                &filename,
                &content_type,
                value.content,
            )
            .await?;
        gql_attachment(gql, a)
    }

    /// 删除附件（上传者本人，或 Maintainer+）。
    async fn delete_attachment(&self, ctx: &Context<'_>, id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let id = parse_ulid(id.as_str())?;
        // 附件只带 workspace_id，先取出来才知道该问哪个工作空间的权限。
        let attachment = gql
            .services
            .attachment
            .get(id)?
            .ok_or(AppError::NotFound)?;
        gql.require_member(attachment.workspace_id)?;
        // 先看是不是 Maintainer+；不是也不立刻拒绝——上传者本人仍可撤回自己的附件。
        let can_moderate = gql
            .require_role(attachment.workspace_id, WorkspaceRole::Maintainer)
            .is_ok();
        gql.services
            .attachment
            .delete(auth.account_id, id, can_moderate)
            .await?;
        Ok(true)
    }
```

- [ ] **Step 5: 放开 `/api/graphql` 的请求体上限**

axum 默认请求体上限 2MB，50MB 上传会在进 handler 之前就被 413 掉。`src/main.rs` 里把 `/api/graphql` 那行改成：

```rust
        .route(
            "/api/graphql",
            post(graphql_handler).layer(DefaultBodyLimit::max(52 * 1024 * 1024)),
        )
```

并在 `use axum::{Extension, Router};` 那行补上 `DefaultBodyLimit`：

```rust
    use axum::extract::DefaultBodyLimit;
    use axum::{Extension, Router};
```

**只挂这一条路由**，不要 `DefaultBodyLimit::disable()` 或放到全局 layer 上——leptos 的 server function 路由不该跟着接受 52MB 请求体。

- [ ] **Step 6: 编译门禁**

```bash
cargo check
```
Expected: 0 错误。

- [ ] **Step 7: 提交**

```bash
git add src/api/graphql.rs src/main.rs
git commit -m "feat(graphql): 附件上传/删除/查询与请求体上限"
```

---

### Task 5: 下载路由

**Files:**
- Create: `src/api/attachments.rs`
- Modify: `src/api/mod.rs`
- Modify: `src/main.rs`（加一条 route）

**Interfaces:**
- Consumes: `AppState`、`AttachmentService::{get,read}`（Task 2、4）
- Produces: `api::attachments::download_attachment`、`api::download_attachment`

- [ ] **Step 1: 写 handler**

创建 `src/api/attachments.rs`：

```rust
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Extension, Path};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use ulid::Ulid;

use crate::api::AppState;

/// 可以内联展示的类型白名单。**只放行栅格图**：SVG 能携带脚本，`text/html`
/// 更是同源存储型 XSS，一律降级成附件下载。
fn inline_content_type(reported: &str) -> Option<&'static str> {
    let base = reported
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "image/png" => Some("image/png"),
        "image/jpeg" | "image/jpg" => Some("image/jpeg"),
        "image/gif" => Some("image/gif"),
        "image/webp" => Some("image/webp"),
        "image/bmp" => Some("image/bmp"),
        _ => None,
    }
}

/// 附件下载。**免鉴权**：`<img src>` 是浏览器发起的裸请求，带不了 `Authorization`
/// 头，而本仓库从不签发 cookie。id 是不可猜的 ULID，语义等同「能力 URL」。
pub async fn download_attachment(
    Extension(state): Extension<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Ok(id) = Ulid::from_string(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let attachment = match state.services.attachment.get(id) {
        Ok(Some(a)) => a,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::error!("读取附件元数据失败 {id}: {e}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let bytes = match state.services.attachment.read(&attachment).await {
        Ok(b) => b,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let inline = inline_content_type(&attachment.content_type);
    let mut resp = Response::new(Body::from(bytes));
    let headers = resp.headers_mut();
    // 绝不回声客户端上报的 contentType：上报值来自上传方，
    // 回声它等于允许上传 text/html 后在应用同源下执行脚本。
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(inline.unwrap_or("application/octet-stream")),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&content_disposition(
            inline.is_some(),
            &attachment.filename,
        ))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // id 决定内容不变，可长期缓存；但路由免鉴权且附件可被删除，所以用 `private` 只让浏览器
    // 自己缓存（用户 2026-09-19 决定，spec §7 原稿的 `public` 已同步改为 `private`）。
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, max-age=31536000, immutable"),
    );
    resp
}

/// 内联图不需要文件名；其余一律 `attachment` + RFC 5987 的 `filename*`
/// （原始名可能含中文，`filename=` 的 latin-1 会乱码）。
fn content_disposition(inline: bool, filename: &str) -> String {
    if inline {
        return "inline".to_string();
    }
    format!(
        "attachment; filename*=UTF-8''{}",
        percent_encode(filename)
    )
}

/// 最小百分号编码：只保留 RFC 3986 的 unreserved 集合，其余逐字节转义。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        let c = *b as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
```

- [ ] **Step 2: 导出**

`src/api/mod.rs` 改为：

```rust
pub mod attachments;
pub mod graphql;

pub use attachments::download_attachment;
pub use graphql::{build_schema, graphql_handler, AppSchema, AppState};
```

- [ ] **Step 3: 挂路由**

`src/main.rs`：`use rodeo::api::{build_schema, graphql_handler, AppState};` 改为

```rust
    use rodeo::api::{build_schema, download_attachment, graphql_handler, AppState};
```

并在 `/api/health` 那行之后加：

```rust
        .route("/api/attachments/{id}", get(download_attachment))
```

- [ ] **Step 4: 编译门禁**

```bash
cargo check
```
Expected: 0 错误。

- [ ] **Step 5: 提交**

```bash
git add src/api/attachments.rs src/api/mod.rs src/main.rs
git commit -m "feat(api): 附件下载路由，下载类型按白名单决定"
```

---

### Task 6: 前端 GraphQL 客户端与 multipart 上传

**Files:**
- Modify: `Cargo.toml`（wasm 目标依赖 + web-sys 特性）
- Modify: `src/frontend/graphql_client.rs`

**Interfaces:**
- Consumes: Task 4 的 GraphQL 契约
- Produces:
  - `frontend::graphql_client::Attachment`（serde 结构）
  - `attachments(entry_code: &str) -> Result<Vec<Attachment>, String>`
  - `delete_attachment(id: &str) -> Result<bool, String>`
  - `upload_attachment(entry_code: &str, file: &web_sys::File) -> Result<Attachment, String>`（仅 wasm）

- [ ] **Step 1: 补 wasm 依赖与 web-sys 特性**

`Cargo.toml` 的 `[target.'cfg(target_arch = "wasm32")'.dependencies]`，把 `web-sys` 那行改成（只加三个特性）：

```toml
web-sys = { version = "0.3", features = ["Clipboard", "Navigator", "Window", "FormData", "File", "Blob"] }
```

并在 `js-sys = "0.3"` 之后加一行：

```toml
wasm-bindgen-futures = "0.4"
```

`wasm-bindgen-futures` 用来把上传的异步结果包成 `Promise` 交给 `glue.js`。

- [ ] **Step 2: 加响应结构与三个调用**

`src/frontend/graphql_client.rs`：在 `Comment` 结构之后加结构，在文件已有的 `comments` / `create_comment` 一族函数之后加三个函数。

```rust
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub entry_code: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    /// 服务端拼好的下载路径。客户端不要自己拼。
    pub url: String,
    pub created_at: String,
    pub created_by: String,
    #[serde(default)]
    pub created_by_account: Option<AccountBrief>,
}

const ATTACHMENT_FIELDS: &str =
    "id entryCode filename contentType size url createdAt createdBy createdByAccount { id email name }";
```

```rust
pub async fn attachments(entry_code: &str) -> Result<Vec<Attachment>, String> {
    let q =
        format!("query($c: String!) {{ attachments(entryCode: $c) {{ {ATTACHMENT_FIELDS} }} }}");
    let data = graphql(&q, json!({ "c": entry_code })).await?;
    serde_json::from_value(data.get("attachments").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn delete_attachment(id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($i: ID!) { deleteAttachment(id: $i) }",
        json!({ "i": id }),
    )
    .await?;
    Ok(data
        .get("deleteAttachment")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 走 GraphQL multipart 上传。字段名 `operations` / `map` / `map` 里映射的键三者必须自洽
/// （async-graphql 按 graphql-multipart-request-spec 解析）。
/// 只在 wasm 下存在：`web_sys::File` 在非 wasm 目标不可用，调用点也是 wasm-only。
#[cfg(target_arch = "wasm32")]
pub async fn upload_attachment(
    entry_code: &str,
    file: &web_sys::File,
) -> Result<Attachment, String> {
    use wasm_bindgen::JsCast;

    let operations = json!({
        "query": format!(
            "mutation($c: String!, $f: Upload!) {{ uploadAttachment(entryCode: $c, file: $f) {{ {ATTACHMENT_FIELDS} }} }}"
        ),
        "variables": { "c": entry_code, "f": null },
    });
    let map = json!({ "0": ["variables.f"] });

    let fd = web_sys::FormData::new().map_err(|e| format!("{e:?}"))?;
    fd.append_with_str("operations", &operations.to_string())
        .map_err(|e| format!("{e:?}"))?;
    fd.append_with_str("map", &map.to_string())
        .map_err(|e| format!("{e:?}"))?;
    fd.append_with_blob_and_filename("0", AsRef::<web_sys::Blob>::as_ref(file), &file.name())
        .map_err(|e| format!("{e:?}"))?;

    // 不手写 Content-Type：浏览器要自己补 multipart 的 boundary，手写会让服务端解析失败。
    let mut req = gloo_net::http::Request::post("/api/graphql");
    if let Some(token) = get_token() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let resp = req
        .body(fd)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json_val: Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(msg) = json_val
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Err(msg.to_string());
    }
    serde_json::from_value(
        json_val
            .pointer("/data/uploadAttachment")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|e| e.to_string())
}
```

两个 API 细节（已核对 web-sys 0.3.104 源码，不是照记忆写的）：
- 方法名是 `append_with_blob_and_filename`（不是 `append_with_blob_with_filename`）。
- `File` 由 `extends = "Blob"` 生成 `AsRef<Blob>`，所以写 `AsRef::<web_sys::Blob>::as_ref(file)` 而不是 `file.as_ref()` —— `File` 同时有多个 `AsRef` 实现，显式指定目标类型免得推断有歧义。

- [ ] **Step 3: 编译门禁（两条都要）**

```bash
cargo check
cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown
```
Expected: 两条都 0 错误。第二条若报 `web_sys::FormData` 不存在，说明 Step 1 的特性名写错。

- [ ] **Step 4: 提交**

```bash
git add Cargo.toml src/frontend/graphql_client.rs
git commit -m "feat(frontend): 附件查询/删除与 multipart 上传客户端"
```

---

### Task 7: 编辑器粘贴上传

**Files:**
- Modify: `src/frontend/tiny_editor.rs`
- Modify: `public/tiny-editor/glue.js:42-62, 68-108`
- Modify: `src/frontend/comment_list.rs:238, 289`
- Modify: `src/frontend/pages/entry.rs:185`
- Modify: `src/frontend/pages/workspace_main.rs:2058`

**Interfaces:**
- Consumes: `upload_attachment`（Task 6）
- Produces: `TinyEditor` 新 prop `entry_code: Signal<String>`；`glue.js` 的 `create(el, deltaJson, opts)` 第三参数 `opts.upload: (File) => Promise<string>`

- [ ] **Step 1: 给 TinyEditor 加 prop 并构造上传回调**

`src/frontend/tiny_editor.rs`：把组件签名与 `mount_when_ready` 改为（只列改动部分，其余函数体不动）：

```rust
#[component]
pub fn TinyEditor(
    #[prop(into)] initial: String,
    /// 所属条目编码。粘贴图片时上传到这条目下；空串表示无归属时上传会失败。
    entry_code: Signal<String>,
    on_change: Callback<String>,
) -> impl IntoView {
    let el: NodeRef<Div> = NodeRef::new();
    let delta = normalize_delta(&initial);

    mount_when_ready(el, delta, entry_code, on_change);

    view! { <div node_ref=el class="tiny-editor"></div> }
}

fn mount_when_ready(
    el: NodeRef<Div>,
    delta: String,
    entry_code: Signal<String>,
    on_change: Callback<String>,
) {
    #[cfg(target_arch = "wasm32")]
    el.on_load(move |node| {
        mount_editor(node, delta, entry_code, on_change);
    });

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (el, delta, entry_code, on_change);
    }
}
```

`mount_editor` 的签名补一个参数，并在 `let args = Array::new();` 之前构造第三个参数：

```rust
#[cfg(target_arch = "wasm32")]
fn mount_editor<N: wasm_bindgen::JsCast>(
    node: N,
    delta_json: String,
    entry_code: Signal<String>,
    on_change: Callback<String>,
) {
    // ... 前面从 bridge 取 create 的部分不变 ...

    let node_js = node.unchecked_into::<JsValue>();

    // ... 重复挂载检查不变 ...

    // 第三个参数：把异步上传包成 Promise 交给 glue.js。
    // 闭包在「调用时」才读 entry_code，而不是挂载时快照——详情面板切换条目后
    // 编辑器节点可能被复用，此时上传必须落在当前条目上。
    let opts = js_sys::Object::new();
    let upload_fn = {
        let closure = Closure::wrap(Box::new(move |file: JsValue| -> JsValue {
            let code = entry_code.get_untracked();
            let fut = async move {
                let file = file
                    .dyn_into::<web_sys::File>()
                    .map_err(|_| JsValue::from_str("粘贴的内容不是文件"))?;
                match crate::frontend::graphql_client::upload_attachment(&code, &file).await {
                    Ok(a) => Ok(JsValue::from_str(&a.url)),
                    Err(e) => Err(JsValue::from_str(&e)),
                }
            };
            wasm_bindgen_futures::future_to_promise(fut).into()
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        // into_js_value 会泄漏这个闭包，但也正因此它不会被回收；
        // 再把引用挂到节点上，与 __rodeo_editor / __rodeo_onchange 同一套保活方式。
        let js = closure.into_js_value();
        let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_upload"), &js);
        js
    };
    let _ = Reflect::set(&opts, &JsValue::from_str("upload"), &upload_fn);

    let args = Array::new();
    args.push(&node_js);
    args.push(&JsValue::from_str(&delta_json));
    args.push(&opts);
```

`mount_editor` 顶部已有的 `use js_sys::{Array, Function, Reflect};` 与 `use wasm_bindgen::{closure::Closure, JsCast, JsValue};` 已覆盖 `Closure`、`JsValue`、`Reflect` 与 `dyn_into`（`JsCast`）。不需要新增 import。

- [ ] **Step 2: 替换 glue.js 的拦截逻辑**

`public/tiny-editor/glue.js`：删掉整个 `blockImages` 函数（第 42-62 行），替换为：

```js
// 正文支持图片：不再是「拦截」，而是接手上传。粘贴/拖入的图片文件经 opts.upload
// 上传后以 /api/attachments/<id> 的 URL 插入，而不是 Quill 默认的 base64 data URI
// （后者会让单条内容涨到数 MB，且只读渲染会把 data: 原样输出）。
// 仍在捕获阶段监听：Quill 的 clipboard 模块在冒泡阶段处理粘贴，要抢在它之前。
function attachImageUpload(el, editor, opts) {
  const imageFiles = (dt) => {
    const items = dt && dt.items;
    if (!items) return [];
    return Array.from(items)
      .filter((it) => it.kind === 'file' && (it.type || '').startsWith('image/'))
      .map((it) => it.getAsFile())
      .filter(Boolean);
  };
  const guard = (e) => {
    const files = imageFiles(e.clipboardData || e.dataTransfer);
    if (!files.length) return;
    e.preventDefault();
    e.stopPropagation();
    if (!opts || typeof opts.upload !== 'function') return;
    uploadIntoEditor(editor, opts.upload, files);
  };
  el.addEventListener('paste', guard, true);
  el.addEventListener('drop', guard, true);
}

// 串行插入：每个文件上传完成后再动下一个，位置由前一个的结果决定，
// 避免并发 await 让插入点互相错位。占位文本会在上传完成后被替换掉，
// 且保存的是 getContents() 的结果，占位不会落库。
async function uploadIntoEditor(editor, upload, files) {
  const sel = editor.getSelection(true);
  let index = sel ? sel.index : editor.getLength();
  for (const file of files) {
    const at = index;
    const placeholder = '上传中…';
    editor.insertText(at, placeholder, 'user');
    index = at + placeholder.length;
    try {
      const url = await upload(file);
      editor.deleteText(at, placeholder.length, 'user');
      editor.insertEmbed(at, 'image', url, 'user');
      index = at + 1;
    } catch (err) {
      editor.deleteText(at, placeholder.length, 'user');
      const fail = '图片上传失败';
      editor.insertText(at, fail, 'user');
      index = at + fail.length;
      console.warn('图片上传失败', err);
    }
    editor.setSelection(index, 0);
  }
}
```

然后 `create(el, deltaJson)` 改成 `create(el, deltaJson, opts)`，并把结尾那行 `blockImages(el);` 换成 `attachImageUpload(el, editor, opts);`。

**工具栏不加图片按钮**：`modules.toolbar` 只是按钮数组，不构成格式白名单，`image` blot 本来就可用，无需改配置。

- [ ] **Step 3: 四个调用点传 entry_code**

`src/frontend/comment_list.rs` 两处（`code` 是 `CommentList` 的 `Signal<String>` prop，`Signal` 是 `Copy`，两处都能传）：

```rust
// 第 238 行附近（就地编辑）
<TinyEditor initial=body_for_editor entry_code=code on_change=on_edit_change />
// 第 289 行附近（新增框）
<TinyEditor initial=String::new() entry_code=code on_change=on_composer_change />
```

`src/frontend/pages/entry.rs:185`：

```rust
<TinyEditor initial entry_code=Signal::derive(code) on_change=on_editor_change />
```

`code` 是 `move || params.get().get("code").unwrap_or_default()`；`params` 是 `Memo<ParamsMap>`（`Copy`），因此该闭包是 `Copy`，`Signal::derive(code)` 之后下面几处 `code()` 仍然可用。编译若报「use of moved value」，改成在文件顶部先建 `let code_sig = Signal::derive(code);` 并把后续 `code()` 全部换成 `code_sig.get()`。

`src/frontend/pages/workspace_main.rs:2058`（`code` 在该作用域已是 signal，同文件 `CommentList` 的写法一致）：

```rust
<TinyEditor initial entry_code=Signal::derive(move || code.get()) on_change=on_editor_change />
```

- [ ] **Step 4: 编译门禁（两条都要）**

```bash
cargo check
cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown
```
Expected: 两条都 0 错误。

- [ ] **Step 5: 提交**

```bash
git add src/frontend/tiny_editor.rs public/tiny-editor/glue.js src/frontend/comment_list.rs src/frontend/pages/entry.rs src/frontend/pages/workspace_main.rs
git commit -m "feat(frontend): 编辑器粘贴图片改为上传后插入 URL"
```

---

### Task 8: 附件区 UI

**Files:**
- Create: `src/frontend/attachment_list.rs`
- Modify: `src/frontend/mod.rs`（加 `pub mod attachment_list;`）
- Modify: `src/frontend/components.rs`（加 `human_size`）
- Modify: `src/frontend/pages/entry.rs:210-213`
- Modify: `style/main.css`（`.att` 已有，只补一条 hover）

**Interfaces:**
- Consumes: `attachments` / `delete_attachment` / `upload_attachment`（Task 6）、`human_size`
- Produces: `AttachmentList` 组件，props `code: Signal<String>` / `workspace_id: Signal<String>` / `on_changed: Callback<()>`

- [ ] **Step 1: 加字节数格式化**

`src/frontend/components.rs` 末尾（`fmt_datetime` 之后）加：

```rust
/// 字节数 → 人类可读。附件行用它，免得把 5242880 这种数直给用户。
pub fn human_size(bytes: i64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let n = bytes.max(0);
    let b = n as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{n} B")
    }
}
```

- [ ] **Step 2: 写附件区组件**

创建 `src/frontend/attachment_list.rs`（结构照 `comment_list.rs`：disposal 守卫 + `load()` + 二次确认删除）：

```rust
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::components::{human_size, logged_out, role_at_least, short_time};
use crate::frontend::graphql_client::{
    attachments, delete_attachment, me, my_role, upload_attachment, Attachment,
};
use crate::frontend::icons::{ic_folder, ic_upload};

#[component]
pub fn AttachmentList(
    /// 条目编码；空串时不取数也不渲染。
    code: Signal<String>,
    /// 所属工作空间，用来查当前用户角色。
    workspace_id: Signal<String>,
    /// 增删后通知外层：条目的「更新时间」要跟着刷新。
    on_changed: Callback<()>,
) -> impl IntoView {
    let items = RwSignal::new(None::<Result<Vec<Attachment>, String>>);
    let role = RwSignal::new(String::new());
    let my_id = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let confirm_del = RwSignal::new(None::<String>);
    let uploading = RwSignal::new(false);

    let load = move || {
        // 组件可能已被卸载（双击行会用全屏浮层替换侧栏），此后读 `code` 会 panic。
        if items.is_disposed() {
            return;
        }
        let c = code.get();
        let ws = workspace_id.get();
        if c.is_empty() || ws.is_empty() {
            return;
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let r = async {
                    let list = attachments(&c).await?;
                    let user = me().await?;
                    let r = my_role(&ws).await?;
                    Ok::<_, String>((list, user, r))
                }
                .await;
                if items.is_disposed() || code.get_untracked() != c {
                    return;
                }
                match r {
                    Ok((list, user, r)) => {
                        my_id.set(user.map(|u| u.id).unwrap_or_default());
                        role.set(r);
                        items.set(Some(Ok(list)));
                    }
                    Err(e) => items.set(Some(Err(e))),
                }
            });
        }
    };

    Effect::new(move |_| {
        code.get();
        workspace_id.get();
        if logged_out() {
            return;
        }
        load();
    });

    let do_delete = move |id: String| {
        spawn_local(async move {
            match delete_attachment(&id).await {
                Ok(_) => {
                    confirm_del.set(None);
                    error.set(None);
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let on_pick = move |ev: leptos::ev::Event| {
        let Some(input) = ev
            .target()
            .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        else {
            return;
        };
        let Some(files) = input.files() else { return };
        let Some(file) = files.get(0) else { return };
        // 选完即传；input 立刻清空，同一个文件重选也能再触发 change。
        input.set_value("");
        let c = code.get();
        if c.is_empty() {
            return;
        }
        uploading.set(true);
        spawn_local(async move {
            match upload_attachment(&c, &file).await {
                Ok(_) => {
                    error.set(None);
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
            uploading.set(false);
        });
    };

    view! {
        <div class="grp-h">
            {ic_upload()}
            {move || format!(
                "附件（{}）",
                items.get().and_then(|r| r.ok()).map(|v| v.len()).unwrap_or(0),
            )}
        </div>

        {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}

        {move || {
            let can_write = role_at_least(&role.get(), "worker");
            let can_moderate = role_at_least(&role.get(), "maintainer");
            let mine = my_id.get();
            match items.get() {
                None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                Some(Ok(list)) => {
                    if list.is_empty() {
                        view! { <div class="mut">"暂无附件"</div> }.into_any()
                    } else {
                        view! {
                            {list.into_iter().map(|a| {
                                let id = a.id.clone();
                                let id_for_confirm = id.clone();
                                let can_delete = a.created_by == mine || can_moderate;
                                let confirming = confirm_del.get().as_deref() == Some(id.as_str());
                                view! {
                                    <div class="att">
                                        <span class="ic">{ic_folder()}</span>
                                        <a href=a.url.clone() download=a.filename.clone()>{a.filename.clone()}</a>
                                        <span class="sz">{human_size(a.size)}</span>
                                        {can_delete.then(|| view! {
                                            <button
                                                class="btn sm danger"
                                                on:click={
                                                    let sid = id.clone();
                                                    move |_| confirm_del.set(Some(sid.clone()))
                                                }
                                            >"删除"</button>
                                        })}
                                    </div>
                                    {confirming.then(|| view! {
                                        <div class="c-confirm">
                                            <span class="mut">"删除后不可恢复，确定？"</span>
                                            <button class="btn danger sm" on:click={
                                                let sid = id_for_confirm.clone();
                                                move |_| do_delete(sid.clone())
                                            }>"确认删除"</button>
                                            <button class="btn sm" on:click=move |_| confirm_del.set(None)>"取消"</button>
                                        </div>
                                    })}
                                }
                            }).collect::<Vec<_>>()}
                        }.into_any()
                    }
                }
            }
        }}

        {move || {
            if !role_at_least(&role.get(), "worker") {
                return view! { <div></div> }.into_any();
            }
            view! {
                <label class="btn sm" style="margin-top:8px">
                    {move || if uploading.get() { "上传中…" } else { "上传附件" }}
                    <input
                        type="file"
                        style="display:none"
                        disabled=move || uploading.get()
                        on:change=on_pick
                    />
                </label>
            }.into_any()
        }}
    }
}
```

注意：`upload_attachment` 是 wasm-only，而组件里对它的调用在 `if cfg!(target_arch = "wasm32")` 块内 —— 但 `spawn_local` 的闭包仍会在 ssr 编译期被类型检查。若 ssr 编译报「找不到 `upload_attachment`」，把 `on_pick` 里的上传分支抽成一个 `#[cfg(target_arch = "wasm32")]` 的自由函数 `spawn_upload(file, code, ...)`，非 wasm 下给一个空实现（与 `tiny_editor::delta_to_html` 的双 cfg 写法同构）。

`src/frontend/mod.rs` 加 `pub mod attachment_list;`（保持字母序）。

- [ ] **Step 3: 换掉 entry 页占位**

`src/frontend/pages/entry.rs` 的 210-213 行，把

```rust
                    <div>
                        <div class="grp-h">{ic_upload()}"附件（≤ 50MB）"<span class="mut">"即将上线"</span></div>
                        <div class="mut">"暂无附件"</div>
                    </div>
```

替换为

```rust
                    <div>
                        <AttachmentList
                            code=Signal::derive(code)
                            workspace_id=ws_id
                            on_changed=on_changed
                        />
                    </div>
```

并加 `use crate::frontend::attachment_list::AttachmentList;`。`ic_upload` 若不再被该文件使用，从 `use crate::frontend::icons::{...}` 里移除（`ic_upload` 现在由 `AttachmentList` 内部使用）。

- [ ] **Step 4: 补一条 hover 样式**

`style/main.css` 的 `.att:hover .ibtn { opacity: 1; }` 之后加：

```css
.att a:hover {
    text-decoration: underline;
}
```

`.att` / `.att .ic` / `.att .sz` 已存在（`style/main.css:850-871`），此前无使用点，本任务起启用。

- [ ] **Step 5: 编译门禁（两条都要）**

```bash
cargo check
cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown
```
Expected: 两条都 0 错误。

- [ ] **Step 6: 提交**

```bash
git add src/frontend/attachment_list.rs src/frontend/mod.rs src/frontend/components.rs src/frontend/pages/entry.rs style/main.css
git commit -m "feat(frontend): entry 页附件区（列表 / 删除 / 手动上传）"
```

---

### Task 9: 端到端真机验证

**Files:**
- Create: `/tmp/verify-attachments.js`（验证脚本，不入库）
- Modify: `docs/superpowers/specs/2026-09-18-attachment-images-design.md`（若发现与实现不符则同步）

**Interfaces:**
- Consumes: 前八个任务的全部产出
- Produces: 一份验证结论

- [ ] **Step 1: 备份数据并建隔离实例**

不碰用户 :3000 的 watcher 与 `./data`。用副本起 3099：

```bash
rm -rf /tmp/rodeo-att-data && cp -R ./data /tmp/rodeo-att-data
cat > /tmp/rodeo-att.toml <<'EOF'
[server]
host = "127.0.0.1"
port = 3099
base_url = "http://127.0.0.1:3099"

[storage]
data_dir = "/tmp/rodeo-att-data"
EOF
cargo leptos build
LEPTOS_SITE_ADDR=127.0.0.1:3099 LEPTOS_SITE_ROOT=target/site LEPTOS_HASH_FILES=true LEPTOS_OUTPUT_NAME=rodeo ./target/debug/rodeo /tmp/rodeo-att.toml &
```

用副本而非空目录，是为了同时验证「老库自动补建两个新列族」。启动日志里不应有 column family 相关报错。

`LEPTOS_HASH_FILES=true` 与 `LEPTOS_OUTPUT_NAME=rodeo` 两个环境变量都是必需的（`hash-files` 已启用，手工起的二进制不会自动注入）。

- [ ] **Step 2: 写验证脚本**

创建 `/tmp/verify-attachments.js`。除第 5 条（超 50MB）用 curl 之外，其余都在浏览器里跑：

```js
const { chromium } = require('playwright-core');
const EXEC = '/Users/wangxiaoyan/Library/Caches/ms-playwright/chromium-1217/chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing';
(async () => {
  const browser = await chromium.launch({ executablePath: EXEC });
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const errs = [];
  page.on('pageerror', (e) => errs.push('pageerror: ' + e.message));
  await page.goto('http://127.0.0.1:3099/login');
  await page.fill('form.login-card input[type=email]', 'admin@local');
  await page.fill('form.login-card input[type=password]', 'Admin12345');
  await page.click('form.login-card button[type=submit]');
  await page.waitForURL('**/workspaces', { timeout: 25000 });
  await page.goto('http://127.0.0.1:3099/smoke-cmt');
  await page.waitForSelector('.tbl tbody tr', { timeout: 15000 });
  await page.waitForTimeout(2000);

  // 进详情面板
  await page.evaluate(() => {
    document.querySelector('.tbl tbody tr td.code')
      .dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, detail: 1 }));
  });
  await page.waitForTimeout(3500);

  // 1. 粘贴一张真 PNG（走剪贴板 DataTransfer，绕过系统剪贴板权限）
  const marker = '/api/attachments/';
  const pasted = await page.evaluate(async (marker) => {
    const res = await fetch('/pkg/rodeo.css').catch(() => null); // 仅确认页面上下文可用
    const canvas = document.createElement('canvas');
    canvas.width = 8; canvas.height = 8;
    const ctx = canvas.getContext('2d');
    ctx.fillStyle = '#f00'; ctx.fillRect(0, 0, 8, 8);
    const blob = await new Promise((r) => canvas.toBlob(r, 'image/png'));
    const file = new File([blob], 'paste-test.png', { type: 'image/png' });
    const dt = new DataTransfer();
    dt.items.add(file);
    const editor = document.querySelector('.view-body aside.detail .editor .ql-editor');
    if (!editor) return 'no-editor';
    editor.focus();
    editor.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
    return 'dispatched';
  }, marker);
  await page.waitForTimeout(4000);
  console.log('paste:', pasted);
  console.log('editor html has attachment url:', await page.evaluate((m) =>
    (document.querySelector('.view-body aside.detail .editor .ql-editor') || {}).innerHTML?.indexOf(m) >= 0, marker));
  console.log('no data: uri in editor:', await page.evaluate(() =>
    !((document.querySelector('.view-body aside.detail .editor .ql-editor') || {}).innerHTML || '').includes('data:')));

  // 2. 直连下载：断言 200 + image/png + nosniff
  const url = await page.evaluate((m) => {
    const img = document.querySelector(`img[src*="${m}"]`);
    return img ? img.getAttribute('src') : null;
  }, marker);
  console.log('attachment url:', url);
  if (url) {
    const head = await page.evaluate(async (u) => {
      const r = await fetch(u);
      return { status: r.status, ct: r.headers.get('content-type'),
               nosniff: r.headers.get('x-content-type-options') };
    }, url);
    console.log('download headers:', JSON.stringify(head));
  }

  // 3. 附件区
  await page.waitForTimeout(1000);
  console.log('attachment rows:', await page.locator('.view-body aside.detail .att').count().catch(() => 'n/a'));
  await page.locator('.entry-side .att').first().count().catch(() => {});

  console.log('ERRORS:', errs.length ? errs.join(' || ') : 'none');
  await browser.close();
})().catch((e) => { console.error('FATAL', e); process.exit(1); });
```

再补 curl 两条：

```bash
# 3. 手动传一个 .html，断言降级为附件下载
TOK=$(curl -s -X POST http://127.0.0.1:3099/api/graphql -H 'content-type: application/json' \
  -d '{"query":"mutation{login(email:\"admin@local\",password:\"Admin12345\"){token}}"}' \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["login"]["token"])')
printf '<script>alert(1)</script>' > /tmp/xss.html
# 用真实 entry code 替换 ENTRY_CODE
OP=$(python3 -c 'import json;print(json.dumps({"query":"mutation($c:String!,$f:Upload!){uploadAttachment(entryCode:$c,file:$f){id url}}","variables":{"c":"ENTRY_CODE","f":None}}))')
AID=$(curl -s -X POST http://127.0.0.1:3099/api/graphql -H "authorization: Bearer $TOK" \
  -F "operations=$OP" -F 'map={"0":["variables.f"]}' -F '0=@/tmp/xss.html;type=text/html' \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["data"]["uploadAttachment"]["id"])')
curl -s -D- -o /dev/null "http://127.0.0.1:3099/api/attachments/$AID" | grep -i 'content-type\|content-disposition\|x-content-type-options'

# 4. 超 50MB 上传 → GraphQL 错误而非 HTTP 413
head -c 55000000 /dev/urandom > /tmp/big.bin
OP2=$(python3 -c 'import json;print(json.dumps({"query":"mutation($c:String!,$f:Upload!){uploadAttachment(entryCode:$c,file:$f){id}}","variables":{"c":"ENTRY_CODE","f":None}}))')
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:3099/api/graphql \
  -H "authorization: Bearer $TOK" -F "operations=$OP2" -F 'map={"0":["variables.f"]}' -F '0=@/tmp/big.bin'
# 期望 200 且响应体是 {"errors":[{"message":"附件超过 50MB"}]}；
# 若得到 413，说明 DefaultBodyLimit 没挂对（Task 4 Step 5）。
```

- [ ] **Step 3: 逐条核对预期**

| # | 检查 | 预期 |
|---|---|---|
| 1 | 粘贴 PNG 后编辑器 HTML | 含 `/api/attachments/`，**不含** `data:` |
| 2 | 直连该 URL | 200、`content-type: image/png`、`x-content-type-options: nosniff` |
| 3 | 上传 `.html` 后直连 | `application/octet-stream` + `Content-Disposition: attachment` |
| 4 | 超 50MB 上传 | HTTP 200 + GraphQL 错误「附件超过 50MB」（**不是** 413） |
| 5 | 附件区 | 出现该行；文件名可点、大小可读、作者本人有「删除」 |
| 6 | 删除 | 列表消失，`ls /tmp/rodeo-att-data/attachments/**` 里文件也没了 |
| 7 | entry 页「历史」 | 出现「上传附件」「删除附件」（`action_label` 需要能认这两个 action，见下） |
| 8 | 条目「更新时间」 | 上传后前进 |
| 9 | 启动日志 | 老库自动补建两个列族，无报错 |

**第 7 条有一步前置工作**：`src/frontend/components.rs::action_label` 需要为 `AttachmentUploaded` / `AttachmentDeleted` 加文案映射。脚本里先执行：

```bash
grep -n "AttachmentUploaded\|CommentCreated" src/frontend/components.rs
```

若没有，在 `action_label` 的匹配里按评论的写法补 `"上传附件"` / `"删除附件"` 两条（`AuditAction` 的这几个变体在 `src/domain/audit.rs:27-28` 已存在）。

- [ ] **Step 4: 收尾**

关掉 3099 的进程（**不要**动用户的 :3000 watcher），清理 `/tmp/rodeo-att-data`、`/tmp/big.bin`、`/tmp/xss.html`。

若验证中发现与 spec 不符之处，同步更新 spec 并在提交信息里说明。

- [ ] **Step 5: 提交**

```bash
git add -A docs/superpowers/specs/2026-09-18-attachment-images-design.md src/frontend/components.rs
git commit -m "docs: 附件功能验收结果"
```

（若第 3 步没有产生改动，这一步跳过。）

---

## Self-Review

**Spec 覆盖对照**

| spec 节 | 覆盖它的任务 |
|---|---|
| §3 数据模型 | Task 1 Step 3 |
| §4 列族与键 | Task 1 Step 1-2 |
| §5.1 磁盘布局 / 文件名净化 | Task 2 Step 1 |
| §5.2 `save`（含推进 updated_at、批写失败清文件） | Task 2 Step 1 |
| §5.3 `get`/`list`/`delete`（含删文件不回滚、delete 不推进 updated_at） | Task 2 Step 1 |
| §5.4 审计（resource_id = entry_code） | Task 2 Step 1 |
| §5.5 权限 | Task 2 Step 1 + Task 4 Step 3-4 |
| §6 类型与三个字段 | Task 4 Step 1-4 |
| §6.1 `Upload` 可用性 | Task 4 Step 1（无需改特性，已核对） |
| §6.2 落盘方式 | Task 2 Step 1（上限判定在服务里，已说明取舍） |
| §6.3 请求体上限 | Task 4 Step 5 |
| §7 下载路由（白名单 / nosniff / 缓存 / 404） | Task 5 |
| §8.1 上传客户端 | Task 6 |
| §8.2 编辑器桥（含 glue.js、四个调用点、不收紧外部 img） | Task 7 |
| §8.3 附件区 | Task 8 |
| §2 非目标（不做 trait/S3、不做配置项、不做 tab、不做单测） | 全计划的 Global Constraints |
| §10 验证 7 条 | Task 9 |

**主动补充的两处**（spec 未预见，已在任务里说明理由）：Task 2 的 0 字节拒绝、Task 3 的纯图片评论放行。另 Task 9 Step 3 第 7 条要求给 `action_label` 补两个文案映射——spec §5.4 只说了审计要落到 entry 页，没提前端文案映射这一步。

**类型一致性**：`AttachmentService::save` 的参数顺序 `(actor, entry_code, filename, content_type, content)` 在 Task 2 定义、Task 4 调用，一致；`delete(actor, id, can_moderate)` 在 Task 2 定义、Task 4 调用，一致；`read(&Attachment)` 在 Task 2 定义、Task 5 调用，一致；前端 `Attachment.url` 字段在 Task 6 定义、Task 8 使用，一致；`human_size(i64)` 在 Task 8 Step 1 定义、同任务 Step 2 使用，`Attachment.size` 前端类型是 `i64`（GraphQL `Int` → JSON number），一致。

**占位符扫描**：无 TBD / TODO、「照 Task N 那样做」、「加适当的错误处理」之类。
