# 视图颜色与布局 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`).

**Goal:** 标签可设基础色与「值→色」；视图可配「标题颜色规则」（客户端求值）；页面全窗口、侧栏可收缩、详情 Esc/单击/双击全屏；补侧栏条目计数与 `labelings_by_workspace` 回填。

**Architecture:** 颜色与规则存到 `LabelSchema`/`View`（bincode），随 GraphQL 下发 JSON；标签值色与标题色都在前端用纯 JSON 函数计算（前端不引用 ssr-only 的 domain）。后端只存/传/校验。

**Tech Stack:** Rust / Leptos 0.8 (SSR + wasm) / async-graphql 7 / RocksDB / bincode / serde_json

**Spec:** `docs/superpowers/specs/2026-09-11-view-colors-layout-design.md`

## Global Constraints

- **后端不写单元测试**（用户明确）；以后端 `cargo build` + 前端 wasm `cargo check` 为编译门。
- `src/domain/*` 仅 `ssr` 可编译；wasm 前端**不得**引用 `domain` 类型，颜色/规则一律以 `serde_json::Value` 承载。
- 颜色字符串为 `#rrggbb`；非法在进入保存前返回 `AppError::InvalidQuery`。
- `LabelSchema`/`View` 新增字段用 `#[serde(default)]`，避免旧数据反序列化失败。
- 破坏性变更：开发期可 `rm -rf data`。
- 本机 8GB/4 核：cargo 用绝对路径 `/Users/wangxiaoyan/.cargo/bin/cargo`，前缀 `CARGO_BUILD_JOBS=1`，Bash 命令设 `dangerouslyDisableSandbox: true`，输出重定向到文件再读；不要并发跑两个 cargo（构建锁死锁）。

---

### Task 1: 领域层 —— 标签颜色与视图标题规则

**Files:**
- Modify: `src/domain/label.rs`（`LabelSchema` 加字段 + `ValueColor` + `resolve_color`）
- Modify: `src/domain/view.rs`（`View` 加 `title_colors` + `TitleColorRule`）

**Interfaces:**
- Produces:
  - `LabelSchema { …, color: Option<String>, value_colors: Vec<ValueColor> }`，两者 `#[serde(default)]`
  - `struct ValueColor { color: String, min: Option<f64>, max: Option<f64>, value: Option<String> }`（`#[serde(rename_all="camelCase")]`，`PartialEq`）。**不用 tagged enum**：`LabelSchema` 走 bincode，bincode 不支持 `deserialize_any`。语义：`value` 为 `Some` → 枚举精确匹配；否则按 `min`/`max` 数值区间（`None` 无界）。
  - `LabelSchema::new(ws, name, title, value_type, enum_values)` 保持原签名（color/value_colors 默认空）；新增 `with_colors(self, color, value_colors) -> Self`
  - `pub fn resolve_color(schema: &LabelSchema, value: &serde_json::Value) -> Option<String>`
  - `View { …, title_colors: Vec<TitleColorRule> }`（`#[serde(default)]`）
  - `struct TitleColorRule { pub query: Query, pub color: String }`（`PartialEq`）

- [ ] **Step 1: 实现 `LabelSchema` 扩展与 `ValueColor`**

在 `src/domain/label.rs`：给 `LabelSchema` 加 `color`/`value_colors`（`#[serde(default)]`），`new` 里置 `None`/`vec![]`，加 `with_colors`。加 `ValueColor` **普通 struct**（非 tagged enum：bincode 不支持 `deserialize_any`；派生 `Debug, Clone, PartialEq, Serialize, Deserialize`）。

- [ ] **Step 2: 实现 `resolve_color`**

```rust
/// 标签值 → 颜色：Enum 精确匹配；数值按区间（左闭右开，首个命中）；未命中回退基础色。
pub fn resolve_color(schema: &LabelSchema, value: &serde_json::Value) -> Option<String> {
    for vc in &schema.value_colors {
        let hit = if let Some(want) = &vc.value {
            value.as_str() == Some(want.as_str())
        } else if let Some(v) = value.as_f64() {
            vc.min.map_or(true, |lo| v >= lo) && vc.max.map_or(true, |hi| v < hi)
        } else {
            false
        };
        if hit {
            return Some(vc.color.clone());
        }
    }
    schema.color.clone()
}
```

- [ ] **Step 3: 实现 `View.title_colors` 与 `TitleColorRule`**

在 `src/domain/view.rs`：给 `View` 加 `title_colors: Vec<TitleColorRule>`（`#[serde(default)]`）；定义 `TitleColorRule { query: Query, color: String }`。修正 `View` 的所有构造点（如 `ViewService::create/update` —— Task 2 处理；本步若有 `#[cfg(test)]` 构造需补 `title_colors: vec![]`）。

- [ ] **Step 4: 编译门**

Run: `CARGO_BUILD_JOBS=1 <cargo> check --lib`
Expected: 通过（可能有 Task 2 未改的构造点报错 → 属预期，Task 2 修）。

- [ ] **Step 5: 提交**

```bash
git add src/domain/label.rs src/domain/view.rs
git commit -m "feat(domain): add label colors and view title-color rules"
```

---

### Task 2: 服务层 —— 颜色入参、视图规则、回填

**Files:**
- Modify: `src/service/label.rs`（create/update 增加 color/value_colors + 校验）
- Modify: `src/service/view.rs`（create/update 增加 title_colors）
- Modify: `src/service/mod.rs`（`Services::new` 补 `labelings_by_workspace` 回填）
- Modify: `src/service/entry.rs`（如需加 `count(ws, query) -> usize` 供侧栏计数）

**Interfaces:**
- Produces:
  - `LabelService::create_schema(actor, ws, name, title, value_type, enum_values, color: Option<String>, value_colors: Vec<ValueColor>) -> Result<LabelSchema>`
  - `LabelService::update_schema(actor, ws, name, title, enum_values, color, value_colors) -> Result<LabelSchema>`
  - `ViewService::create/update(…, title_colors: Vec<TitleColorRule>)`
  - `EntryService::labelings_by_workspace_backfill(&self) -> Result<usize>`（列族为空则遍历 `ENTRIES` + `LABELINGS` 重建）
  - `EntryService::count(&self, ws: Ulid, query: &Query, labels_map…)`（或复用 `query` 的 total）——侧栏计数用

- [ ] **Step 1: 颜色校验助手**

在 `src/service/label.rs` 加 `fn check_color(c: &str) -> Result<(), AppError>`：`^#[0-9a-fA-F]{6}$`，否则 `InvalidQuery("颜色格式应为 #rrggbb")`；对 `ValueColor` 里每个 `color` 与 Enum 的 `value` 字段（须在 `enum_values` 内）一并校验。

- [ ] **Step 2: create/update 签名与落库**（label.rs）——把 `color`/`value_colors` 写进 `LabelSchema`，保存前调 `check_color`。

- [ ] **Step 3: ViewService 增加 `title_colors`**（view.rs）——`create`/`update` 的 `View { … title_colors }`；校验每条规则的 `color` 格式（复用 `check_color`，可提到公共处或复制）。`View` 的所有构造点补齐。

- [ ] **Step 4: `labelings_by_workspace` 回填**（entry.rs + mod.rs）

```rust
/// 列族为空时从 ENTRIES + LABELINGS 回填 labelings_by_workspace（幂等）。
pub fn labelings_by_workspace_backfill(&self, store: &DocStore) -> Result<usize, AppError> { ... }
```
`Services::new` 里在 `search.backfill` 之后调用。

- [ ] **Step 5: 侧栏计数**（entry.rs）——加 `pub fn count(&self, ws: Ulid, query: &Query) -> Result<usize, AppError>`：复用 `query(ws, query, &SortSpec::default(), PageInput{page:1,page_size:1})` 取 `total`，避免重复逻辑。

- [ ] **Step 6: 编译门 + 提交**

Run: `CARGO_BUILD_JOBS=1 <cargo> build`
commit: `feat(service): persist label colors, view title rules, and backfill labelings index`

---

### Task 3: API 层 —— GraphQL 入参/出参

**Files:** Modify: `src/api/graphql.rs`

**Interfaces:**
- `GqlLabelSchema` 加 `color: Option<String>`、`value_colors: Json<Vec<...>>`（或直接 `serde_json::Value`）
- `createLabelSchema`/`updateLabelSchema` 加 `color: Option<String>`、`valueColors: Option<Json<Value>>`
- `GqlView` 加 `title_colors: Json<Value>`（`rename_fields` → `titleColors`）
- `createView`/`updateView` 加 `titleColors: Option<Json<Value>>`
- `GqlView` 加 `entry_count: i32`（→ `entryCount`）；`views` resolver 对每个视图算 count

- [ ] **Step 1: label schema 出参/入参**——`From<LabelSchema> for GqlLabelSchema` 带上 color/value_colors（JSON）；两个 mutation 解析新入参并反序列化为 `Vec<ValueColor>`（`serde_json::from_value`，错误映射 `InvalidQuery`）。
- [ ] **Step 2: view 出参/入参**——`GqlView::new` 带 `title_colors`；create/updateView 解析 `titleColors` 为 `Vec<TitleColorRule>`；`GqlView` 加 `entry_count` 字段，`new` 接收它（或先置 0，由 `views` 填）。
- [ ] **Step 3: `views` 计数**——resolver 内对每个视图 `gql.services.entry.count(ws, &v.query)?` 填充 `entry_count`。注意：`views` 只有 `workspaceId`，用户可能非活跃视图，计数用视图自身 query。
- [ ] **Step 4: 编译门 + 提交**

Run: `CARGO_BUILD_JOBS=1 <cargo> build`
commit: `feat(api): expose label colors, view title rules, and view entry counts`

---

### Task 4: 前端 —— 客户端颜色/求值器 + graphql_client

**Files:**
- Create: `src/frontend/query_eval.rs`（JSON AST 求值 + 颜色解析）
- Modify: `src/frontend/graphql_client.rs`（schema/view 字段 + 变更签名）
- Modify: `src/frontend/mod.rs`

**Interfaces:**
- `pub fn eval(query: &Value, entry: &Entry, labels: &[Labeling]) -> bool`——`And/Or/Not/Cond`；Label 各算子、updatedAt/createdAt 时间比较、Text 朴素子串。
- `pub fn resolve_label_color(schema_color: Option<&Value>, value_colors: &Value, value: &Value) -> Option<String>`
- `pub fn title_color(rules: &Value, entry: &Entry, labels: &[Labeling]) -> Option<String>`——按序求值首个命中。
- graphql_client：`LabelSchema { …, color: Option<String>, value_colors: Vec<Value> }`；`View { …, title_colors: Value, entry_count: i64 }`；`create_label_schema/update_label_schema` 增参；`create_view/update_view` 增 `title_colors`。

- [ ] **Step 1: `query_eval.rs` 求值器**
  - 递归 `And/Or/Not/Cond`；`Cond{field,op,value}`。
  - Label：present/absent 看 labels；其余按 `value` 与算子比较（数值比大小、字符串 `~` 子串、in/notIn 数组包含）。
  - updatedAt/createdAt：字符串日期与 `value` 比较（RFC3339 或 `YYYY-MM-DD`，按天/秒比较；实现可解析为时间戳字符串比较）。
  - Text：对 `entry.title`、`entry.detail`、所有标签值做不区分大小写子串匹配。
  - `resolve_label_color` / `title_color` 按 spec §4 规则。
- [ ] **Step 2: graphql_client 字段与函数签名**——加字段、改 `create_view/update_view` 增 `title_colors: &Value`、`create_label_schema/update_label_schema` 增 `color`/`value_colors`；`VIEW_FIELDS` 加 `titleColors entryCount`。
- [ ] **Step 3: `frontend/mod.rs` 加 `pub mod query_eval;`**
- [ ] **Step 4: 编译门**

Run（wasm）: `CARGO_BUILD_JOBS=1 <cargo> check --no-default-features --features hydrate --target wasm32-unknown-unknown`
commit: `feat(frontend): add client query evaluator and color helpers`

---

### Task 5: 前端 —— 标签编辑器颜色配置

**Files:** Modify: `src/frontend/label_editor.rs`、`style/main.css`

- [ ] **Step 1**: 基础色取色器 `<input type="color">` + 清除按钮。
- [ ] **Step 2**: 「值→颜色」列表——按 `value_type`：Integer/Float 显示 最小/最大/色 行；Enum 显示 枚举值下拉/色 行；可增删。`props` 里带上当前 `color`/`value_colors`（从 LabelSchema 传入）。
- [ ] **Step 3**: 提交时把 `color`/`value_colors` 传给 `update_label_schema`。
- [ ] **Step 4**: 编译门（wasm）+ 提交：`feat(frontend): label color and value-color editor`

---

### Task 6: 前端 —— 视图标题颜色规则编辑

**Files:** Modify: `src/frontend/pages/workspace_main.rs`、`style/main.css`

- [ ] **Step 1**: 视图配置弹窗里加「标题颜色规则」区块：每行 = 条件（用 `CondChip` 编辑器或一个文本表达式 + `parse_view_query`）+ 取色器；可增删。
- [ ] **Step 2**: 保存时把规则序列化为 `[{query, color}]` 传给 `update_view` 的 `title_colors`。
- [ ] **Step 3**: 编译门（wasm）+ 提交：`feat(frontend): view title-color rules editor`

---

### Task 7: 前端 —— 表格着色 + 侧栏条目计数

**Files:** Modify: `src/frontend/pages/workspace_main.rs`、`components.rs`（如需要）

- [ ] **Step 1**: 表格标题按 `title_color(&active_view.title_colors, entry, labels)` 上色（`style="color:…"`）。
- [ ] **Step 2**: 标签格/芯片用 `resolve_label_color(schema.color, schema.value_colors, value)`；无配置回退现有 `label_chip_class`。
- [ ] **Step 3**: 侧栏每行渲染 `.n` 计数（`view.entry_count`）。
- [ ] **Step 4**: 编译门（wasm）+ 提交：`feat(frontend): color table by rules and show view entry counts`

---

### Task 8: 前端 —— 布局（全窗口 / 收缩 / 详情交互）

**Files:** Modify: `src/frontend/pages/workspace_main.rs`、`style/main.css`

- [ ] **Step 1**: `.page` 撑满视口（`height:100vh` 等），`.ws-layout` flex 撑满、内部滚动。
- [ ] **Step 2**: 侧栏收缩按钮，宽/窄切换，状态存 `localStorage`。
- [ ] **Step 3**: 详情：Esc 关闭；单击行开合（已开则关）；双击行打开全屏浮层（`position:fixed` 覆盖），浮层内 Esc 退出。
- [ ] **Step 4**: 编译门（wasm）+ 提交：`feat(frontend): full-window layout with collapsible sidebar and fullscreen detail`

---

### Task 9: 端到端验证与收尾

- [ ] **Step 1**: `CARGO_BUILD_JOBS=1 <cargo> test`（现有 75 测试不回归）、`cargo build`、wasm `check` 全绿。
- [ ] **Step 2**: 用户浏览器验收（`rm -rf data && make dev`）：标签设基础色/值色 → 芯片与表格着色；视图配标题规则 → 标题变色；全窗口/收缩/Esc/单击/双击全屏；侧栏计数。
- [ ] **Step 3**: 如有实现偏差，同步 spec 并提交。

---

## 自审结论

- **Spec 覆盖**：§3 数据模型 → T1；§5 存储/服务/GraphQL → T2/T3；§4 客户端计算 → T4；§6 前端 UI/布局 → T5–T8；§8 测试（无后端单测）→ 各任务编译门；§9 回填 → T2。
- **类型一致性**：`ValueColor`/`TitleColorRule`/`resolve_color`/`query_eval::eval` 在各任务间签名一致。
- **占位符**：无 TBD；关键算法（`resolve_color`、求值器）给出实现要点。
