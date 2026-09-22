# Entry 评论 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 Entry 增加富文本评论：可发表、编辑、删除，正文进全文检索，增删改写审计，发表评论推进条目的更新时间。

**Architecture:** 新增 `Comment` 领域实体与单个 `COMMENTS` 列族（键 = `entry_code + comment_id`）；`CommentService` 单向依赖 `EntryService`，用一次 `write_batch` 原子写完评论、条目 `updated_at` 与审计；检索 schema 增加 `comments` 字段复用既有的「缺字段删库重建 + 回填」升级路径；前端新增 `CommentList` 组件挂到全屏详情页与侧栏详情面板，并用 `glue.js` 新增的 `toHtml` 做 Delta 只读渲染。

**Tech Stack:** Rust（async-graphql / rocksdb / bincode / tantivy / chrono）、Leptos 0.8（SSR + WASM hydrate）、Quill 2.0（`@opentiny/fluent-editor`）。

**Spec:** `docs/superpowers/specs/2026-09-17-entry-comments-design.md`

## Global Constraints

- **编译门禁**：每个任务收尾跑 `make check`（= `cargo check` + `cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown`）。两个目标都必须通过。
- **不写新单测**（用户明确要求）：任何模块都不新增 `#[cfg(test)] mod tests`。**但既有测试不得改坏**——`SearchIndex::index_entry` 在本计划里加了参数，它的既有测试调用点必须跟着改签名，且 `cargo test` 要全绿。
- **bincode 兼容**：`AuditAction` 按变体序号编码，新变体**只能追加在末尾**，绝不插入中间。存量数据不得反序列化失败。
- **新增列族必须注册进 `src/storage/rocksdb.rs` 的 `ALL_CFS`**，否则 `DB::open_cf` 报错。
- **注释用中文**，只写「为什么」，不写「做了什么」。照抄邻近代码的注释风格。
- **表达式语法用 AND / OR / NOT**，不用「且 / 或 / 非」。
- **同一时刻页面上只能有一个带工具栏的编辑器**（新增框与某条评论的就地编辑互斥）。这是 `glue.js` 工具栏清理作用域的前提。
- 前端所有数据加载都走 `spawn_local`（SSR 阶段不取数），照抄现有页面的写法。

---

### Task 1: 领域模型与存储

**Files:**
- Create: `src/domain/comment.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/domain/audit.rs`
- Modify: `src/storage/keys.rs`
- Modify: `src/storage/rocksdb.rs`

**Interfaces:**
- Consumes: 无（本任务是地基）
- Produces:
  - `domain::Comment { id, entry_code, workspace_id, body, created_by, updated_by, created_at, updated_at }`
  - `Comment::new(entry_code: String, workspace_id: Ulid, body: String, actor: Ulid) -> Comment`
  - `AuditAction::{CommentCreated, CommentUpdated, CommentDeleted}`
  - `keys::comment_key(entry_code: &str, comment_id: Ulid) -> Vec<u8>`
  - `cf::COMMENTS`

- [ ] **Step 1: 新建 `src/domain/comment.rs`**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Entry 评论。正文是 Quill Delta JSON，与 `Entry.detail` 同构。
///
/// 没有 `deleted_at`：评论走物理删除（对齐 `remove_labeling`），被删掉的正文
/// 留在审计的 before 快照里，不需要在列表里留空洞。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub id: Ulid,
    pub entry_code: String,
    /// 冗余存一份：权限校验与「这条评论属于哪个空间」不必每次回查 Entry。
    /// 不参与键布局——Entry Code 全局唯一，`entry_code` 单独作前缀已足够。
    pub workspace_id: Ulid,
    pub body: String,
    pub created_by: Ulid,
    pub updated_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Comment {
    pub fn new(entry_code: String, workspace_id: Ulid, body: String, actor: Ulid) -> Self {
        let now = Utc::now();
        Self {
            id: Ulid::new(),
            entry_code,
            workspace_id,
            body,
            created_by: actor,
            updated_by: actor,
            created_at: now,
            updated_at: now,
        }
    }
}
```

- [ ] **Step 2: 在 `src/domain/mod.rs` 注册模块**

在 `pub mod audit;` 之后加 `pub mod comment;`，并在 `pub use` 区加一行：

```rust
pub use comment::Comment;
```

- [ ] **Step 3: 在 `src/domain/audit.rs` 的 `AuditAction` 末尾追加三个变体**

把枚举的最后三行改成（**只在末尾追加**）：

```rust
    MemberJoined,
    InviteDeclined,
    InviteRevoked,
    // 追加在末尾：bincode 按变体序号编码，新变体只能往后加，否则存量审计日志会错位。
    CommentCreated,
    CommentUpdated,
    CommentDeleted,
}
```

- [ ] **Step 4: 在 `src/storage/keys.rs` 增加键构造函数**

加在 `labeling_key` 之后：

```rust
/// (entry_code, comment_id) 复合键，16 + 16 字节。
/// `comment_id` 是 ULID，字节序即时间序，因此按 entry_code 前缀扫描
/// 天然得到按发表时间升序的评论列表，不需要额外的排序字段。
pub fn comment_key(entry_code: &str, comment_id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(entry_code.len() + 16);
    key.extend_from_slice(entry_code.as_bytes());
    key.extend_from_slice(&comment_id.to_bytes());
    key
}
```

- [ ] **Step 5: 在 `src/storage/rocksdb.rs` 注册列族**

在 `pub const LABELINGS_BY_WORKSPACE` 之后加：

```rust
pub const COMMENTS: &str = "comments";
```

并把 `cf::COMMENTS` 加进 `ALL_CFS` 数组（漏了会导致 `DB::open_cf` 报错）。

- [ ] **Step 6: 编译门禁**

Run: `make check`
Expected: 两个目标均通过。

- [ ] **Step 7: 既有测试仍然全绿**

Run: `cargo test`
Expected: 全绿（本任务没动既有签名，应无失败）。

- [ ] **Step 8: Commit**

```bash
git add src/domain/comment.rs src/domain/mod.rs src/domain/audit.rs src/storage/keys.rs src/storage/rocksdb.rs
git commit -m "feat(domain): 新增 Comment 实体与 COMMENTS 列族"
```

---

### Task 2: 检索索引收录评论正文

**Files:**
- Modify: `src/service/search.rs`（`open` / `add_entry_doc` / `index_entry` / `backfill`，以及文件内既有测试的调用点）

**Interfaces:**
- Consumes: `cf::COMMENTS`、`domain::Comment`（Task 1）
- Produces:
  - `search::comments_text(store: &DocStore, entry_code: &str) -> Result<String, AppError>`
  - `SearchIndex::index_entry(&self, entry: &Entry, labels: &[Labeling], comments_text: &str) -> Result<(), AppError>`
  - 常量 `COMMENTS_FIELD = "comments"`

- [ ] **Step 1: 常量与 schema 字段**

在 `const CODE_TEXT_FIELD` 之后加：

```rust
/// 评论正文的检索副本。单开一个字段而不是拼进 `content`，
/// 是为了不让标题/详情/评论混在一起影响相关度。
const COMMENTS_FIELD: &str = "comments";
```

在 `SearchIndex` 结构体加字段 `f_comments: Field`，在 `open` 里加：

```rust
let f_comments = text(&mut b, COMMENTS_FIELD);
```

- [ ] **Step 2: 让索引升级路径识别新字段**

`open` 里的重建判断加上新字段（旧索引缺 `comments` 时同样删库重建，由 `backfill` 回灌）：

```rust
Ok(existing)
    if existing.schema().get_field(CODE_TEXT_FIELD).is_err()
        || existing.schema().get_field(COMMENTS_FIELD).is_err() =>
{
```

结构体初始化处补上 `f_comments,`。

- [ ] **Step 3: 新增评论正文抽取函数**

加在 `strip_rich_text` 之后：

```rust
/// 把一条条目的全部评论正文抽成纯文本，供检索索引拼接。
pub fn comments_text(store: &DocStore, entry_code: &str) -> Result<String, AppError> {
    let mut out = String::new();
    for (_, v) in store.scan_prefix(cf::COMMENTS, entry_code.as_bytes())? {
        let c: crate::domain::Comment = bincode::deserialize(&v)?;
        let t = strip_rich_text(&c.body);
        if !t.trim().is_empty() {
            out.push_str(&t);
            out.push('\n');
        }
    }
    Ok(out)
}
```

- [ ] **Step 4: 索引写入带评论**

`add_entry_doc` 加参数并写入新字段：

```rust
fn add_entry_doc(
    &self,
    writer: &IndexWriter,
    entry: &Entry,
    labels: &[Labeling],
    comments: &str,
) -> Result<(), AppError> {
```

`doc!` 里加一行：

```rust
self.f_comments => comments.to_string(),
```

`index_entry` 加参数：

```rust
pub fn index_entry(
    &self,
    entry: &Entry,
    labels: &[Labeling],
    comments: &str,
) -> Result<(), AppError> {
```

`search` 里 `QueryParser::for_index` 的字段列表加上 `self.f_comments`。

- [ ] **Step 5: 回填一并灌评论**

`backfill` 的循环里，取完 labels 之后加：

```rust
let comments = comments_text(store, &entry.code)?;
self.add_entry_doc(&writer, &entry, &labels, &comments)?;
```

- [ ] **Step 6: 改既有测试调用点（不加新用例）**

文件内 `#[cfg(test)] mod tests` 中每一处 `idx.index_entry(&e, &[])` / `idx.index_entry(&ea, &[])` / `idx.index_entry(&e, &[l])` 都要补第三个参数。检索评论的用例按下面写（这是**改造既有用例**，不是新增）：

```rust
    #[test]
    fn indexes_and_searches_chinese_substring() {
        // ...原有代码...
        idx.index_entry(&e, &[], "用户反馈邮箱收不到验证码").unwrap();
        // ...
    }
```

其余不关心评论的调用点统一传 `""`。

- [ ] **Step 7: 编译与测试**

Run: `make check && cargo test`
Expected: 两个目标通过，`cargo test` 全绿。

- [ ] **Step 8: Commit**

```bash
git add src/service/search.rs
git commit -m "feat(search): 检索索引收录评论正文"
```

---

### Task 3: EntryService 暴露重索引入口

**Files:**
- Modify: `src/service/entry.rs`（`EntryService` 定义、`reindex`）

**Interfaces:**
- Consumes: `search::comments_text`（Task 2）
- Produces:
  - `EntryService: Clone`
  - `EntryService::reindex_by_code(&self, code: &str) -> Result<(), AppError>`

- [ ] **Step 1: 让 `EntryService` 可克隆**

字段全是 `Arc`，克隆只是复制两个指针，供 `CommentService` 持有同一份实例：

```rust
#[derive(Clone)]
pub struct EntryService {
    store: Arc<DocStore>,
    search: Option<Arc<SearchIndex>>,
}
```

- [ ] **Step 2: `reindex` 带上评论正文**

```rust
    fn reindex(&self, entry: &Entry) {
        let Some(search) = &self.search else { return };
        let labels = self.labelings(&entry.code).unwrap_or_default();
        // 评论正文也进检索：搜得到评论内容，但检索字段与标题/详情分开，不会互相干扰相关度。
        let comments = crate::service::search::comments_text(&self.store, &entry.code)
            .unwrap_or_default();
        // 归档与软删除一样，都让条目退出全文检索：归档条目不该再被搜索命中。
        let out_of_play = entry.is_deleted() || self.is_archived(&entry.code).unwrap_or(false);
        if out_of_play {
            if let Err(e) = search.remove_entry(&entry.code) {
                tracing::warn!("移除检索索引失败 {}: {e}", entry.code);
            }
        } else if let Err(e) = search.index_entry(entry, &labels, &comments) {
            tracing::warn!("更新检索索引失败 {}: {e}", entry.code);
        }
    }
```

- [ ] **Step 3: 增加按 code 重索引的公开入口**

加在 `get` 之后：

```rust
    /// 供评论服务在评论变更后重建该条目的检索文档——评论不属于 Entry 的字段，
    /// 只能由外部触发这次重建。
    pub fn reindex_by_code(&self, code: &str) -> Result<(), AppError> {
        if let Some(entry) = self.get(code)? {
            self.reindex(&entry);
        }
        Ok(())
    }
```

- [ ] **Step 4: 编译门禁**

Run: `make check && cargo test`
Expected: 均通过。

- [ ] **Step 5: Commit**

```bash
git add src/service/entry.rs
git commit -m "feat(service): EntryService 支持按 code 重索引并收录评论"
```

---

### Task 4: CommentService

**Files:**
- Create: `src/service/comment.rs`
- Modify: `src/service/mod.rs`

**Interfaces:**
- Consumes: `EntryService::{get, reindex_by_code}`（Task 3）、`keys::comment_key`、`cf::COMMENTS`（Task 1）、`service::audit::audit_ops`
- Produces:
  - `CommentService::new(store: Arc<DocStore>, entries: EntryService) -> Self`
  - `list(&self, entry_code: &str) -> Result<Vec<Comment>, AppError>`
  - `count(&self, entry_code: &str) -> Result<usize, AppError>`
  - `get(&self, entry_code: &str, id: Ulid) -> Result<Option<Comment>, AppError>`
  - `create(&self, actor: Ulid, entry_code: &str, body: &str) -> Result<Comment, AppError>`
  - `update(&self, actor: Ulid, entry_code: &str, id: Ulid, body: &str) -> Result<Comment, AppError>`
  - `delete(&self, actor: Ulid, entry_code: &str, id: Ulid, can_moderate: bool) -> Result<(), AppError>`
  - `Services.comment: CommentService`

- [ ] **Step 1: 新建 `src/service/comment.rs`**

```rust
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

/// 只有空白内容的评论不算评论。直接用检索侧的 Delta 抽文本逻辑，
/// 这样「Delta 里只有空白片段」和「纯文本全是空格」两种情况一并覆盖。
fn is_blank_body(body: &str) -> bool {
    crate::service::search::strip_rich_text(body).trim().is_empty()
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
```

- [ ] **Step 2: 在 `src/service/mod.rs` 注册服务**

模块声明加 `pub mod comment;`，`pub use` 区加 `pub use comment::CommentService;`。

`Services` 结构体加字段：

```rust
    pub comment: CommentService,
```

`Services::new` 里**先构造 `entry` 再构造 `comment`**（后者要拿前者的克隆）：

```rust
        let entry = EntryService::with_search(store.clone(), search.clone());
        let comment = CommentService::new(store.clone(), entry.clone());
        let services = Self {
            auth: AuthService::new(store.clone(), config.clone()),
            workspace: WorkspaceService::new(store.clone()),
            entry,
            comment,
            label: LabelService::new(store.clone()),
```

- [ ] **Step 3: 编译门禁**

Run: `make check && cargo test`
Expected: 均通过。

- [ ] **Step 4: Commit**

```bash
git add src/service/comment.rs src/service/mod.rs
git commit -m "feat(service): 新增 CommentService，评论增删改写审计并推进条目更新时间"
```

---

### Task 5: GraphQL 接口

**Files:**
- Modify: `src/api/graphql.rs`

**Interfaces:**
- Consumes: `CommentService`（Task 4）、既有 `GraphqlContext::{require_auth, require_role, require_member}`、`parse_ulid`、`GqlAccount`、`gql_entry` 的账号回填写法
- Produces:
  - `GqlComment`、`GqlCommentCount`
  - 查询 `comments(entryCode)`、`commentCounts(entryCodes)`
  - 变更 `createComment`、`updateComment`、`deleteComment`

- [ ] **Step 1: 新增输出类型与组装函数**

加在 `gql_entry` 函数之后：

```rust
#[derive(SimpleObject, Clone)]
pub struct GqlComment {
    id: ID,
    entry_code: String,
    body: String,
    created_by: ID,
    updated_by: ID,
    created_at: String,
    updated_at: String,
    /// 作者 / 最后修改人账号；账号已删除则为 null。与 GqlEntry 同一套回填方式。
    created_by_account: Option<GqlAccount>,
    updated_by_account: Option<GqlAccount>,
}

#[derive(SimpleObject, Clone)]
pub struct GqlCommentCount {
    entry_code: String,
    count: i32,
}

/// 组装 GqlComment，顺带补上作者与最后修改人的账号。
fn gql_comment(gql: &GraphqlContext, c: Comment) -> GqlResult<GqlComment> {
    let created_by_account = gql.services.auth.find_by_id(c.created_by)?.map(Into::into);
    let updated_by_account = gql.services.auth.find_by_id(c.updated_by)?.map(Into::into);
    Ok(GqlComment {
        id: c.id.to_string().into(),
        entry_code: c.entry_code,
        body: c.body,
        created_by: c.created_by.to_string().into(),
        updated_by: c.updated_by.to_string().into(),
        created_at: c.created_at.to_rfc3339(),
        updated_at: c.updated_at.to_rfc3339(),
        created_by_account,
        updated_by_account,
    })
}
```

`Comment` 要进 `use crate::domain::{...}` 的导入列表。

- [ ] **Step 2: 两个查询**

加在 `Query` 的 `entry` 查询之后：

```rust
    /// 某条目的全部评论，按发表时间升序。成员即可读（与 labelSchemas 一致）。
    async fn comments(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
    ) -> GqlResult<Vec<GqlComment>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let entry = gql.services.entry.get(&entry_code)?.ok_or(AppError::NotFound)?;
        gql.require_member(entry.workspace_id)?;
        gql.services
            .comment
            .list(&entry_code)?
            .into_iter()
            .map(|c| gql_comment(gql, c))
            .collect()
    }

    /// 视图表格当前页的评论计数。越权或已删除的条目直接跳过——不泄露其存在性。
    async fn comment_counts(
        &self,
        ctx: &Context<'_>,
        entry_codes: Vec<String>,
    ) -> GqlResult<Vec<GqlCommentCount>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let mut out = Vec::with_capacity(entry_codes.len());
        for code in entry_codes {
            let Some(entry) = gql.services.entry.get(&code)? else {
                continue;
            };
            if gql
                .services
                .workspace
                .get_member(entry.workspace_id, auth.account_id)?
                .is_none()
            {
                continue;
            }
            out.push(GqlCommentCount {
                entry_code: code.clone(),
                count: gql.services.comment.count(&code)? as i32,
            });
        }
        Ok(out)
    }
```

- [ ] **Step 3: 三个变更**

加在 `Mutation` 的 `delete_entry` 之后：

```rust
    /// 发表评论（Worker+）。同时推进条目的 updated_at。
    async fn create_comment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        body: String,
    ) -> GqlResult<GqlComment> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&entry_code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        let c = gql.services.comment.create(auth.account_id, &entry_code, &body)?;
        gql_comment(gql, c)
    }

    /// 编辑评论（Worker+ 且作者本人）。
    async fn update_comment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        id: ID,
        body: String,
    ) -> GqlResult<GqlComment> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&entry_code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        let id = parse_ulid(id.as_str())?;
        let c = gql.services.comment.update(auth.account_id, &entry_code, id, &body)?;
        gql_comment(gql, c)
    }

    /// 删除评论（作者本人，或 Maintainer+）。
    async fn delete_comment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        id: ID,
    ) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&entry_code)?.ok_or(AppError::NotFound)?;
        gql.require_member(entry.workspace_id)?;
        // 先看是不是 Maintainer+；不是也不立刻拒绝——作者本人仍可撤回自己的评论。
        let can_moderate = gql
            .require_role(entry.workspace_id, WorkspaceRole::Maintainer)
            .is_ok();
        let id = parse_ulid(id.as_str())?;
        gql.services
            .comment
            .delete(auth.account_id, &entry_code, id, can_moderate)?;
        Ok(true)
    }
```

- [ ] **Step 4: 编译门禁**

Run: `make check`
Expected: 两个目标均通过。

- [ ] **Step 5: 手工验证 schema 暴露正确**

启动 `make serve`，在浏览器里执行以下查询（控制台或任意 GraphQL 客户端，注意带登录态），确认字段名与类型符合预期：

```graphql
query { comments(entryCode: "填入一个真实 code") { id body createdAt createdByAccount { name } } }
```

Expected: 返回空数组而不是报错（此时还没写评论）。字段名不对会在这里暴露。

- [ ] **Step 6: Commit**

```bash
git add src/api/graphql.rs
git commit -m "feat(api): 评论的 GraphQL 查询与变更"
```

---

### Task 6: 富文本只读渲染与工具栏作用域修复

**Files:**
- Modify: `public/tiny-editor/glue.js`
- Modify: `src/frontend/tiny_editor.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - JS：`window.__rodeo_tiny_editor__.toHtml(deltaJson) -> string`
  - Rust：`tiny_editor::delta_to_html(delta: &str) -> String`

- [ ] **Step 1: 修 `create()` 的全局工具栏清理**

`create()` 的第 12–31 行现在是：

```js
  create(el, deltaJson) {
    // 清理上一次编辑器残留的工具栏/浮层。工具栏可能被渲染为容器外的兄弟节点，
    // 随容器卸载不会被一并移除，因此这里按类名全局清一遍（本应用同一时刻只有一个编辑器）。
    document.querySelectorAll('.ql-toolbar, .ql-tooltip').forEach((n) => n.remove());
    // 再清空容器，避免复用节点时叠加旧内容。
    el.innerHTML = '';
    const editor = new FluentEditor(el, {
      theme: 'snow',
      placeholder: '输入详情…',
      modules: {
        toolbar: [
          ['bold', 'italic', 'underline', 'strike'],
          [{ header: 1 }, { header: 2 }, { header: 3 }],
          [{ list: 'ordered' }, { list: 'bullet' }],
          ['blockquote', 'code-block'],
          ['link'],
          ['clean'],
        ],
      },
    });

    if (deltaJson) {
      try {
        editor.setContents(JSON.parse(deltaJson), 'silent');
      } catch (e) {
        // 非法 Delta 时忽略，编辑器保持空内容。
      }
    }

    return editor;
  },
```

那个全局清理假设「本应用同一时刻只有一个编辑器」，而侧栏详情面板本来就有一个详情编辑器，评论框一出现就会把它的工具栏抹掉。改成按容器作用域。在 `window.__rodeo_tiny_editor__ = {` **之前**加：

```js
// 只清理本容器上一次留下的工具栏/浮层。Quill 的 snow 主题把工具栏插成容器的
// 前一个兄弟节点，容器卸载时不会被一并带走；早先这里按类名全局清理，但页面上
// 同时存在详情编辑器与评论编辑器之后，全局清理会抹掉另一个编辑器的工具栏。
function cleanupAux(el) {
  if (el.__rodeo_aux) {
    el.__rodeo_aux.forEach((n) => n.remove());
    el.__rodeo_aux = [];
  }
  // 浮层（ql-tooltip）可能被挂到 body 上，只扫 body 的直接子节点，
  // 不会碰到其他编辑器容器内部的东西。
  document.querySelectorAll('body > .ql-tooltip').forEach((n) => n.remove());
}
```

`create()` 的头尾改为（中间的 `FluentEditor` 配置与 `setContents` 原样保留）：

```js
  create(el, deltaJson) {
    cleanupAux(el);
    // 再清空容器，避免复用节点时叠加旧内容。
    el.innerHTML = '';
    // 记下这次新建过程中新增的兄弟节点（工具栏/浮层），供下次创建时精确清理。
    const before = new Set(el.parentNode.children);
    const editor = new FluentEditor(el, {
      /* ……原样保留…… */
    });
    el.__rodeo_aux = Array.from(el.parentNode.children).filter((n) => !before.has(n));

    if (deltaJson) {
      /* ……原样保留…… */
    }

    return editor;
  },
```

- [ ] **Step 2: 新增 `toHtml`**

在桥接对象上增加方法（与 `create` 并列）：

```js
  // 把 Delta JSON 渲染成 HTML，供评论列表这类只读场景使用。
  // 复用同一个离屏实例：整页评论只占一个编辑器对象，按条新建既慢，
  // 又会和工具栏清理互相干扰。
  toHtml(deltaJson) {
    if (!deltaJson) return '';
    if (!renderEditor) {
      renderHost = document.createElement('div');
      renderHost.style.display = 'none';
      document.body.appendChild(renderHost);
      renderEditor = new FluentEditor(renderHost, {
        readOnly: true,
        modules: { toolbar: false },
      });
    }
    try {
      renderEditor.setContents(JSON.parse(deltaJson), 'silent');
      return renderEditor.getSemanticHTML();
    } catch (e) {
      return '';
    }
  },
```

在同一个位置（`cleanupAux` 旁边、`window.__rodeo_tiny_editor__ = {` 之前）声明两个模块级变量：

```js
// 只读渲染复用同一个离屏实例；整页评论共用一个，与评论条数无关。
let renderHost = null;
let renderEditor = null;
```

- [ ] **Step 3: Rust 侧包装**

在 `src/frontend/tiny_editor.rs` 末尾加：

```rust
/// 把 Delta JSON 渲染成 HTML。非 wasm 目标或桥接未加载时返回空串
/// （与 `TinyEditor` 一样，渲染只发生在浏览器里）。
pub fn delta_to_html(delta: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        use js_sys::{Array, Function, Reflect};
        use wasm_bindgen::{JsCast, JsValue};

        if delta.trim().is_empty() {
            return String::new();
        }
        let global = js_sys::global();
        let Ok(bridge) = Reflect::get(&global, &JsValue::from_str("__rodeo_tiny_editor__")) else {
            return String::new();
        };
        let Some(to_html) = Reflect::get(&bridge, &JsValue::from_str("toHtml"))
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
        else {
            return String::new();
        };
        let args = Array::new();
        args.push(&JsValue::from_str(delta));
        Reflect::apply(&to_html, &bridge, args.as_ref())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = delta;
        String::new()
    }
}
```

- [ ] **Step 4: 编译门禁**

Run: `make check`
Expected: 两个目标均通过。

- [ ] **Step 5: 实测两项风险（在浏览器控制台执行）**

启动 `make serve` 并登录，在控制台依次执行：

```js
window.__rodeo_tiny_editor__.toHtml('{"ops":[{"insert":"加粗","attributes":{"bold":true}}]}')
window.__rodeo_tiny_editor__.toHtml('{"ops":[{"insert":"点我","attributes":{"link":"javascript:alert(1)"}}]}')
```

Expected：第一条返回含 `<strong>` 的 HTML；第二条**必须**确认不输出可执行的 `javascript:` 链接。若第二条返回了带 `javascript:` 的 `<a href>`，在 `components.rs::CommentList` 渲染前加一层协议过滤（把非 `http/https/mailto` 的 href 去掉），并在提交前记录该处理。

- [ ] **Step 6: Commit**

```bash
git add public/tiny-editor/glue.js src/frontend/tiny_editor.rs
git commit -m "feat(frontend): 富文本只读渲染，并修复工具栏全局清理"
```

---

### Task 7: 前端 GraphQL 客户端与角色比较

**Files:**
- Modify: `src/frontend/graphql_client.rs`
- Modify: `src/frontend/components.rs`

**Interfaces:**
- Consumes: GraphQL 接口（Task 5）
- Produces:
  - `graphql_client::Comment { id, entry_code, body, created_by, updated_by, created_at, updated_at, created_by_account, updated_by_account }`
  - `graphql_client::{comments, comment_counts, create_comment, update_comment, delete_comment}`
  - `components::role_at_least(role: &str, min: &str) -> bool`

- [ ] **Step 1: 前端 `Comment` 结构体与字段常量**

加在 `Labeling` 结构体之后：

```rust
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub entry_code: String,
    pub body: String,
    pub created_by: String,
    pub updated_by: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub created_by_account: Option<AccountBrief>,
    #[serde(default)]
    pub updated_by_account: Option<AccountBrief>,
}
```

在 `ENTRY_FIELDS` 之后加：

```rust
const COMMENT_FIELDS: &str = "id entryCode body createdAt updatedAt createdBy updatedBy \
     createdByAccount { id name email } updatedByAccount { id name email }";
```

- [ ] **Step 2: 五个客户端函数**

加在 `update_entry` 之前：

```rust
pub async fn comments(entry_code: &str) -> Result<Vec<Comment>, String> {
    let q = format!("query($c: String!) {{ comments(entryCode: $c) {{ {COMMENT_FIELDS} }} }}");
    let data = graphql(&q, json!({ "c": entry_code })).await?;
    serde_json::from_value(data.get("comments").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 取一批条目的评论条数。服务端会跳过越权或不存在的条目。
pub async fn comment_counts(entry_codes: &[String]) -> Result<Vec<(String, i32)>, String> {
    let data = graphql(
        "query($c: [String!]!) { commentCounts(entryCodes: $c) { entryCode count } }",
        json!({ "c": entry_codes }),
    )
    .await?;
    let list = data
        .get("commentCounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list
        .into_iter()
        .filter_map(|v| {
            let code = v.get("entryCode")?.as_str()?.to_string();
            let count = v.get("count")?.as_i64()? as i32;
            Some((code, count))
        })
        .collect())
}

pub async fn create_comment(entry_code: &str, body: &str) -> Result<Comment, String> {
    let q = format!(
        "mutation($c: String!, $b: String!) {{ createComment(entryCode: $c, body: $b) {{ {COMMENT_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "c": entry_code, "b": body })).await?;
    serde_json::from_value(data.get("createComment").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn update_comment(entry_code: &str, id: &str, body: &str) -> Result<Comment, String> {
    let q = format!(
        "mutation($c: String!, $i: ID!, $b: String!) {{ updateComment(entryCode: $c, id: $i, body: $b) {{ {COMMENT_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "c": entry_code, "i": id, "b": body })).await?;
    serde_json::from_value(data.get("updateComment").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// `id` 与 `entryCode` 必须成对给出：评论主键是 (entry_code, comment_id)。
pub async fn delete_comment(entry_code: &str, id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!, $i: ID!) { deleteComment(entryCode: $c, id: $i) }",
        json!({ "c": entry_code, "i": id }),
    )
    .await?;
    Ok(data
        .get("deleteComment")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}
```

- [ ] **Step 3: 角色比较助手**

加在 `components.rs` 的 `role_label` 之后：

```rust
/// 角色是否达到指定等级。与后端 `WorkspaceRole` 的声明顺序一致
/// （owner > maintainer > worker > reader）。
/// 后端 `as_str()` 返回小写，这里仍照 `role_label` 的做法归一化一次，
/// 免得上游哪天改成大写时静默退化成 Reader。
pub fn role_at_least(role: &str, min: &str) -> bool {
    let rank = |r: &str| match r.to_ascii_lowercase().as_str() {
        "owner" => 3,
        "maintainer" => 2,
        "worker" => 1,
        _ => 0,
    };
    rank(role) >= rank(min)
}
```

- [ ] **Step 4: 编译门禁**

Run: `make check`
Expected: 两个目标均通过（新函数此时还没被调用，wasm 目标下若因 dead_code 报警告属正常，Task 8 会消除）。

- [ ] **Step 5: Commit**

```bash
git add src/frontend/graphql_client.rs src/frontend/components.rs
git commit -m "feat(frontend): 评论的 GraphQL 客户端与角色比较助手"
```

---

### Task 8: CommentList 组件

**Files:**
- Create: `src/frontend/comment_list.rs`
- Modify: `src/frontend/mod.rs`
- Modify: `style/main.css`
- Modify: `Cargo.toml`（加 `gloo-timers`，见 Step 1）

**Interfaces:**
- Consumes: Task 6 的 `delta_to_html`、`TinyEditor`；Task 7 的五个客户端函数与 `role_at_least`；`components::{Avatar, short_time, logged_out}`；`icons::ic_comment`
- Produces: `comment_list::CommentList` 组件，props 为
  - `code: Signal<String>`（条目编码）
  - `workspace_id: Signal<String>`
  - `on_changed: Callback<()>`

- [ ] **Step 1: 加一个 wasm 侧的定时依赖**

发表评论后要把输入框清空，而 `TinyEditor` 只在挂载时读一次 `initial`，改 prop 不会清内容，只能把编辑器整个换掉（`composer_ready` 从 true 翻到 false 再翻回 true）。

问题在于：两次 `set` 之间如果没有真正的挂起，Leptos 会把它们合批处理，观察者只看到最终值 `true`，组件根本不会卸载——输入框清不掉。所以中间必须等一个宏任务。`gloo-timers` 提供 `TimeoutFuture`（底层就是 `setTimeout(0)`），是这件事的标准做法，且与项目已有的 `gloo-net` / `gloo-storage` 同族。

`gloo-timers` 只有 wasm 实现，native 目标编不过，所以必须加进 `Cargo.toml` 里那个 **`[target.'cfg(target_arch = "wasm32")'.dependencies]`** 段（与 `gloo-net` / `gloo-storage` 同段），紧跟 `gloo-storage` 一行：

```toml
gloo-timers = { version = "0.3", features = ["futures"] }
```

- [ ] **Step 2: 新建 `src/frontend/comment_list.rs`**

```rust
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::components::{logged_out, role_at_least, short_time, Avatar};
use crate::frontend::graphql_client::{
    comments, create_comment, delete_comment, me, my_role, update_comment, Comment,
};
use crate::frontend::icons::ic_comment;
use crate::frontend::tiny_editor::{delta_to_html, TinyEditor};

/// 列表项：正文的 HTML 在取数时一次算好。
/// 不在 `view!` 里现算是因为 SSR 阶段没有 JS 桥，渲染会得到空串。
#[derive(Clone)]
struct CommentView {
    comment: Comment,
    html: String,
}

/// 让出一个宏任务。Leptos 只在这次让渡之后才会把信号变更真正落到 DOM 上，
/// 因此「卸载再挂回编辑器」中间必须有它。
/// 双 `cfg` 两个实现，照 `graphql()` 的写法办：`gloo-timers` 只有 wasm 实现，
/// native 目标下这个函数体只能是空的（SSR 阶段也不会走到发表评论）。
#[cfg(target_arch = "wasm32")]
async fn next_tick() {
    gloo_timers::future::TimeoutFuture::new(0).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn next_tick() {}

#[component]
pub fn CommentList(
    /// 条目编码；空串时不取数也不渲染（详情面板未选中条目时）。
    code: Signal<String>,
    /// 所属工作空间，用来查当前用户角色。
    workspace_id: Signal<String>,
    /// 评论增删改后通知外层：表格的「更新时间」列与评论徽标要跟着刷新。
    on_changed: Callback<()>,
) -> impl IntoView {
    let items = RwSignal::new(None::<Result<Vec<CommentView>, String>>);
    let role = RwSignal::new(String::new());
    let my_id = RwSignal::new(String::new());
    let draft = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<String>);
    let edit_body = RwSignal::new(String::new());
    let confirm_del = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);
    // 发表后要清空输入框，而 TinyEditor 只在挂载时读一次 initial，改 prop 不会清内容。
    // 用一个「卸载 → 让出一帧 → 重新挂载」的开关把编辑器整个换掉。
    let composer_ready = RwSignal::new(true);

    let load = move || {
        let c = code.get();
        let ws = workspace_id.get();
        if c.is_empty() || ws.is_empty() {
            return;
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let r = async {
                    let list = comments(&c).await?;
                    let user = me().await?;
                    let r = my_role(&ws).await?;
                    Ok::<_, String>((list, user, r))
                }
                .await;
                if code.get_untracked() != c {
                    return;
                }
                match r {
                    Ok((list, user, r)) => {
                        my_id.set(user.map(|u| u.id).unwrap_or_default());
                        role.set(r);
                        items.set(Some(Ok(list
                            .into_iter()
                            .map(|comment| CommentView {
                                html: delta_to_html(&comment.body),
                                comment,
                            })
                            .collect())));
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

    let on_composer_change = Callback::new(move |d: String| draft.set(d));
    let on_edit_change = Callback::new(move |d: String| edit_body.set(d));

    let submit = move |_| {
        let c = code.get();
        let body = draft.get();
        spawn_local(async move {
            match create_comment(&c, &body).await {
                Ok(_) => {
                    error.set(None);
                    // 先卸载编辑器，等一个宏任务让这次卸载真正落到 DOM 上，再挂回来
                    // ——内容自然是空的。中间不能省这一步：两次 set 同一批次完成的话
                    // 观察者只看到最终值，编辑器根本不会卸载。
                    composer_ready.set(false);
                    next_tick().await;
                    composer_ready.set(true);
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let save_edit = move |id: String| {
        let c = code.get();
        let body = edit_body.get();
        spawn_local(async move {
            match update_comment(&c, &id, &body).await {
                Ok(_) => {
                    editing.set(None);
                    error.set(None);
                    load();
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let do_delete = move |id: String| {
        let c = code.get();
        spawn_local(async move {
            match delete_comment(&c, &id).await {
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

    view! {
        <div class="comments">
            <div class="grp-h">
                {ic_comment()}
                {move || format!(
                    "评论（{}）",
                    items.get().and_then(|r| r.ok()).map(|v| v.len()).unwrap_or(0),
                )}
            </div>

            {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}

            {move || {
                let can_write = role_at_least(&role.get(), "worker");
                let can_moderate = role_at_least(&role.get(), "maintainer");
                let editing_now = editing.get();
                match items.get() {
                    None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            view! { <div class="mut">"暂无评论"</div> }.into_any()
                        } else {
                            let mine = my_id.get();
                            view! {
                                <div class="c-list">
                                    {list.into_iter().map(|cv| {
                                        let c = cv.comment.clone();
                                        let id = c.id.clone();
                                        let id_for_edit = id.clone();
                                        let id_for_save = id.clone();
                                        let id_for_del = id.clone();
                                        let id_for_confirm = id.clone();
                                        let is_mine = c.created_by == mine;
                                        let can_edit = can_write && is_mine;
                                        let can_delete = is_mine || can_moderate;
                                        let is_editing = editing_now.as_deref() == Some(id.as_str());
                                        let author = c
                                            .created_by_account
                                            .as_ref()
                                            .map(|a| a.name.clone())
                                            .unwrap_or_else(|| "已注销账号".to_string());
                                        let body_html = cv.html.clone();
                                        // 两份克隆：「编辑」按钮的闭包和编辑器组件各要一份，
                                        // 一份会被闭包 move 走，另一份留给 `initial=`。
                                        let body_for_btn = c.body.clone();
                                        let body_for_editor = c.body.clone();
                                        let edited = c.updated_at != c.created_at;
                                        view! {
                                            <div class="c-item">
                                                <Avatar text=author.clone() />
                                                <div class="c-main">
                                                    <div class="c-head">
                                                        <span class="c-name">{author}</span>
                                                        <span class="t">{short_time(&c.created_at)}</span>
                                                        {edited.then(|| view! {
                                                            <span class="mut">"已编辑"</span>
                                                        })}
                                                        <span style="margin-left:auto" class="c-acts">
                                                            {can_edit.then(|| view! {
                                                                <button
                                                                    class="btn sm"
                                                                    disabled=is_editing
                                                                    on:click=move |_| {
                                                                        editing.set(Some(id_for_edit.clone()));
                                                                        edit_body.set(body_for_btn.clone());
                                                                    }
                                                                >"编辑"</button>
                                                            })}
                                                            {can_delete.then(|| view! {
                                                                <button
                                                                    class="btn sm danger"
                                                                    on:click=move |_| confirm_del.set(Some(id_for_del.clone()))
                                                                >"删除"</button>
                                                            })}
                                                        </span>
                                                    </div>
                                                    {if is_editing {
                                                        view! {
                                                            <div class="c-edit">
                                                                <TinyEditor initial=body_for_editor on_change=on_edit_change />
                                                                <div class="c-edit-acts">
                                                                    <button class="btn pri sm" on:click={
                                                                        let sid = id_for_save.clone();
                                                                        move |_| save_edit(sid.clone())
                                                                    }>"保存"</button>
                                                                    <button class="btn sm" on:click=move |_| editing.set(None)>"取消"</button>
                                                                </div>
                                                            </div>
                                                        }.into_any()
                                                    } else {
                                                        view! {
                                                            <div class="c-body" inner_html=body_html></div>
                                                        }.into_any()
                                                    }}
                                                    {(confirm_del.get().as_deref() == Some(id_for_confirm.as_str())).then(|| view! {
                                                        <div class="c-confirm">
                                                            <span class="mut">"删除后不可恢复，确定？"</span>
                                                            <button class="btn danger sm" on:click={
                                                                let sid = id_for_confirm.clone();
                                                                move |_| do_delete(sid.clone())
                                                            }>"确认删除"</button>
                                                            <button class="btn sm" on:click=move |_| confirm_del.set(None)>"取消"</button>
                                                        </div>
                                                    })}
                                                </div>
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()}
                                </div>
                            }.into_any()
                        }
                    }
                }
            }}

            {move || {
                if !role_at_least(&role.get(), "worker") {
                    return view! { <div></div> }.into_any();
                }
                // 已有评论进入编辑态时收起新增框：页面上同时只能有一个带工具栏的编辑器，
                // glue.js 的工具栏清理作用域依赖这条不变式。
                if editing.get().is_some() {
                    return view! {
                        <div class="mut">"正在编辑评论，保存或取消后可继续发表"</div>
                    }.into_any();
                }
                view! {
                    <div class="c-composer">
                        {move || if composer_ready.get() {
                            view! {
                                <TinyEditor initial=String::new() on_change=on_composer_change />
                            }.into_any()
                        } else {
                            view! { <div class="mut">"…"</div> }.into_any()
                        }}
                        <div class="c-edit-acts">
                            <button class="btn pri sm" on:click=submit>"发表评论"</button>
                        </div>
                    </div>
                }.into_any()
            }}
        </div>
    }
}
```

- [ ] **Step 3: 在 `src/frontend/mod.rs` 注册模块**

```rust
pub mod comment_list;
```

- [ ] **Step 4: 样式**

加到 `style/main.css` 末尾：

```css
/* ===== 评论 ===== */
.comments{display:flex;flex-direction:column;gap:10px}
.c-list{display:flex;flex-direction:column;gap:14px}
.c-item{display:flex;gap:8px}
.c-main{flex:1;min-width:0;display:flex;flex-direction:column;gap:4px}
.c-head{display:flex;align-items:center;gap:8px;font-size:13px}
.c-name{font-weight:500}
.c-acts{display:flex;gap:4px}
.c-body{font-size:13px;line-height:1.6;word-break:break-word}
.c-body p{margin:0 0 4px}
.c-body img{max-width:100%}
.c-edit-acts{display:flex;gap:6px;margin-top:6px}
.c-confirm{display:flex;align-items:center;gap:8px}
.c-composer{display:flex;flex-direction:column;gap:6px}
```

- [ ] **Step 5: 编译门禁**

Run: `make check`
Expected: 两个目标均通过。

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/frontend/comment_list.rs src/frontend/mod.rs style/main.css
git commit -m "feat(frontend): CommentList 组件与评论样式"
```

---

### Task 9: 挂载到两处详情

**Files:**
- Modify: `src/frontend/pages/entry.rs`（全屏详情页）
- Modify: `src/frontend/pages/workspace_main.rs`（侧栏/浮层详情面板）

**Interfaces:**
- Consumes: `CommentList`（Task 8）
- Produces: 无（纯挂载）

- [ ] **Step 1: 全屏页挂载**

`src/frontend/pages/entry.rs` 里加导入：

```rust
use crate::frontend::comment_list::CommentList;
```

在 `EntryFullScreen` 内、`load` 定义之后加一个派生信号（`data` 里带着 workspace）：

```rust
    // 评论组件要按工作空间查当前用户角色；data 里的 workspace 是唯一来源。
    let ws_id = Signal::derive(move || {
        data.get()
            .and_then(|r| r.ok())
            .map(|(w, _, _, _, _)| w.id)
            .unwrap_or_default()
    });
```

在 `entry-side` 的「标签」分组 `</div>`（`<LabelEditor .../>` 所在那个 div 的闭合）之后、「附件」分组之前插入：

```rust
                    <div>
                        <CommentList
                            code=Signal::derive(code)
                            workspace_id=ws_id
                            on_changed=on_changed
                        />
                    </div>
```

- [ ] **Step 2: 侧栏面板挂载**

`src/frontend/pages/workspace_main.rs` 里加导入：

```rust
use crate::frontend::comment_list::CommentList;
```

`EntryPanel` 的签名加一个 prop（插在 `slug` 之后）：

```rust
fn EntryPanel(
    code: RwSignal<String>,
    slug: String,
    workspace_id: Signal<String>,
    schemas: RwSignal<Vec<LabelSchema>>,
    members: RwSignal<Vec<Member>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
```

`EntryPanel` 里**已有**一个 `on_changed`（当前服务于 `LabelEditor`，第 1905 行附近），内容恰好就是评论需要的：

```rust
    let on_changed = Callback::new(move |_| {
        load(false);
        refresh.update(|n| *n += 1);
    });
```

直接复用它，不要新建第二个回调。在 `<LabelEditor code=code schemas labels members on_changed />`（第 2029 行）之后、`</aside>` 之前插入：

```rust
                        <CommentList
                            code=Signal::derive(move || code.get())
                            workspace_id=workspace_id
                            on_changed=on_changed
                        />
```

（不加额外包裹 `div`：同级兄弟 `div.editor` 与 `LabelEditor` 都是裸挂的，`CommentList` 自带 `.grp-h` 分组标题。）

`on_changed` 里已经带 `refresh.update`：发表评论会推进条目的 `updated_at`，表格的「更新时间」列必须跟着变。

- [ ] **Step 3: 两处调用点补 prop**

`WorkspaceMain` 里定义派生信号（`data` 定义在第 204 行，`ws_members` 在第 208 行；把 `ws_id` 紧跟在 `ws_members` 之后）：

```rust
    // 评论组件查角色用；与 data 同源，避免多打一次 workspace 请求。
    let ws_id = Signal::derive(move || {
        data.get()
            .and_then(|r| r.ok())
            .map(|(w, _, _)| w.id)
            .unwrap_or_default()
    });
```

在第 1138 行（侧栏）与第 1156 行（全屏浮层）两处 `<EntryPanel ... />` 各加一个 prop：

```rust
<EntryPanel code=selected slug=slug().to_string() workspace_id=ws_id schemas members=ws_members refresh />
```

- [ ] **Step 4: 编译门禁**

Run: `make check`
Expected: 两个目标均通过。

- [ ] **Step 5: 浏览器实测基本链路**

Run: `make serve`，登录后打开任意条目详情。
Expected：
1. 侧栏与全屏两处都出现「评论（0）」与输入框；
2. 发表一条带加粗、列表、代码块的评论 → 立即出现在列表且格式正确；
3. 输入框在发表后被清空；
4. 刷新页面后评论仍在；
5. 控制台无 page error。

- [ ] **Step 6: Commit**

```bash
git add src/frontend/pages/entry.rs src/frontend/pages/workspace_main.rs
git commit -m "feat(frontend): 详情页与侧栏面板挂载评论区"
```

---

### Task 10: 表格评论徽标

**Files:**
- Modify: `src/frontend/pages/workspace_main.rs`（`EntryTable` 函数）
- Modify: `src/frontend/components.rs`（`action_label`）

**Interfaces:**
- Consumes: `graphql_client::comment_counts`（Task 7）
- Produces: 无

- [ ] **Step 1: 审计动作的中文标签**

`components.rs::action_label` 里补三个分支（保持与 `AuditAction` 变体名一致）：

```rust
        "CommentCreated" => "发表评论",
        "CommentUpdated" => "编辑评论",
        "CommentDeleted" => "删除评论",
```

- [ ] **Step 2: `EntryTable` 内取当前页计数**

在 `EntryTable` 的 `let visible_codes = move || ...`（第 1628 行）之后、`view!` 之前加：

```rust
    // 当前页的评论计数。只取屏幕上这几行的 code——服务端按 code 逐个前缀扫描，
    // 不需要工作空间级的评论索引列族。
    let counts = RwSignal::new(std::collections::HashMap::<String, i32>::new());
    Effect::new(move |_| {
        let codes = visible_codes();
        if codes.is_empty() || logged_out() {
            return;
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                if let Ok(list) = crate::frontend::graphql_client::comment_counts(&codes).await {
                    counts.set(list.into_iter().collect());
                }
            });
        }
    });
```

`logged_out` 与 `spawn_local` 已在 `workspace_main.rs` 顶部导入，不用再加。

- [ ] **Step 3: 标题单元格渲染徽标**

标题单元格在这个文件的第 1758–1764 行，形如：

```rust
                                    <td style=move || match query_eval::title_color(
                                        &title_colors.get(), &entry_for_color, &entry_for_color.labels,
                                        &schemas_for_color,
                                    ) {
                                        Some(c) => format!("color:{c}"),
                                        None => String::new(),
                                    }>{title_text.clone()}</td>
```

在 `{title_text.clone()}` 之后、`</td>` 之前插入徽标：

```rust
                                    {counts
                                        .get()
                                        .get(&code_for_badge)
                                        .copied()
                                        .filter(|n| *n > 0)
                                        .map(|n| view! {
                                            <span class="c-badge" title="评论条数">
                                                {ic_comment()}{n}
                                            </span>
                                        })}
```

行内克隆处（第 1692–1698 行那一串 `let code_for_xxx = e.code.clone();`）补一个：

```rust
                            let code_for_badge = e.code.clone();
```

并把 `ic_comment` 加进本文件顶部的 `use crate::frontend::icons::{...}` 列表（当前没有它）。

- [ ] **Step 4: 徽标样式**

`style/main.css` 末尾追加：

```css
.c-badge{display:inline-flex;align-items:center;gap:3px;margin-left:6px;font-size:12px;color:var(--kimi-color-text-tertiary,#8f959e)}
```

- [ ] **Step 5: 编译门禁**

Run: `make check`
Expected: 两个目标均通过。

- [ ] **Step 6: 浏览器实测**

在视图表格里对有评论的条目确认徽标数字与评论条数一致；新发一条评论后徽标立即 +1。

- [ ] **Step 7: Commit**

```bash
git add src/frontend/pages/workspace_main.rs src/frontend/components.rs style/main.css
git commit -m "feat(frontend): 视图表格展示评论条数徽标"
```

---

### Task 11: 端到端验收

**Files:** 无（纯验收，出问题回到对应任务修）

- [ ] **Step 1: 逐条跑验收清单**

按 spec §11 的 10 条逐项在浏览器里验证。需要用不同角色的账号（Reader / Worker / Maintainer）各登录一次。逐条记录结果，任一失败即回到对应任务修复后重跑。

- [ ] **Step 2: 重点复验工具栏回归**

在侧栏详情面板里：
1. 打开一个条目（详情编辑器出现）；
2. 在评论框点「编辑」进入编辑态；
3. 确认**详情编辑器的工具栏仍然存在**（这是 `glue.js` 全局清理 bug 的回归点）；
4. 保存或取消编辑，确认评论框恢复可用。

- [ ] **Step 3: 复验检索**

发一条含独有关键词的评论，然后在检索框搜该关键词，确认能命中该条目；再删掉该评论，确认搜不到了。

- [ ] **Step 4: 复验审计**

打开条目的「历史」，确认「发表评论 / 编辑评论 / 删除评论」三条记录都出现，且带正文快照。

- [ ] **Step 5: 编译门禁与既有测试**

Run: `make check && cargo test`
Expected: 全部通过。

- [ ] **Step 6: 记录验收结果**

把验收结果按 `9517cff`（「docs: record AI summary acceptance results」）的做法落成一段记录并提交。

```bash
git add docs/
git commit -m "docs: 记录评论功能验收结果"
```

---

## 验收记录

- 日期：2026-09-18
- 验收环境：隔离实例（`127.0.0.1:3099` + 临时 `data_dir`），未触碰开发库；
  浏览器由 playwright-core 无头驱动，真实登录表单登录。
- 数据：新工作空间 `comments-verify`，两个条目，外加 Reader / Worker / Maintainer
  三个角色账号（注册 → 邀请 → 接受）。

**spec §11 的 10 条逐项结果（32/32 断言通过，无 page error）**

| # | 验收项 | 结果 | 证据 |
|---|--------|------|------|
| 1 | 发表后立即出现，加粗/列表/代码块渲染正确 | 通过 | 新评论 HTML 含 `<strong>` / `<ul><li>` / `<pre>`；发表后输入框清空 |
| 2 | 编辑自己的评论，正文更新、时间不动 | 通过 | 正文追加「【已编辑】」生效；创建时间不变；出现「已编辑」标记 |
| 3 | 删除自己的评论，列表移除 | 通过 | 「评论（1）」→「评论（0）」 |
| 4 | Reader 看不到输入框与操作按钮 | 通过 | `.c-composer` 0 个、按钮 0 个，但评论内容仍可读（1 条） |
| 5 | Worker 看不到他人评论的编辑按钮；Maintainer 能看到删除按钮 | 通过 | Worker 对他人评论编辑/删除按钮均为 0，对自己评论编辑按钮为 1；Maintainer 对他人评论删除按钮为 1、编辑按钮为 0 |
| 6 | 检索能通过评论正文命中；删除后不再命中 | 通过 | 发表后命中该条目；删除后命中数 0 |
| 7 | 历史出现评论记录 | 通过 | 发表评论 / 编辑评论 / 删除评论三条均出现，且带正文快照 |
| 8 | 更新时间因发表评论前移、评论徽标正确 | 通过 | `2026-09-18 01:19:37 → 01:19:54`；徽标数字 1，发表后立即 +1 |
| 9 | 打开评论编辑框后侧栏详情编辑器工具栏仍在 | 通过 | 详情工具栏 1 个、评论编辑工具栏 1 个（页面共 2 个），取消后输入框恢复 |
| 10 | 控制台无 page error | 通过 | 四个角色阶段 pageerror / console.error 均为 0 |

补充：检索入口是视图顶部的「搜索本视图」输入框（`.vhead label.inp input`），
不是标签表达式框——表达式框把裸词当作标签名，会报「标签不存在」。

**验收中发现的缺陷（已修复，见 `7a13c58`）**

- 现象：连续发表几条评论后，评论的「保存」按钮点不动。
- 定位：Quill 把工具栏插成编辑容器的**兄弟节点**，Leptos 卸载容器时带不走它。
  发表评论会把输入框整个卸载再挂回（`composer_ready` 那段），每次留下一份孤儿工具栏；
  几轮之后残留工具栏层层堆叠，盖住下方的「保存」按钮，Playwright 报
  `… from <div class="mut">…</div> subtree intercepts pointer events`。
- 实测：修复前每次发表 +3 个 `.ql-toolbar`（1 次发表后 2 → 5，2 次后 → 8）；修复后稳定为 2 个（详情 1 + 评论 1）。
- 修法：`glue.js` 用「附属节点 → 宿主」表登记并在创建时清掉宿主已离开文档的残留；
  创建前后各扫一次，覆盖同一次响应式更新里卸载与新建的任意先后顺序。
- 说明：这是既有设计的既有缺口（`tiny_editor.rs` 注释里提过工具栏堆叠），
  评论功能把「每次发表都重挂载编辑器」变成高频路径后才暴露成可复现故障。

**编译门与既有测试**

- `make check`：native 与 wasm32 两个目标均通过。
- `cargo test`：134 passed，与改动前基线一致。
- 未新增后端单测（按 §11 与用户要求）。
