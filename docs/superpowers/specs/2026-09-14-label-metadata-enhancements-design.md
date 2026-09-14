# 标签与元数据增强 设计文档

> 日期：2026-09-14
> 依据：用户 2026-09-14 需求 1 / 3 / 4 / 5、`spec/20260910-补充设计.md`
> 状态：设计已与用户确认
> 关联：AI 总结（需求 2）见 `2026-09-14-ai-entry-summary-design.md`

## 1. 背景与目标

补齐 0910 补充设计里未实现的标签类型体系，并把 Entry 的内置元数据纳入表达式与展示。

1. **Enum 多选**：值为 Enum 的标签默认单选，可另配「多选」。
2. **五种新值类型**：Date / Time / DateTime / Currency / Email。
3. **时间选择器**：表达式输入框里写到时间型标签 key（或内置 CreatedAt / UpdatedAt）加比较运算符时，自动浮出时间选择控件；展示格式默认 `2006-01-02 15:04:05`。
4. **详情元信息**：Entry 详情包含创建人 / 创建时间 / 更新人 / 更新时间。
5. **内置元数据关键字**：`Code` / `Title` / `Detail` / `CreatedBy` / `CreatedAt` / `UpdatedBy` / `UpdatedAt` 视作内置元数据，可用表达式查询、出现在 `/` 提示列表，且不允许自定义标签 key 与它们重名。

## 2. 范围与非目标

**范围**

- `LabelValueType` 增 5 型；`LabelValue` 增对应变体 + `EnumList`；`LabelSchema` 增 `multi` / `format` / `currency_symbol` / `unit`。
- `Field` 增 5 个内置元数据；词法器识别 7 个关键字；`LabelService` 加保留名校验。
- 前端：设置页新类型选项与属性、标签编辑器多选与新型控件、表达式提示列表注入内置名、时间选择器、详情元信息、`query_eval` 同步。

**非目标（本轮不做）**

- Integer / Float 的数值展示格式与单位（0910 里有，但这几条需求没要求）。
- String 长度范围校验。
- 完整数值格式语言：Currency 只认 `#,##0` / `#,##0.00` 这类常见分组模式。
- 表达式里 `Code` / `Title` / `Detail` 的正则或模糊匹配（只做等值与子串包含）。

## 3. 数据模型（`src/domain/label.rs`、`src/domain/query.rs`）

### 3.1 LabelValueType 新增 5 型

```rust
pub enum LabelValueType {
    Null, Boolean, Integer, Float, String, Enum,
    Date, Time, DateTime, Currency, Email, // 新增
}
```

`as_str` / `from_str` 对应 `"date"` / `"time"` / `"datetime"` / `"currency"` / `"email"`。

### 3.2 LabelValue 新增变体

bincode 按**声明顺序**编码枚举变体，新增变体只能**追加在末尾**（现有 6 个索引不动）：

```rust
pub enum LabelValue {
    Null, Bool(bool), Int(i64), Float(f64), String(String), Enum(String),
    // 追加，勿插入到上方
    EnumList(Vec<String>),
    Date(String),      // 规范化后的时间串
    Time(String),
    DateTime(String),
    Currency(f64),
    Email(String),
}
```

`from_json` 按 schema 的 `value_type` 校验转换：

- `Enum` 且 `multi=true`：接受数组（每个值须 ∈ `enum_values`）→ `EnumList`；也接受单个字符串 → 单元素 `EnumList`。
- `Enum` 且 `multi=false`：只接受字符串；数组报 `InvalidLabelValue`。
- `Date`/`Time`/`DateTime`：字符串，按 schema `format`（缺省用默认布局，见 §6）解析，解析失败报 `InvalidLabelValue`。
- `Currency`：数值。
- `Email`：字符串，且通过基础格式校验（恰一个 `@`，本地与域名均非空）。

`to_json`：`EnumList` → JSON 数组；时间型/Email → 字符串；`Currency` → 数值。

### 3.3 LabelSchema 新增属性

```rust
pub struct LabelSchema {
    // …现有字段…
    #[serde(default)] pub multi: bool,                 // 仅 Enum 有效
    #[serde(default)] pub format: Option<String>,      // 时间型=Go 布局；Currency=数值格式
    #[serde(default)] pub currency_symbol: Option<String>, // Currency 货币符号，缺省 ¥
    #[serde(default)] pub unit: Option<String>,        // Currency 单位，如「元」「万」
}
```

`multi` 仅当 `value_type == Enum` 时允许为 true，否则保存报错。

构造函数：`Task` / `Bug` 不变（新字段取默认）；新增 `with_attrs(multi, format, currency_symbol, unit)` 链式设置，与既有 `with_colors` 并列。

### 3.4 Field 新增内置元数据

```rust
pub enum Field {
    Label(String),
    UpdatedAt, CreatedAt, Text,
    Code, Title, Detail, CreatedBy, UpdatedBy, // 新增
}
```

`Field` 以 camelCase 序列化进 `View.query`（已存视图的 AST 不受影响）：新增变体序列化为 `"code"` / `"title"` / `"detail"` / `"createdBy"` / `"updatedBy"`。

## 4. 表达式语法（`src/domain/query.rs`）

### 4.1 关键字与保留名

词法器目前把 `created` / `updated` 当关键字。改为（**大小写不敏感**）识别：

| 关键字 | 落成 |
|---|---|
| `Code` / `Title` / `Detail` | `Field::Code` / `Title` / `Detail` |
| `CreatedBy` / `UpdatedBy` | `Field::CreatedBy` / `UpdatedBy` |
| `CreatedAt` / `UpdatedAt` | `Field::CreatedAt` / `UpdatedAt` |
| `text` | `Field::Text`（保留，全文检索） |

**移除** `created` / `updated` 旧别名。已存视图存的是 AST 而非表达式文本，故不受影响；但用户手写的旧表达式文本（`updated >= "…"`）需改为 `UpdatedAt >= "…"`。

保留名常量（供解析器与保留校验共用）：

```rust
pub const RESERVED_FIELDS: [&str; 7] =
    ["Code", "Title", "Detail", "CreatedBy", "CreatedAt", "UpdatedBy", "UpdatedAt"];
```

`LabelService::create_schema` 拒绝与保留名重名的 key（大小写不敏感）。为此在 `AppError` 新增变体 `LabelNameReserved`（code `LABEL_NAME_RESERVED`，消息「与内置元数据重名」），并在 `error.rs` 的 code 映射里登记，前端可据此给出明确提示。历史上已存在同名标签（如名为 `Title` 者）不再可用表达式查询，属可接受的破坏。

### 4.2 各类型允许的运算符

`op_allowed` 扩展（`Present` / `Absent` 对所有类型恒可用，不变）：

| 类型 | 允许的运算符 |
|---|---|
| Date / Time / DateTime | `=` `!=` `>` `>=` `<` `<=` |
| Currency | `=` `!=` `>` `>=` `<` `<=` |
| Email | `=` `!=` `~` `!~` `in` `not in` |
| Code / Title / Detail | `=` `!=` `~` `!~` `in` `not in` |
| CreatedBy / UpdatedBy | `=` `!=` `~` `!~` `in` `not in` |

### 4.3 值解析与校验

- Date / Time / DateTime 标签的比较值：按 schema `format`（缺省默认布局）解析，失败报 `InvalidQuery`。
- `CreatedAt` / `UpdatedAt`：沿用扩展后的 `parse_time`（§6.3）。
- Currency 标签：比较值须为数值。
- Email / Code / Title / Detail / CreatedBy / UpdatedBy：字符串运算符的值须为字符串；`in` / `not in` 的值须为数组。

### 4.4 表达式呈现

`to_expr` / `expr_op` 输出规范名：`Code` / `Title` / `Detail` / `CreatedBy` / `CreatedAt` / `UpdatedBy` / `UpdatedAt` / `text`（当前小写的 `created` / `updated` 改为规范名）。往返测试用例同步更新。

## 5. 求值语义

### 5.1 多值 Enum：集合语义

把标签值统一看成**字符串集合**（单值 Enum 是 1 元素特例），正负运算符成对：

| 运算符 | 命中条件 |
|---|---|
| `=` | 任一元素等于目标 |
| `!=` | 无元素等于目标 |
| `~` | 任一元素**子串**包含目标 |
| `!~` | 无元素包含目标 |
| `in` | 任一元素 ∈ 目标集合 |
| `not in` | 无元素 ∈ 目标集合 |

前后端（`query.rs::cmp_value` 与 `frontend/query_eval.rs::cmp_value`）都按此实现。`resolve_color`（`label.rs`）对数组值：任一元素命中某条 `value_colors` 规则即用该色。

### 5.2 时间 / 金额 / 邮箱

- Date / Time / DateTime：两侧按布局解析为时刻后按时间先后比较（`=` / `!=` 为时刻相等）。
- Currency：沿用数值比较（`as_f64`）。
- Email：按字符串运算符处理。

### 5.3 CreatedBy / UpdatedBy 的账号解析

`Entry` 只存账号 `Ulid`。求值前解析成 `(显示名, 邮箱)`，**任一匹配**即命中（大小写不敏感）：

- `=`：显示名或邮箱等于目标。
- `~`：显示名或邮箱子串包含目标。
- `in` / `not in`：目标集合里存在（或不存在）等于显示名或邮箱的项。

`EntryService::query` 扫 `cf::ACCOUNTS` 建 `HashMap<Ulid, (String, String)>`（`Account.name` / `Account.email`），仅当查询树里出现 `CreatedBy` / `UpdatedBy` 时才构建（`Query::contains_account_field()`）。

### 5.4 求值环境

`Query::evaluate` 现签名 `(entry, labels, text_hit: &dyn Fn(&str) -> bool)`；新增账号解析需再传一个闭包。引入：

```rust
pub struct EvalEnv<'a> {
    pub text_hit: &'a dyn Fn(&str) -> bool,
    /// Ulid → (显示名, 邮箱)；未知账号返回 None
    pub account_of: &'a dyn Fn(Ulid) -> Option<(String, String)>,
}
```

`evaluate(&self, entry: &Entry, labels: &[Labeling], env: &EvalEnv)`。`domain/query.rs` 现有单测同步改为构造 `EvalEnv`（`account_of` 传空映射即可）。

## 6. 展示格式：Go 布局串

### 6.1 共享模块（`src/golayout.rs`，不 feature-gate）

Rust 与 JS 都要格式化，且 hydrate 构建没有 `chrono`（`chrono` 是 ssr-only 依赖）。故该模块**不依赖 chrono**，在纯 `YmdHms` 上工作：

```rust
pub struct YmdHms { pub year: i32, pub month: u32, pub day: u32,
                    pub hour: u32, pub minute: u32, pub second: u32 }

pub fn format(layout: &str, t: YmdHms) -> String;
pub fn parse(layout: &str, s: &str) -> Option<YmdHms>;
```

后端用 `chrono` 的 `DateTime` ↔ `YmdHms` 互转后调用；前端用 JS `Date` ↔ `YmdHms`。`golayout` 不在 `domain` 里，故不受「前端不得引用 domain」约束。

### 6.2 默认布局与映射表

按 token **最长匹配**扫描，未识别的字符原样输出（充当分隔符）：

| Go token | 含义 | 示例 |
|---|---|---|
| `2006` / `06` | 年（4 位 / 2 位） | 2026 / 26 |
| `01` / `1` | 月（补零 / 不补） | 09 / 9 |
| `02` / `2` | 日（补零 / 不补） | 04 / 4 |
| `15` | 24 小时制时 | 15 |
| `03` | 12 小时制时 | 03 |
| `04` | 分 | 04 |
| `05` | 秒 | 05 |
| `Jan` / `January` | 月名缩写 / 全称 | Sep / September |
| `Mon` / `Monday` | 星期缩写 / 全称 | Sun / Sunday |
| `PM` / `pm` | 上下午 | PM / pm |

默认布局：

- Date：`2006-01-02`
- Time：`15:04:05`
- DateTime：`2006-01-02 15:04:05`（即需求里的默认展示格式）

### 6.3 `parse_time` 扩展（`domain/query.rs`）

现有只认 RFC3339 与 `%Y-%m-%d`；扩展为依次尝试 RFC3339、`2006-01-02 15:04:05`、`2006-01-02 15:04`、`2006-01-02`、`15:04:05`、`15:04`。否则 `InvalidQuery`。

## 7. 存储 · 服务 · GraphQL

- **存储**：`LabelSchema` / `LabelValue` 的字段与变体变更属 bincode 不兼容，按既有约定开发期 `rm -rf data`（见 §11）。`SearchIndex::add_entry_doc` 的标签文本对数组值改为**元素空格连接**（当前 `to_json().to_string()` 会把数组写成 JSON 串）。
- **服务**：`create_schema` / `update_schema` 入参已偏多，改为收一个 `LabelSchemaInput` 结构（name / title / value_type / enum_values / multi / format / currency_symbol / unit / color / value_colors），并做保留名与类型属性校验。
- **GraphQL**：
  - `GqlLabelSchema` 加 `multi` / `format` / `currencySymbol` / `unit`；`createLabelSchema` / `updateLabelSchema` 加同名入参（用一个 input object 承载）。
  - `GqlEntry` 加 `createdByAccount` / `updatedByAccount`（`GqlAccount`，账号已删则为 null）——**不动**现有的 `createdBy` / `updatedBy`（ID）。
  - `queryEntries` 的筛选逻辑随之支持新字段，无需改签名。

## 8. 前端

### 8.1 设置页（`pages/settings.rs`）

- 类型下拉加 Date / Time / DateTime / Currency / Email。
- 按类型显示属性输入：时间型 → 展示格式（Go 布局，占位默认值）；Currency → 货币符号 / 单位 / 数值格式；Enum → 「多选」勾选。
- `schema_row` 展示新增属性；`components.rs::value_type_label` 加中文名（日期 / 时间 / 日期时间 / 金额 / 邮箱）。

### 8.2 标签编辑器（`label_editor.rs`）

- Enum 且 `multi`：渲染复选组，勾选集合整体 `set_labeling`（`Value::Array`）。
- Date / Time / DateTime：`<input type="date|time|datetime-local">`，按 schema `format` 转换后落库。
- Currency：数值输入 + 符号/单位前缀展示。
- Email：文本输入 + 基础格式提示。
- 多值展示：表格标签格与芯片用 `, ` 连接。

### 8.3 表达式输入（`pages/workspace_main.rs`）

**提示列表**：把 7 个内置名恒定并入 `/` 候选（带中文展示名：编码 / 标题 / 详情 / 创建人 / 创建时间 / 更新人 / 更新时间）。当前候选只来自 `ep.label_names`（本页出现过的标签名），内置部分与其合并、去重。

**时间选择器**：新增 `detect_time_picker(text_before_caret) -> Option<(TimeKind, usize)>`，**纯手写扫描**（本仓库不引入 `regex` 依赖）：取光标前的最后一段，若形如「键 + 比较运算符 + 结尾空白」（运算符后尚未写值）且键属于「时间型标签 ∪ {CreatedAt, UpdatedAt}」，则浮出原生 `date` / `time` / `datetime-local` 控件。选中后按对应 Go 布局格式化成字符串、**加引号**插入光标处（时间值不加引号会被词法器当数字解析）。

### 8.4 详情元信息（`workspace_main.rs` 详情面板、`pages/entry.rs` 全屏页）

展示：编码、创建人、创建时间、更新人、更新时间；已归档时加归档时间。时间按 Go 布局默认格式渲染。

### 8.5 `frontend/query_eval.rs` 同步

- 支持新 `Field`（Code / Title / Detail / CreatedBy / UpdatedBy）与新型比较。
- `frontend/graphql_client.rs` 的 `Entry` 加 `created_by_account` / `updated_by_account`、`LabelSchema` 加新属性，供客户端复算标题色。

## 9. 错误处理

- 类型属性非法（`multi` 用于非 Enum、时间布局非法、Currency 值非数、Email 格式错）：保存或打标签时拒，报 `InvalidQuery` / `InvalidLabelValue`。
- 表达式里键未知（含已被保留名遮住的旧标签）→ 现有「标签不存在」提示。
- 保留名冲突 → 明确的「与内置元数据重名」提示。

## 10. 测试策略

- **后端不写新单测**（用户明确）；以 `cargo build` + wasm `check` 为编译门。`domain/query.rs` 里**已存在**的单测需随签名与语法变更同步修改到能编译、能过。
- `golayout` 为纯函数（前后端各有调用），若时间允许加少量内联测试，否则以编译门为准。
- 端到端由用户在浏览器验收。

## 11. 破坏性变更与迁移

- `LabelSchema` / `LabelValue` 增字段与变体：bincode 旧数据不兼容 → 开发期 `rm -rf data`（沿用既有约定）。
- 表达式 `created` / `updated` 别名移除：手写表达式文本需改用 `CreatedAt` / `UpdatedAt`；已存视图（AST）不受影响。
- 与 7 个保留名重名的既有标签将无法用表达式查询，且不能再新建同名标签。

## 12. 后续留白

- Integer / Float 的数值展示格式与单位、String 长度范围。
- 完整数值格式语言（Currency 目前只认常见分组模式）。
- 把内置元数据也做成可选表格列 / 排序字段（本轮只进表达式与提示列表）。
