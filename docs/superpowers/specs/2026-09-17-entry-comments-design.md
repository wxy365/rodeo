# Entry 评论 设计文档

> 日期：2026-09-17
> 依据：用户 2026-09-17 需求「支持评论功能」
> 状态：设计已与用户确认
> 关联：`2026-09-14-ai-entry-summary-design.md`（`reindex` 与审计约定）、`2026-09-16-event-automation-design.md`（同期在研）

## 1. 背景与目标

Rodeo 的 Entry 目前只能通过标题、详情和标签表达信息，多人协作时缺少「按时间追加讨论、保留发言人和时间」的位置。本次为 Entry 增加评论：在详情面板里按时间正序列出，可发表、编辑、删除，正文为富文本，并接入既有的审计、全文检索与条目更新时间。

## 2. 范围与非目标

**范围**

- `Comment` 领域实体与 `cf::COMMENTS` 列族。
- 评论的增 / 改 / 删服务，含审计、`Entry.updated_at` 推进、检索重索引。
- GraphQL 查询与变更，含权限校验。
- 富文本正文的只读渲染（`glue.js` 新增 `toHtml`）。
- 全屏详情页与侧栏详情面板的评论区（含新增框、就地编辑、删除）。
- 详情面板的评论条数、视图表格的评论条数徽标。

**非目标（本轮不做）**

- 实时推送。本仓库没有 WebSocket / SSE 基础设施（已确认 `src/` 内无相关实现），评论不做广播。
- @提及、通知、嵌套回复、表情回应。
- 评论附件与图片。TinyEditor 工具栏不含图片按钮；粘贴图片会被拦截（见 §7）。
- 评论分页。先全量返回（见 §11 待确认）。
- 评论纳入事件自动化的事件源。本轮标签事件与评论互不影响。

## 3. 数据模型（`src/domain/comment.rs`）

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub id: Ulid,
    pub entry_code: String,
    pub workspace_id: Ulid,
    /// Quill Delta JSON（与 `Entry.detail` 同构，复用同一套编辑器与归一化逻辑）。
    pub body: String,
    pub created_by: Ulid,
    pub updated_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

`workspace_id` 冗余存一份，让权限与列表校验不必每次回查 `Entry`。

`body` 一律为 Delta JSON。前端 `TinyEditor` 的 `normalize_delta` 已能处理历史纯文本，因此即使出现非 Delta 正文也不会崩。

## 4. 存储

**只新增一个列族** `cf::COMMENTS`：

- 键 = `entry_code(16) + comment_id(16)` = 32 字节
- 值 = `bincode(Comment)`

前缀扫描 `entry_code` 即得到该条目的全部评论；`comment_id` 为 ULID，字节序即时间序，因此扫描结果天然按发表时间升序，无需额外排序字段。

键里不含 `workspace_id`：Entry Code 是 16 位全局唯一编码（`entry::generate_entry_code`），跨工作空间不会撞号，因此 `entry_code` 单独作前缀已足够。`Comment` 结构体里冗余的 `workspace_id` 只服务于权限校验，不参与键布局。

键构造函数加入 `src/storage/keys.rs`，列族名加入 `src/storage/rocksdb.rs` 的 `ALL_CFS`（漏注册会导致库打不开）。

**不加 `COMMENTS_BY_WORKSPACE`**。评论条数只在两类场景需要：详情面板（用已加载列表的长度即可）与视图表格当前页（至多 100 行）。后者按 code 逐个前缀扫描即可，无需工作空间级索引——多一个列族就多一条写入路径与索引漂移风险，`LABELINGS_BY_WORKSPACE` 当年正是为此补写过 `backfill`。

**删除为硬删除**（物理 `BatchOp::delete`），对齐 `remove_labeling` 的既有做法：评论没有恢复需求，软删除只会在列表里留下空洞。删除前的内容留在审计的 `before` 快照里。

## 5. 服务层（`src/service/comment.rs`）

```rust
pub struct CommentService {
    store: Arc<DocStore>,
    entries: EntryService,
}
```

单向依赖 `EntryService`（后者不感知评论），因此不存在循环依赖。

| 方法 | 行为 |
|------|------|
| `list(entry_code) -> Result<Vec<Comment>>` | 前缀扫描，按 ULID 升序（正序展示，像对话） |
| `create(actor, entry_code, body) -> Result<Comment>` | 见下 |
| `update(actor, id, body) -> Result<Comment>` | 仅作者本人；不推进 `Entry.updated_at` |
| `delete(actor, id) -> Result<()>` | 作者本人，或 Maintainer+；不推进 `Entry.updated_at` |

`create` 在**一次 `write_batch` 内原子完成**四件事：

1. 写入 `cf::COMMENTS`；
2. 读回 `Entry`、推进 `updated_at` 与 `updated_by`、写回 `cf::ENTRIES`；
3. 写一条 `CommentCreated` 审计；
4. 之后调 `EntryService::reindex`（索引写入不能进 batch，与现有各写路径一致）。

`update` / `delete` 同样只走「读评论 → 校验作者 → 一次 write_batch 写评论与审计 → reindex」。

**只有新增评论推进 `Entry.updated_at`**。编辑是纠错、删除是撤回，都不该把条目顶到默认视图（按更新时间倒序）的最前面。

**副作用（已知取舍）**：`Entry.update` 走 `expected_updated_at` 乐观并发。有人正在编辑详情时若他人发表评论，`updated_at` 被推进，编辑者保存会拿到 `ConflictDetected`（「内容已被他人修改」），尽管标题与详情都没变。接受该行为——同一秒内「有人编辑详情 + 有人评论」属低频，且前端已有冲突后重载的处理路径。

## 6. 审计

沿用 `LabelingSet` 的既有约定：`resource_type = "comment"`、`resource_id = <entry_code>`（评论 id 放在 before/after 快照里）。这样详情页按 `resource_id == code` 过滤的「历史」能直接显示评论记录。

`AuditAction` 追加三个变体：`CommentCreated`、`CommentUpdated`、`CommentDeleted`。**必须追加在枚举末尾**——bincode 按变体序号编码，插在中间会让存量审计日志错位（`src/domain/audit.rs` 已有注释警告）。

只写评论自身的审计，**不额外写 `EntryUpdated`**：一次发言产生两条记录会让历史噪音过大；`updated_at` 的推进已由评论记录的时间体现。

`src/frontend/components.rs` 的 `action_label` 配三个中文标签。

## 7. 富文本的只读渲染

评论列表需要把 Delta 渲染成 HTML，而现有代码只有编辑器、没有只读渲染器。方案：在 `public/tiny-editor/glue.js` 新增 `toHtml(deltaJson)`。

- 内部维护**一个复用的离屏 Quill 实例**（`readOnly: true`、`toolbar: false`），每次 `setContents(delta)` 后取 `getSemanticHTML()`，返回 HTML 字符串。
- Rust 侧取回后写进 `inner_html`。整个列表只占一个 Quill 实例，与评论条数无关。
- 复用单实例也天然避开了「每条评论挂一个编辑器」会引发的工具栏冲突与性能问题。

**必须同轮修掉的 bug**：`glue.js` 的 `create()` 里 `document.querySelectorAll('.ql-toolbar, .ql-tooltip').forEach(n => n.remove())` 是全局清理，注释写着「本应用同一时刻只有一个编辑器」。侧栏详情面板已经有一个详情编辑器，评论编辑框一出现就会把详情编辑器的工具栏抹掉。改为按容器作用域清理：记录本容器上次创建的工具栏节点并只移除它，同时保持「同一时刻只有一个带工具栏的编辑器」（编辑某条评论时不允许多条同时进入编辑态）。

**两项待实测风险**

1. `getSemanticHTML()` 对 `link` 的 href 是否做协议白名单。若 `javascript:` 链接被原样输出，需要在 Rust 侧对渲染结果做一次协议过滤后再写入 `inner_html`。
2. Quill 默认把粘贴/拖入的图片转成 base64 embed，单条评论可达数 MB，且 `getSemanticHTML` 会输出 `data:` 图片。需在编辑器模块配置里拦掉图片粘贴，或在粘贴时提示不支持。

## 8. GraphQL（`src/api/graphql.rs`）

```graphql
comments(entryCode: String!): [GqlComment!]!                            # Reader+
commentCounts(entryCodes: [String!]!): [GqlCommentCount!]!              # Reader+
createComment(entryCode: String!, body: String!): GqlComment!           # Worker+
updateComment(entryCode: String!, id: ID!, body: String!): GqlComment!  # Worker+ 且作者本人
deleteComment(entryCode: String!, id: ID!): Boolean!                    # 作者本人 或 Maintainer+
```

`updateComment` / `deleteComment` 必须同时收 `entryCode`：`COMMENTS` 的键是 `entry_code + comment_id`，单凭 `id` 定位不到记录（要另建反向索引列族才能做到，为这点收益不值得）。前端调用的地方本来就知道当前条目的 code。

- `GqlComment { id entryCode body createdAt updatedAt createdBy updatedBy createdByAccount updatedByAccount }`，账号信息沿用 `GqlEntry` 的回填方式（`AccountBrief`），前端才好显示头像与姓名。
- `GqlCommentCount { entryCode count }`。
- `commentCounts` 对每个 code 做一次只计数的前缀扫描，不反序列化正文。
- **评论不加入 `ENTRY_FIELDS`**。该常量被 `entries` / `entry` / `query_entries` / `archivedEntries` 共用，加入评论会让每次列表请求都带上全部评论正文。

## 9. 前端

### 9.1 GraphQL 客户端（`src/frontend/graphql_client.rs`）

新增 `Comment`、`CommentCount` 结构体与 `comments()` / `comment_counts()` / `create_comment()` / `update_comment()` / `delete_comment()` 五个函数。

### 9.2 `components.rs::CommentList`

新增组件，两处详情复用。包含：

- 评论列表：头像、姓名、时间、正文（`inner_html` 渲染）、作者本人可见的「编辑 / 删除」、Maintainer+ 可见的删除。
- 新增框：一个 `TinyEditor` + 发表按钮。
- 就地编辑：点击「编辑」把该条正文换成 `TinyEditor`，带保存 / 取消。
- 同一时刻只允许一个带工具栏的编辑器存在（新增框或某一条的编辑态，二者互斥）。
- Reader 不渲染输入框与操作按钮。

### 9.3 挂载点

- 全屏详情页 `src/frontend/pages/entry.rs`：`entry-side` 内「标签」之后、「附件」之前，新增一组「评论（N）」。
- 侧栏详情面板 `src/frontend/pages/workspace_main.rs::EntryPanel`：`<LabelEditor/>` 之后新增同一组。
- 评论发表后需触发既有的刷新路径（全屏页 `on_changed`、侧栏 `refresh.update`），否则表格的「更新时间」列会停在旧值。

### 9.4 评论条数

- 详情面板：分组标题显示 `评论（N）`，`N` 直接取已加载列表长度，零额外请求。
- 视图表格：标题单元格内加一个「气泡图标 + 数字」的小徽标，数据来自 `commentCounts`，只为当前页的 code 取一次。不改动列配置（`ColumnPicker`）体系。

### 9.5 样式

`style/main.css` 增加评论区样式，沿用现有 `.grp-h` / `.av` / `.mut` / `.tl` 等既有类。

## 10. 权限与错误处理

| 操作 | 要求 |
|------|------|
| 读取评论 | `Reader` 及以上 |
| 发表评论 | `Worker` 及以上 |
| 编辑评论 | `Worker` 及以上 **且** 作者本人 |
| 删除评论 | `Maintainer` 及以上 **或** 作者本人 |

删除的判定顺序：先看是否 `Maintainer+`，否则再看是否作者本人，两者都不满足才返回 `Forbidden`。这样即使作者后来被降级为 `Reader`，仍能撤回自己写过的内容。

- 越权：返回既有的 `AppError::Forbidden`（`code = "FORBIDDEN"`）。
- 正文为空（去除空白后为空，含 Delta 只有空白片段的情况）：`AppError::InvalidQuery("评论内容不能为空")`。此处与 `create_with_detail` 对空标题用 `Internal` 的做法不同——正文直接来自用户输入，`InvalidQuery` 的语义更准确。
- 条目不存在或已删除：`AppError::NotFound`。
- 评论 id 不存在：`AppError::NotFound`。
- 不新增 `AppError` 变体。

## 11. 测试策略

- **后端不写新单测**（用户明确）。以 `cargo build` + wasm `check` 为编译门。
- `SearchIndex::index_entry` 会增加参数（见 §12），现有测试的调用点需跟着改签名——这是修改既有调用点，不是新增用例。
- 浏览器实测（playwright-core，见既有验证 recipe），覆盖：
  1. 发表评论后立即出现在列表，格式（加粗 / 列表 / 代码块）正确渲染；
  2. 编辑自己的评论，正文更新，时间不动；
  3. 删除自己的评论，列表移除；
  4. Reader 账号看不到输入框与操作按钮；
  5. Worker 账号看不到他人评论的编辑按钮，Maintainer 能看到删除按钮；
  6. 全文检索能通过评论正文命中该条目；
  7. 详情的「历史」出现评论记录；
  8. 视图表格的「更新时间」因发表评论而前移，评论徽标数字正确；
  9. **打开评论编辑框后，侧栏详情编辑器的工具栏仍然存在**（§7 那个 bug 的回归验证）；
  10. 控制台无 page error。

## 12. 全文检索

检索 schema 新增 `comments` 字段，复用现成的「schema 缺字段 → 删索引重建 → `backfill` 回灌」升级路径（`SearchIndex::open` 里已有针对 `CODE_TEXT_FIELD` 的同款处理）。索引是纯派生物，重建安全。

- `SearchIndex::index_entry` 增加评论正文参数，`add_entry_doc` 写入 `f_comments`。
- `backfill` 一并回灌评论正文。
- `EntryService::reindex` 内部多读一次 `cf::COMMENTS` 前缀扫描，抽纯文本走既有的 `search::strip_rich_text`。

## 13. 破坏性变更

无。新列族、新增 GraphQL 字段与变更，均为增量。检索索引会因新字段走一次重建 + 回填，属既有升级路径。

## 14. 待确认 / 后续留白

- **评论分页**：本轮全量返回。单条目评论数很大时再单独一轮引入分页。
- **`getSemanticHTML` 的链接协议白名单**：若实测不安全，需在渲染前做协议过滤（§7）。
- **粘贴图片**：本轮拦截。若后续需要图片，应接入附件体系而非 base64 内联。
- **实时推送**：待基础设施具备后再谈。
