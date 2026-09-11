# 视图颜色与布局 设计文档

> 日期：2026-09-11
> 依据：`spec/20260911-补充设计.md`、`spec/20260910-补充设计.md`
> 状态：设计已与用户确认（客户端计算标题色；值色存 LabelSchema；后端不写单测）

## 1. 背景与目标

上一轮把「视图筛选与标签查询」做成可用闭环。本轮补齐两处遗漏，并实现 0911 补充设计：

1. **标签颜色**：标签可设**基础色**；数值/枚举标签的颜色可**随值变化**（如 Score 0–60 红、60–90 黄、90–100 绿）。
2. **标题颜色规则**：视图可配**一组规则**（条件复用查询 AST，可组合多标签），命中即给 Entry 标题着色。
3. **布局**：页面撑满窗口，左侧栏可收缩，右侧详情支持 Esc 关闭、单击开合、双击全屏浮层。
4. **两处遗漏**：侧栏每个视图的条目计数；`labelings_by_workspace` 启动回填。

## 2. 范围与非目标

**本轮范围**

- `LabelSchema` 增加 `color`（基础色）与 `value_colors`（值→色）。
- `View` 增加 `title_colors`（规则集）。
- 前端：标签编辑器颜色配置、视图标题规则配置、表格着色（标题 + 标签格）、布局改造。
- 侧栏条目计数；`labelings_by_workspace` 回填。

**非目标（本轮不做）**

- 0910 里的 Date/Time/DateTime/Currency/Email 标签类型、String 长度范围、数值格式/单位校验（当前也未实现）。
- 标签颜色随值变化的「按视图覆写」（本轮值色是标签级、全工作空间生效）。
- 颜色主题/深色模式。

## 3. 数据模型

### 3.1 LabelSchema 扩展（`src/domain/label.rs`）

```rust
pub struct LabelSchema {
    pub workspace_id: Ulid,
    pub name: String,
    pub title: String,
    pub value_type: LabelValueType,
    pub enum_values: Vec<String>,
    pub color: Option<String>,          // 基础色 #rrggbb，默认 None
    pub value_colors: Vec<ValueColor>,  // 值 → 色
}

#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ValueColor {
    Range { min: Option<f64>, max: Option<f64>, color: String }, // Integer/Float，左闭右开
    Value { value: String, color: String },                      // Enum 精确匹配
}
```

**取色规则**（`fn resolve_color(label: &LabelSchema, value: &serde_json::Value) -> Option<String>`，纯函数，前端也有一份等价实现）：

1. Enum：按 `value_colors` 里 `Value{value}` 精确匹配，首个命中。
2. Integer/Float：按 `Range` 顺序，`min`（含）≤ v < `max`（不含），`min`/`max` 省略视为无界，首个命中。
3. 未命中 → `label.color`；再没有 → `None`（前端回退到现有哈希配色）。

### 3.2 View 扩展（`src/domain/view.rs`）

```rust
pub struct View {
    // …现有字段…
    pub title_colors: Vec<TitleColorRule>, // 按顺序命中即用
}
pub struct TitleColorRule { pub query: Query, pub color: String }
```

`TitleColorRule.query` 复用现有 `Query` AST（serde camelCase JSON），前端以 `Value` 承载。

## 4. 颜色计算

- **标签值色**：`LabelSchema.value_colors` 随 `labelSchemas` 查询下发到前端；表格里标签格与筛选芯片用前端 `resolve_color`（JSON）算，无配置回退哈希色。后端不参与。
- **标题色**：**客户端计算**。前端新增 `query_eval`：把规则里的 `Query` JSON 在浏览器里对 `Entry` + 其 `Labeling` 求值。视图加载后，表格每行按 `title_colors` 顺序求值，首个命中取其 `color` 作为标题色；都不命中用默认色。

前端求值器支持 `And/Or/Not/Cond`；`Cond` 的 `Label`（present/absent/=/!=/in/not in/>/>=/</<=/~/!~）、`updatedAt`/`createdAt`（按时间比较）、`Text`（对 title/detail/标签值做不区分大小写的子串匹配——客户端无全文索引，用朴素子串近似）。

> 约束：`src/domain/*` 仅 ssr 可编译，前端不得引用 domain 类型；故 `query_eval` 只吃 `serde_json::Value`。

## 5. 存储 · 服务 · GraphQL

- **存储**：`LabelSchema`/`View` 直接 bincode 序列化，新字段自带（旧数据缺字段需默认值——`#[serde(default)]`）。
- **服务**：`LabelService::create_schema/update_schema` 增加 `color`、`value_colors` 入参；`ViewService::create/update` 增加 `title_colors`。`Services::new` 补 `labelings_by_workspace` 回填（列族为空则遍历 `ENTRIES` + `LABELINGS` 重建，幂等）。
- **GraphQL**：
  - `GqlLabelSchema` 加 `color`、`valueColors`；`createLabelSchema`/`updateLabelSchema` 加同名入参。
  - `GqlView` 加 `titleColors`（JSON）；`createView`/`updateView` 加 `titleColors: JSON`。
  - `views` 每项返回 `entryCount: Int`（侧栏计数）——在 resolver 里对每个视图按其查询条件计数（复用 `EntryService::query` 的 total 或轻量计数）。
  - `queryEntries` 不变（标题色客户端算）。

## 6. 前端

### 6.1 标签编辑器（`label_editor.rs`）
- 基础色：取色器 + 「清除」。
- 值→色列表：按类型显示「区间（最小/最大 + 色）」或「枚举值（下拉 + 色）」行，可增删；提交随 `updateLabelSchema` 落库。

### 6.2 视图标题颜色规则
- 视图配置弹窗里新增「标题颜色规则」区块：每条规则 = 条件（复用筛选芯片编辑器）+ 取色器，可增删排序；随 `updateView` 落库。

### 6.3 表格着色
- 标题：按 `title_colors` 客户端求值着色（首个命中）。
- 标签格/芯片：按 `value_colors` 客户端求值着色；无配置回退 `label_chip_class`。

### 6.4 布局
- `.page` 撑满视口（`100vh`，去多余外边距）；`.ws-layout` flex 撑满。
- 侧栏收缩：按钮切换宽/窄（仅图标），状态存 `localStorage`。
- 详情面板：Esc 关闭；单击表格行开合详情；双击表格行打开**全屏浮层**（`position:fixed` 覆盖表格），浮层内 Esc 退出。
- 复用现有 `EntryPanel`/`EntryTable`，调整其容器与键盘/鼠标事件。

## 7. 错误处理

- 颜色字符串格式在后端做基本校验（`#rrggbb`，否则 `InvalidQuery`）；前端取色器保证。
- 值色/标题规则非法（如 Enum 值不在 `enum_values`）在保存时校验，报 `InvalidQuery`。

## 8. 测试策略

- **后端不写单元测试**（用户明确）。以 `cargo build` + wasm `check` 为编译门。
- 前端 `query_eval`/`resolve_color` 为纯函数，属可测逻辑；若时间允许加少量内联测试，否则以 wasm check 为准。
- 端到端仍需用户浏览器验收。

## 9. 破坏性变更与回填

- `LabelSchema`/`View` 增字段：bincode 旧数据不兼容 → 开发期 `rm -rf data`。
- `labelings_by_workspace` 回填补齐旧库。

## 10. 后续留白

- 0910 完整标签类型系统（Date/Currency/Email 等）与校验。
- 标签值色的按视图覆写。
