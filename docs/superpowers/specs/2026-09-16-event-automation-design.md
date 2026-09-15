# 标签事件自动化 设计文档

> 日期：2026-09-16
> 依据：`spec/20260915-补充设计.md`
> 状态：设计已与用户确认
> 关联：标签定义与查询表达式见 `2026-09-14-label-metadata-enhancements-design.md`

## 1. 背景与目标

标签写入目前是「一次请求一次落库」的终点。本轮把它变成事件源：任何标签变更都产生一个事件，
工作空间内可配置规则——**触发条件**（DSL，对事件求值）+ **触发动作**（圈定条目 → 写标签）。

典型用例（spec 原文）：`Status` 被设置为 `Finished` 时，自动给该条目打上 `FinishedAt = 当前时间`。

## 2. 范围与非目标

**范围**

- 事件：标签写入（`set_labeling` / `set_labelings` / `remove_labeling`）产生事件，内容固定无需配置。
- 规则：存储、校验、CRUD、GraphQL、设置页界面。
- 触发条件 DSL：在既有查询表达式文法上追加事件字段 `$label` / `$old` / `$new`。
- 执行引擎：规则动作写入与主写入**同批原子提交**，允许级联（限深度 3）。
- 审计：规则命中记 `RuleApplied`，被写入的标签照旧各记 `LabelingSet` / `LabelingRemoved`。

**非目标（本轮不做）**

- 标签 schema 定义本身作为事件源（spec 明确排除）。
- 异步 / 后台执行：规则在触发它的那次写请求内同步执行完毕。
- 定时触发、定时批量规则（`now()` 只是取值，不是触发器）。
- 动作写 `Entry` 的标题 / 详情 / 归档状态，以及新建条目——动作只写标签。
- 规则的启用区间（生效起止时间）、执行次数统计、失败重试。
- 动作值来源的表达式计算（如 `$old + 1`）——只支持字面量 / `now` / `$new` / `$old` 四种取值。
- 规则模板、导入导出、跨工作空间共享。

## 3. 事件模型（`src/domain/rule.rs`）

```rust
/// 一次标签变更。新增时 old 为 None，删除时 new 为 None。
pub struct LabelEvent {
    pub workspace_id: Ulid,
    pub entry_code: String,
    pub label_name: String,
    pub old: Option<LabelValue>,
    pub new: Option<LabelValue>,
    pub actor: Ulid,
    pub at: DateTime<Utc>,
    pub level: u8, // 0 = 用户直接写入；1..=3 = 规则动作产生的写入
}
```

事件源固定为三个写入路径，与 spec「除开定义标签 schema」一致：

| 写入路径 | 产生的事件 |
| --- | --- |
| `EntryService::set_labeling` | 1 个 |
| `EntryService::set_labelings` | 条目数 × 标签数 个 |
| `EntryService::remove_labeling` | 1 个 |
| 规则动作产生的写入 | 同样产生事件，`level = 父事件 + 1` |

规则动作产生的事件，`actor` 仍是**触发者**（规则不是主体，没有自己的身份）。

**只有 `old != new` 的事件参与规则求值**（`LabelValue` 全等比较）。用户在值没变的情况下重复写入，
照旧落库、照旧写审计——现有写入语义不变——但不触发规则。否则「重复保存 `Status = Finished`」
会反复刷新 `FinishedAt`，是个隐蔽的坑。

## 4. 规则模型

```rust
pub struct AutomationRule {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub name: String,
    pub enabled: bool,
    pub trigger: Query,         // 触发条件 AST（真相）；文本由 to_expr() 反推，不存副本
    pub action: RuleAction,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct RuleAction {
    pub target: ActionTarget,
    pub writes: Vec<LabelWrite>,
}

pub enum ActionTarget {
    EventSource,      // 事件源条目
    Query(Query),     // 标签表达式圈定条目（工作空间内）
}

pub struct LabelWrite {
    pub label_name: String,
    pub op: WriteOp,                // Set（涵盖新增/修改/upsert）/ Remove
    pub value: Option<ValueSource>, // Remove 时为 None，Set 时必填
}

pub enum ValueSource {
    Literal(serde_json::Value),
    Now,  // 当前时间，按目标标签 schema 的 format 渲染
    New,  // 事件的新值（$new）
    Old,  // 事件的旧值（$old）
}
```

`Set` 不区分「新增 / 修改 / upsert」——底层都是 `Labeling` 的覆盖写，行为一致，分三个 operation
只是给同一件事起三个名字。`Remove` 对应删除。

## 5. 存储

照 `ViewService` 的分法新增两个列族，键编码加进 `src/storage/keys.rs`：

| 列族 | 键 | 值 |
| --- | --- | --- |
| `cf::AUTOMATION_RULES` | `rule_key(id)` = 16 字节 Ulid | `bincode(AutomationRule)` |
| `cf::AUTOMATION_RULES_BY_WORKSPACE` | `rule_by_workspace_key(ws, id)` = 32 字节 | 空 |

两者都要注册进 `src/storage/rocksdb.rs` 的 `ALL_CFS`。

## 6. 触发条件 DSL

### 6.1 文法扩展

`Field` 枚举**末尾追加**三个变体（bincode 按变体序号编码，只能追加，末尾注释与
`AuditAction` 同款）：

```rust
pub enum Field {
    Label(String),
    UpdatedAt,
    CreatedAt,
    Text,
    // 追加在末尾：内置元数据
    Code,
    Title,
    Detail,
    CreatedBy,
    UpdatedBy,
    // 追加在末尾：事件字段，仅规则触发条件可用
    EventLabel, // $label
    EventOld,   // $old
    EventNew,   // $new
}
```

`Tok` 是私有枚举、不进库，可自由改动——新增 `Tok::EventField(Field)`，词法器识别
`$label` / `$old` / `$new`（大小写不敏感，与内置元数据一致）。`$` 不在标签 key 的字符集内
（大小写字母、数字、`_`、`-`），因此事件字段与标签名不可能撞名，`RESERVED_FIELDS`
不需要动。

### 6.2 语义

条件求值同时看得见**事件**与**事件源条目写入后的状态**：

| 条件 | 含义 |
| --- | --- |
| `$label = "Status"` | 本次变更的是 Status 标签 |
| `$new = "Finished"` | 新值是 Finished（与事件标签的 schema 同类型比较） |
| `!$new` | 删除事件（新的值为空） |
| `$old = "InProgress"` | 旧值是 InProgress |
| `!$old` | 新增事件（旧的值不存在） |
| `Priority >= 3` | 事件源条目写入后的 Priority 值满足条件 |

存在性沿用既有文法糖：标签名 / 事件字段单独出现即「存在」，前缀 `!` 即「不存在」
（现有实现里 `absent` 就不是关键字）。注意设计讨论中写过的 `$old ABSENT` 在现有文法里
对应 **`!$old`**，以本文为准。

`$old` / `$new` 与字面量比较时，按**事件标签自己的 schema** 决定比较方式：时间型标签
（Date / Time / DateTime）先按 schema 的 `format`（缺省 `default_layout`）解析成时刻再比，
复用 `Condition::evaluate` 里 `Field::Label` 已有的那套逻辑；其余类型直接比 JSON 标量。

### 6.3 求值入口

事件挂在 `EvalEnv` 上，与既有三个依赖并列：

```rust
pub struct EvalEnv<'a> {
    pub text_hit: &'a dyn Fn(&str) -> bool,
    pub account_of: &'a dyn Fn(Ulid) -> Option<(String, String)>,
    pub label_of: &'a dyn Fn(&str) -> Option<(LabelValueType, Option<String>)>,
    // 追加：当前事件。视图查询为 None。
    pub event: Option<&'a LabelEvent>,
}
```

`event` 为 `None` 时事件字段恒为假——但这不该发生：**用事件字段的位置一律在保存时被拒**
（下节），否则会出现一条永远不触发的规则或永远不匹配的视图。视图 / 标题色规则走的
`Query::validate(schemas)` 一并加上「拒绝事件字段」的检查。

### 6.4 校验

`Query` 上分两个入口，避免三种上下文各写一套判断：

| 入口 | 用于 | 规则 |
| --- | --- | --- |
| `validate(schemas)` | 视图、标题色规则 | 既有规则 + **拒绝事件字段** |
| `validate_for_rule(schemas, allow_event)` | 规则触发条件（`true`）、规则目标表达式（`false`） | 既有规则 + **拒绝 `Text`** + 按 `allow_event` 决定是否允许事件字段 |

规则保存时（`RuleService` 的 validate）逐条校验，任一不过即拒绝保存：

1. `name` 非空。
2. 触发条件：`Query::parse` 成功 → `validate_for_rule(schemas, true)` 通过。
3. 动作目标为 `Query` 时：`validate_for_rule(schemas, false)` 通过。
4. 每个 `LabelWrite.label_name` 必须在 schema 里存在。
5. `op = Remove` 时 `value` 必须为空；`op = Set` 时 `value` 必填。
6. `ValueSource::Literal` 的值用 `LabelValue::from_json(literal, schema)` 校验——字面量不合法
   在**保存时**就报错，不留到运行时。`Now` / `New` / `Old` 无法在保存时静态校验类型，
   运行时校验（见 §8）。

禁止 `Text` 的理由：事件场景没有 tantivy 命中集、目标过滤也不走全文检索，`text` 恒为假，
规则静默不触发比报错更糟。已有 `Query::contains_text()` 可判定。

不强制要求触发条件引用事件字段：只写 `Priority >= 3` 是合法的「这些条目的任何标签变更都触发」，
界面提示但不阻止。

## 7. 执行引擎（`src/service/rule.rs` 的 `RuleEngine`）

### 7.1 分层批处理

引擎按**层（level）**推进，而不是逐个事件推进。层内 overlay 冻结，层末统一应用：

```
level 0  用户写入 → 暂存；应用 → overlay0；事件 events0 = 每条 (entry,label) 的 before→after 差
for d in 1..=3:
    用 overlay(d-1) 求值：哪些规则命中 events(d-1) 中的事件
    命中的规则按 id 升序（Ulid 时间有序 = 创建顺序），依次计算目标集合与待写值，暂存
    暂存写入应用到 overlay(d-1) → overlay(d)，并按 (entry,label) 汇总出 events(d)
    若 events(d) 为空则跳出
```

分层的三个好处：层内语义确定（同一层的事件看到同一份前像）；同一 `(entry, label)` 在一层内
只产生一个事件（多次写入合并成 before→after 一次差）；目标表达式在层内可缓存，
避免「事件数 × 规则数 × 全表扫描」的乘积。

三层上限即 spec 的「限深度」：最多 3 轮规则动作（level 1/2/3），到顶后不再继续，静默截断，
审计里如实记录 level。

### 7.2 后置状态 overlay

触发条件要看「写入后的状态」，但此刻还没提交。引擎维护内存 overlay
（`entry_code → Vec<Labeling>`，从 `cf::LABELINGS_BY_WORKSPACE` 播种，随暂存写入更新），
求值一律走 overlay。标签写入不动 `Entry` 行本身，所以条目元数据（Code / Title / Detail /
创建与更新时间）直接读库即可，不存在前像问题。

关键实现约束：**不能复用 `EntryService::query`**——它读已提交状态、且会做全文检索。
引擎自己过滤：取候选条目（复用 §7.5 的读法）+ overlay 里的标签 + `Query::evaluate`，
`EvalEnv` 的四个成员这样给：

- `text_hit`：恒 `false`（校验已禁止 `text`，走不到）。
- `account_of`：与 `EntryService::query` 同样的账号表读法——只在条件真的引用
  `CreatedBy` / `UpdatedBy` 时扫一次 `cf::ACCOUNTS`，否则给空表。
- `label_of`：本工作空间的 `LabelSchema`（扫 `cf::LABEL_SCHEMAS` 前缀）。
- `event`：当前事件。

`now` 在一次请求内只取一次时刻，同批所有 `Now` 取值一致。

### 7.3 去重与顺序

- 规则动作要写的值与该 `(entry, label)` 在 overlay 里的当前值相等（`LabelValue` 全等）→
  丢弃该写入，不写审计、不产生事件。这是级联的安全阀：`A→B`、`B→A` 两条规则靠它自然收敛。
  用户直接写入**不**去重（见 §3）。
- 同一层内多条规则写同一个 `(entry, label)`：按规则 id 升序依次写入，后者覆盖前者。
  审计如实记录最终值与产出它的规则。

### 7.4 原子性

主写入、全部规则动作、全部审计 → 攒成**一个** `write_batch`。任一步失败（`$new` / `$old` 与目标
标签类型不兼容、按 schema 渲染 `now` 失败等）整体回滚，把可读错误返回给用户，库中不留半成品。
提交后再对受影响条目 reindex，每个条目一次。

### 7.5 接线（不改 `EntryService` 的公开签名）

`RuleEngine` 只依赖 `DocStore`，不依赖 `EntryService`——避免循环依赖。`EntryService` 内部
持有 `RuleEngine`，在 `with_search` / `new` 里自行构造（签名不变，既有调用点零改动）。

写标签的三个方法改为：构造用户写入的 ops → `engine.plan(ws, &staged)` → 拼接
`plan.ops` → 一次 `write_batch` → 按 `plan.affected ∪ 用户写入条目` reindex。

`RuleEngine` 需要读工作空间条目，读法（扫 `ENTRIES_BY_WORKSPACE` 后逐条取 `ENTRIES`，
排除已删除与已归档）与 `EntryService::list` 相同。把它抽成 `service/entry.rs` 的自由函数
`list_entries(store, ws)`，`EntryService::list` 改为薄封装，`RuleEngine` 直接调——两处共用，
不复制一遍。目标圈定的口径因此与视图列表一致（不含已删除、不含已归档）。

`Services` 增加 `pub rule: RuleService`（规则 CRUD 与校验，供 GraphQL 层用）。

### 7.6 已知成本

规则求值是同步的：一次写请求的耗时 = 主写入 + 规则求值 + 动作写入。目标表达式圈定
`Query::all()` 的规则在层内要扫一遍工作空间条目。v1 按「每个工作空间规则数 ≤ 50」的规模
设计，不做增量索引；规则数量成为瓶颈时再谈。

## 8. 错误处理

`AppError` 末尾追加一个变体（该枚举不落库，末尾追加同样兼容存量）：

```rust
#[error("{0}")]
RuleFailed(String), // code: "RULE_FAILED"
```

- 规则保存校验失败 → `InvalidQuery(具体原因)`（沿用现有「面向用户、原样展示」的语义）。
- 运行时动作失败（`$new` / `$old` 与目标标签类型不兼容、按 schema 渲染 `now` 失败等）→
  `RuleFailed("规则「X」写标签「Y」失败：…")`，**整个批次回滚**，主写入一并失败。
- 目标表达式圈定 0 条 → 正常，不报错；`RuleApplied` 审计记 0 条。
- 规则被禁用 / 触发条件无事件字段 / 条件不命中 → 正常路径，无错误。
- 标签 schema 目前没有删除入口，类型也不可改（`update_label_schema` 保留库中已有类型），
  因此「规则引用了已删标签」这条路径当前不存在，不做防御。

## 9. 审计

追加 `AuditAction::RuleApplied`（同样只能追加在枚举末尾，见 `src/domain/audit.rs` 的注释）。每条
(规则, 层) 命中写一条，避免 R×N 条刷屏：

- `resource_type = "rule"`，`resource_id = rule.id`
- `after` = `{rule_id, rule_name, level, triggers: [{entry_code, label_name, old, new}], writes: [{entry_code, label_name, old, new}]}`
- `before` = `None`

被规则写入的标签**仍各写自己的** `LabelingSet` / `LabelingRemoved` 审计（`actor` = 触发者），
保住「标签变更必有审计」这个不变式。`RuleApplied` 负责解释「为什么会有这些写入」——
两个记录合起来，审计页能读出「因为张三把 Status 设为 Finished，规则 X 把 FinishedAt 设为 now」。

## 10. GraphQL（`src/api/graphql.rs`）

**查询**

- `automationRules(workspaceId: ID!): [GqlAutomationRule!]!` — 成员可读。
  `GqlAutomationRule { id, name, enabled, triggerExpr, targetEventSource, targetExpr, writes, createdAt, updatedAt }`，
  其中 `triggerExpr` / `targetExpr` 由 AST `to_expr()` 反推下发。
- `parseRuleTrigger(workspaceId: ID!, expr: String!): JSON!` — 解析 + 按规则规则校验并返回 AST，
  供编辑器实时校验。镜像既有 `parseViewQuery`，解析器实现只有一份。

**变更**（均 Maintainer+）

- `createAutomationRule(workspaceId: ID!, name: String!, enabled: Boolean!, triggerExpr: String!,
  targetEventSource: Boolean!, targetExpr: String, writes: [RuleWriteInput!]!): GqlAutomationRule!`
- `updateAutomationRule(id: ID!, name: String!, enabled: Boolean!, triggerExpr: String!,
  targetEventSource: Boolean!, targetExpr: String, writes: [RuleWriteInput!]!): GqlAutomationRule!`
- `deleteAutomationRule(id: ID!): Boolean!`

`input RuleWriteInput { labelName: String!, op: String!, valueKind: String!, value: JSON }`，
`op ∈ set|remove`，`valueKind ∈ literal|now|new|old`。`targetEventSource = true` 时忽略 `targetExpr`。

## 11. 前端

### 11.1 设置页新增「自动化」标签页（`src/frontend/pages/settings.rs`）

- 侧栏加一项「自动化」（`tab = "automation"`）。
- 规则列表：名称 / 触发条件文本 / 动作摘要 / 启用开关 / 编辑 / 删除。非 Maintainer 只读。
- 编辑表单（弹窗或内联，沿用 `.dmodal` / `.dmbox`）：
  - 名称、启用开关。
  - 触发条件：文本框 + 标签插入下拉 + 「校验」按钮（调 `parseRuleTrigger`），
    文本框下方常驻一行 `$label` / `$old` / `$new` 的说明与可点击插入。
    **不做** `/` 补全与时间控件——那套表达式编辑器内联在 `workspace_main.rs` 的页面组件里，
    抽成公共组件是大重构，本轮不动。
  - 目标：单选「事件源条目 / 表达式圈定」，选后者时出现表达式输入框（同样带校验按钮）。
  - 动作：可增删的行，每行 = 标签下拉（来自 `labelSchemas`）+ 操作（set/remove）+
    值来源下拉（字面量/当前时间/事件新值/事件旧值）+ 值输入（仅字面量时出现）。

### 11.2 审计页

`src/frontend/components.rs` 的 `action_label()` 增加一行 `"RuleApplied" => "规则触发"`。
`audit_change()` 对规则快照给出一句话摘要（「规则「X」写入 2 条标签」），新增 action 默认落到
`_ => "变更"`，所以不补也不崩。

### 11.3 既有页面

标签写入后列表本来就会重查，规则动作的效果随之可见，无需额外机制。规则动作不改变
`Entry.updated_at`（标签写入不动 Entry 行），这与现有打标行为一致。

## 12. 权限

| 操作 | 要求 |
| --- | --- |
| 读规则（`automationRules` / `parseRuleTrigger`） | 成员（`require_member`），与 `labelSchemas` 一致 |
| 增删改规则 | Maintainer+（与标签定义一致） |
| 触发执行 | 无额外校验，以**触发者身份**写入 |

规则动作可以写「触发者本人不会去写」的标签（规则由 Maintainer 授权，Worker 的一次打标
引发规则动作是预期行为）。规则动作只写同一个工作空间内、触发者已有 Worker 权限的条目，
不越过工作空间边界。

## 13. 测试策略

- **编译门禁**：`cargo build` + wasm `check`。
- **破例单测**（用户明确同意，仅此一处）：`src/service/rule.rs` 的 `#[cfg(test)] mod tests`，
  覆盖四块纯逻辑——分层级的后置状态求值、级联到深度上限后停止、`A→B`/`B→A` 靠去重收敛、
  同层同 `(entry,label)` 的合并与覆盖顺序。其余模块不新增单测。
- **浏览器验收**（用户实测）：
  1. 打 `Status = Finished` → 自动出现 `FinishedAt = 当前时间`；
  2. 从 `InProgress` 改成 `Finished` 触发、重复写入 `Finished` 不触发；
  3. 三级级联到顶后停止；
  4. `A→B`、`B→A` 两条规则跑完不死循环；
  5. 目标用表达式圈定多条条目，动作批量落到每条；
  6. `remove` 动作能把标签删掉；
  7. 审计页出现「规则触发」，且被规则写入的标签各有自己的「设置标签」记录；
  8. 规则保存时非法表达式 / 不存在的标签被拒绝，错误信息可读；
  9. 控制台无 page error。

## 14. 破坏性变更

无。新列族 + 新枚举变体（均在末尾追加）+ 新增 GraphQL 字段与变更，全部增量。
`Field` 追加变体对存量视图的 bincode 解码无影响（变体序号只增不改）。

## 15. 待确认 / 后续留白

- 规则数量上限（§7.6 假设 ≤ 50）与目标表达式全表扫描的性能，留待实测后决定是否要索引。
- 异步执行 / 失败重试 / 规则执行统计的留白。
- 规则命中失败时目前整体回滚主写入：若某个规则长期配置错误，会连带阻断正常的打标操作。
  缓解手段是保存时严格校验（§6.3）与启用开关，本轮不做「跳过失败规则继续提交」的模式。
- `now` 的取值时刻是**规则求值时刻**（一次请求内所有 `now` 取同一个时刻，保证同批一致）。
