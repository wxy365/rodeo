# Entry 附件与图片粘贴 设计文档

> 日期：2026-09-18
> 依据：用户 2026-09-18 缺陷 1「编辑器中无法粘贴图片」；方向已确认为「接入附件体系，而非 base64 内联」
> 状态：设计已与用户确认
> 关联：`2026-09-17-entry-comments-design.md`（`reindex`、审计、富文本约定）、`spec/technical_solution.md` §4/§5/§9/§11/§12

## 1. 背景与目标

`public/tiny-editor/glue.js` 的 `blockImages()` 在捕获阶段拦掉了编辑器里粘贴/拖入的图片文件，且不给任何提示——用户 2026-09-18 报的缺陷 1。拦截的动机是成立的（Quill 会把图片转成 base64 embed，单条内容能到数 MB，且只读渲染会把 `data:` URI 原样吐出），但结论不对：图片应当走附件体系。

本次实现 Entry 附件：图片粘贴后上传、以 URL 插入正文；entry 页的「附件（≤ 50MB）即将上线」占位落地为真列表（含删除与手动上传）。

## 2. 范围与非目标

**范围**

- `Attachment` 领域实体与 `cf::ATTACHMENTS` / `cf::ATTACHMENTS_BY_ENTRY` 两个列族。
- `attachment_by_entry_key` 复合键。
- `AttachmentService`：元数据进 RocksDB、文件落 `{data_dir}/attachments`，含审计、`Entry.updated_at` 推进、检索重索引。
- GraphQL：`uploadAttachment` / `deleteAttachment` / `attachments`，含权限校验；`Upload` scalar 走 multipart。
- 下载路由 `GET /api/attachments/{id}`。
- `glue.js` 去掉 `blockImages()`，改为「上传 → 以 URL 插入」。
- entry 页附件区（列表 / 删除 / 手动上传）。

**非目标（本轮不做）**

- `FileStorage` trait 与 S3 实现。只有一个实现、一个调用方，抽 trait 是纯开销；等真有第二个后端再抽。
- `[attachment] max_size_mb` 配置项。50MB 写成常量，与 UI 文案一致。
- entry 删除时清理附件文件。会留孤儿文件，属既有缺口。
- 详情面板 `workspace_main.rs` 那个 disabled 的「附件」tab。它需要先给详情面板实现 tab 切换，是另一件事。
- 图片工具栏按钮。粘贴与拖入是唯一入口，工具栏保持不变。
- 收紧富文本里指向外部域名的 `<img src>`。该路径（Quill 的 HTML 剪贴板）在本轮之前就是通的，非本次引入，用户明确表示不动。
- 后端 Rust 单测（用户硬约束）。
- 附件分页、附件纳入事件自动化事件源。

## 3. 数据模型（`src/domain/attachment.rs`）

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub id: Ulid,
    pub entry_code: String,
    /// 冗余存一份：权限校验与「这个附件属于哪个空间」不必每次回查 Entry。
    /// 不参与键布局——Entry Code 全局唯一，`entry_code` 单独作前缀已足够。
    pub workspace_id: Ulid,
    /// 用户上传时的原始文件名，仅用于展示与下载响应头。磁盘文件名是净化后的版本。
    pub filename: String,
    /// 客户端上报的 MIME，只当提示看：下载响应绝不回声此值（见 §7）。
    pub content_type: String,
    pub size: u64,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
}
```

**存储路径不进实体**，由 `workspace_id` / `entry_code` / `id` / `filename` 现算。这样文件名与路径不会各自漂移，也不需要在删除时反查路径字段。

`Attachment::new(entry_code, workspace_id, filename, content_type, size, actor)` 生成 `Ulid::new()` 与 `Utc::now()`。

## 4. 列族与键（`src/storage/rocksdb.rs`、`src/storage/keys.rs`）

```rust
/// 附件主键：16 字节 ulid。下载路由手里只有 id，必须能单键直取。
pub const ATTACHMENTS: &str = "attachments";
/// 附件索引：(entry_code, attachment_id) → 空值，供按条目前缀扫描。
pub const ATTACHMENTS_BY_ENTRY: &str = "attachments_by_entry";
```

`ALL_CFS` 一并加入。`DocStore::open` 已开 `create_missing_column_families(true)`，存量库启动时自动补建，无需迁移代码。

`keys.rs` 新增：

```rust
/// (entry_code, attachment_id) 复合键。`attachment_id` 是 ULID，字节序即时间序，
/// 因此按 entry_code 前缀扫描天然得到按上传时间升序的列表——与 `comment_key` 同构。
pub fn attachment_by_entry_key(entry_code: &str, attachment_id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(entry_code.len() + 16);
    key.extend_from_slice(entry_code.as_bytes());
    key.extend_from_slice(&attachment_id.to_bytes());
    key
}
```

## 5. 服务（`src/service/attachment.rs`）

照 `CommentService` 的形状：`store: Arc<DocStore>` + `entries: EntryService`（单向依赖，`EntryService` 不感知附件）。

```rust
/// 单文件上限。与 UI 文案「附件（≤ 50MB）」一致。
const MAX_ATTACHMENT_SIZE: u64 = 50 * 1024 * 1024;
```

### 5.1 磁盘布局

基目录 `{data_dir}/attachments`，相对文件路径：

```
{workspace_id}/{entry_code}/{attachment_id}_{safe_name}
```

路径前两段无需净化：`workspace_id` 是 ULID，`entry_code` 是 16 位 base62 字母数字（`domain::entry::generate_entry_code`，已核对）。**唯一由客户端控制的路径段是 `filename`**，净化对象只有它。

`safe_name` 的规则：逐字符保留字母/数字/`-`/`_`/`.`/空格与 UTF-8 多字节字符，替换 `/`、`\`、连续的 `..` 与控制字符；结果为空则用 `file`。目的是不让客户端文件名参与路径解析（目录穿越、绝对路径注入）。

磁盘操作全部走 `tokio::fs`（需要给 `tokio` 加 `fs` + `io-util` 特性）。

### 5.2 `save`

```rust
pub async fn save(
    &self,
    actor: Ulid,
    entry_code: &str,
    filename: &str,
    content_type: &str,
    content: std::fs::File,   // async-graphql 的 UploadValue.content
    size: u64,
) -> Result<Attachment, AppError>
```

1. `size > MAX_ATTACHMENT_SIZE` → `AppError::InvalidQuery("附件超过 50MB")`，不落盘。
2. `self.entries.get(entry_code)?` 存在且 `!is_deleted()`，否则 `NotFound`。
3. 构造 `Attachment`，算目标路径，`create_dir_all` 父目录，`tokio::fs::File::from_std` + `tokio::io::copy` 把临时文件流式写入（不整份读进内存）。
4. **推进Entry 活动**：`entry.updated_by = actor`，`entry.updated_at = Utc::now()`。理由同评论——上传附件是发生在条目上的动作，应让条目回到「按更新时间倒序」的最前。副作用是正在编辑详情的人保存时会撞乐观并发冲突，与评论的既有取舍一致。
5. 一个 `WriteBatch`：`ATTACHMENTS` 的主键、`ATTACHMENTS_BY_ENTRY` 的索引键、`ENTRIES`（更新后的 entry）、`audit_ops(&AuditLog::new(AttachmentUploaded, …))`。
6. **批写失败则尽力删除刚落的文件**（`let _ = remove_file`），不让失败的上传留下孤儿文件；成功后 `entries.reindex_by_code(entry_code)`。

### 5.3 `get` / `list` / `delete`

- `get(id: Ulid) -> Option<Attachment>`：主键直取，下载路由用。
- `list(entry_code) -> Vec<Attachment>`：前缀扫 `ATTACHMENTS_BY_ENTRY` 拿 id，再逐个 `get`。索引命中但主键缺失时跳过（不 panic）。
- `delete(actor, id, can_moderate) -> Result<()>`：
  - 权限：`attachment.created_by == actor || can_moderate`，否则 `Forbidden`。与 `CommentService::delete` 同构——删除他人附件要 Maintainer+，作者本人即使被降级也仍可撤回自己的上传。
  - 一个 `WriteBatch` 删两个 CF + 审计 `AttachmentDeleted`。
  - **批写成功之后**再删磁盘文件，文件删失败只记 warning 不回滚元数据：元数据是真相来源，宁可留孤儿文件，也不要「DB 说删了但文件还在被引用」之外的第二种不一致。
  - **不推进 `Entry.updated_at`**：与 `CommentService::delete` 一致，只有「新增」算条目活动。

### 5.4 审计

`resource_type = "attachment"`，**`resource_id = entry_code`**。这不是笔误：entry 页的「审计历史」按 `resource_id == 条目 code` 过滤（`entry.rs`），用 attachment id 的话上传记录不会出现在任何地方。附件自身的 id 与文件名在 `after`/`before` 快照里（`serde_json` 序列化的 `Attachment`），照评论的写法。

### 5.5 权限汇总

| 操作 | 要求 |
|---|---|
| 上传 | `WorkspaceRole::Worker`+ |
| 列表 / 下载 | 成员可读（下载路由见 §7，免鉴权） |
| 删除 | 作者本人，或 `WorkspaceRole::Maintainer`+ |

## 6. GraphQL（`src/api/graphql.rs`）

```graphql
type GqlAttachment {
  id: ID!
  entryCode: String!
  filename: String!
  contentType: String!
  size: Int!
  url: String!              # 服务端拼 /api/attachments/{id}
  createdAt: String!
  createdByAccount: GqlAccount
}

uploadAttachment(entryCode: String!, file: Upload!): GqlAttachment!   # Worker+
deleteAttachment(id: ID!): Boolean!                                   # 作者本人或 Maintainer+
attachments(entryCode: String!): [GqlAttachment!]!                    # 成员可读
```

- `url` 由服务端拼，客户端不自己拼路径——下载路径只有一处定义。
- `size` 在结构体里是 `i32`（GraphQL `Int` 是 32 位；50MB 上限远在范围内），Rust 侧的 `u64` 在组装 `GqlAttachment` 时收窄。
- `uploadAttachment` 的权限判定要**先取 entry 拿 workspace_id**（对齐 `create_comment`），不能只靠 `Upload` 参数。
- `deleteAttachment` 只拿到 id，需先从 attachment 取 `workspace_id` 再校验；`can_moderate` 由 `require_role(Maintainer).is_ok()` 算好传入，照 `delete_comment`。
- **偏离 `spec/technical_solution.md` §5 的 `Entry.attachments` 字段**，改用独立 `attachments(entryCode:)` 查询：上传/删除后只需刷新附件列表，不必重取整个 entry（重取会连带 `detail` 覆盖编辑器里未保存的内容）。与既有 `comments(entryCode:)` 查询形状一致。

### 6.1 `Upload` scalar 的可用性（已核对 7.2.1 源码）

- `async_graphql::Upload` **不在 feature gate 之后**，只有 `unblock` / `tempfile` 变体在。当前 `async-graphql = { version = "7" }` 用默认特性（含 `tempfile`），因此 `Upload` 直接可用。
- `async_graphql_axum::GraphQLRequest` 的 `FromRequest` 实现已经按 Content-Type 分流：multipart 走 `receive_batch_body` → `receive_batch_multipart`，字段名 `operations` / `map` + 文件部分，即 graphql-multipart-request-spec。
- 结论：**`Cargo.toml` 里 async-graphql / async-graphql-axum 的特性一个字都不用改。**

### 6.2 落盘方式

`tempfile` 开启时 `UploadValue.content` 是 `std::fs::File`，`UploadValue::size()` 取 metadata 长度。resolver 里先用 `size()` 判上限，再把 `File` 交给 `AttachmentService::save`（内部 `tokio::fs::File::from_std` + `tokio::io::copy`）。避免把 50MB 读进 `Vec<u8>`。

### 6.3 请求体上限

**axum 默认请求体上限 2MB**。必须给 `/api/graphql` 单独挂 `DefaultBodyLimit::max(52 * 1024 * 1024)`，否则 50MB 上传在进入 handler 之前就被 413 拒绝，且错误形式是 HTTP 413 而非 GraphQL 错误。只挂这一条路由，不动全局（leptos 的 server function 路由不该跟着放到 52MB）。

## 7. 下载路由（`src/main.rs`）

```rust
.route("/api/attachments/{id}", get(download_attachment))
```

- **免鉴权**（用户确认）：`<img src>` 是浏览器发起的裸请求，带不了 `Authorization: Bearer`；而代码库里**没有任何地方写过 cookie**（无 `Set-Cookie`，前端也不写 `document.cookie`），所以 cookie 路径实际不存在。id 是不透明 ULID，语义等同「能力 URL」（类似分享链接）。私有部署 + 内网，接受该模型；代价是拿到 URL 的人永不过期可读，这一点已向用户说明。
- 元数据查不到，或元数据在但文件不在 → 404。
- **响应类型按白名单决定，绝不回声客户端上报的 `content_type`**：上报值来自上传方，回声它等于允许上传 `text/html` 后在应用同源下执行脚本（存储型 XSS）。
  - `image/png`、`image/jpeg`、`image/gif`、`image/webp`、`image/bmp`（按上报值归一化后比对）→ 原样 inline。
  - 其余一律 `application/octet-stream` + `Content-Disposition: attachment; filename*=UTF-8''<百分号编码的原始文件名>`。**SVG 明确不在 inline 白名单内**（可携带脚本）。
- 一律附加 `X-Content-Type-Options: nosniff` 与 `Cache-Control: public, max-age=31536000, immutable`（ULID 决定内容不变，可长期缓存）。
- 用 `axum::body::Body::from(Vec<u8>)` 构造响应，不引入 `tower-http` 的额外特性。

## 8. 前端

### 8.1 上传客户端（`src/frontend/graphql_client.rs`）

```rust
pub async fn upload_attachment(entry_code: &str, file: web_sys::File) -> Result<Attachment, String>
```

- `FormData`：`operations` = `{"query": …, "variables": {"entryCode": …, "file": null}}`，`map` = `{"0": ["variables.file"]}`，文件字段名 `"0"`。三者的键必须与 `map` 里的路径自洽。
- **不手写 `Content-Type`**：浏览器要自己补 multipart 的 boundary，手写会让服务端解析失败。
- `Authorization: Bearer` 头照旧从 `get_token()` 取（与 `graphql()` 同一处逻辑，不复制 token 读取路径）。
- 需要给 wasm 目标依赖加：`wasm-bindgen-futures`；给 `web-sys` 加 `FormData`、`File`、`Blob` 特性。

### 8.2 编辑器桥（`src/frontend/tiny_editor.rs` + `public/tiny-editor/glue.js`）

`TinyEditor` 新增 prop `entry_code: Signal<String>`，四个调用点都需要传：

| 调用点 | 取值来源 |
|---|---|
| `pages/workspace_main.rs` 详情面板编辑器 | 当前选中条目的 code |
| `pages/entry.rs` 整页编辑器 | 路由参数 code |
| `comment_list.rs` 评论 composer | 外层传入的 code |
| `comment_list.rs` 就地编辑框 | 同上 |

`mount_editor` 额外给 glue.js 传一个 `upload(file) -> Promise<url>`：用 `wasm_bindgen_futures::future_to_promise` 包住 `upload_attachment`，闭包引用 `entry_code` 这个 `Signal`，在**调用时**读当前值（`get_untracked()`），而不是挂载时快照——这样详情面板切换选中条目后即使编辑器节点被复用，上传也落在当前条目上。闭包照 `__rodeo_onchange` 的写法挂在节点上防 GC。

`glue.js`：删除 `blockImages()`，新增 `attachImageUpload(el, editor, opts)`：

- 仍在**捕获阶段**监听 `paste` / `drop`：要抢在 Quill 的 clipboard 模块（冒泡阶段）之前。
- 只处理 `kind === 'file'` 且 `type` 以 `image/` 开头的项；命中则 `preventDefault()` + `stopPropagation()`，接手处理。
- 处理流程：**先**捕获当前选区索引（异步期间光标会漂，必须在 `await` 之前取）→ `insertText(index, '上传中…')` 占位 → `await opts.upload(file)` → 成功则删占位、`insertEmbed(index, 'image', url, 'user')` 并把光标移到图片之后；失败则把占位替换为「图片上传失败」并 `console.warn`。
- 一次粘贴多个文件时逐个串行处理。
- 工具栏不加图片按钮。`modules.toolbar` 是按钮数组、不构成格式白名单，`image` blot 本来就可用，无需改配置。
- 富文本里指向外部域名的 `<img src>` **不收紧**（用户决定）：该路径本就走 Quill 的 HTML 剪贴板，在本轮之前就是通的。

### 8.3 附件区（新增 `src/frontend/attachment_list.rs`）

替换 `pages/entry.rs` 占位（`附件（≤ 50MB）` + `暂无附件`）。结构照 `comment_list.rs`：`code` / `workspace_id` / `on_changed` 三个 prop，内部 `items` 信号 + `load()` + disposal 守卫。

- 列表行：文件名（指向 `url` 的下载链接）、人类可读大小、上传时间（`short_time`）、上传人（`Avatar` + 名字）。上传人与当前用户一致、或角色达到 `maintainer` 时显示删除。
- 删除走二次确认，照评论的 `confirm_del` 写法。
- 手动上传：`<input type="file">`，选择后立刻上传（不限制文件类型，图片与非图片都允许）。上传中禁用输入并显示「上传中…」；失败把 `AppError` 文案显示在 `.hint` 里。
- 上传/删除成功后重新 `load()` 并触发 `on_changed`（外层据此刷新条目更新时间）。
- 空态：「暂无附件」。

## 9. 待确认项

无。设计中的两个开放问题已由用户拍板：

- 上传推进 `Entry.updated_at` —— 要。
- 收紧富文本里的外部 `<img src>` —— 不要。

## 10. 验证

门禁：`cargo check`（0 错误） + `cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown`（0 错误）。不新增后端 Rust 单测。

真机验证走 3099 隔离实例（不动用户 :3000 的 watcher），用 playwright-core：

1. 在详情编辑器里粘贴一张真 PNG → 编辑器内出现 `<img src="/api/attachments/…">`；`toHtml` 输出含该 URL 且不含 `data:`。
2. 直连 `/api/attachments/{id}` → 200，`content-type: image/png`，带 `nosniff`。
3. 手动上传一个 `.html`，再直连其 URL → `application/octet-stream` + `Content-Disposition: attachment`，内容不被当作 HTML 渲染。
4. 上传超过 50MB 的文件 → 得到 GraphQL 错误而非 HTTP 413（反过来即说明 `DefaultBodyLimit` 没挂对）。
5. 手动上传 / 删除走通；删除后文件从磁盘消失、列表不再出现。
6. entry 页「审计历史」出现「上传附件」「删除附件」；条目「更新时间」在上传后前进。
7. 用一个旧的 `./data` 副本启动 → 两个新列族自动补建，无迁移报错。
