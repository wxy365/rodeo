//! 标签表达式语法与求值。
//!
//! 本模块是 rodeo 的「标签表达式」核心：定义 AST、词法/语法分析器、求值引擎，
//! 同一份类型前后端共用（`frontend::query_eval` 复算用于过滤本地缓存）。
//!
//! ## 一、语法速查
//!
//! 顶层文法（`b_expr` 是布尔表达式，`c_expr` 是单条条件，`f` 是字段，`op` 是运算符）：
//!
//! ```text
//! query     := or_expr
//! or_expr   := and_expr ( OR and_expr )*
//! and_expr  := unary    ( AND unary )*
//! unary     := NOT unary | '!' primary | primary
//! primary   := '(' query ')' | c_expr
//! c_expr    := f op scalar
//!             | f op '(' scalar (',' scalar)* ')'
//!             | f                                  // 标签 / 事件字段单独出现 = present
//!             | f 'changed' [FROM str] [TO str]    // 标签值历史变更
//! op        := '=' | '!=' | '>' | '>=' | '<' | '<=' | '~' | '!~'
//!             | 'in' | 'not' 'in' | 'has' | 'is' | 'changed'
//! f         := builtin | '$' event | ident ( '+' )?
//! builtin   := 'Code' | 'Title' | 'Detail' | 'CreatedBy' | 'CreatedAt'
//!             | 'UpdatedBy' | 'UpdatedAt' | 'text'
//! event     := 'label' | 'old' | 'new'
//! scalar    := '"' chars '"' | "'" chars "'" | number | 'true' | 'false' | ident
//! ```
//!
//! 顶层 `AND`/`OR` 关键字**必须大写**（大小写不敏感时词法器会优先命中 `and`/`or`）；
//! `not`/`in`/`has`/`is`/`changed`/`text`/`true`/`false` 大小写不敏感。
//!
//! ## 二、字段表
//!
//! | 字段语法 | 语义 | 适用 Op |
//! |---|---|---|
//! | `LabelName` | 当前 entry 上的标签 `LabelName` 是否被直接打标 / 比较值 | 全部（依赖 schema 的 `valueType` / `multi`） |
//! | `LabelName+` | 同上，但**包含**继承关联（key 继承也命中） | 同上 |
//! | `Code` / `Title` / `Detail` / `CreatedBy` / `UpdatedBy` | 内置元数据字段 | `=` `!=` `~` `!~` `in` `not in` |
//! | `CreatedAt` / `UpdatedAt` | 内置时间字段 | `=` `!=` `>` `>=` `<` `<=`（值是 RFC3339 或 `YYYY-MM-DD[ HH:MM[:SS]]` 或 `HH:MM[:SS]`） |
//! | `text` | 全文检索命中（tantivy） | 仅 `~` |
//! | `$label` / `$old` / `$new` | 自动化规则触发事件（**仅规则**；视图中恒假） | 同内置元数据 + 时间型标签的对应 Op |
//!
//! ## 三、运算符表
//!
//! | 语法 | Op | 适用值类型 | 语义 |
//! |---|---|---|---|
//! | `=` | `Eq` | 全部非 Null | 数值优先走 `f64` 比较；其余按标量字符串集合等值（数组值任一元素命中即真） |
//! | `!=` | `Ne` | 同 `=` | `Eq` 取反 |
//! | `>` `>=` `<` `<=` | `Gt`/`Ge`/`Lt`/`Le` | Integer / Float / Currency / Date / Time / DateTime；String（仅字典序，按 &str 序） | 数值走 `f64`；时间按各自布局解析成 (Y, M, D, h, m, s) 元组比较 |
//! | `~` | `Contains` | String / Email / Enum / 全文 | 大小写不敏感子串匹配 |
//! | `!~` | `NotContains` | 同 `~` | `Contains` 取反 |
//! | `in` | `In` | 同 `~` 适用类型（Enum 候选须在 `enum_values` 内） | 值落在右值数组中 |
//! | `not in` | `NotIn` | 同 `in` | `In` 取反 |
//! | `has` | `SetContains` | **多值**标签（Account / Enum） | 标签值集合**包含**右值集合的每个元素 |
//! | `is` | `SetEquals` | **多值**标签 | 标签值集合**等于**右值集合 |
//! | `present()` / `absent()` | — | — | **已移除**——单独写 `Label` 即「存在」；前缀 `!Label` 即「不存在」 |
//! | `Label` | `Present` | 标签 / 事件字段 | 「该标签存在」——继承 / 直接打标任一命中即真（继承按 `+` 后缀决定） |
//! | `!Label` | `Absent` | 同 `Present` | `Present` 取反 |
//! | `Label changed` | `ChangedBetween` | 标签 | 区间内至少一次 `LabelingSet` / `LabelingRemoved` 事件 |
//! | `Label changed from "X" to "Y"` | `NetChangedIn` | 标签 | 闭区间 `[from, to]` 内首末状态对比发生变化；`from` 与 `to` 都必填 |
//!
//! ## 四、值字面量
//!
//! - **字符串**：`"…"` 或 `'…'`，支持 `\"` / `\'` / `\\` 转义。Enum 值允许省略引号（裸标识符），由 `expr_scalar` 反向序列化时统一加引号。
//! - **数字**：可选负号 + 十进制 + 小数点；底层 `f64`，整数与浮点共用同一比较路径。
//! - **布尔**：`true` / `false`（大小写不敏感）。
//! - **数组**：`in`/`not in`/`has`/`is` 的右值必须是 `(a, b, c)` 形式的元组——数组里元素按 `scalar` 文法解析。
//! - **`{from, to}` 对象**：`changed` 的右值是 keyword 形式，**不**用括号，由 `parse_change_window` 按 `from "X"` / `to "Y"` 顺序读取；缺省端点视作「无限早 / 无限远」。
//!
//! ## 五、典型用法
//!
//! ```text
//! Task = "Open" AND Priority >= 3
//! (Status = "Doing" OR Status = "Review") AND NOT !Priority
//! Assignee has ("alice@x.io", "bob@x.io")
//! Tags is ("frontend", "p0")
//! text ~ "登录"
//! CreatedAt >= "2026-09-01" AND CreatedAt < "2026-10-01"
//! Priority+ >= 3                       // 含继承
//! Task changed from "Open" to "Done"   // 区间内值发生过净变化
//! ```
//!
//! ## 六、保留关键字
//!
//! 词法器把下列 token 视为运算符 / 内置字段 / 字面量，**不可**作为标签名：
//! `and` `or` `not` `in` `has` `is` `changed` `text` `true` `false`
//! `Code` `Title` `Detail` `CreatedBy` `CreatedAt` `UpdatedBy` `UpdatedAt`
//! `$label` `$old` `$new`（以及任何 `$xxx` 形式）

use std::collections::{BTreeSet, HashSet};

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::domain::{
    DerivedLabel, Entry, LabelEvent, LabelSchema, LabelValue, LabelValueType, Labeling,
};
use crate::error::AppError;

/// 标签表达式的根节点。
///
/// `And` / `Or` 的子节点可为空（`Vec::new()`）：空 `And` 恒真，空 `Or` 恒假，
/// 与布尔代数的幺元 / 零元语义一致。`Not` 的内部必须是 `Query`，不允许重复否定
/// ——`parse_unary` 会把连续的 `NOT` 折叠成一层。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Query {
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    Cond(Condition),
}

/// 单条比较条件：`<field> <op> <value>`。
///
/// `value` 的 JSON 形状由 `op` 决定：
/// - 标量运算符（`=`/`!=`/`>`/`<`/…）：`Some(json_scalar)`
/// - 存在性（`Present`/`Absent`）：`None`
/// - 列表运算符（`in`/`not in`/`has`/`is`）：`Some(json_array)`
/// - `ChangedBetween`：`Some({from: Option<rfc3339>, to: Option<rfc3339>})`，两端可空
/// - `NetChangedIn`：`Some({from: rfc3339, to: rfc3339})`，两端都必填
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub field: Field,
    pub op: Op,
    pub value: Option<serde_json::Value>,
}

/// 表达式左侧的「字段」节点。
///
/// 语法映射：
/// - `LabelName`           → `Field::Label(s)`
/// - `LabelName+`          → `Field::LabelInherited(s)`（带后缀 `+`，含继承关联）
/// - `Code` / `Title` / `Detail` / `CreatedBy` / `UpdatedBy` / `CreatedAt` / `UpdatedAt`
///                         → 对应 builtin 变体
/// - `text`                → `Field::Text`
/// - `$label` / `$old` / `$new` → `Field::EventLabel` / `EventOld` / `EventNew`（仅规则）
///
/// `validate` 会按 `field` 与 `op` 的组合拒绝非法用法（如 `Text` 不接 `>`、事件字段出现在视图中）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    /// 直接打上的标签。语法：`LabelName`。
    ///
    /// 适用 Op 取决于 schema：`present` / `absent` 总是合法；
    /// 比较型 Op 由 `op_allowed(value_type, op, multi)` 决定（数字 / 时间 / String / Email / Enum / Account）。
    Label(String),
    /// entry 最近一次更新时间。语法：`UpdatedAt`，值是 RFC3339 或 `YYYY-MM-DD[ HH:MM[:SS]]` 或 `HH:MM[:SS]`。
    ///
    /// 适用 Op：`=` `!=` `>` `>=` `<` `<=`。
    UpdatedAt,
    /// entry 创建时间。语法：`CreatedAt`，同 `UpdatedAt`。
    CreatedAt,
    /// 全文检索命中判定（tantivy 提供）。语法：`text`，**只**接 `~ scalar`。
    ///
    /// 与 `String` 标签的 `~` 不同：`text ~` 命中由 `EvalEnv::text_hit` 闭包决定，
    /// 不是子串匹配。一条表达式里至多一个全文条件（见 `collect_text_keywords`）。
    Text,
    /// entry 的全局唯一编码。语法：`Code`。
    Code,
    /// entry 的标题。语法：`Title`，适用 `~` / `!~` 子串匹配。
    Title,
    /// entry 的富文本详情。语法：`Detail`，适用 `~` / `!~`。
    Detail,
    /// entry 创建者的 account。语法：`CreatedBy`，值与 `(name, email)` 任一大小写不敏感相等即命中。
    CreatedBy,
    /// entry 最近一次修改者的 account。语法：`UpdatedBy`，同 `CreatedBy`。
    UpdatedBy,
    /// 自动化规则触发事件的「被变更标签名」。语法：`$label`，**仅规则**；视图中恒假。
    EventLabel,
    /// 自动化规则触发事件的「旧值」。语法：`$old`，**仅规则**。
    ///
    /// 时间型标签的 `=`/`!=`/范围比较按被变更标签的 `value_type` + `format` 布局解析；
    /// Account 类型走 `acc_set_op`；其余按 `cmp_value`。
    EventOld,
    /// 自动化规则触发事件的「新值」。语法：`$new`，语义同 `$old`。
    EventNew,
    /// 含继承关联的标签。语法：`LabelName+`（后缀 `+`）。
    ///
    /// 与 `Label` 的区别是：当 entry 没有直接打该标签时，**继承推导**出的关联（来自
    /// `InheritanceGraph::derive`）也算命中。`+` 仅对标签有效，内置元数据接 `+` 会被 `parse_condition` 拒绝。
    LabelInherited(String),
}

impl Field {
    /// 标签字段 → (标签名, 是否连继承关联一起算)。非标签字段返回 `None`。
    pub fn label_of(&self) -> Option<(&str, bool)> {
        match self {
            Field::Label(n) => Some((n, false)),
            Field::LabelInherited(n) => Some((n, true)),
            _ => None,
        }
    }
}

/// 内置元数据关键字（大小写不敏感）。供词法器与标签保留名校验共用。
pub const RESERVED_FIELDS: [&str; 7] = [
    "Code",
    "Title",
    "Detail",
    "CreatedBy",
    "CreatedAt",
    "UpdatedBy",
    "UpdatedAt",
];

/// 可被 `LABELINGS_BY_LABEL` 二级索引加速的标签条件。
#[derive(Debug, Clone)]
pub struct IndexHint {
    pub label_name: String,
    pub op: Op,
    /// `Eq`/`Ne`/`Gt`/`Ge`/`Lt`/`Le` 是单值；`In`/`NotIn` 是 JSON 数组。
    pub value: serde_json::Value,
}

/// 顶层为 `And` 时挑出全部可被索引加速的 Cond；其他形状（`Or`/`Not`/嵌套）返回空。
/// 继承标签（`LabelInherited`，语法上写为 `L4+`）与 `Contains`/`NotContains`/
/// `Present`/`Absent` 都不索引——前者必须先做继承推导，后者没有等值语义。
/// 限制在 `And` 顶层的原因：`Or`/`Not` 的集合运算成本大于收益，宁可走全扫。
///
/// Account 类型跳过所有 hint：存储是 ULID，want 是名字/邮箱，
/// 索引字节序比较拿不到「邮箱命中」那种语义；全扫路径由 `acc_set_op` /
/// eval-side account 比较正确处理。schema 缺失也跳过——
/// 让该 hint 在 `codes_for_hint` 走原有的「schema 找不到即跳过」分支，
/// 与未改动前的行为一致。
pub fn extract_index_hints(query: &Query, schemas: &[LabelSchema]) -> Vec<IndexHint> {
    let Query::And(children) = query else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for child in children {
        let Query::Cond(cond) = child else {
            continue;
        };
        let Field::Label(name) = &cond.field else {
            continue;
        };
        let Some(value) = &cond.value else {
            continue;
        };
        let Some(schema) = schemas.iter().find(|s| &s.name == name) else {
            continue;
        };
        // 账号标签跳过：候选全集交给全扫的 `acc_set_op`（或将来的 account-aware
        // Eq/Ne/In/NotIn）去走 ULID ↔ name/email 翻译，索引帮不上忙。
        if schema.value_type == LabelValueType::Account {
            continue;
        }
        // 字符串标签的范围扫描不走索引：`encode_label_value_for_index` 用 4 字节
        // 长度前缀 + UTF-8 编码，长度前缀的字节序不等于字符串字典序——
        // 例如「z」编码为 `[0,0,0,1,'z']` 小于「aa」编码的 `[0,0,0,2,'a','a']`，
        // 跨长度比较会得出错误候选。等值（Eq / Ne / NotIn）不受影响，因为它们走
        // 严格等值 scan_prefix，不比编码长度。
        if schema.value_type == LabelValueType::String
            && matches!(cond.op, Op::Gt | Op::Ge | Op::Lt | Op::Le)
        {
            continue;
        }
        match cond.op {
            Op::Eq | Op::Ne | Op::Gt | Op::Ge | Op::Lt | Op::Le => {
                out.push(IndexHint {
                    label_name: name.clone(),
                    op: cond.op,
                    value: value.clone(),
                });
            }
            Op::In | Op::NotIn if value.is_array() => {
                out.push(IndexHint {
                    label_name: name.clone(),
                    op: cond.op,
                    value: value.clone(),
                });
            }
            // 多值「包含」= 标签值集合里包含右值集合的每个元素；对单值右值来说
            // 退化成「列表里有 X」，可被 (ws, name, value=X) 的 LABELINGS_BY_LABEL 索引命中。
            // 多值「等于」需要的是「所有候选都命中」=多次索引 + 集合交，暂不索引。
            Op::SetContains => {
                out.push(IndexHint {
                    label_name: name.clone(),
                    op: Op::Eq,
                    value: value.clone(),
                });
            }
            _ => {}
        }
    }
    out
}

/// 表达式运算符。语法形式由 `parse_condition` / `parse_unary` 提供。
///
/// 适用域：
/// - `Present` / `Absent`：所有字段（标签 / 事件字段 / 内置元数据）。
/// - `Eq` / `Ne` / `In` / `NotIn` / `Contains` / `NotContains`：所有非 Null 字段
///   （具体可用的字段类型由 `op_allowed` / `string_field_op_allowed` 控制）。
/// - `Gt` / `Ge` / `Lt` / `Le`：数字、货币、时间型标签，以及字符串（按字典序）。
/// - `SetContains` / `SetEquals`：**多值**标签（Account / Enum）的集合语义，单值不可用。
/// - `Changed` / `ChangedValue`：仅标签字段，依赖 `EvalEnv::changed_during` /
///   `changed_value` 闭包；前端 wasm 闭包为 `None`，所以 wasm 端求值恒假。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    /// 「存在」判断。语法：字段名单独出现，或 `field` 不带运算符。
    ///
    /// 例：`Task` 等价于旧的 `present(Task)`。标签 / 事件字段 / 继承标签均可。
    /// 标签条件下：`direct`（直接打标）或 `derived`（继承标签 + 后缀 `+`）任一存在即真。
    Present,
    /// 「不存在」判断。语法：`!Field`（前缀 `!` 是 `Absent` 的语法糖）。
    Absent,
    /// 等值比较。语法：`=`。
    ///
    /// 数字优先走 `f64` 比较；其余按「标量字符串集合等值」——数组值任一元素命中即真。
    /// 账号类型走 `acc_set_op`：want 与存储值的 (name, email) 任一相等。
    Eq,
    /// 不等比较。语法：`!=`。`Eq` 取反。
    Ne,
    /// 「落在集合内」判断。语法：`in (a, b, c)`。
    ///
    /// 数组值任一元素命中候选即真；Enum 候选必须在 `schema.enum_values` 内；候选是数字时按 `f64` 比。
    In,
    /// `In` 取反。语法：`not in (a, b, c)`。
    NotIn,
    /// 大于。语法：`>`。数值按 `f64`；时间按布局解析后比较；字符串按字典序（&str 序）。
    Gt,
    /// 大于等于。语法：`>=`。
    Ge,
    /// 小于。语法：`<`。
    Lt,
    /// 小于等于。语法：`<=`。
    Le,
    /// 子串匹配。语法：`~`。
    ///
    /// 大小写不敏感；非字符串标量（数字 / 布尔 / null）直接返回 false；
    /// 数组值按 `elem_strings` 展平后任一元素命中 needle 即真。
    Contains,
    /// `Contains` 取反。语法：`!~`。注意：非字符串标量也返回 false，**不**是恒真。
    NotContains,
    /// 集合「包含」语义。语法：`has (a, b, c)`，仅**多值**标签。
    ///
    /// 标签值集合**包含**右值集合的每个元素（want 是空 → false）。
    /// 账号类型按 (name, email) 比；Enum 按标量字符串集合比。
    SetContains,
    /// 集合「等于」语义。语法：`is (a, b, c)`，仅**多值**标签。
    ///
    /// 两侧集合相等（包括空 want → 仅当标签值集合也为空时为真）。
    SetEquals,
    /// 标签值发生任意一次变更（可选时间窗）。语法：`field changed [during (T1, T2)]`。
    ///
    /// 仅标签字段；`value` 形状是 `{"during": Option<Interval>}`（无 `during` 时为 `null`）。
    /// `during` 缺省视作「任意时间」= 整个时间轴的区间 `(-∞, +∞)`。
    /// 审计数据由 `EvalEnv::changed_during` 注入；闭包为 `None` 时恒假。
    Changed,
    /// 标签值发生指定变更（from → to），可选时间窗。语法：
    /// `field changed from V1 to V2 [during (T1, T2)]`。
    ///
    /// 仅标签字段；`value` 形状是 `{"from": Value, "to": Value, "during": Option<Interval>}`。
    /// from/to 是 JSON 值（string / number / boolean / null），按 JSON 等值匹配。
    /// 审计事件 `at` 必须落在 `during` 区间内（无 `during` 时任意时间）。
    /// 审计数据由 `EvalEnv::changed_value` 注入；闭包为 `None` 时恒假。
    ChangedValue,
}

/// 标签值历史变更运算符的「时间区间」参数。区间记号：
/// `(T1, T2)` / `[T1, T2]` / `(T1, T2]` / `[T1, T2)`，
/// 端点可以是 RFC3339 字符串、`-`（-∞）、或 `+`（+∞）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Interval {
    /// 区间起点：(端点, 端点开闭)。
    pub start: (IntervalBound, BoundKind),
    /// 区间终点：(端点, 端点开闭)。
    pub end: (IntervalBound, BoundKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntervalBound {
    /// 解析后的时刻（来自 `parse_time`）。
    Time(DateTime<Utc>),
    /// 负无穷。语法：`-`（仅在 `during (...)` 内的端点位出现）。
    NegInfinity,
    /// 正无穷。语法：`+`（仅在 `during (...)` 内的端点位出现）。
    PosInfinity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BoundKind {
    /// 端点**不在**区间内。语法：`-∞` / `+∞` 端点恒为开（语义上不可达），
    /// 时间端点写作 `(` 或 `)`。
    Open,
    /// 端点**在**区间内。时间端点写作 `[` 或 `]`。
    Closed,
}

impl Interval {
    /// 「任意时间」区间：开区间 `(-∞, +∞)`。无 `during` 子句时使用。
    pub fn unbounded() -> Self {
        Self {
            start: (IntervalBound::NegInfinity, BoundKind::Open),
            end: (IntervalBound::PosInfinity, BoundKind::Open),
        }
    }

    /// 审计事件 `at` 是否落在本区间内。
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        let start_ok = match &self.start.0 {
            IntervalBound::Time(t) => match self.start.1 {
                BoundKind::Open => at > *t,
                BoundKind::Closed => at >= *t,
            },
            // -∞ 端点：开闭不影响可达性，闭区间端点也命中所有有效时刻。
            IntervalBound::NegInfinity => true,
            IntervalBound::PosInfinity => unreachable!("PosInfinity 不可作为 start"),
        };
        if !start_ok {
            return false;
        }
        match &self.end.0 {
            IntervalBound::Time(t) => match self.end.1 {
                BoundKind::Open => at < *t,
                BoundKind::Closed => at <= *t,
            },
            IntervalBound::PosInfinity => true,
            IntervalBound::NegInfinity => unreachable!("NegInfinity 不可作为 end"),
        }
    }
}

impl Query {
    pub fn all() -> Query {
        Query::And(Vec::new())
    }

    pub fn contains_label(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_label),
            Query::Not(q) => q.contains_label(),
            Query::Cond(c) => c.field.label_of().is_some(),
        }
    }

    /// 整棵树里是否有 `L4+` 这类「含继承」条件——没有就不必算继承推导。
    pub fn contains_inherited_label(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_inherited_label),
            Query::Not(q) => q.contains_inherited_label(),
            Query::Cond(c) => matches!(c.field, Field::LabelInherited(_)),
        }
    }

    /// 表达式里是否出现 `Label changed [from V1 to V2] [during (T1, T2)]` 这类历史变更条件。
    /// `EntryService::query` 据此决定是否扫审计——没有就不付出「全 workspace 审计前缀扫描」的代价。
    pub fn contains_label_change(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_label_change),
            Query::Not(q) => q.contains_label_change(),
            Query::Cond(c) => matches!(c.op, Op::Changed | Op::ChangedValue),
        }
    }

    /// 表达式里以 `+` 引用的标签名（去重，保持出现顺序）。
    /// 视图的标签列据此把这些标签也带上——否则纯靠继承命中的条目在表格里没有可展示的列。
    pub fn inherited_label_names(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_inherited_names(&mut out);
        out
    }

    fn collect_inherited_names(&self, out: &mut Vec<String>) {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().for_each(|q| q.collect_inherited_names(out)),
            Query::Not(q) => q.collect_inherited_names(out),
            Query::Cond(c) => {
                if let Field::LabelInherited(name) = &c.field {
                    if !out.iter().any(|n| n == name) {
                        out.push(name.clone());
                    }
                }
            }
        }
    }

    pub fn contains_text(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_text),
            Query::Not(q) => q.contains_text(),
            Query::Cond(c) => matches!(c.field, Field::Text),
        }
    }

    /// 查询是否需要把账号表预加载进 `EvalEnv::account_of`。
    ///
    /// 包含三类用法：
    ///  - 内置 `CreatedBy` / `UpdatedBy`：按 id 取名字邮箱参与比较。
    ///  - `Eq` / `Ne` / `In` / `NotIn` 作用在账号型标签上：单值 / 多值都走同一套。
    ///  - 多值账号标签的 `SetContains` / `SetEquals`：集合语义按 (name|email) 比，
    ///    不预加载就连一次都命中不了。
    ///
    /// 标签名位看不出类型（要看 schema），所以这一层只把"含任意标签条件"或
    /// "含 CreatedBy/UpdatedBy"算作需要预加载，过宽一点但不会算错。
    pub fn contains_account_field(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_account_field),
            Query::Not(q) => q.contains_account_field(),
            Query::Cond(c) => {
                matches!(c.field, Field::CreatedBy | Field::UpdatedBy)
                    || c.field.label_of().is_some()
            }
        }
    }

    /// 取第一个全文关键词（本轮 UI 只产生单个 text 条件）。
    pub fn first_text_keyword(&self) -> Option<String> {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().find_map(Query::first_text_keyword),
            Query::Not(q) => q.first_text_keyword(),
            Query::Cond(c) if matches!(c.field, Field::Text) => c
                .value
                .as_ref()
                .and_then(|v| v.as_str())
                .map(str::to_string),
            Query::Cond(_) => None,
        }
    }

    pub fn contains_event_field(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_event_field),
            Query::Not(q) => q.contains_event_field(),
            Query::Cond(c) => matches!(
                c.field,
                Field::EventLabel | Field::EventOld | Field::EventNew
            ),
        }
    }

    pub fn validate(&self, schemas: &[LabelSchema]) -> Result<(), AppError> {
        if self.contains_event_field() {
            return Err(AppError::InvalidQuery(
                "事件字段（$label / $old / $new）只能用在自动化规则的触发条件里".to_string(),
            ));
        }
        self.validate_inner(schemas)
    }

    /// 规则专用校验：触发条件（`allow_event = true`）允许事件字段，
    /// 动作目标（`false`）是对条目的过滤、不得引用事件。
    /// 两者都禁止全文条件——事件场景没有 tantivy 命中集，目标过滤也不走全文检索，
    /// `text` 恒为假，规则静默不触发比报错更糟。
    pub fn validate_for_rule(
        &self,
        schemas: &[LabelSchema],
        allow_event: bool,
    ) -> Result<(), AppError> {
        if !allow_event && self.contains_event_field() {
            return Err(AppError::InvalidQuery(
                "事件字段（$label / $old / $new）只能用在触发条件里".to_string(),
            ));
        }
        if self.contains_text() {
            return Err(AppError::InvalidQuery(
                "自动化规则不支持全文条件（text）".to_string(),
            ));
        }
        self.validate_inner(schemas)
    }

    /// 逐字段校验，不做过不过的准入判断（事件字段在这里视为已通过）。
    fn validate_inner(&self, schemas: &[LabelSchema]) -> Result<(), AppError> {
        // 引擎只按 first_text_keyword 检索，多个不同全文关键词会得出错误结果。
        let mut kws = HashSet::new();
        collect_text_keywords(self, &mut kws);
        if kws.len() > 1 {
            return Err(AppError::InvalidQuery("本轮仅支持单个全文条件".to_string()));
        }
        match self {
            Query::And(v) | Query::Or(v) => v.iter().try_for_each(|q| q.validate_inner(schemas)),
            Query::Not(q) => q.validate_inner(schemas),
            Query::Cond(c) => c.validate(schemas),
        }
    }

    pub fn evaluate(&self, entry: &Entry, labels: &[Labeling], env: &EvalEnv) -> bool {
        match self {
            Query::And(v) => v.iter().all(|q| q.evaluate(entry, labels, env)),
            Query::Or(v) => v.iter().any(|q| q.evaluate(entry, labels, env)),
            Query::Not(q) => !q.evaluate(entry, labels, env),
            Query::Cond(c) => c.evaluate(entry, labels, env),
        }
    }
}

/// 求值环境：条目本身之外的所有外部依赖。
pub struct EvalEnv<'a> {
    /// 全文检索命中判定（由 tantivy 提供）。
    pub text_hit: &'a dyn Fn(&str) -> bool,
    /// 账号 id → (显示名, 邮箱)；未知账号返回 None。
    pub account_of: &'a dyn Fn(Ulid) -> Option<(String, String)>,
    /// 标签名 → (值类型, 时间格式)；未知返回 None。时间型标签比较需要它。
    pub label_of: &'a dyn Fn(&str) -> Option<(LabelValueType, Option<String>)>,
    /// 当前事件。视图查询为 None，此时事件字段恒为假。
    pub event: Option<&'a LabelEvent>,
    /// 继承推导出的标签（`InheritanceGraph::derive` 的结果），只被 `L4+` 这类条件使用。
    /// 传给普通条件也无妨——它们压根不看这一项。
    pub derived: &'a [DerivedLabel],
    /// 标签值在区间内是否至少发生过一次变更（含端点按区间记号决定）。
    /// `None` 表示后端未提供审计（如前端 wasm、规则引擎事件评估）——条件直接为假。
    /// 闭包形参 = (entry_code, label_name, &Interval)。
    pub changed_during: Option<&'a dyn Fn(&str, &str, &Interval) -> bool>,
    /// 标签值发生过指定变更（from → to），且事件 `at` 落在 `during` 区间内。
    /// `from` / `to` 任一为 `None` 时该侧按通配处理（任意值匹配）——覆盖
    /// `changed from V` / `changed to V` 两种半边形式。
    /// `None` 表示后端未提供审计。闭包形参 = (entry_code, label_name, from?, to?, &Interval)。
    pub changed_value: Option<
        &'a dyn Fn(
            &str,
            &str,
            Option<&serde_json::Value>,
            Option<&serde_json::Value>,
            &Interval,
        ) -> bool,
    >,
}

/// 校验 Op::ChangedValue 的 from/to 值类型是否与标签 schema 一致。
/// 与「`In` / `NotIn` 候选值落在 enum_values 内」同思路：值类型不匹配直接报错。
fn validate_change_value_type(
    name: &str,
    vt: LabelValueType,
    role: &str, // "from" 或 "to"
    v: &serde_json::Value,
) -> Result<(), AppError> {
    let ok = match vt {
        LabelValueType::String | LabelValueType::Email => v.is_string(),
        LabelValueType::Enum => v.is_string(),
        LabelValueType::Integer => v.is_i64() || v.is_u64(),
        LabelValueType::Float | LabelValueType::Currency => v.is_number(),
        LabelValueType::Boolean => v.is_boolean(),
        LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime => {
            v.is_string()
                && v.as_str().is_some_and(|s| parse_time(s).is_some())
        }
        LabelValueType::Null | LabelValueType::Account => {
            // Account 标签的值是 ULID 字符串，类型简单按字符串走；
            // Null 标签的值本来就是 null。
            if vt == LabelValueType::Null {
                v.is_null()
            } else {
                v.is_string()
            }
        }
    };
    if !ok {
        return Err(AppError::InvalidQuery(format!(
            "标签 {name} 的 changed value {role} 类型与 schema 不匹配（{vt:?}）"
        )));
    }
    Ok(())
}

impl Condition {
    fn validate(&self, schemas: &[LabelSchema]) -> Result<(), AppError> {
        // 事件字段的类型取决于运行时事件，保存时无可校验之处；
        // 是否允许出现由 `Query::validate_for_rule` 统一把关。
        if matches!(
            self.field,
            Field::EventLabel | Field::EventOld | Field::EventNew
        ) {
            return Ok(());
        }
        // 标签值历史变更只对 Label / LabelInherited 有意义——其他字段直接拒。
        // 这一关放在字段 match 之前，避免内层每个分支都要复述。
        if matches!(self.op, Op::Changed | Op::ChangedValue) && self.field.label_of().is_none() {
            return Err(AppError::InvalidQuery(format!(
                "运算符 {} 只能用于标签字段",
                op_label(self.op)
            )));
        }
        match &self.field {
            // 文法 text := 'text' '~' scalar 只允许 Contains。
            Field::Text => {
                if self.op != Op::Contains {
                    return Err(AppError::InvalidQuery(format!(
                        "全文条件仅支持 ~（包含），收到 {}",
                        op_label(self.op)
                    )));
                }
            }
            Field::UpdatedAt | Field::CreatedAt => {
                if let Some(v) = &self.value {
                    let s = v.as_str().unwrap_or("");
                    if parse_time(s).is_none() {
                        return Err(AppError::InvalidQuery(format!("时间格式无效: {s}")));
                    }
                }
            }
            Field::Code | Field::Title | Field::Detail | Field::CreatedBy | Field::UpdatedBy => {
                if !string_field_op_allowed(self.op) {
                    return Err(AppError::InvalidQuery(format!(
                        "运算符 {} 不适用于内置元数据 {}",
                        op_label(self.op),
                        canonical_name(&self.field)
                    )));
                }
                match self.op {
                    Op::In | Op::NotIn => {
                        if !self.value.as_ref().is_some_and(|v| v.is_array()) {
                            return Err(AppError::InvalidQuery(
                                "in / not in 的值须为数组".to_string(),
                            ));
                        }
                    }
                    _ => {
                        if let Some(v) = &self.value {
                            if !v.is_string() {
                                return Err(AppError::InvalidQuery("比较值须为字符串".to_string()));
                            }
                        }
                    }
                }
            }
            Field::Label(name) | Field::LabelInherited(name) => {
                let schema = schemas
                    .iter()
                    .find(|s| &s.name == name)
                    .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {name}")))?;
                // 标签值历史变更：value 形状因 op 而异。
                //  - Op::Changed      ：`{"during": Option<Interval>}`（during 可缺省）。
                //  - Op::ChangedValue ：`{"from": Value, "to": Value, "during": Option<Interval>}`，
                //                        from/to 都必填且类型须与标签 schema 兼容。
                // 继承/覆盖是显示层概念，审计只看直接打标——`LabelInherited("X") changed` 仍合法。
                if matches!(self.op, Op::Changed | Op::ChangedValue) {
                    let obj = self
                        .value
                        .as_ref()
                        .and_then(|v| v.as_object())
                        .ok_or_else(|| {
                            AppError::InvalidQuery(format!(
                                "标签 {name} 的 changed 值必须是对象"
                            ))
                        })?;
                    if self.op == Op::ChangedValue {
                        // from / to 各自独立可缺省（`changed from V` / `changed to V`），
                        // 缺省侧在 eval 时按通配处理。但显式写 `from null` / `to null` 也按通配。
                        if let Some(from) = obj.get("from") {
                            if !from.is_null() {
                                validate_change_value_type(name, schema.value_type, "from", from)?;
                            }
                        }
                        if let Some(to) = obj.get("to") {
                            if !to.is_null() {
                                validate_change_value_type(name, schema.value_type, "to", to)?;
                            }
                        }
                    }
                    if let Some(during) = obj.get("during") {
                        if !during.is_null() {
                            // Interval 的 serde 反序列化失败意味着结构损坏——直接拒。
                            serde_json::from_value::<Interval>(during.clone()).map_err(|e| {
                                AppError::InvalidQuery(format!(
                                    "标签 {name} 的 during 区间格式无效: {e}"
                                ))
                            })?;
                        }
                    }
                    return Ok(());
                }
                if !op_allowed(schema.value_type, self.op, schema.multi) {
                    return Err(AppError::InvalidQuery(format!(
                        "运算符 {} 不适用于标签 {name}（{}）",
                        op_label(self.op),
                        type_label(schema.value_type)
                    )));
                }
                // in / not in 的候选值必须落在 enum_values 内。
                if matches!(self.op, Op::In | Op::NotIn)
                    && schema.value_type == LabelValueType::Enum
                {
                    let ok = self.value.as_ref().is_some_and(|v| {
                        v.as_array().is_some_and(|arr| {
                            arr.iter().all(|x| {
                                x.as_str()
                                    .is_some_and(|s| schema.enum_values.iter().any(|e| e == s))
                            })
                        })
                    });
                    if !ok {
                        return Err(AppError::InvalidQuery(format!(
                            "标签 {name} 的 in 候选值必须在枚举范围内: {}",
                            schema.enum_values.join(", ")
                        )));
                    }
                }
                match schema.value_type {
                    LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime => {
                        if let Some(v) = &self.value {
                            let s = v.as_str().unwrap_or("");
                            let layout = crate::domain::label::resolve_layout(
                                schema.format.as_deref(),
                                schema.value_type,
                            );
                            if crate::golayout::parse(&layout, s).is_none() {
                                return Err(AppError::InvalidQuery(format!("时间格式无效: {s}")));
                            }
                        }
                    }
                    LabelValueType::Currency => {
                        if let Some(v) = &self.value {
                            if v.as_f64().is_none() {
                                return Err(AppError::InvalidQuery(
                                    "金额标签的比较值必须是数值".to_string(),
                                ));
                            }
                        }
                    }
                    LabelValueType::Email => {
                        if matches!(self.op, Op::In | Op::NotIn) {
                            if !self.value.as_ref().is_some_and(|v| v.is_array()) {
                                return Err(AppError::InvalidQuery(
                                    "in / not in 的值须为数组".to_string(),
                                ));
                            }
                        } else if self.value.as_ref().is_some_and(|v| !v.is_string()) {
                            return Err(AppError::InvalidQuery("比较值须为字符串".to_string()));
                        }
                    }
                    LabelValueType::String => {
                        // 范围运算符必须按字典序解释，want 是数字会被当作字符串读出来，
                        // 给出含混结果（例如 7 想跟 "10" 比哪边大）。提前报错更友好。
                        // Eq/Ne/Contains/NotContains/In/NotIn 不在此约束下：
                        // `cmp_value` 的 `as_f64` 快路径与 `elem_strings` 已能处理非字符串。
                        if matches!(self.op, Op::Gt | Op::Ge | Op::Lt | Op::Le)
                            && self.value.as_ref().is_some_and(|v| !v.is_string())
                        {
                            return Err(AppError::InvalidQuery(format!(
                                "字符串标签 {name} 的范围运算符比较值必须是字符串"
                            )));
                        }
                    }
                    _ => {}
                }
            }
            // 上面的早返回已经处理，这里只为让 match 穷尽。
            Field::EventLabel | Field::EventOld | Field::EventNew => {}
        }
        Ok(())
    }

    fn evaluate(&self, entry: &Entry, labels: &[Labeling], env: &EvalEnv) -> bool {
        // 标签条件（含 `L4+`）在 match 之前处理，免得两个字段变体里抄一遍同样的逻辑。
        if let Some((name, inherited)) = self.field.label_of() {
            return self.eval_label(name, inherited, entry, labels, env);
        }
        match &self.field {
            Field::Text => self
                .value
                .as_ref()
                .and_then(|v| v.as_str())
                .map(env.text_hit)
                .unwrap_or(false),
            Field::UpdatedAt => cmp_time(&entry.updated_at, self.op, self.value.as_ref()),
            Field::CreatedAt => cmp_time(&entry.created_at, self.op, self.value.as_ref()),
            Field::Code => cmp_value(
                &serde_json::Value::String(entry.code.clone()),
                self.op,
                self.value.as_ref(),
            ),
            Field::Title => cmp_value(
                &serde_json::Value::String(entry.title.clone()),
                self.op,
                self.value.as_ref(),
            ),
            Field::Detail => cmp_value(
                &serde_json::Value::String(entry.detail.clone()),
                self.op,
                self.value.as_ref(),
            ),
            Field::CreatedBy | Field::UpdatedBy => {
                let id = if matches!(self.field, Field::CreatedBy) {
                    entry.created_by
                } else {
                    entry.updated_by
                };
                let Some((name, email)) = (env.account_of)(id) else {
                    return false;
                };
                match self.op {
                    Op::Eq => acc_eq(&name, &email, self.value.as_ref()),
                    Op::Ne => !acc_eq(&name, &email, self.value.as_ref()),
                    Op::Contains => acc_contains(&name, &email, self.value.as_ref()),
                    Op::NotContains => !acc_contains(&name, &email, self.value.as_ref()),
                    Op::In => acc_in(&name, &email, self.value.as_ref()),
                    Op::NotIn => !acc_in(&name, &email, self.value.as_ref()),
                    _ => false,
                }
            }
            // 上面已早返回，这里只为让 match 穷尽。
            Field::Label(_) | Field::LabelInherited(_) => false,
            // 事件字段：无事件（视图查询）时恒假。校验已保证这种用法存不进库。
            Field::EventLabel | Field::EventOld | Field::EventNew => {
                let Some(ev) = env.event else { return false };
                let got: Option<serde_json::Value> = match &self.field {
                    Field::EventLabel => Some(serde_json::Value::String(ev.label_name.clone())),
                    Field::EventOld => ev.old.as_ref().map(LabelValue::to_json),
                    _ => ev.new.as_ref().map(LabelValue::to_json),
                };
                let Some(got) = got else {
                    // 没有值：只有存在性判断能成立，其余比较一律为假。
                    return matches!(self.op, Op::Absent);
                };
                if self.op == Op::Present {
                    return true;
                }
                // 时间型比较要按被变更标签自己的布局解析，与 Field::Label 同构。
                if matches!(self.field, Field::EventOld | Field::EventNew) {
                    if let Some((vt, fmt)) = (env.label_of)(&ev.label_name) {
                        if matches!(
                            vt,
                            LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime
                        ) && matches!(
                            self.op,
                            Op::Eq | Op::Ne | Op::Gt | Op::Ge | Op::Lt | Op::Le
                        ) {
                            let layout = fmt
                                .as_deref()
                                .unwrap_or_else(|| crate::domain::label::default_layout(vt));
                            return cmp_time_layout(&got, layout, self.op, self.value.as_ref());
                        }
                    }
                }
                cmp_value(&got, self.op, self.value.as_ref())
            }
        }
    }

    /// 单个标签条件求值。`inherited` = 是否把继承关联来的标签也算上（`L4+`）。
    ///
    /// 值比较只在「本条件能拿到一个值」时才成立：`L4+` 遇到一个只继承了 key 的
    /// L4（值为 `None`）会返回假——key 继承本就只承诺存在性。
    fn eval_label(
        &self,
        name: &str,
        inherited: bool,
        entry: &Entry,
        labels: &[Labeling],
        env: &EvalEnv,
    ) -> bool {
        // 标签值历史变更：不读当前打标，直接走 EvalEnv 注入的审计钩子。
        // 没钩子（如前端 wasm、规则引擎事件评估）一律视为「没有审计数据」= 假。
        match self.op {
            Op::Changed => {
                let Some(f) = env.changed_during else {
                    return false;
                };
                let value = self.value.as_ref().unwrap_or(&serde_json::Value::Null);
                let interval = interval_from_value(value).unwrap_or_else(Interval::unbounded);
                return f(&entry.code, name, &interval);
            }
            Op::ChangedValue => {
                let Some(f) = env.changed_value else {
                    return false;
                };
                let value = match self.value.as_ref().and_then(|v| v.as_object()) {
                    Some(obj) => obj,
                    // validate 已强制为对象；理论上不可达，保守起见视为假。
                    None => return false,
                };
                // from / to 各自可缺省：缺省侧在闭包里按通配处理（不比较 from/to）。
                let from = value.get("from").and_then(|v| {
                    if v.is_null() {
                        None
                    } else {
                        Some(v)
                    }
                });
                let to = value.get("to").and_then(|v| {
                    if v.is_null() {
                        None
                    } else {
                        Some(v)
                    }
                });
                let interval = interval_from_value(&serde_json::Value::Object(value.clone()))
                    .unwrap_or_else(Interval::unbounded);
                return f(&entry.code, name, from, to, &interval);
            }
            _ => {}
        }
        let direct = labels.iter().find(|l| &l.label_name == name);
        // 外层 `Option` = 有没有继承来，内层 = 继承来的那一份带没带值。两者不能混：
        // key 继承（`L5` 继承 `L3`，无关值）只承诺存在性，值仍是 `None`。
        let derived = env
            .derived
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_ref());
        let held = direct.is_some() || (inherited && derived.is_some());
        match self.op {
            Op::Present => held,
            Op::Absent => !held,
            _ => {
                // 直接打上的值优先；没直接打上才退到继承来的值。
                let value = match (direct, inherited) {
                    (Some(l), _) => Some(&l.value),
                    (None, true) => derived.flatten(),
                    (None, false) => None,
                };
                let Some(value) = value else { return false };
                // 时间型标签按布局解析成时刻再比，避免自定义布局下字符串比较出错。
                if let Some((vt, fmt)) = (env.label_of)(name) {
                    if matches!(
                        vt,
                        LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime
                    ) && matches!(self.op, Op::Eq | Op::Ne | Op::Gt | Op::Ge | Op::Lt | Op::Le)
                    {
                        let layout = crate::domain::label::resolve_layout(fmt.as_deref(), vt);
                        return cmp_time_layout(
                            &value.to_json(),
                            &layout,
                            self.op,
                            self.value.as_ref(),
                        );
                    }
                    // 账号类型的所有值比较都走 `acc_set_op`：存储是 id（ULID），
                    // want 是名字或邮箱；裸 id 字符串比较拿不到「邮箱命中」语义，
                    // 一定要先靠 `env.account_of` 把每个 id 翻成 (name, email) 再比。
                    // 单值（`Account`）与多值（`AccountList`）的 Eq/Ne/In/NotIn 也走同一条
                    // 路径——`Eq` 在单值上退化为单元素集合的等值比较，语义与 SetEquals 一致。
                    // EnumList 的 `has` / `is` 两侧都是字符串数组，下面的通用 cmp_value
                    // 路径直接命中，不需要单独一条分支。
                    if vt == LabelValueType::Account
                        && matches!(
                            self.op,
                            Op::Eq | Op::Ne | Op::In | Op::NotIn | Op::SetContains | Op::SetEquals
                        )
                    {
                        return acc_set_op(
                            &value.account_ids(),
                            self.op,
                            self.value.as_ref(),
                            env.account_of,
                        );
                    }
                    // 字符串标签的范围运算符按 UTF-8 code-point 序比较（Rust `&str`
                    // 默认序，即字典序）。绕开下面 `cmp_value` 的 `as_f64` 快路径——
                    // 数值字面量会污染字典序判断，例如「10」 > 「9」 按数值是真，按字典序
                    // 是「10」 < 「9」（'1'(0x31) < '9'(0x39)）。want 是字符串、
                    // 存储值也是字符串（schema 守门），两侧都直接走 `&str` 序。
                    if vt == LabelValueType::String
                        && matches!(self.op, Op::Gt | Op::Ge | Op::Lt | Op::Le)
                    {
                        let Some(b) = self.value.as_ref().and_then(|v| v.as_str()) else {
                            return false;
                        };
                        let got_json = value.to_json();
                        let Some(a) = got_json.as_str() else {
                            return false;
                        };
                        return match self.op {
                            Op::Gt => a > b,
                            Op::Ge => a >= b,
                            Op::Lt => a < b,
                            _ => a <= b,
                        };
                    }
                }
                cmp_value(&value.to_json(), self.op, self.value.as_ref())
            }
        }
    }
}

fn acc_eq(name: &str, email: &str, want: Option<&serde_json::Value>) -> bool {
    let Some(w) = want.and_then(|v| v.as_str()) else {
        return false;
    };
    name.eq_ignore_ascii_case(w) || email.eq_ignore_ascii_case(w)
}

fn acc_contains(name: &str, email: &str, want: Option<&serde_json::Value>) -> bool {
    let Some(w) = want.and_then(|v| v.as_str()) else {
        return false;
    };
    let w = w.to_lowercase();
    name.to_lowercase().contains(&w) || email.to_lowercase().contains(&w)
}

fn acc_in(name: &str, email: &str, want: Option<&serde_json::Value>) -> bool {
    let Some(list) = want.and_then(|v| v.as_array()) else {
        return false;
    };
    list.iter().any(|x| {
        x.as_str()
            .is_some_and(|w| name.eq_ignore_ascii_case(w) || email.eq_ignore_ascii_case(w))
    })
}

/// 多值账号标签的 `has` / `is`：把每个存储的 id 通过 `account_of` 翻成 (name, email)，
/// 两侧都按 (name|email) 集合比较——用户表达式里写邮箱、写显示名都命中。
///
///  - `SetContains`（`has`）：want 的每一个元素都至少匹配到 stored 集合里的一个 (name, email)。
///  - `SetEquals`（`is`）：stored 与 want 一一对应（按 name/email 任一相等）。
///  - `Eq` / `Ne`（`=` / `!=`）：单值上与 name/email 任一相等；多值上等价 SetEquals 集合相等，
///    但保留 `Eq`/`Ne` 形态以兼容「Assignee = 王晓雁」这类最常见的单值写法。
///  - `In` / `NotIn`（`in` / `not in`）：任一 stored (name|email) 落在 want 数组内。
///
/// want 接受标量或数组；空 want 一律返回 false（与 `SetEquals` 的"集合相等"语义一致——
/// 空 want 不应误命中空 stored）。
fn acc_set_op(
    stored_ids: &[&str],
    op: Op,
    want: Option<&serde_json::Value>,
    account_of: &dyn Fn(Ulid) -> Option<(String, String)>,
) -> bool {
    let stored: Vec<(String, String)> = stored_ids
        .iter()
        .filter_map(|id| Ulid::from_string(id).ok().and_then(|u| account_of(u)))
        .collect();
    if stored.is_empty() {
        return false;
    }
    let wants: Vec<&str> = match want {
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_str()).collect(),
        Some(serde_json::Value::String(s)) => vec![s.as_str()],
        _ => Vec::new(),
    };
    if wants.is_empty() {
        return false;
    }
    // 两侧按 (name|email) 集合做双向命中：每个 want 都能找到一个未占用的 stored，
    // 反过来每个 stored 也都被某个 want 命中——单值时两侧各 1 元素，退化为等值比较。
    let mut used = vec![false; stored.len()];
    let all_paired = |used: &mut [bool]| -> bool {
        for w in &wants {
            let Some((idx, _)) = stored.iter().enumerate().find(|(i, (n, e))| {
                !used[*i] && (n.eq_ignore_ascii_case(w) || e.eq_ignore_ascii_case(w))
            }) else {
                return false;
            };
            used[idx] = true;
        }
        used.iter().all(|&u| u)
    };
    match op {
        Op::SetContains => wants.iter().all(|w| {
            stored
                .iter()
                .any(|(n, e)| n.eq_ignore_ascii_case(w) || e.eq_ignore_ascii_case(w))
        }),
        Op::SetEquals | Op::Eq => all_paired(&mut used),
        Op::Ne => !all_paired(&mut used),
        Op::In => stored.iter().any(|(n, e)| {
            wants
                .iter()
                .any(|w| n.eq_ignore_ascii_case(w) || e.eq_ignore_ascii_case(w))
        }),
        Op::NotIn => !stored.iter().any(|(n, e)| {
            wants
                .iter()
                .any(|w| n.eq_ignore_ascii_case(w) || e.eq_ignore_ascii_case(w))
        }),
        _ => false,
    }
}

/// 时间型标签比较：两侧按同一布局解析成 YmdHms，按字典序比较字段元组。
fn cmp_time_layout(
    got: &serde_json::Value,
    layout: &str,
    op: Op,
    want: Option<&serde_json::Value>,
) -> bool {
    use std::cmp::Ordering;
    let (Some(a), Some(b)) = (got.as_str(), want.and_then(|v| v.as_str())) else {
        return false;
    };
    let (Some(x), Some(y)) = (
        crate::golayout::parse(layout, a),
        crate::golayout::parse(layout, b),
    ) else {
        return false;
    };
    let ord = (x.year, x.month, x.day, x.hour, x.minute, x.second)
        .cmp(&(y.year, y.month, y.day, y.hour, y.minute, y.second));
    match op {
        Op::Eq => ord == Ordering::Equal,
        Op::Ne => ord != Ordering::Equal,
        Op::Gt => ord == Ordering::Greater,
        Op::Ge => ord != Ordering::Less,
        Op::Lt => ord == Ordering::Less,
        Op::Le => ord != Ordering::Greater,
        _ => false,
    }
}

/// 运算符的可读中文/符号标签，用于用户可见的错误消息。
fn op_label(op: Op) -> &'static str {
    match op {
        Op::Present => "present",
        Op::Absent => "absent",
        Op::Eq => "=",
        Op::Ne => "!=",
        Op::In => "in",
        Op::NotIn => "not in",
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Lt => "<",
        Op::Le => "<=",
        Op::Contains => "~",
        Op::NotContains => "!~",
        Op::SetContains => "has",
        Op::SetEquals => "is",
        Op::Changed | Op::ChangedValue => "changed",
    }
}

/// 标签值类型的中文名，用于用户可见的错误消息。
fn type_label(vt: LabelValueType) -> &'static str {
    match vt {
        LabelValueType::Null => "空",
        LabelValueType::Boolean => "布尔",
        LabelValueType::Integer => "整数",
        LabelValueType::Float => "浮点",
        LabelValueType::String => "字符串",
        LabelValueType::Enum => "枚举",
        LabelValueType::Date => "日期",
        LabelValueType::Time => "时间",
        LabelValueType::DateTime => "日期时间",
        LabelValueType::Currency => "金额",
        LabelValueType::Email => "邮箱",
        LabelValueType::Account => "账号",
    }
}

/// 字段的规范名：内置字段给出关键字原文，Label 无规范名（返回空串）。
pub fn canonical_name(f: &Field) -> &'static str {
    match f {
        Field::Label(_) | Field::LabelInherited(_) => "",
        Field::Code => "Code",
        Field::Title => "Title",
        Field::Detail => "Detail",
        Field::CreatedBy => "CreatedBy",
        Field::CreatedAt => "CreatedAt",
        Field::UpdatedBy => "UpdatedBy",
        Field::UpdatedAt => "UpdatedAt",
        Field::Text => "text",
        Field::EventLabel => "$label",
        Field::EventOld => "$old",
        Field::EventNew => "$new",
    }
}

/// 收集整棵查询树中所有 Text 条件的关键词，用于检测多个全文条件。
fn collect_text_keywords(q: &Query, out: &mut HashSet<String>) {
    match q {
        Query::And(v) | Query::Or(v) => v.iter().for_each(|c| collect_text_keywords(c, out)),
        Query::Not(c) => collect_text_keywords(c, out),
        Query::Cond(c) => {
            if matches!(c.field, Field::Text) {
                if let Some(s) = c.value.as_ref().and_then(|v| v.as_str()) {
                    out.insert(s.to_string());
                }
            }
        }
    }
}

/// 运算符是否可用于该标签。`multi` 决定 Account / Enum 是否启用集合语义运算符：
/// 单值与多值都允许 `Eq/Ne/In/NotIn`；只有多值 Account/Enum 才允许 `SetContains/SetEquals`。
fn op_allowed(vt: LabelValueType, op: Op, multi: bool) -> bool {
    use LabelValueType::*;
    // 标签值历史变更不依赖值的语义（只关心有没有变更事件），
    // 任何值类型都放行；字段层会进一步拒绝 Label/Event 之外的字段。
    if matches!(op, Op::Changed | Op::ChangedValue) {
        return true;
    }
    let existence = matches!(op, Op::Present | Op::Absent);
    let cmp = match vt {
        Null => false,
        Boolean => matches!(op, Op::Eq | Op::Ne),
        Integer | Float | Currency => {
            matches!(op, Op::Eq | Op::Ne | Op::Gt | Op::Ge | Op::Lt | Op::Le)
        }
        Date | Time | DateTime => {
            matches!(op, Op::Eq | Op::Ne | Op::Gt | Op::Ge | Op::Lt | Op::Le)
        }
        // 字符串：精确 + 子串 + 列表匹配 + 按字典序的范围比较。
        // 范围比较在 `eval_label` 里走特殊分支，绕开 `cmp_value` 的 `as_f64` 快路径
        // （数值字面量会污染字典序判断，比如「10」 > 「9」按数值是对的但字典序是反的）。
        String => matches!(
            op,
            Op::Eq
                | Op::Ne
                | Op::Contains
                | Op::NotContains
                | Op::In
                | Op::NotIn
                | Op::Gt
                | Op::Ge
                | Op::Lt
                | Op::Le
        ),
        // 邮箱：精确 + 子串 + 列表匹配。范围比较留白——
        // 邮箱是语义标识符，字典序没有用户预期意义。
        Email => matches!(
            op,
            Op::Eq | Op::Ne | Op::Contains | Op::NotContains | Op::In | Op::NotIn
        ),
        // Enum：单值时与字符串等价（精确 / 子串 / 列表匹配）；
        // 多值时额外允许 SetContains / SetEquals，语义是值集合而非字符串集合。
        Enum => {
            let base = matches!(
                op,
                Op::Eq | Op::Ne | Op::Contains | Op::NotContains | Op::In | Op::NotIn
            );
            let set_ops = multi && matches!(op, Op::SetContains | Op::SetEquals);
            base || set_ops
        }
        // 账号：Eq/Ne/In/NotIn 在 `eval_label` 里走 id → 名字邮箱 转换；
        // SetContains/SetEquals（仅多值）也走同一套，集合语义不靠裸 id 字符串。
        Account => {
            let base = matches!(op, Op::Eq | Op::Ne | Op::In | Op::NotIn);
            let set_ops = multi && matches!(op, Op::SetContains | Op::SetEquals);
            base || set_ops
        }
    };
    existence || cmp
}

/// 内置文本型元数据（Code/Title/Detail/CreatedBy/UpdatedBy）允许的运算符。
fn string_field_op_allowed(op: Op) -> bool {
    matches!(
        op,
        Op::Eq | Op::Ne | Op::Contains | Op::NotContains | Op::In | Op::NotIn
    )
}

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
}

/// 标量 JSON 值 → 可比较的字符串。数组/对象不是标量，返回 None。
///
/// 布尔、数字、null 也要参与比较：多值集合语义不应丢掉 Task 3 之前
/// `got == want` 对这几类标量的判断能力。
fn scalar_string(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Null => Some("null".to_string()),
        serde_json::Value::Array(_) | serde_json::Value::Object(_) => None,
    }
}

/// 把标签值归一成字符串集合：EnumList 是数组（逐元素，仅标量），
/// 单标量即 1 元素，其余（数组里的非标量、对象）不入集合。
fn elem_strings(got: &serde_json::Value) -> Vec<String> {
    match got {
        serde_json::Value::Array(a) => a.iter().filter_map(scalar_string).collect(),
        other => scalar_string(other).into_iter().collect(),
    }
}

fn cmp_value(got: &serde_json::Value, op: Op, want: Option<&serde_json::Value>) -> bool {
    let Some(want) = want else { return false };
    match op {
        // 正负成对：任一命中 / 无一命中。
        Op::Eq | Op::Ne => {
            if let (Some(a), Some(b)) = (as_f64(got), as_f64(want)) {
                return if op == Op::Eq { a == b } else { a != b };
            }
            let want_s = want
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| want.to_string());
            let hit = elem_strings(got).iter().any(|s| s == &want_s);
            if op == Op::Eq {
                hit
            } else {
                !hit
            }
        }
        Op::Contains | Op::NotContains => {
            let Some(b) = want.as_str() else { return false };
            // 非字符串标量（数字/布尔/null）不参与包含判断：与旧行为一致，
            // 正负两种运算符都返回 false，而不是让 `!~` 恒真。
            if !matches!(
                got,
                serde_json::Value::String(_) | serde_json::Value::Array(_)
            ) {
                return false;
            }
            let needle = b.to_lowercase();
            let hit = elem_strings(got)
                .iter()
                .any(|s| s.to_lowercase().contains(&needle));
            if op == Op::Contains {
                hit
            } else {
                !hit
            }
        }
        Op::In | Op::NotIn => {
            let Some(list) = want.as_array() else {
                return false;
            };
            let elems = elem_strings(got);
            // 数值候选先按数值比较（解析器产出的数字是 float，标签值可能是 int），
            // 其余按标量字符串集合语义：数组值任一元素命中候选即可。
            let hit = list.iter().any(|x| match (as_f64(got), as_f64(x)) {
                (Some(a), Some(b)) => a == b,
                _ => scalar_string(x).is_some_and(|w| elems.iter().any(|s| s == &w)),
            });
            if op == Op::In {
                hit
            } else {
                !hit
            }
        }
        Op::Gt | Op::Ge | Op::Lt | Op::Le => match (as_f64(got), as_f64(want)) {
            (Some(a), Some(b)) => match op {
                Op::Gt => a > b,
                Op::Ge => a >= b,
                Op::Lt => a < b,
                _ => a <= b,
            },
            _ => false,
        },
        // 多值集合语义：`got` 与 `want` 都按标量字符串集合处理，want 接受标量或数组。
        // 集合里出现非标量元素（对象等）会被 elem_strings 静默丢掉——这与既有 In/Contains 路径一致。
        Op::SetContains => {
            let got_set: BTreeSet<String> = elem_strings(got).into_iter().collect();
            let want_set: BTreeSet<String> = elem_strings(want).into_iter().collect();
            // 空 want 是无意义查询：避免「has 」匹配到一切有任意值的标签。
            !want_set.is_empty() && want_set.iter().all(|w| got_set.contains(w))
        }
        Op::SetEquals => {
            let got_set: BTreeSet<String> = elem_strings(got).into_iter().collect();
            let want_set: BTreeSet<String> = elem_strings(want).into_iter().collect();
            got_set == want_set
        }
        Op::Present | Op::Absent => false,
        // 标签值历史变更在 `eval_label` 走专用分支，不进 cmp_value。
        Op::Changed | Op::ChangedValue => false,
    }
}

/// 从 `Condition.value` 里的 `{"during": ...}` 字段反序列化出 `Interval`。
/// 缺省 / Null → 返回 `None`（调用方按「任意时间」处理）。
/// 解析失败 → 返回 `None`（容错；validate 已挡住非法输入，理论上不可达）。
fn interval_from_value(v: &serde_json::Value) -> Option<Interval> {
    let obj = v.as_object()?;
    let during = obj.get("during")?;
    if during.is_null() {
        return None;
    }
    serde_json::from_value::<Interval>(during.clone()).ok()
}

/// `Interval` → JSON 写入 `Condition.value` 的 `"during"` 字段。
/// 失败时退化为 Null（理论上 chrono + serde 不会失败）。
fn interval_to_json(i: Interval) -> serde_json::Value {
    serde_json::to_value(i).unwrap_or(serde_json::Value::Null)
}

/// `Interval` → 表达式字串，如 `(-, "2026-09-01"]` / `["2026-01-01", +)`。
/// 仅供 `Condition::to_expr` 渲染用；`Unbounded`（全开）走全开字串。
fn interval_to_expr(i: &Interval) -> String {
    if matches!(i.start.0, IntervalBound::NegInfinity)
        && matches!(i.end.0, IntervalBound::PosInfinity)
    {
        return "(-, +)".to_string();
    }
    let open = |k: BoundKind| match k {
        BoundKind::Open => '(',
        BoundKind::Closed => '[',
    };
    let close = |k: BoundKind| match k {
        BoundKind::Open => ')',
        BoundKind::Closed => ']',
    };
    let bound = |b: &IntervalBound| match b {
        IntervalBound::Time(t) => quote(&t.to_rfc3339()),
        IntervalBound::NegInfinity => "-".to_string(),
        IntervalBound::PosInfinity => "+".to_string(),
    };
    format!(
        "{}{}, {}{}",
        open(i.start.1),
        bound(&i.start.0),
        bound(&i.end.0),
        close(i.end.1),
    )
}

/// 依次尝试 RFC3339、日期+时间、日期、纯时间；全部失败返回 None。
///
/// 纯时间（如 `15:04`）没有日期部分：按计划的要求补齐为一个日期字段取零值的
/// 时刻（纪元日 1970-01-01），使 `%H:%M:%S` / `%H:%M` 两个格式真正可达，
/// 而不是被守卫短路成死分支。
fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    for (fmt, has_date, has_time) in [
        ("%Y-%m-%d %H:%M:%S", true, true),
        ("%Y-%m-%d %H:%M", true, true),
        ("%Y-%m-%d", true, false),
        ("%H:%M:%S", false, true),
        ("%H:%M", false, true),
    ] {
        match (has_date, has_time) {
            (true, true) => {
                if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
                    return Some(dt.and_utc());
                }
            }
            (true, false) => {
                if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
                    return d.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc());
                }
            }
            (false, true) => {
                if let Ok(t) = NaiveTime::parse_from_str(s, fmt) {
                    return NaiveDate::from_ymd_opt(1970, 1, 1).map(|d| d.and_time(t).and_utc());
                }
            }
            _ => {}
        }
    }
    None
}

fn cmp_time(got: &DateTime<Utc>, op: Op, want: Option<&serde_json::Value>) -> bool {
    let Some(s) = want.and_then(|v| v.as_str()) else {
        return false;
    };
    let Some(t) = parse_time(s) else { return false };
    match op {
        Op::Eq => *got == t,
        Op::Ne => *got != t,
        Op::Gt => *got > t,
        Op::Ge => *got >= t,
        Op::Lt => *got < t,
        Op::Le => *got <= t,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    LBracket,
    RBracket,
    Comma,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Tilde,
    NotTilde,
    Bang,
    Plus,
    And,
    Or,
    Not,
    In,
    Text,
    Builtin(Field),
    Ident(String),
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    EventField(Field),
}

fn lex(input: &str) -> Result<Vec<Tok>, AppError> {
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '[' => {
                out.push(Tok::LBracket);
                i += 1;
            }
            ']' => {
                out.push(Tok::RBracket);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '=' => {
                out.push(Tok::Eq);
                i += 1;
            }
            '>' => {
                if chars.get(i + 1) == Some(&'=') {
                    out.push(Tok::Ge);
                    i += 2;
                } else {
                    out.push(Tok::Gt);
                    i += 1;
                }
            }
            '<' => {
                if chars.get(i + 1) == Some(&'=') {
                    out.push(Tok::Le);
                    i += 2;
                } else {
                    out.push(Tok::Lt);
                    i += 1;
                }
            }
            '!' => match chars.get(i + 1) {
                Some('=') => {
                    out.push(Tok::Ne);
                    i += 2;
                }
                Some('~') => {
                    out.push(Tok::NotTilde);
                    i += 2;
                }
                _ => {
                    out.push(Tok::Bang);
                    i += 1;
                }
            },
            '~' => {
                out.push(Tok::Tilde);
                i += 1;
            }
            '+' => {
                out.push(Tok::Plus);
                i += 1;
            }
            '$' => {
                let start = i + 1;
                let mut j = start;
                while j < chars.len() && chars[j].is_ascii_alphabetic() {
                    j += 1;
                }
                let word: String = chars[start..j].iter().collect();
                match word.to_ascii_lowercase().as_str() {
                    "label" => out.push(Tok::EventField(Field::EventLabel)),
                    "old" => out.push(Tok::EventField(Field::EventOld)),
                    "new" => out.push(Tok::EventField(Field::EventNew)),
                    _ => return Err(AppError::InvalidQuery(format!("未知事件字段: ${word}"))),
                }
                i = j;
            }
            '"' | '\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != quote {
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        let next = chars[i + 1];
                        if next == quote || next == '\\' {
                            s.push(next);
                            i += 2;
                            continue;
                        }
                    }
                    s.push(chars[i]);
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(AppError::InvalidQuery("字符串未闭合".to_string()));
                }
                i += 1;
                out.push(Tok::Str(s));
            }
            _ => {
                if c.is_ascii_digit()
                    || (c == '-' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit()))
                {
                    let start = i;
                    i += 1;
                    while i < chars.len()
                        && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '-')
                    {
                        i += 1;
                    }
                    let text: String = chars[start..i].iter().collect();
                    let n = text
                        .parse::<f64>()
                        .map_err(|_| AppError::InvalidQuery(format!("非法数字: {text}")))?;
                    out.push(Tok::Num(n));
                } else if c.is_alphanumeric() || c == '_' || c == '-' {
                    let start = i;
                    while i < chars.len()
                        && (chars[i].is_alphanumeric()
                            || chars[i] == '_'
                            || chars[i] == '-'
                            || chars[i] == '.')
                    {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    match word.to_ascii_lowercase().as_str() {
                        "and" => out.push(Tok::And),
                        "or" => out.push(Tok::Or),
                        "not" => out.push(Tok::Not),
                        "in" => out.push(Tok::In),
                        "text" => out.push(Tok::Text),
                        "code" => out.push(Tok::Builtin(Field::Code)),
                        "title" => out.push(Tok::Builtin(Field::Title)),
                        "detail" => out.push(Tok::Builtin(Field::Detail)),
                        "createdby" => out.push(Tok::Builtin(Field::CreatedBy)),
                        "createdat" => out.push(Tok::Builtin(Field::CreatedAt)),
                        "updatedby" => out.push(Tok::Builtin(Field::UpdatedBy)),
                        "updatedat" => out.push(Tok::Builtin(Field::UpdatedAt)),
                        "true" => out.push(Tok::Bool(true)),
                        "false" => out.push(Tok::Bool(false)),
                        // 标签值历史变更的 `from null to "open"` / `from "open" to null` 走 JSON null。
                        "null" => out.push(Tok::Null),
                        _ => out.push(Tok::Ident(word)),
                    }
                } else {
                    return Err(AppError::InvalidQuery(format!("无法识别的字符: {c}")));
                }
            }
        }
    }
    Ok(out)
}

/// 把词法单元转成可读文本，避免在错误消息中泄漏 Debug 表示。
fn tok_label(t: Option<&Tok>) -> String {
    match t {
        None => "输入结束".to_string(),
        Some(Tok::LParen) => "(".to_string(),
        Some(Tok::RParen) => ")".to_string(),
        Some(Tok::Comma) => ",".to_string(),
        Some(Tok::Eq) => "=".to_string(),
        Some(Tok::Ne) => "!=".to_string(),
        Some(Tok::Gt) => ">".to_string(),
        Some(Tok::Ge) => ">=".to_string(),
        Some(Tok::Lt) => "<".to_string(),
        Some(Tok::Le) => "<=".to_string(),
        Some(Tok::Tilde) => "~".to_string(),
        Some(Tok::NotTilde) => "!~".to_string(),
        Some(Tok::Bang) => "!".to_string(),
        Some(Tok::Plus) => "+".to_string(),
        Some(Tok::And) => "AND".to_string(),
        Some(Tok::Or) => "OR".to_string(),
        Some(Tok::Not) => "NOT".to_string(),
        Some(Tok::In) => "in".to_string(),
        Some(Tok::Text) => "text".to_string(),
        Some(Tok::Builtin(f)) => canonical_name(f).to_string(),
        Some(Tok::EventField(f)) => canonical_name(f).to_string(),
        Some(Tok::Ident(s)) => s.clone(),
        Some(Tok::Str(s)) => s.clone(),
        Some(Tok::Num(n)) => n.to_string(),
        Some(Tok::Bool(b)) => b.to_string(),
        Some(Tok::LBracket) => "[".to_string(),
        Some(Tok::RBracket) => "]".to_string(),
        Some(Tok::Null) => "null".to_string(),
    }
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn eat(&mut self, t: &Tok) -> Result<(), AppError> {
        if self.peek() == Some(t) {
            self.pos += 1;
            Ok(())
        } else {
            Err(AppError::InvalidQuery(format!(
                "期望 {}，实际 {}",
                tok_label(Some(t)),
                tok_label(self.peek())
            )))
        }
    }

    fn parse_or(&mut self) -> Result<Query, AppError> {
        let mut parts = vec![self.parse_and()?];
        while self.peek() == Some(&Tok::Or) {
            self.next();
            parts.push(self.parse_and()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            Query::Or(parts)
        })
    }

    fn parse_and(&mut self) -> Result<Query, AppError> {
        let mut parts = vec![self.parse_unary()?];
        while self.peek() == Some(&Tok::And) {
            self.next();
            parts.push(self.parse_unary()?);
        }
        Ok(if parts.len() == 1 {
            parts.pop().unwrap()
        } else {
            Query::And(parts)
        })
    }

    fn parse_unary(&mut self) -> Result<Query, AppError> {
        if self.peek() == Some(&Tok::Not) {
            self.next();
            return Ok(Query::Not(Box::new(self.parse_unary()?)));
        }
        if self.peek() == Some(&Tok::Bang) {
            self.next();
            // `!标签名` 是存在性取反的语法糖，直接落成缺席条件（等价于旧的 absent()）；
            // 其余形式按普通一元否定处理，`!x` 即 `NOT x`。
            return Ok(match self.parse_primary()? {
                // 字段原样带走：`!L4+` 是「直接与继承都没有」。
                Query::Cond(Condition {
                    field,
                    op: Op::Present,
                    value: None,
                }) if field.label_of().is_some() => Query::Cond(Condition {
                    field,
                    op: Op::Absent,
                    value: None,
                }),
                other => Query::Not(Box::new(other)),
            });
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Query, AppError> {
        match self.peek() {
            Some(Tok::LParen) => {
                self.next();
                let q = self.parse_or()?;
                self.eat(&Tok::RParen)?;
                Ok(q)
            }
            Some(Tok::Text) => {
                self.next();
                let op = self.comparison_op()?;
                let v = self.scalar()?;
                Ok(Query::Cond(Condition {
                    field: Field::Text,
                    op,
                    value: Some(v),
                }))
            }
            Some(Tok::Builtin(_)) | Some(Tok::Ident(_)) | Some(Tok::EventField(_)) => {
                self.parse_condition()
            }
            other => Err(AppError::InvalidQuery(format!(
                "无法解析: {}",
                tok_label(other)
            ))),
        }
    }

    fn comparison_op(&mut self) -> Result<Op, AppError> {
        let op = match self.next() {
            Some(Tok::Eq) => Op::Eq,
            Some(Tok::Ne) => Op::Ne,
            Some(Tok::Gt) => Op::Gt,
            Some(Tok::Ge) => Op::Ge,
            Some(Tok::Lt) => Op::Lt,
            Some(Tok::Le) => Op::Le,
            Some(Tok::Tilde) => Op::Contains,
            Some(Tok::NotTilde) => Op::NotContains,
            Some(Tok::In) => Op::In,
            Some(Tok::Not) => {
                self.eat(&Tok::In)?;
                Op::NotIn
            }
            // 上下文相关：`is` / `has` 在运算符位被解释为集合语义运算符；
            // 在标签名位仍是普通 `Tok::Ident`，所以「标签叫 is」的工作空间不会被这一行打坏。
            // 不在 lex 阶段一上来就关键字化（那是 plan 阶段反复强调的点）。
            Some(Tok::Ident(s)) => match s.to_ascii_lowercase().as_str() {
                "has" => Op::SetContains,
                "is" => Op::SetEquals,
                // 标签值历史变更的入口关键字。`from V1 to V2 [during (...)]` 由
                // `parse_condition` 在 op 是 Op::Changed 时延后处理；缺省 `from` 时
                // 直接落成 Changed（任意变更），提供 `from/to` 时升级为 ChangedValue。
                "changed" => Op::Changed,
                _ => return Err(AppError::InvalidQuery(format!("期望运算符，实际 {s}"))),
            },
            other => {
                return Err(AppError::InvalidQuery(format!(
                    "期望运算符，实际 {}",
                    tok_label(other.as_ref())
                )))
            }
        };
        Ok(op)
    }

    fn scalar(&mut self) -> Result<serde_json::Value, AppError> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(serde_json::Value::String(s)),
            Some(Tok::Num(n)) => Ok(serde_json::Number::from_f64(n)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)),
            Some(Tok::Bool(b)) => Ok(serde_json::Value::Bool(b)),
            Some(Tok::Null) => Ok(serde_json::Value::Null),
            Some(Tok::Ident(s)) => Ok(serde_json::Value::String(s)),
            other => Err(AppError::InvalidQuery(format!(
                "期望值，实际 {}",
                tok_label(other.as_ref())
            ))),
        }
    }

    fn parse_condition(&mut self) -> Result<Query, AppError> {
        let field = match self.next() {
            Some(Tok::Builtin(f)) => f,
            Some(Tok::EventField(f)) => f,
            Some(Tok::Ident(s)) => {
                // 后缀 `+`：连继承关联来的条目一起匹配（`L4+`）。
                if self.peek() == Some(&Tok::Plus) {
                    self.next();
                    Field::LabelInherited(s)
                } else {
                    Field::Label(s)
                }
            }
            other => {
                return Err(AppError::InvalidQuery(format!(
                    "期望字段，实际 {}",
                    tok_label(other.as_ref())
                )))
            }
        };
        // `+` 只对标签有意义：内置元数据没有继承可言。
        if self.peek() == Some(&Tok::Plus) && field.label_of().is_none() {
            return Err(AppError::InvalidQuery(
                "只有标签支持 +（连继承关联一起匹配）".to_string(),
            ));
        }
        if field.label_of().is_some() {
            if self.peek() == Some(&Tok::LParen) {
                return Err(AppError::InvalidQuery(
                    "不再支持 present()/absent()：标签名单独出现即表示「存在」，前缀 ! 表示「不存在」"
                        .to_string(),
                ));
            }
            // 标签名后不接运算符时是存在性判断：`Task` 等价于旧的 present(Task)。
            if !matches!(
                self.peek(),
                Some(
                    Tok::Eq
                        | Tok::Ne
                        | Tok::Gt
                        | Tok::Ge
                        | Tok::Lt
                        | Tok::Le
                        | Tok::Tilde
                        | Tok::NotTilde
                        | Tok::In
                        | Tok::Not
                        | Tok::Ident(_)
                )
            ) {
                return Ok(Query::Cond(Condition {
                    field,
                    op: Op::Present,
                    value: None,
                }));
            }
        }
        if matches!(field, Field::EventLabel | Field::EventOld | Field::EventNew) {
            if self.peek() == Some(&Tok::LParen) {
                return Err(AppError::InvalidQuery(
                    "事件字段不支持 present()/absent()：字段名单独出现即表示「存在」，前缀 ! 表示「不存在」"
                        .to_string(),
                ));
            }
            // 事件字段单独出现同样是存在性判断：`$new` 即「有值」，`!$new` 即「无值」。
            if !matches!(
                self.peek(),
                Some(
                    Tok::Eq
                        | Tok::Ne
                        | Tok::Gt
                        | Tok::Ge
                        | Tok::Lt
                        | Tok::Le
                        | Tok::Tilde
                        | Tok::NotTilde
                        | Tok::In
                        | Tok::Not
                        | Tok::Ident(_)
                )
            ) {
                return Ok(Query::Cond(Condition {
                    field,
                    op: Op::Present,
                    value: None,
                }));
            }
        }
        let op = self.comparison_op()?;
        let (op, value) = if op == Op::Changed {
            // `changed [[from V1] to V2] [during (T1, T2)]`：from / to 各自独立可缺省。
            // - 全缺省 → `Op::Changed`（任意变更）
            // - 仅 to / 仅 from / 都有 → `Op::ChangedValue`，缺失侧在 eval 时按通配处理
            // `from V1 to V2` 的两段强顺序不变；`to V2` 不需要先写 `from`。
            let mut from: Option<serde_json::Value> = None;
            let mut to: Option<serde_json::Value> = None;
            let mut during: Option<Interval> = None;
            let mut final_op = Op::Changed;
            if self.peek_keyword("from") {
                self.next();
                from = Some(self.scalar()?);
                // `from V1 to V2` 紧跟 to 是允许的；不写 to 视为「from V1 → 任意」。
                if self.peek_keyword("to") {
                    self.next();
                    to = Some(self.scalar()?);
                }
                final_op = Op::ChangedValue;
            } else if self.peek_keyword("to") {
                // 仅 `to V2`：「任意 → V2」，不要求 from。
                self.next();
                to = Some(self.scalar()?);
                final_op = Op::ChangedValue;
            }
            if self.peek_keyword("during") {
                self.next();
                during = Some(self.parse_interval()?);
            }
            let value = match final_op {
                Op::ChangedValue => serde_json::json!({
                    "from": from,
                    "to": to,
                    "during": during.map(interval_to_json).unwrap_or(serde_json::Value::Null),
                }),
                Op::Changed => serde_json::json!({
                    "during": during.map(interval_to_json).unwrap_or(serde_json::Value::Null),
                }),
                _ => unreachable!(),
            };
            (final_op, value)
        } else if self.peek() == Some(&Tok::LParen) {
            self.next();
            let mut items = vec![self.scalar()?];
            while self.peek() == Some(&Tok::Comma) {
                self.next();
                items.push(self.scalar()?);
            }
            self.eat(&Tok::RParen)?;
            (op, serde_json::Value::Array(items))
        } else {
            (op, self.scalar()?)
        };
        Ok(Query::Cond(Condition {
            field,
            op,
            value: Some(value),
        }))
    }

    /// 读取 `[from "X"] [to "Y"]` 的 keyword 形式（仅 Op::ChangedValue 路径需要）。
    fn eat_keyword(&mut self, kw: &str) -> Result<(), AppError> {
        let peeked = self.peek();
        match peeked {
            Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw) => {
                self.next();
                Ok(())
            }
            _ => Err(AppError::InvalidQuery(format!(
                "期望 {kw}，实际 {}",
                tok_label(peeked)
            ))),
        }
    }

    /// 不消费：仅判断下一个 token 是不是该 keyword（大小写不敏感）。
    fn peek_keyword(&self, kw: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(s)) if s.eq_ignore_ascii_case(kw))
    }

    /// 读取 `during (...)` 的区间参数：`(T1, T2)` / `[T1, T2]` / `(T1, T2]` / `[T1, T2)`。
    /// 端点可以是 RFC3339 字符串、`-`（-∞）、`+`（+∞，与 `L4+` 后缀不冲突——
    /// 此处已经在 `during` 关键字后，语法上下文已锁定）。
    fn parse_interval(&mut self) -> Result<Interval, AppError> {
        let start_kind = match self.next() {
            Some(Tok::LParen) => BoundKind::Open,
            Some(Tok::LBracket) => BoundKind::Closed,
            other => {
                return Err(AppError::InvalidQuery(format!(
                    "期望区间起点 ( 或 [，实际 {}",
                    tok_label(other.as_ref())
                )))
            }
        };
        let start_bound = self.parse_interval_bound()?;
        self.eat(&Tok::Comma)?;
        let end_bound = self.parse_interval_bound()?;
        let end_kind = match self.next() {
            Some(Tok::RParen) => BoundKind::Open,
            Some(Tok::RBracket) => BoundKind::Closed,
            other => {
                return Err(AppError::InvalidQuery(format!(
                    "期望区间终点 ) 或 ]，实际 {}",
                    tok_label(other.as_ref())
                )))
            }
        };
        Ok(Interval {
            start: (start_bound, start_kind),
            end: (end_bound, end_kind),
        })
    }

    fn parse_interval_bound(&mut self) -> Result<IntervalBound, AppError> {
        match self.peek() {
            Some(Tok::Ident(s)) if s == "-" => {
                self.next();
                Ok(IntervalBound::NegInfinity)
            }
            Some(Tok::Plus) => {
                // 在 `during ( ... )` 内部 `+` 唯一含义就是正无穷；
                // 字段名后的 `+`（继承后缀）在 `parse_condition` 字段位就处理掉了。
                self.next();
                Ok(IntervalBound::PosInfinity)
            }
            _ => {
                let v = self.scalar()?;
                let s = v.as_str().ok_or_else(|| {
                    AppError::InvalidQuery(format!(
                        "区间端点必须是字符串、- 或 +，实际 {v}"
                    ))
                })?;
                let t = parse_time(s).ok_or_else(|| {
                    AppError::InvalidQuery(format!("区间端点时间格式无效: {s}"))
                })?;
                Ok(IntervalBound::Time(t))
            }
        }
    }
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn expr_op(op: Op) -> &'static str {
    match op {
        Op::Eq => "=",
        Op::Ne => "!=",
        Op::Gt => ">",
        Op::Ge => ">=",
        Op::Lt => "<",
        Op::Le => "<=",
        Op::Contains => "~",
        Op::NotContains => "!~",
        Op::In => "in",
        Op::NotIn => "not in",
        // 多值集合语义在表达式里以 `has` / `is` 写出来（与 `comparison_op` 反向一致）。
        Op::SetContains => "has",
        Op::SetEquals => "is",
        // 序列化时按 value 形状区分 from/to 与 during 段：实现放在 Condition::to_expr。
        Op::Changed | Op::ChangedValue => "changed",
        Op::Present | Op::Absent => "",
    }
}

fn expr_scalar(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => quote(s),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Array(a) => {
            let items: Vec<String> = a.iter().map(expr_scalar).collect();
            format!("({})", items.join(", "))
        }
        other => quote(&other.to_string()),
    }
}

impl Query {
    pub fn parse(input: &str) -> Result<Query, AppError> {
        let toks = lex(input)?;
        if toks.is_empty() {
            return Ok(Query::all());
        }
        let mut p = Parser { toks, pos: 0 };
        let q = p.parse_or()?;
        if p.pos != p.toks.len() {
            return Err(AppError::InvalidQuery(format!(
                "表达式尾部有多余内容: pos {}",
                p.pos
            )));
        }
        Ok(q)
    }

    pub fn to_expr(&self) -> String {
        match self {
            Query::And(v) => join_expr(v, "AND"),
            Query::Or(v) => join_expr(v, "OR"),
            Query::Not(q) => {
                let inner = q.to_expr();
                if matches!(**q, Query::And(_) | Query::Or(_)) {
                    format!("NOT ({inner})")
                } else {
                    format!("NOT {inner}")
                }
            }
            Query::Cond(c) => c.to_expr(),
        }
    }
}

fn join_expr(v: &[Query], op: &str) -> String {
    v.iter()
        .map(|q| {
            let s = q.to_expr();
            if matches!(q, Query::Or(_)) && op == "AND" {
                format!("({s})")
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join(&format!(" {op} "))
}

impl Condition {
    fn to_expr(&self) -> String {
        let field = match &self.field {
            Field::Label(name) => name.clone(),
            Field::LabelInherited(name) => format!("{name}+"),
            other => canonical_name(other).to_string(),
        };
        match self.op {
            Op::Present => field,
            Op::Absent => format!("!{field}"),
            Op::Changed | Op::ChangedValue => {
                // 序列化时按 value 形状还原：from/to/during 三段都是可选。
                // Op::ChangedValue 必有 from/to；Op::Changed 无 from/to；
                // 两者都可挂 during。
                let mut parts: Vec<String> = Vec::new();
                if let Some(obj) = self.value.as_ref().and_then(|v| v.as_object()) {
                    if self.op == Op::ChangedValue {
                        let from = obj.get("from").unwrap_or(&serde_json::Value::Null);
                        let to = obj.get("to").unwrap_or(&serde_json::Value::Null);
                        parts.push(format!("from {}", expr_scalar(from)));
                        parts.push(format!("to {}", expr_scalar(to)));
                    }
                    if let Some(d) = obj.get("during") {
                        if !d.is_null() {
                            // 走解析路径拿回 Interval，再渲染成 `(T1, T2]` 字串。
                            if let Ok(iv) = serde_json::from_value::<Interval>(d.clone()) {
                                parts.push(format!("during {}", interval_to_expr(&iv)));
                            }
                        }
                    }
                }
                if parts.is_empty() {
                    format!("{field} changed")
                } else {
                    format!("{field} changed {}", parts.join(" "))
                }
            }
            _ => {
                let value = self.value.as_ref().map(expr_scalar).unwrap_or_default();
                format!("{field} {} {value}", expr_op(self.op))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Entry, LabelSchema, LabelValue, LabelValueType, Labeling};
    use ulid::Ulid;

    fn entry() -> Entry {
        Entry::new(Ulid::new(), "找回密码失败".to_string(), Ulid::new())
    }

    fn labeling(name: &str, value: serde_json::Value) -> Labeling {
        let lv = match value {
            serde_json::Value::Bool(b) => LabelValue::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    LabelValue::Int(i)
                } else {
                    LabelValue::Float(n.as_f64().unwrap())
                }
            }
            serde_json::Value::String(s) => LabelValue::Enum(s),
            _ => LabelValue::Null,
        };
        Labeling::new("CODE".to_string(), name.to_string(), lv, Ulid::new())
    }

    #[test]
    fn ast_serializes_to_camel_case_json() {
        let q = Query::And(vec![Query::Cond(Condition {
            field: Field::Label("Task".to_string()),
            op: Op::Ne,
            value: Some(serde_json::json!("Open")),
        })]);
        let json = serde_json::to_value(&q).unwrap();
        assert_eq!(json["and"][0]["cond"]["field"]["label"], "Task");
        assert_eq!(json["and"][0]["cond"]["op"], "ne");
        assert_eq!(serde_json::from_value::<Query>(json).unwrap(), q);
    }

    #[test]
    fn parse_and_to_expr_roundtrip() {
        let cases = [
            "Task = \"Open\"",
            "Task",
            "!Priority",
            "Task in (\"Open\", \"Done\")",
            "UpdatedAt >= \"2026-09-01\"",
            "text ~ \"检索\"",
            "Task = \"Open\" AND NOT !Priority",
            "(Task = \"Open\" OR Bug = \"Fixed\") AND UpdatedAt >= \"2026-09-01\"",
            r#"Title = "他说 \"你好\"""#,
        ];
        for src in cases {
            let q = Query::parse(src).unwrap_or_else(|e| panic!("parse {src} 失败: {e}"));
            let expr = q.to_expr();
            let again = Query::parse(&expr).unwrap_or_else(|e| panic!("re-parse {expr} 失败: {e}"));
            assert_eq!(q, again, "round-trip 不稳定: {src} -> {expr}");
        }
        // 内置元数据字段往返稳定。
        assert_eq!(
            Query::parse("Title ~ \"登录\"").unwrap().to_expr(),
            "Title ~ \"登录\""
        );
    }

    #[test]
    fn parse_integer_eq_matches_integer_label() {
        let e = entry();
        let labels = vec![labeling("Score", serde_json::json!(7))];
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let env = EvalEnv {
            text_hit: &never,
            account_of: &no_acct,
            label_of: &no_label,
            event: None,
            derived: &[],
            changed_during: None,
            changed_value: None,
        };
        assert!(Query::parse("Score = 7")
            .unwrap()
            .evaluate(&e, &labels, &env));
        assert!(!Query::parse("Score != 7")
            .unwrap()
            .evaluate(&e, &labels, &env));
    }

    #[test]
    fn string_escape_roundtrip() {
        // value contains both a backslash and escaped double-quotes
        let src = r#"Title = "a\b \"c\"""#;
        let q = Query::parse(src).unwrap();
        let val = match &q {
            Query::Cond(c) => c.value.as_ref().unwrap().as_str().unwrap().to_string(),
            _ => panic!("expected a single Cond"),
        };
        assert_eq!(val, r#"a\b "c""#);
        let expr = q.to_expr();
        let again = Query::parse(&expr).unwrap_or_else(|e| panic!("re-parse {expr} 失败: {e}"));
        assert_eq!(q, again, "escape round-trip 不稳定: {src} -> {expr}");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(Query::parse("Task =").is_err());
        assert!(Query::parse("(Task = \"a\"").is_err());
        assert!(Query::parse("Task >< \"a\"").is_err());
    }

    #[test]
    fn bare_label_is_present_and_bang_is_absent() {
        let e = entry();
        let labels = vec![labeling("Task", serde_json::json!("Open"))];
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let env = EvalEnv {
            text_hit: &never,
            account_of: &no_acct,
            label_of: &no_label,
            event: None,
            derived: &[],
            changed_during: None,
            changed_value: None,
        };

        let present = Query::parse("Task").unwrap();
        assert!(present.evaluate(&e, &labels, &env));
        let absent = Query::parse("!Task").unwrap();
        assert!(!absent.evaluate(&e, &labels, &env));
        assert!(Query::parse("!Priority")
            .unwrap()
            .evaluate(&e, &labels, &env));

        // 语法糖落成与旧函数等价的条件，且格式化后仍可回读。
        assert_eq!(Query::parse("Task").unwrap().to_expr(), "Task");
        assert_eq!(Query::parse("!Task").unwrap().to_expr(), "!Task");
        // `!` 后接比较时退化为普通取反。
        assert!(Query::parse("!(Task = \"Done\")")
            .unwrap()
            .evaluate(&e, &labels, &env));
        // 组合表达式中存在性判断不再需要括号。
        assert!(Query::parse("Task AND !Priority")
            .unwrap()
            .evaluate(&e, &labels, &env));
    }

    #[test]
    fn removed_present_and_absent_functions_are_rejected() {
        for src in [
            "present(Task)",
            "absent(Priority)",
            "Task AND present(Priority)",
        ] {
            let err = Query::parse(src).unwrap_err();
            assert!(
                err.to_string().contains("present"),
                "{src} 应当提示函数已移除，实际：{err}"
            );
        }
    }

    #[test]
    fn evaluate_present_absent_and_missing() {
        let e = entry();
        let labels = vec![labeling("Task", serde_json::json!("Open"))];
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let env = EvalEnv {
            text_hit: &never,
            account_of: &no_acct,
            label_of: &no_label,
            event: None,
            derived: &[],
            changed_during: None,
            changed_value: None,
        };

        let present = Query::Cond(Condition {
            field: Field::Label("Task".into()),
            op: Op::Present,
            value: None,
        });
        assert!(present.evaluate(&e, &labels, &env));
        // 打标缺失时比较一律 false
        let missing_cmp = Query::Cond(Condition {
            field: Field::Label("Priority".into()),
            op: Op::Eq,
            value: Some(serde_json::json!("P0")),
        });
        assert!(!missing_cmp.evaluate(&e, &labels, &env));
        let absent = Query::Cond(Condition {
            field: Field::Label("Priority".into()),
            op: Op::Absent,
            value: None,
        });
        assert!(absent.evaluate(&e, &labels, &env));
    }

    #[test]
    fn evaluate_comparisons_by_type() {
        let e = entry();
        let labels = vec![
            labeling("Task", serde_json::json!("Open")),
            labeling("Score", serde_json::json!(7)),
            labeling("Title", serde_json::json!("前后端联调")),
        ];
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let env = EvalEnv {
            text_hit: &never,
            account_of: &no_acct,
            label_of: &no_label,
            event: None,
            derived: &[],
            changed_during: None,
            changed_value: None,
        };
        let cond = |name: &str, op: Op, v: serde_json::Value| {
            Query::Cond(Condition {
                field: Field::Label(name.into()),
                op,
                value: Some(v),
            })
        };

        assert!(cond("Score", Op::Gt, serde_json::json!(5)).evaluate(&e, &labels, &env));
        assert!(!cond("Score", Op::Lt, serde_json::json!(5)).evaluate(&e, &labels, &env));
        assert!(cond("Title", Op::Contains, serde_json::json!("联调")).evaluate(&e, &labels, &env));
        assert!(
            cond("Task", Op::In, serde_json::json!(["Open", "Done"])).evaluate(&e, &labels, &env)
        );
        assert!(!cond("Task", Op::In, serde_json::json!(["Done"])).evaluate(&e, &labels, &env));
    }

    #[test]
    fn evaluate_time_and_boolean_logic() {
        let e = entry();
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let env = EvalEnv {
            text_hit: &never,
            account_of: &no_acct,
            label_of: &no_label,
            event: None,
            derived: &[],
            changed_during: None,
            changed_value: None,
        };
        let future = Query::Cond(Condition {
            field: Field::UpdatedAt,
            op: Op::Gt,
            value: Some(serde_json::json!("2099-01-01")),
        });
        assert!(!future.evaluate(&e, &[], &env));
        assert!(Query::Not(Box::new(future.clone())).evaluate(&e, &[], &env));
        assert!(Query::And(vec![]).evaluate(&e, &[], &env), "空 AND 恒真");
        assert!(!Query::Or(vec![]).evaluate(&e, &[], &env), "空 OR 恒假");
    }

    #[test]
    fn evaluate_text_uses_closure() {
        let e = entry();
        let q = Query::Cond(Condition {
            field: Field::Text,
            op: Op::Contains,
            value: Some(serde_json::json!("检索")),
        });
        let hit = |kw: &str| kw == "检索";
        let miss = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let hit_env = EvalEnv {
            text_hit: &hit,
            account_of: &no_acct,
            label_of: &no_label,
            event: None,
            derived: &[],
            changed_during: None,
            changed_value: None,
        };
        let miss_env = EvalEnv {
            text_hit: &miss,
            account_of: &no_acct,
            label_of: &no_label,
            event: None,
            derived: &[],
            changed_during: None,
            changed_value: None,
        };
        assert!(q.evaluate(&e, &[], &hit_env));
        assert!(!q.evaluate(&e, &[], &miss_env));
    }

    #[test]
    fn validate_rejects_unknown_label_and_bad_op() {
        let schemas = vec![LabelSchema::new(
            Ulid::new(),
            "Score".into(),
            "分数".into(),
            LabelValueType::Integer,
            vec![],
        )];
        let unknown = Query::Cond(Condition {
            field: Field::Label("Nope".into()),
            op: Op::Present,
            value: None,
        });
        assert!(unknown.validate(&schemas).is_err());

        let bad_op = Query::Cond(Condition {
            field: Field::Label("Score".into()),
            op: Op::Contains,
            value: Some(serde_json::json!("x")),
        });
        assert!(bad_op.validate(&schemas).is_err(), "Integer 不支持 ~");

        let ok = Query::Cond(Condition {
            field: Field::Label("Score".into()),
            op: Op::Ge,
            value: Some(serde_json::json!(5)),
        });
        assert!(ok.validate(&schemas).is_ok());
    }

    #[test]
    fn validate_text_only_contains() {
        let schemas: Vec<LabelSchema> = vec![];
        let ok = Query::Cond(Condition {
            field: Field::Text,
            op: Op::Contains,
            value: Some(serde_json::json!("检索")),
        });
        assert!(ok.validate(&schemas).is_ok());

        for bad_op in [Op::Eq, Op::Ne, Op::NotContains, Op::In] {
            let q = Query::Cond(Condition {
                field: Field::Text,
                op: bad_op,
                value: Some(serde_json::json!("检索")),
            });
            assert!(
                matches!(q.validate(&schemas), Err(AppError::InvalidQuery(_))),
                "{bad_op:?} 应当被拒绝"
            );
        }
    }

    #[test]
    fn validate_enum_in_candidates_must_be_in_enum_values() {
        let schemas = vec![LabelSchema::new(
            Ulid::new(),
            "Task".into(),
            "任务".into(),
            LabelValueType::Enum,
            vec!["Open".into(), "Done".into()],
        )];
        let ok = Query::Cond(Condition {
            field: Field::Label("Task".into()),
            op: Op::In,
            value: Some(serde_json::json!(["Open", "Done"])),
        });
        assert!(ok.validate(&schemas).is_ok());

        let out_of_range = Query::Cond(Condition {
            field: Field::Label("Task".into()),
            op: Op::In,
            value: Some(serde_json::json!(["Open", "Nope"])),
        });
        assert!(matches!(
            out_of_range.validate(&schemas),
            Err(AppError::InvalidQuery(_))
        ));

        let not_array = Query::Cond(Condition {
            field: Field::Label("Task".into()),
            op: Op::NotIn,
            value: Some(serde_json::json!("Open")),
        });
        assert!(matches!(
            not_array.validate(&schemas),
            Err(AppError::InvalidQuery(_))
        ));
    }

    #[test]
    fn validate_rejects_invalid_time_format() {
        let schemas: Vec<LabelSchema> = vec![];
        let bad = Query::Cond(Condition {
            field: Field::UpdatedAt,
            op: Op::Ge,
            value: Some(serde_json::json!("2026-13-40")),
        });
        assert!(matches!(
            bad.validate(&schemas),
            Err(AppError::InvalidQuery(_))
        ));

        let ok_date = Query::Cond(Condition {
            field: Field::CreatedAt,
            op: Op::Ge,
            value: Some(serde_json::json!("2026-09-01")),
        });
        assert!(ok_date.validate(&schemas).is_ok());

        let ok_rfc = Query::Cond(Condition {
            field: Field::UpdatedAt,
            op: Op::Le,
            value: Some(serde_json::json!("2026-09-01T00:00:00Z")),
        });
        assert!(ok_rfc.validate(&schemas).is_ok());
    }

    #[test]
    fn validate_rejects_multiple_text_keywords() {
        let schemas: Vec<LabelSchema> = vec![];
        let two = Query::And(vec![
            Query::Cond(Condition {
                field: Field::Text,
                op: Op::Contains,
                value: Some(serde_json::json!("a")),
            }),
            Query::Cond(Condition {
                field: Field::Text,
                op: Op::Contains,
                value: Some(serde_json::json!("b")),
            }),
        ]);
        assert!(matches!(
            two.validate(&schemas),
            Err(AppError::InvalidQuery(_))
        ));

        // 相同关键词只算一个，仍然允许。
        let same = Query::And(vec![
            Query::Cond(Condition {
                field: Field::Text,
                op: Op::Contains,
                value: Some(serde_json::json!("a")),
            }),
            Query::Cond(Condition {
                field: Field::Text,
                op: Op::Contains,
                value: Some(serde_json::json!("a")),
            }),
        ]);
        assert!(same.validate(&schemas).is_ok());

        let nested = Query::Not(Box::new(Query::Cond(Condition {
            field: Field::Text,
            op: Op::Contains,
            value: Some(serde_json::json!("x")),
        })));
        let nested_two = Query::And(vec![
            nested,
            Query::Cond(Condition {
                field: Field::Text,
                op: Op::Contains,
                value: Some(serde_json::json!("y")),
            }),
        ]);
        assert!(matches!(
            nested_two.validate(&schemas),
            Err(AppError::InvalidQuery(_))
        ));
    }
}
