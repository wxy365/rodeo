# 视图筛选与标签查询 设计文档

> 日期：2026-09-11
> 依据：`spec/20260910-补充设计.md`（标签查询语法、视图=查询条件+列）、`spec/原型.html`（视图主页）、`spec/technical_solution.md` §5/§7
> 状态：设计已评审通过，待转实现计划

## 1. 背景与目标

工作空间主界面当前的「筛选 / 全文检索」「视图配置」「新建视图」均为占位。本轮把**视图**从占位做成可用闭环：

1. 用户可定义**查询条件**（标签、时间、全文）与**表格列**，保存为命名视图（个人 / 共享）。
2. 查询条件既能用**结构化芯片**编辑，也能用**表达式文本**编辑，两者共享同一份 AST。
3. 全文检索接入 Tantivy，中文可检索。
4. 表格支持**排序**与**分页**。

## 2. 范围与非目标

**本轮范围**

- View 实体与 CRUD（含审计、权限）。
- 查询 AST、表达式语法（解析 + 反解析）、内存求值。
- 全文检索（Tantivy）+ 索引写入同步 + 启动回填。
- 表格动态列配置、排序、分页。
- GraphQL 接口与前端 UI。

**非目标（本轮明确不做，留后续切片）**

- 标签二级索引 `labelings_by_label`、索引定时 reconcile。
- 「指派人」筛选维度（需先给 Entry 增加 assignee 字段）。
- 中文分词升级为 jieba（`cang-jie`），本轮用 Ngram。
- 视图共享给指定账号（本轮 is_shared 即对整个 workspace 可见）。
- 实时协作、附件、OAuth。

## 3. 领域模型

### 3.1 View（`src/domain/view.rs`）

```rust
pub struct View {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub name: String,
    pub query: Query,            // 过滤 AST，结构化即真相
    pub sort: SortSpec,
    pub columns: Vec<String>,    // 追加展示为列的标签 name，顺序即列序
    pub is_shared: bool,
    pub owner_id: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct SortSpec { pub field: SortField, pub desc: bool }

pub enum SortField { UpdatedAt, CreatedAt, Title }
```

- 表格**内置列**固定为 `Code`、`标题`；`columns` 中的标签按顺序追加，最后固定显示 `更新时间`。
- `columns` 中的 name 必须是该 workspace 已存在的 LabelSchema.name，否则视为非法输入。

### 3.2 查询 AST（`src/domain/query.rs`）

```rust
pub enum Query {
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    Cond(Condition),
}

pub struct Condition {
    pub field: Field,
    pub op: Op,
    pub value: Option<serde_json::Value>,
}

pub enum Field { Label(String), UpdatedAt, CreatedAt, Text }

pub enum Op {
    Present, Absent,
    Eq, Ne, In, NotIn,
    Gt, Ge, Lt, Le,
    Contains, NotContains,
}
```

- `Query::all()` = `And(vec![])`，语义恒真（用于「全部条目」的默认视图）。
- AST 以 serde **外部标签 + camelCase** 序列化为 JSON，供 GraphQL `Json` 标量前后端传输。约定形状：

```json
{ "and": [ { "cond": { "field": { "label": "Task" }, "op": "eq", "value": "Open" } },
           { "cond": { "field": "updatedAt", "op": "ge", "value": "2026-09-01" } } ] }
{ "not": { "cond": { "field": "text", "op": "contains", "value": "检索" } } }
```

`Op` 的序列化名为：`present / absent / eq / ne / in / notIn / gt / ge / lt / le / contains / notContains`。

### 3.3 表达式语言

**文法（EBNF）**

```
expr       := or_expr
or_expr    := and_expr ( 'OR' and_expr )*
and_expr   := unary ( 'AND' unary )*
unary      := 'NOT' unary | primary
primary    := '(' expr ')' | present | absent | comparison | text
present    := 'present' '(' ident ')'
absent     := 'absent' '(' ident ')'
comparison := field op value
text       := 'text' '~' scalar            // 归一化为 Cond{field:Text, op:Contains}
field      := 'updated' | 'created' | ident
op         := '=' | '!=' | '>' | '>=' | '<' | '<=' | '~' | '!~' | 'in' | 'not in'
value      := scalar | '(' scalar ( ',' scalar )* ')'
scalar     := string | number | 'true' | 'false'
```

- 关键字 `AND / OR / NOT / in / not in / present / absent / text / updated / created` 大小写不敏感。
- `~` 适用于 String / Enum（子串包含）；`>` 等适用于 Integer / Float / 时间字段。
- `updated` / `created` 的值接受 `YYYY-MM-DD` 或 RFC3339；仅日期时按当日 00:00:00 UTC 解释。

**接口**

- `Query::parse(&str) -> Result<Query, AppError>`
- `Query::to_expr(&Query) -> String`
- 二者互逆：`parse(to_expr(q)) == q`（对规范化后的 q）。

### 3.4 求值语义

`Query::evaluate(&self, entry: &Entry, labels: &[Labeling], text_hit: &dyn Fn(&str) -> bool) -> bool`

| 场景 | 语义 |
| --- | --- |
| `present(Task)` | 存在 label_name = Task 的打标 |
| `absent(Task)` | 不存在该打标 |
| 打标缺失时的比较 | 一律 `false`（`absent` 除外），避免三值逻辑 |
| `Null` 类型标签 | 仅支持 present / absent；出现比较运算符视为非法查询 |
| Boolean | `=` `!=` |
| Integer / Float | `=` `!=` `>` `>=` `<` `<=`（数值比较） |
| String | `=` `!=` `~` `!~` `in` `notIn` |
| Enum | `=` `!=` `~` `!~` `in` `notIn`（`in` 的候选值须在 enum_values 内） |
| `UpdatedAt` / `CreatedAt` | `=` `!=` `>` `>=` `<` `<=`（时间比较） |
| `Text` | `text_hit(关键词)`，由服务层用 Tantivy 结果集回答 |

- `And(vec![])` → true；`Or(vec![])` → false。
- 校验（非法运算符/类型不匹配/未知标签）在 **parse 与服务层进入查询前** 完成，返回 `AppError::InvalidQuery`。

## 4. 存储设计

### 4.1 新增 Column Family（`src/storage/rocksdb.rs`）

| CF | 键 | 值 |
| --- | --- | --- |
| `views` | `view_key(id)` = 16B ulid | bincode(View) |
| `views_by_workspace` | `view_by_workspace_key(ws, id)` = 32B | 空 |
| `labelings_by_workspace` | `labeling_by_workspace_key(ws, code, name)` | bincode(Labeling) |

### 4.2 键（`src/storage/keys.rs`）

```rust
pub fn view_key(id: Ulid) -> [u8; 16]
pub fn view_by_workspace_key(ws: Ulid, id: Ulid) -> [u8; 32]
pub fn labeling_by_workspace_key(ws: Ulid, code: &str, name: &str) -> Vec<u8>  // 16 + code + name
```

### 4.3 打标索引维护

`set_labeling` / `remove_labeling` 在既有 `LABELINGS` 点查键之外，同事务（`write_batch`）写 / 删 `labelings_by_workspace`（值即序列化的 Labeling，使「取 workspace 全部打标」一次前缀扫描即可拿到数据，避免 N+1）。

`LABELINGS` 与 `labelings_by_workspace` 由同一次 `write_batch` 保证原子一致。

## 5. 服务层

### 5.1 SearchIndex（`src/service/search.rs`，仅 `ssr`）

```rust
pub struct SearchIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
}

impl SearchIndex {
    pub fn open(dir: &str) -> Result<Self, AppError>;               // 目录 {data_dir}/search
    pub fn index_entry(&self, entry: &Entry, labels: &[Labeling]) -> Result<(), AppError>;
    pub fn remove_entry(&self, code: &str) -> Result<(), AppError>;
    pub fn search(&self, ws_id: Ulid, q: &str, limit: usize) -> Result<Vec<String>, AppError>;
    pub fn num_docs(&self) -> u64;
}
```

- **schema**：`entry_code`(STRING|STORED)、`workspace_id`(STRING)、`title`/`content`/`labels`(TEXT, tokenizer=`cjk`)。（实现不含 `updated_at` 字段：排序在 §5.2 内存中按 `Entry` 完成，索引不读该字段。）
- **分词器**：注册 `NgramTokenizer::new(1, 2)`（unigram + bigram）为 `cjk`。零新依赖，解决中文整句成单一 token 导致「查不到」的问题。
- **写入**：`index_entry` 先按 `entry_code` `delete_term` 再 `add_document`，`commit()` 后 `reader.reload()`。`content` 取 `strip_rich_text(&entry.detail)` —— 新增助手，从 Delta JSON 的 `ops[].insert` 提取纯文本（非 Delta 时原样返回）。
- **查询**：`QueryParser`（字段 title/content/labels，tokenizer `cjk`）与 `workspace_id` 的 TermQuery 取交集，`TopDocs::with_limit(limit)`，返回 entry_code 列表。
- **并发**：`IndexWriter` 置于 `Mutex`；`IndexReader` 轻量克隆。
- **回填**：启动时若 `num_docs() == 0`，扫描全部 Entry 与其打标并建索引（一次性）。增量 + 定时 reconcile 留后续。

### 5.2 EntryService 扩展

- `labelings_by_workspace(ws) -> HashMap<String, Vec<Labeling>>`：一次前缀扫描。
- 写路径（create / update / soft_delete / set_labeling / remove_labeling）落库成功后调用 `SearchIndex` 更新索引；**索引失败只记 `tracing::warn`，不使已落库的写操作失败**。
- `query(ws, query: &Query, sort: &SortSpec, page: PageInput) -> QueryResult`，其中
  `QueryResult { items: Vec<(Entry, Vec<Labeling>)>, total: usize }`：

```
1. entries = list(ws)                       // 已排除软删除
2. 若 query 含 Label 条件 → labels_map = labelings_by_workspace(ws)
3. text_hits: Option<HashSet<String>> =
       若 query 含 Text 条件 → SearchIndex.search(ws, kw, LIMIT).into_set()
4. rows = entries.filter(|e| query.evaluate(e, labels_map[e.code], |_| text_hits.contains(e.code)))
5. sort rows (SortSpec)
6. total = rows.len(); slice page
7. 仅对当页行取标签（labels_map 已有则取之，否则逐条 labelings(code)）
```

- `LIMIT`（全文候选上限）常量，取 `page_size * page * 若干倍` 与上限 1000 的折中；本轮固定 1000，翻页不改变候选集。

### 5.3 ViewService（`src/service/view.rs`）

```rust
pub struct ViewService { store: Arc<DocStore> }

create(actor, ws_id, name, query, sort, columns, is_shared) -> View
list(actor, ws_id) -> Vec<View>        // owner == actor 的 + ws 内 is_shared 的
get(view_id) -> Option<View>
update(actor, view_id, name, query, sort, columns, is_shared) -> View
delete(actor, view_id) -> ()
```

- 审计动作复用已存在的 `AuditAction::ViewCreated / ViewUpdated / ViewDeleted`，`resource_type = "view"`，`resource_id = view.id`，`before/after` 为 JSON 快照。
- `create` 校验：name 非空、`columns` 中标签存在、`query` 通过 schema 校验。

**权限**

| 操作 | 最低角色 |
| --- | --- |
| list / get | Reader（成员） |
| create 个人视图（is_shared=false） | Worker |
| update / delete 本人视图 | Worker |
| create/update/delete 共享视图（is_shared=true） | Maintainer |

（GraphQL 层用既有 `require_member` / `require_role` 表达。）

## 6. GraphQL 接口（`src/api/graphql.rs`）

**类型**

```graphql
type GqlView {
  id: ID!
  name: String!
  query: JSON!            # AST
  queryExpr: String!      # 规范表达式（由 AST 反解析）
  sort: SortSpec!
  columns: [String!]!
  isShared: Boolean!
  ownerId: ID!
  createdAt: String!
  updatedAt: String!
}
type SortSpec { field: String!, desc: Boolean! }
type EntryConnection { items: [GqlEntry!]!, total: Int!, page: Int!, pageSize: Int! }
input SortInput { field: String, desc: Boolean }
input PageInput { page: Int, pageSize: Int }
```

> GraphQL 类型名沿用 Rust 结构体名（`GqlView` 等），前端按字段名选取，不依赖类型名。

**Query**

```graphql
views(workspaceId: ID!): [GqlView!]!
view(id: ID!): GqlView
parseViewQuery(workspaceId: ID!, expr: String!): JSON!    # 表达式 -> AST；非法则报错
formatViewQuery(workspaceId: ID!, query: JSON!): String!  # AST -> 表达式
queryEntries(workspaceId: ID!, query: JSON, sort: SortInput, page: PageInput): EntryConnection!
```

- `queryEntries` 取代原 `entries(workspaceId)`；`query` 省略等价于 `Query::all()`，`sort` 默认 `{field:"updatedAt", desc:true}`，`page` 默认 `{1, 20}`、`pageSize` 上限 100。
- `SortInput.field` 取值为 `"updatedAt" | "createdAt" | "title"`，与 `SortSpec.field` 的序列化名一致，非法值报 `InvalidQuery`。

**Mutation**

```graphql
createView(workspaceId: ID!, name: String!, query: JSON!, sort: SortInput,
           columns: [String!]!, isShared: Boolean!): GqlView!
updateView(id: ID!, name: String!, query: JSON!, sort: SortInput,
           columns: [String!]!, isShared: Boolean!): GqlView!
deleteView(id: ID!): Boolean!
```

## 7. 前端设计（`src/frontend/`）

> 约束：`domain` 模块仅 `ssr` 可编译，wasm 侧无法使用 `Query` 类型。前端以 `serde_json::Value` 承载 AST；语法解析/反解析一律经服务端。

- **`graphql_client.rs`** 新增：`View` 结构（`query: Value`、`queryExpr`、`sort`、`columns`、`isShared`、`ownerId`）、`views / create_view / update_view / delete_view / parse_view_query / format_view_query / query_entries`。
- **新增 `view_filter.rs`**：AST JSON 的读写助手——生成筛选芯片数据、把芯片变更写回 AST、判断 AST 是否含某字段条件。
- **`workspace_main.rs`** 改造：
  - 侧栏：渲染「我的视图 / 共享视图」（含条目计数），点击切换活动视图；「新建视图」弹窗（名称 + 是否共享）；重命名 / 删除。
  - 顶栏：搜索框接通，输入合并为 `text ~ "关键词"` 的 **ad-hoc** 条件（不写回视图）；`视图配置` 打开列配置。
  - 筛选条：由 AST 渲染条件芯片（字段 + 运算符 + 值），可增删改；「表达式」开关展开 `queryExpr` 文本框，应用时 `parseViewQuery` 解析回 AST。
  - 表格：动态列 = `Code` + `标题` + 配置的标签列 + `更新时间`；标签格复用 `components::label_chip_class`。
  - 排序：列头/排序控件（更新时间 / 创建时间 / 标题 ↑↓）；分页：页码控件，对齐原型 pager。
- 复用 `components.rs` 的 `label_chip_class / display_enum_value / value_to_string` 与 `icons.rs`。
- 筛选/排序/分页/搜索任一变化 → 重新 `queryEntries`。

## 8. 错误处理

- 新增 `AppError::InvalidQuery(String)`，`code() = "INVALID_QUERY"`，消息为可读提示（**不**带「内部错误:」前缀，沿用 `LabelNameExists` 的先例）。
- 触发点：表达式语法错误、运算符与标签类型不匹配、引用不存在的标签、`in` 候选超出 enum_values、非法时间格式。
- Tantivy 索引写失败：记 `warn` 日志，不回滚、不报错（RocksDB 已持久化）。
- `SearchIndex::open` 失败：启动即失败（核心功能不可用，不静默降级）。

## 9. 测试策略

| 层 | 测试 |
| --- | --- |
| `domain::query` | parse↔format round-trip；求值语义表（每种 Op × 类型 × 值存在/缺失）；非法表达式返回 `InvalidQuery` |
| `storage` | 新键编码前缀扫描；`labelings_by_workspace` 随 set/remove 原子增删 |
| `service::search` | 中文子串命中；workspace 隔离；`remove_entry` 后不再命中；回填幂等 |
| `service::entry` | `query` 的标签过滤、时间过滤、全文求交、排序、分页、`total` 正确 |
| `service::view` | CRUD + 审计；权限矩阵；非法 columns/query 被拒 |
| GraphQL | 端到端：`parseViewQuery` → `createView` → `queryEntries` 返回预期条目 |

**端到端（需浏览器，用户验收）**：`cargo leptos dev` → 登录 → 进入 workspace → 新建视图 → 加标签过滤芯片 → 切表达式模式 → 配列 → 排序 → 翻页 → 顶栏中文全文搜索。

## 10. 破坏性变更与回填

- 新增 CF 与 `{data_dir}/search` 索引目录。开发数据可 `rm -rf data`（沿用前几轮做法）。
- 启动时：`labelings_by_workspace` 为空 → 遍历 Entry + 打标回填；`SearchIndex.num_docs() == 0` → 建索引。两者均幂等。

## 11. 后续留白

- `labelings_by_label` 二级索引与索引 reconcile 定时任务。
- `cang-jie`(jieba) 替换 Ngram 分词。
- 视图共享给指定账号；视图收藏/排序。
- assignee 字段与「指派人」筛选。
