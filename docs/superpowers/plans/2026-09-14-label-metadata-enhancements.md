# 标签与元数据增强 实施计划（A 簇）

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enum 标签支持多选；新增 Date / Time / DateTime / Currency / Email 五种值类型与 Go 布局串格式化；表达式输入框写到时间型键 + 比较运算符时浮出原生时间控件；Entry 详情展示创建人 / 创建时间 / 更新人 / 更新时间；`Code` / `Title` / `Detail` / `CreatedBy` / `CreatedAt` / `UpdatedBy` / `UpdatedAt` 成为内置元数据关键字（可查询、进 `/` 提示列表、禁止自定义标签重名）。

**Architecture:** 新增与 `chrono` 解耦的 `src/golayout.rs`（纯 `YmdHms` 结构，前后端共用）。`LabelValueType` / `LabelValue` / `LabelSchema` / `Field` 做**追加式**扩展（bincode 位置编码：结构体加字段 = 不兼容 → 开发期 `rm -rf data`；枚举变体只能追加在末尾）。后端求值引入 `EvalEnv`（全文命中 / 账号解析 / 标签类型与格式三个闭包）。前端不引用 `domain`，新增字段一律以 `serde_json::Value` 承载。

**Tech Stack:** Rust / Leptos 0.8（SSR + wasm hydrate）/ async-graphql 7 / RocksDB / bincode / serde_json / tantivy

**Spec:** `docs/superpowers/specs/2026-09-14-label-metadata-enhancements-design.md`

## Global Constraints

- **后端不写单元测试**（用户明确）；以后端 `cargo build` + 前端 wasm `cargo check` 为编译门。
- **已有**的单测（`src/domain/query.rs`、`src/domain/label.rs` 无关、`src/service/label.rs`、`src/service/entry.rs`、`src/service/view.rs`、`src/error.rs`）因签名变更而**必须同步改到能编译、能过**，不得删除。
- `src/domain/*`、`src/service/*`、`src/api/*` 仅 `ssr` 可编译；wasm 前端**不得**引用 `domain` 类型，一律以 `serde_json::Value` 承载。
- `src/golayout.rs` **不 feature-gate**（前后端都要用），且**不依赖 `chrono`**（`chrono` 是 ssr-only）。
- bincode 位置编码：`LabelValue` 的新变体**只能追加在末尾**；`LabelSchema` 新字段用 `#[serde(default)]`；`Entry` / `Workspace` **不改结构体**（账号信息从 `cf::ACCOUNTS` 现查）。
- 破坏性变更：开发期 `rm -rf data`（`make reset-data`）。已存**视图**存的是 AST，不受表达式语法变更影响；用户**手写**的 `updated >= "…"` / `created >= "…"` 表达式须改成 `UpdatedAt >= "…"` / `CreatedAt >= "…"`。
- 颜色串一律 `#rrggbb`。
- 本机 8GB/4 核：cargo 用绝对路径 `/Users/wangxiaoyan/.cargo/bin/cargo`，前缀 `CARGO_BUILD_JOBS=1`，Bash 命令设 `dangerouslyDisableSandbox: true`，输出重定向到文件再读；不要并发跑两个 cargo（构建锁死锁）。

### 编译门命令

```bash
# 后端（ssr）
CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib
# 前端（wasm hydrate）
CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib \
  --no-default-features --features hydrate --target wasm32-unknown-unknown
# 已有单测必须过
CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo test --lib
```

---

### Task 1: `src/golayout.rs` —— Go 布局串共享模块

**Files:**
- Create: `src/golayout.rs`
- Modify: `src/lib.rs`（在 `pub mod frontend;` 旁加 `pub mod golayout;`，**不 gate**）

**Interfaces:**
- Consumes: 无
- Produces:
  - `pub struct YmdHms { pub year: i32, pub month: u32, pub day: u32, pub hour: u32, pub minute: u32, pub second: u32 }`（`Debug, Clone, Copy, PartialEq, Eq, Default`）
  - `pub const DATE_LAYOUT: &str = "2006-01-02";`
  - `pub const TIME_LAYOUT: &str = "15:04:05";`
  - `pub const DATETIME_LAYOUT: &str = "2006-01-02 15:04:05";`
  - `pub fn format(layout: &str, t: YmdHms) -> String`
  - `pub fn parse(layout: &str, s: &str) -> Option<YmdHms>`

- [ ] **Step 1: 写 `src/golayout.rs`**

```rust
//! Go 参考时间布局串的最小实现：不依赖 chrono，后端与 wasm 前端共用。
//!
//! 按 token **最长匹配**扫描布局串，未识别的字符原样输出（格式化）/ 逐字符匹配（解析）。
//! 支持的 token：`2006` `06` `01` `1` `02` `2` `15` `03` `04` `05`
//! `Jan` `January` `Mon` `Monday` `PM` `pm`。

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct YmdHms {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

pub const DATE_LAYOUT: &str = "2006-01-02";
pub const TIME_LAYOUT: &str = "15:04:05";
pub const DATETIME_LAYOUT: &str = "2006-01-02 15:04:05";

#[derive(Clone, Copy, PartialEq)]
enum Tok {
    Year4, Year2, Month2, Month1, Day2, Day1,
    Hour24, Hour12, Minute, Second,
    MonAbbr, MonFull, WdAbbr, WdFull, PmUpper, PmLower,
}

/// 顺序即优先级：多字符 token 必须排在它的单字符前缀之前。
const TOKENS: &[(&str, Tok)] = &[
    ("January", Tok::MonFull),
    ("Monday", Tok::WdFull),
    ("2006", Tok::Year4),
    ("Jan", Tok::MonAbbr),
    ("Mon", Tok::WdAbbr),
    ("06", Tok::Year2),
    ("01", Tok::Month2),
    ("02", Tok::Day2),
    ("15", Tok::Hour24),
    ("03", Tok::Hour12),
    ("04", Tok::Minute),
    ("05", Tok::Second),
    ("PM", Tok::PmUpper),
    ("pm", Tok::PmLower),
    ("1", Tok::Month1),
    ("2", Tok::Day1),
];

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June",
    "July", "August", "September", "October", "November", "December",
];
const WEEKDAYS: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];

fn match_token(rest: &str) -> Option<(&'static str, Tok)> {
    TOKENS.iter().find(|(t, _)| rest.starts_with(t)).copied()
}

fn pad2(n: u32) -> String {
    format!("{n:02}")
}

/// Howard Hinnant days_from_civil：公历 → 距 1970-01-01 的天数。
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let (m, d) = (m as i64, d as i64);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn weekday(t: YmdHms) -> usize {
    ((days_from_civil(t.year, t.month, t.day) + 4).rem_euclid(7)) as usize
}

pub fn format(layout: &str, t: YmdHms) -> String {
    let mut out = String::new();
    let mut rest = layout;
    while !rest.is_empty() {
        if let Some((tok, kind)) = match_token(rest) {
            out.push_str(&render(kind, t));
            rest = &rest[tok.len()..];
        } else {
            let c = rest.chars().next().unwrap();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

fn render(kind: Tok, t: YmdHms) -> String {
    let month = MONTHS[(t.month.clamp(1, 12) - 1) as usize];
    match kind {
        Tok::Year4 => format!("{:04}", t.year),
        Tok::Year2 => format!("{:02}", t.year.rem_euclid(100)),
        Tok::Month2 => pad2(t.month),
        Tok::Month1 => t.month.to_string(),
        Tok::Day2 => pad2(t.day),
        Tok::Day1 => t.day.to_string(),
        Tok::Hour24 => pad2(t.hour),
        Tok::Hour12 => {
            let h = t.hour % 12;
            pad2(if h == 0 { 12 } else { h })
        }
        Tok::Minute => pad2(t.minute),
        Tok::Second => pad2(t.second),
        Tok::MonAbbr => month[..3].to_string(),
        Tok::MonFull => month.to_string(),
        Tok::WdAbbr => WEEKDAYS[weekday(t)][..3].to_string(),
        Tok::WdFull => WEEKDAYS[weekday(t)].to_string(),
        Tok::PmUpper => if t.hour < 12 { "AM" } else { "PM" }.to_string(),
        Tok::PmLower => if t.hour < 12 { "am" } else { "pm" }.to_string(),
    }
}

fn take_digits(s: &str, min: usize, max: usize) -> Option<(u64, &str)> {
    let b = s.as_bytes();
    let mut end = 0;
    while end < b.len() && end < max && b[end].is_ascii_digit() {
        end += 1;
    }
    if end < min || end == 0 {
        return None;
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// 取月份名（全称优先，否则 3 字母缩写），大小写不敏感。
fn take_month(rest: &str) -> Option<(u32, &str)> {
    if rest.len() < 3 || !rest.is_char_boundary(3) {
        return None;
    }
    for (i, name) in MONTHS.iter().enumerate() {
        if !name[..3].eq_ignore_ascii_case(&rest[..3]) {
            continue;
        }
        let take = if rest.len() >= name.len() && name.eq_ignore_ascii_case(&rest[..name.len()]) {
            name.len()
        } else {
            3
        };
        return Some(((i + 1) as u32, &rest[take..]));
    }
    None
}

/// 取星期名并丢弃（解析时不使用，但布局里可能出现）。
fn take_weekday(rest: &str) -> Option<&str> {
    if rest.len() < 3 || !rest.is_char_boundary(3) {
        return None;
    }
    for name in WEEKDAYS.iter() {
        if !name[..3].eq_ignore_ascii_case(&rest[..3]) {
            continue;
        }
        let take = if rest.len() >= name.len() && name.eq_ignore_ascii_case(&rest[..name.len()]) {
            name.len()
        } else {
            3
        };
        return Some(&rest[take..]);
    }
    None
}

pub fn parse(layout: &str, s: &str) -> Option<YmdHms> {
    let mut t = YmdHms::default();
    let (mut rest, mut lay) = (s, layout);
    let (mut pm, mut has_ampm) = (false, false);
    while !lay.is_empty() {
        if let Some((tok, kind)) = match_token(lay) {
            lay = &lay[tok.len()..];
            let (n, r) = match kind {
                Tok::Year4 => take_digits(rest, 4, 4).map(|(n, r)| (n, r))?,
                Tok::Year2 => take_digits(rest, 1, 2)?,
                Tok::Month2 | Tok::Month1 => take_digits(rest, 1, 2)?,
                Tok::Day2 | Tok::Day1 => take_digits(rest, 1, 2)?,
                Tok::Hour24 | Tok::Hour12 => take_digits(rest, 1, 2)?,
                Tok::Minute | Tok::Second => take_digits(rest, 1, 2)?,
                Tok::MonAbbr | Tok::MonFull => {
                    let (m, r) = take_month(rest)?;
                    t.month = m;
                    rest = r;
                    continue;
                }
                Tok::WdAbbr | Tok::WdFull => {
                    rest = take_weekday(rest)?;
                    continue;
                }
                Tok::PmUpper | Tok::PmLower => {
                    if rest.len() >= 2 && rest[..2].eq_ignore_ascii_case("PM") {
                        pm = true;
                    } else if rest.len() >= 2 && rest[..2].eq_ignore_ascii_case("AM") {
                        pm = false;
                    } else {
                        return None;
                    }
                    has_ampm = true;
                    rest = &rest[2..];
                    continue;
                }
            };
            rest = r;
            match kind {
                Tok::Year4 => t.year = n as i32,
                Tok::Year2 => t.year = 2000 + n as i32,
                Tok::Month2 | Tok::Month1 => t.month = n as u32,
                Tok::Day2 | Tok::Day1 => t.day = n as u32,
                Tok::Hour24 | Tok::Hour12 => t.hour = n as u32,
                Tok::Minute => t.minute = n as u32,
                Tok::Second => t.second = n as u32,
                _ => {}
            }
        } else {
            let c = lay.chars().next().unwrap();
            let got = rest.chars().next()?;
            // 空白宽松匹配，其余逐字符严格匹配。
            if !(c == got || (c.is_whitespace() && got.is_whitespace())) {
                return None;
            }
            lay = &lay[c.len_utf8()..];
            rest = &rest[got.len_utf8()..];
        }
    }
    if !rest.trim().is_empty() {
        return None;
    }
    if has_ampm {
        if pm && t.hour < 12 {
            t.hour += 12;
        } else if !pm && t.hour == 12 {
            t.hour = 0;
        }
    }
    if !(1..=12).contains(&t.month) || !(1..=31).contains(&t.day) || t.hour > 23 || t.minute > 59 || t.second > 60 {
        return None;
    }
    Some(t)
}
```

- [ ] **Step 2: `src/lib.rs` 注册模块**

在 `pub mod frontend;` 之后加一行：

```rust
pub mod golayout;
```

（**不要**放进 `#[cfg(feature = "ssr")]` 块——前端 wasm 也要用。）

- [ ] **Step 3: 编译门（两个目标）**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib`
Expected: PASS

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: PASS（证明该模块在 wasm 下也可编译，未拖入 chrono）

- [ ] **Step 4: 提交**

```bash
git add src/golayout.rs src/lib.rs
git commit -m "feat(golayout): add dependency-free Go layout time formatter"
```

---

### Task 2: 领域层 —— 标签新值类型与属性

**Files:**
- Modify: `src/domain/label.rs`

**Interfaces:**
- Consumes: `crate::golayout::{parse, DATE_LAYOUT, TIME_LAYOUT, DATETIME_LAYOUT, YmdHms}`
- Produces:
  - `LabelValueType { …, Date, Time, DateTime, Currency, Email }`，`as_str` / `from_str` 对应 `"date"` / `"time"` / `"datetime"` / `"currency"` / `"email"`
  - `LabelValue { …, EnumList(Vec<String>), Date(String), Time(String), DateTime(String), Currency(f64), Email(String) }`（**追加在末尾**）
  - `LabelSchema { …, multi: bool, format: Option<String>, currency_symbol: Option<String>, unit: Option<String> }`
  - `LabelSchema::with_attrs(self, multi, format, currency_symbol, unit) -> Self`
  - `pub fn default_layout(vt: LabelValueType) -> &'static str`
  - `resolve_color` 支持数组值

- [ ] **Step 1: 扩展 `LabelValueType`**

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LabelValueType {
    Null,
    Boolean,
    Integer,
    Float,
    String,
    Enum,
    // 新增：只能追加在末尾（bincode 位置编码）
    Date,
    Time,
    DateTime,
    Currency,
    Email,
}
```

`as_str` 补 `Date => "date"`、`Time => "time"`、`DateTime => "datetime"`、`Currency => "currency"`、`Email => "email"`；`from_str` 补反查。

- [ ] **Step 2: 扩展 `LabelValue`（末尾追加）**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LabelValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Enum(String),
    // 追加，勿插入到上方
    EnumList(Vec<String>),
    Date(String),
    Time(String),
    DateTime(String),
    Currency(f64),
    Email(String),
}
```

- [ ] **Step 3: 改 `from_json`**

在 `match schema.value_type` 里加分支（`Enum` 分支要重写以支持 `multi`）：

```rust
LabelValueType::Enum => {
    if schema.multi {
        let vals = match value {
            serde_json::Value::Array(a) => a
                .iter()
                .map(|x| x.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or(AppError::InvalidLabelValue)?,
            serde_json::Value::String(s) => vec![s.clone()],
            _ => return Err(AppError::InvalidLabelValue),
        };
        if vals.iter().any(|v| !schema.enum_values.iter().any(|e| e == v)) {
            return Err(AppError::InvalidLabelValue);
        }
        Ok(LabelValue::EnumList(vals))
    } else {
        let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
        if !schema.enum_values.iter().any(|v| v == s) {
            return Err(AppError::InvalidLabelValue);
        }
        Ok(LabelValue::Enum(s.to_string()))
    }
}
LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime => {
    let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
    let layout = schema
        .format
        .as_deref()
        .unwrap_or_else(|| default_layout(schema.value_type));
    crate::golayout::parse(layout, s).ok_or(AppError::InvalidLabelValue)?;
    Ok(match schema.value_type {
        LabelValueType::Date => LabelValue::Date(s.to_string()),
        LabelValueType::Time => LabelValue::Time(s.to_string()),
        _ => LabelValue::DateTime(s.to_string()),
    })
}
LabelValueType::Currency => value
    .as_f64()
    .map(LabelValue::Currency)
    .ok_or(AppError::InvalidLabelValue),
LabelValueType::Email => {
    let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
    let mut parts = s.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(AppError::InvalidLabelValue);
    };
    if local.is_empty() || domain.is_empty() {
        return Err(AppError::InvalidLabelValue);
    }
    Ok(LabelValue::Email(s.to_string()))
}
```

同步补 `pub fn default_layout`：

```rust
/// 时间型标签的默认展示布局（schema.format 缺省时使用）。
pub fn default_layout(vt: LabelValueType) -> &'static str {
    match vt {
        LabelValueType::Date => crate::golayout::DATE_LAYOUT,
        LabelValueType::Time => crate::golayout::TIME_LAYOUT,
        LabelValueType::DateTime => crate::golayout::DATETIME_LAYOUT,
        _ => "",
    }
}
```

- [ ] **Step 4: 改 `to_json`**

```rust
LabelValue::EnumList(v) => serde_json::Value::Array(
    v.iter().map(|s| serde_json::Value::String(s.clone())).collect(),
),
LabelValue::Date(s) | LabelValue::Time(s) | LabelValue::DateTime(s) | LabelValue::Email(s) => {
    serde_json::Value::String(s.clone())
}
LabelValue::Currency(f) => serde_json::json!(f),
```

- [ ] **Step 5: 扩展 `LabelSchema` 与 `with_attrs`**

```rust
pub struct LabelSchema {
    pub workspace_id: Ulid,
    pub name: String,
    pub title: String,
    pub value_type: LabelValueType,
    pub enum_values: Vec<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub value_colors: Vec<ValueColor>,
    // 追加在末尾，全部 #[serde(default)]
    #[serde(default)]
    pub multi: bool, // 仅 Enum 有效
    #[serde(default)]
    pub format: Option<String>, // 时间型 = Go 布局；Currency 可留空
    #[serde(default)]
    pub currency_symbol: Option<String>, // 缺省 ¥（仅展示用）
    #[serde(default)]
    pub unit: Option<String>, // 如「元」「万」（仅展示用）
}
```

`new()` 里补四个默认值（`false` / `None` / `None` / `None`）。新增：

```rust
/// 链式设置类型属性，与 `with_colors` 并列。
pub fn with_attrs(
    mut self,
    multi: bool,
    format: Option<String>,
    currency_symbol: Option<String>,
    unit: Option<String>,
) -> Self {
    self.multi = multi;
    self.format = format;
    self.currency_symbol = currency_symbol;
    self.unit = unit;
    self
}
```

- [ ] **Step 6: `resolve_color` 支持数组值（多选 Enum）**

把 `value.as_str() == Some(want.as_str())` 换成：

```rust
let hit = if let Some(want) = &vc.value {
    match value {
        serde_json::Value::Array(a) => {
            a.iter().any(|x| x.as_str() == Some(want.as_str()))
        }
        _ => value.as_str() == Some(want.as_str()),
    }
} else if let Some(v) = value.as_f64() {
    vc.min.map_or(true, |lo| v >= lo) && vc.max.map_or(true, |hi| v < hi)
} else {
    false
};
```

- [ ] **Step 7: 编译门**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib`
Expected: 报 `src/domain/query.rs` 的穷尽匹配缺分支（`type_label` / `op_allowed`）——Task 3 修；其余通过。

- [ ] **Step 8: 提交**

```bash
git add src/domain/label.rs
git commit -m "feat(domain): add label value types date/time/datetime/currency/email and enum multi"
```

---

### Task 3: 领域层 —— 内置元数据字段、`EvalEnv`、多值集合语义

**Files:**
- Modify: `src/domain/query.rs`
- Modify: `src/domain/mod.rs`（导出 `EvalEnv`、`RESERVED_FIELDS`）

**Interfaces:**
- Consumes: `crate::golayout::{parse, YmdHms}`、`crate::domain::label::default_layout`
- Produces:
  - `Field { Label(String), UpdatedAt, CreatedAt, Text, Code, Title, Detail, CreatedBy, UpdatedBy }`
  - `pub const RESERVED_FIELDS: [&str; 7]`
  - `pub struct EvalEnv<'a> { pub text_hit: &'a dyn Fn(&str) -> bool, pub account_of: &'a dyn Fn(Ulid) -> Option<(String, String)>, pub label_of: &'a dyn Fn(&str) -> Option<(LabelValueType, Option<String>)> }`
  - `Query::evaluate(&self, entry: &Entry, labels: &[Labeling], env: &EvalEnv) -> bool`
  - `Query::contains_account_field(&self) -> bool`

> **与 spec §5.4 的偏差（有意）**：spec 只列了 `text_hit` / `account_of` 两个闭包。这里加第三个 `label_of`，否则 Date / Time / DateTime 标签的 `>` `<` 只能按字符串字面比较（自定义布局下会给出错误顺序）。`label_of` 返回该标签的值类型与时间格式。

- [ ] **Step 1: 扩展 `Field` 与保留名常量**

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
}

/// 内置元数据关键字（大小写不敏感）。供词法器与标签保留名校验共用。
pub const RESERVED_FIELDS: [&str; 7] = [
    "Code", "Title", "Detail", "CreatedBy", "CreatedAt", "UpdatedBy", "UpdatedAt",
];
```

序列化结果：`{"code"}` / `{"title"}` / `{"detail"}` / `{"createdBy"}` / `{"updatedBy"}`（已存视图 AST 不受影响）。

- [ ] **Step 2: 加 `contains_account_field`**

```rust
pub fn contains_account_field(&self) -> bool {
    match self {
        Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_account_field),
        Query::Not(q) => q.contains_account_field(),
        Query::Cond(c) => matches!(c.field, Field::CreatedBy | Field::UpdatedBy),
    }
}
```

- [ ] **Step 3: 引入 `EvalEnv` 并改 `evaluate` 签名**

```rust
/// 求值环境：条目本身之外的所有外部依赖。
pub struct EvalEnv<'a> {
    /// 全文检索命中判定（由 tantivy 提供）。
    pub text_hit: &'a dyn Fn(&str) -> bool,
    /// 账号 id → (显示名, 邮箱)；未知账号返回 None。
    pub account_of: &'a dyn Fn(Ulid) -> Option<(String, String)>,
    /// 标签名 → (值类型, 时间格式)；未知返回 None。时间型标签比较需要它。
    pub label_of: &'a dyn Fn(&str) -> Option<(LabelValueType, Option<String>)>,
}

impl Query {
    pub fn evaluate(&self, entry: &Entry, labels: &[Labeling], env: &EvalEnv) -> bool {
        match self {
            Query::And(v) => v.iter().all(|q| q.evaluate(entry, labels, env)),
            Query::Or(v) => v.iter().any(|q| q.evaluate(entry, labels, env)),
            Query::Not(q) => !q.evaluate(entry, labels, env),
            Query::Cond(c) => c.evaluate(entry, labels, env),
        }
    }
}
```

- [ ] **Step 4: 词法器识别 7 个关键字（大小写不敏感）**

`Tok` 加一个变体并**删除** `Updated` / `Created`：

```rust
enum Tok {
    // …原有：LParen RParen Comma Eq Ne Gt Ge Lt Le Tilde NotTilde Bang And Or Not In Text…
    Builtin(Field),
    Ident(String),
    Str(String),
    Num(f64),
    Bool(bool),
}
```

词法器的关键字匹配改成：

```rust
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
    _ => out.push(Tok::Ident(word)),
}
```

`tok_label` 删掉 `Updated`/`Created` 两臂，加：

```rust
Some(Tok::Builtin(f)) => canonical_name(f).to_string(),
```

- [ ] **Step 5: 解析器认内置字段**

`parse_primary`：`Some(Tok::Updated) | Some(Tok::Created) | Some(Tok::Ident(_))` → `Some(Tok::Builtin(_)) | Some(Tok::Ident(_))`。

`parse_condition` 取字段：

```rust
let field = match self.next() {
    Some(Tok::Builtin(f)) => f,
    Some(Tok::Ident(s)) => Field::Label(s),
    other => {
        return Err(AppError::InvalidQuery(format!(
            "期望字段，实际 {}",
            tok_label(other.as_ref())
        )))
    }
};
```

`Field::Label` 的存在性语法糖逻辑保持不动（内置字段后必须接运算符）。

- [ ] **Step 6: `to_expr` 输出规范名**

```rust
pub fn canonical_name(f: &Field) -> &'static str {
    match f {
        Field::Label(_) => "",
        Field::Code => "Code",
        Field::Title => "Title",
        Field::Detail => "Detail",
        Field::CreatedBy => "CreatedBy",
        Field::CreatedAt => "CreatedAt",
        Field::UpdatedBy => "UpdatedBy",
        Field::UpdatedAt => "UpdatedAt",
        Field::Text => "text",
    }
}
```

`Condition::to_expr` 里 `Field::Label(name) => name.clone()` 之外全部走 `canonical_name(self.field)`。

- [ ] **Step 7: `op_allowed` 覆盖新类型**

```rust
fn op_allowed(vt: LabelValueType, op: Op) -> bool {
    use LabelValueType::*;
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
        String | Enum | Email => matches!(
            op,
            Op::Eq | Op::Ne | Op::Contains | Op::NotContains | Op::In | Op::NotIn
        ),
    };
    existence || cmp
}

/// 内置文本型元数据（Code/Title/Detail/CreatedBy/UpdatedBy）允许的运算符。
fn string_field_op_allowed(op: Op) -> bool {
    matches!(op, Op::Eq | Op::Ne | Op::Contains | Op::NotContains | Op::In | Op::NotIn)
}
```

`type_label` 补 `Date => "日期"`、`Time => "时间"`、`DateTime => "日期时间"`、`Currency => "金额"`、`Email => "邮箱"`。

- [ ] **Step 8: `Condition::validate` 覆盖内置字段与新型标签**

`Field::UpdatedAt | Field::CreatedAt` 分支不变（`parse_time` 已扩展，见 Step 9）。在其后加：

```rust
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
                return Err(AppError::InvalidQuery("in / not in 的值须为数组".to_string()));
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
```

`Field::Label(name)` 分支在 `op_allowed` 之后追加类型相关校验：

```rust
match schema.value_type {
    LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime => {
        if let Some(v) = &self.value {
            let s = v.as_str().unwrap_or("");
            let layout = schema
                .format
                .as_deref()
                .unwrap_or_else(|| crate::domain::label::default_layout(schema.value_type));
            if crate::golayout::parse(layout, s).is_none() {
                return Err(AppError::InvalidQuery(format!("时间格式无效: {s}")));
            }
        }
    }
    LabelValueType::Currency => {
        if let Some(v) = &self.value {
            if v.as_f64().is_none() {
                return Err(AppError::InvalidQuery("金额标签的比较值必须是数值".to_string()));
            }
        }
    }
    LabelValueType::Email => {
        if matches!(self.op, Op::In | Op::NotIn) {
            if !self.value.as_ref().is_some_and(|v| v.is_array()) {
                return Err(AppError::InvalidQuery("in / not in 的值须为数组".to_string()));
            }
        } else if self.value.as_ref().is_some_and(|v| !v.is_string()) {
            return Err(AppError::InvalidQuery("比较值须为字符串".to_string()));
        }
    }
    _ => {}
}
```

（Enum 的 `in` 候选必须 ∈ `enum_values` 的既有校验保持不动。）

- [ ] **Step 9: 扩展 `parse_time`**

```rust
fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    for (fmt, has_time) in [
        ("%Y-%m-%d %H:%M:%S", true),
        ("%Y-%m-%d %H:%M", true),
        ("%Y-%m-%d", false),
        ("%H:%M:%S", true),
        ("%H:%M", true),
    ] {
        if has_time {
            if fmt.starts_with("%H") {
                continue; // 纯时间无法映射到时刻，交给 NaiveDate 分支外的调用方
            }
            if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
                return Some(dt.and_utc());
            }
        } else if let Ok(d) = NaiveDate::parse_from_str(s, fmt) {
            return d.and_hms_opt(0, 0, 0).map(|dt| dt.and_utc());
        }
    }
    None
}
```

> 纯时间（`15:04`）不作为 `UpdatedAt` 的比较值；`CreatedAt` / `UpdatedAt` 的 `parse_time` 只需接受 RFC3339 与日期、以及日期+时间。

- [ ] **Step 10: 多值集合语义 —— 重写 `cmp_value`**

```rust
/// 把标签值归一成字符串集合：EnumList 是数组，单值即 1 元素，非字符串值给空集。
fn elem_strings(got: &serde_json::Value) -> Vec<String> {
    match got {
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::String(s) => vec![s.clone()],
        _ => Vec::new(),
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
            if op == Op::Eq { hit } else { !hit }
        }
        Op::Contains | Op::NotContains => {
            let Some(b) = want.as_str() else { return false };
            let needle = b.to_lowercase();
            let hit = elem_strings(got)
                .iter()
                .any(|s| s.to_lowercase().contains(&needle));
            if op == Op::Contains { hit } else { !hit }
        }
        Op::In | Op::NotIn => {
            let Some(list) = want.as_array() else { return false };
            let cand: Vec<String> = list
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
            let hit = elem_strings(got).iter().any(|s| cand.iter().any(|w| w == s));
            if op == Op::In { hit } else { !hit }
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
        Op::Present | Op::Absent => false,
    }
}
```

- [ ] **Step 11: `Condition::evaluate` 覆盖全部字段**

```rust
fn evaluate(&self, entry: &Entry, labels: &[Labeling], env: &EvalEnv) -> bool {
    match &self.field {
        Field::Text => self
            .value
            .as_ref()
            .and_then(|v| v.as_str())
            .map(env.text_hit)
            .unwrap_or(false),
        Field::UpdatedAt => cmp_time(&entry.updated_at, self.op, self.value.as_ref()),
        Field::CreatedAt => cmp_time(&entry.created_at, self.op, self.value.as_ref()),
        Field::Code => cmp_value(&serde_json::Value::String(entry.code.clone()), self.op, self.value.as_ref()),
        Field::Title => cmp_value(&serde_json::Value::String(entry.title.clone()), self.op, self.value.as_ref()),
        Field::Detail => cmp_value(&serde_json::Value::String(entry.detail.clone()), self.op, self.value.as_ref()),
        Field::CreatedBy | Field::UpdatedBy => {
            let id = if matches!(self.field, Field::CreatedBy) {
                entry.created_by
            } else {
                entry.updated_by
            };
            let Some((name, email)) = (env.account_of)(id) else { return false };
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
        Field::Label(name) => match self.op {
            Op::Present => labels.iter().any(|l| &l.label_name == name),
            Op::Absent => !labels.iter().any(|l| &l.label_name == name),
            _ => {
                let Some(l) = labels.iter().find(|l| &l.label_name == name) else {
                    return false;
                };
                // 时间型标签按布局解析成时刻再比，避免自定义布局下字符串比较出错。
                if let Some((vt, fmt)) = (env.label_of)(name) {
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
                        return cmp_time_layout(&l.value.to_json(), layout, self.op, self.value.as_ref());
                    }
                }
                cmp_value(&l.value.to_json(), self.op, self.value.as_ref())
            }
        },
    }
}

fn acc_eq(name: &str, email: &str, want: Option<&serde_json::Value>) -> bool {
    let Some(w) = want.and_then(|v| v.as_str()) else { return false };
    name.eq_ignore_ascii_case(w) || email.eq_ignore_ascii_case(w)
}

fn acc_contains(name: &str, email: &str, want: Option<&serde_json::Value>) -> bool {
    let Some(w) = want.and_then(|v| v.as_str()) else { return false };
    let w = w.to_lowercase();
    name.to_lowercase().contains(&w) || email.to_lowercase().contains(&w)
}

fn acc_in(name: &str, email: &str, want: Option<&serde_json::Value>) -> bool {
    let Some(list) = want.and_then(|v| v.as_array()) else { return false };
    list.iter().any(|x| {
        x.as_str()
            .is_some_and(|w| name.eq_ignore_ascii_case(w) || email.eq_ignore_ascii_case(w))
    })
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
```

- [ ] **Step 12: 更新已有单测到新签名（**必做**，不得删除）**

- 全部 `x.evaluate(&e, &labels, &never)` / `evaluate(&e, &[], &never)` / `q.evaluate(&e, &[], &|kw| …)` 改为构造 `EvalEnv`：

```rust
fn env<'a>(
    text: &'a dyn Fn(&str) -> bool,
    account: &'a dyn Fn(Ulid) -> Option<(String, String)>,
    label: &'a dyn Fn(&str) -> Option<(LabelValueType, Option<String>)>,
) -> EvalEnv<'a> {
    EvalEnv { text_hit: text, account_of: account, label_of: label }
}
```

各测试内改用：

```rust
let never = |_: &str| false;
let no_acct = |_: Ulid| None;
let no_label = |_: &str| None;
let env = EvalEnv { text_hit: &never, account_of: &no_acct, label_of: &no_label };
assert!(Query::parse("Score = 7").unwrap().evaluate(&e, &labels, &env));
```

`evaluate_text_uses_closure` 里 `|kw| kw == "检索"` 需先 `let hit = |kw: &str| kw == "检索";` 再放进 `EvalEnv`。

- `parse_and_to_expr_roundtrip` 的用例把 `updated >= "2026-09-01"` 改成 `UpdatedAt >= "2026-09-01"`，第二个用例里的 `updated >= "2026-09-01"` 同样改。
- `validate_rejects_invalid_time_format` 不变（`CreatedAt` / `UpdatedAt` 已是新写法）。
- 新增一个内置字段的往返用例（放在同一测试内即可）：

```rust
assert_eq!(Query::parse("Title ~ \"登录\"").unwrap().to_expr(), "Title ~ \"登录\"");
```

- [ ] **Step 13: `src/domain/mod.rs` 导出**

```rust
pub use label::{default_layout, resolve_color, LabelSchema, LabelValue, LabelValueType, Labeling, ValueColor};
pub use query::{Condition, EvalEnv, Field, Op, Query, RESERVED_FIELDS};
```

- [ ] **Step 14: 编译门 + 已有单测**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo test --lib`
Expected: 编译通过；`domain::query` 下的单测全绿（`src/service/*` 里调用 `query.evaluate` 的地方此时仍编译失败 —— Task 6 修）。

- [ ] **Step 15: 提交**

```bash
git add src/domain/query.rs src/domain/mod.rs
git commit -m "feat(domain): builtin metadata fields, EvalEnv, multi-value set semantics"
```

---

### Task 4: 服务层 —— 标签 schema 输入结构与保留名校验

**Files:**
- Modify: `src/error.rs`
- Modify: `src/service/label.rs`
- Modify: `src/service/entry.rs`（仅测试辅助 `add_status_schema`）
- Modify: `src/service/view.rs`（仅测试里的 `create_schema` 调用）

**Interfaces:**
- Consumes: `crate::domain::{LabelSchema, LabelValueType, ValueColor, RESERVED_FIELDS, default_layout}`、`crate::golayout`
- Produces:
  - `AppError::LabelNameReserved`（code `LABEL_NAME_RESERVED`，消息「与内置元数据重名」）
  - `pub struct LabelSchemaInput { pub name: String, pub title: String, pub value_type: LabelValueType, pub enum_values: Vec<String>, pub multi: bool, pub format: Option<String>, pub currency_symbol: Option<String>, pub unit: Option<String>, pub color: Option<String>, pub value_colors: Vec<ValueColor> }`
  - `LabelService::create_schema(&self, actor: Ulid, ws_id: Ulid, input: LabelSchemaInput) -> Result<LabelSchema, AppError>`
  - `LabelService::update_schema(&self, actor: Ulid, ws_id: Ulid, input: LabelSchemaInput) -> Result<LabelSchema, AppError>`（`input.name` 为键；`input.value_type` 忽略，沿用库中已有类型）

- [ ] **Step 1: `AppError` 加变体**

```rust
#[error("与内置元数据重名")]
LabelNameReserved,
```

`code()` 里登记：

```rust
AppError::LabelNameReserved => "LABEL_NAME_RESERVED",
```

（`src/error.rs` 的 `#[cfg(test)]` 单测不受影响。）

- [ ] **Step 2: `LabelSchemaInput` 与属性校验**

```rust
/// 标签属性的集中校验：保留名、multi 与时间布局。
fn validate_attrs(input: &LabelSchemaInput) -> Result<(), AppError> {
    if RESERVED_FIELDS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(input.name.trim()))
    {
        return Err(AppError::LabelNameReserved);
    }
    if input.multi && input.value_type != LabelValueType::Enum {
        return Err(AppError::InvalidQuery("「多选」仅适用于枚举标签".to_string()));
    }
    if matches!(
        input.value_type,
        LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime
    ) {
        if let Some(layout) = input.format.as_deref() {
            let probe = crate::golayout::YmdHms {
                year: 2006, month: 1, day: 2, hour: 15, minute: 4, second: 5,
            };
            let rendered = crate::golayout::format(layout, probe);
            // 布局必须能往返：否则它既格式化不出东西，也解析不回来。
            if crate::golayout::parse(layout, &rendered).is_none() {
                return Err(AppError::InvalidQuery(format!("时间布局无效: {layout}")));
            }
        }
    }
    Ok(())
}
```

> `default_layout` 的 `#[allow(unused_imports)]` 不需要——`validate_attrs` 不用它，但同文件其他位置（若用）需要。若 `cargo check` 报未使用，删掉 import。

- [ ] **Step 3: 改写 `create_schema` / `update_schema` 签名**

```rust
pub fn create_schema(
    &self,
    actor: Ulid,
    ws_id: Ulid,
    input: LabelSchemaInput,
) -> Result<LabelSchema, AppError> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err(AppError::Internal("标签名称不能为空".to_string()));
    }
    if self.get_schema(ws_id, name)?.is_some() {
        return Err(AppError::LabelNameExists);
    }
    validate_attrs(&input)?;
    validate_colors(
        input.value_type,
        &input.enum_values,
        &input.color,
        &input.value_colors,
    )?;
    let schema = LabelSchema::new(
        ws_id,
        name.to_string(),
        input.title.trim().to_string(),
        input.value_type,
        input.enum_values,
    )
    .with_colors(input.color, input.value_colors)
    .with_attrs(input.multi, input.format, input.currency_symbol, input.unit);
    // …审计与写盘保持原样…
    Ok(schema)
}

pub fn update_schema(
    &self,
    actor: Ulid,
    ws_id: Ulid,
    input: LabelSchemaInput,
) -> Result<LabelSchema, AppError> {
    let name = input.name.trim();
    let mut schema = self.get_schema(ws_id, name)?.ok_or(AppError::NotFound)?;
    let mut check = LabelSchemaInput { value_type: schema.value_type, ..input };
    check.name = name.to_string();
    validate_attrs(&check)?;
    validate_colors(schema.value_type, &check.enum_values, &check.color, &check.value_colors)?;
    let before = serde_json::to_string(&schema).unwrap_or_default();
    schema.title = check.title.trim().to_string();
    schema.enum_values = check.enum_values;
    schema.color = check.color;
    schema.value_colors = check.value_colors;
    schema.multi = check.multi;
    schema.format = check.format;
    schema.currency_symbol = check.currency_symbol;
    schema.unit = check.unit;
    // …审计与写盘保持原样（审计 resource_id 用 name）…
    Ok(schema)
}
```

> `LabelSchemaInput` 需 `#[derive(Clone)]`（用于上面的结构更新语法）。

- [ ] **Step 4: 更新既有单测调用点（**必做**）**

`src/service/label.rs` 测试里 7 处、`src/service/entry.rs::add_status_schema`、`src/service/view.rs::view_with_value_conditions_survives_save_and_read_back` 各 1 处，全部改成结构体入参。例如：

```rust
.create_schema(actor, ws_id, LabelSchemaInput {
    name: "Priority".into(), title: "优先级".into(),
    value_type: LabelValueType::Enum,
    enum_values: vec!["High".into(), "Low".into()],
    multi: false, format: None, currency_symbol: None, unit: None,
    color: None, value_colors: vec![],
})
```

`update_schema` 的两处同理（`src/service/label.rs` 的 `create_list_and_update_schema`、`update_schema_on_missing_name_returns_not_found`）。

**新增一条断言**（放在 `create_list_and_update_schema` 里）：保留名被拒。

```rust
let reserved = svc
    .create_schema(actor, ws_id, LabelSchemaInput {
        name: "Title".into(), title: "标题".into(),
        value_type: LabelValueType::String, enum_values: vec![],
        multi: false, format: None, currency_symbol: None, unit: None,
        color: None, value_colors: vec![],
    })
    .unwrap_err();
assert!(matches!(reserved, AppError::LabelNameReserved));
```

**新增一条断言**：multi 用于非 Enum 被拒（`InvalidQuery`）。

- [ ] **Step 5: 编译门 + 单测**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo test --lib`
Expected: `service::label` / `service::view` 单测绿；`api/graphql.rs` 与 `service/entry.rs` 生产代码仍编译失败（Task 5/6 修）。

- [ ] **Step 6: 提交**

```bash
git add src/error.rs src/service/label.rs src/service/entry.rs src/service/view.rs
git commit -m "feat(service): LabelSchemaInput, reserved-name and type-attr validation"
```

---

### Task 5: 服务层 —— 查询求值接线与检索索引多值

**Files:**
- Modify: `src/service/entry.rs`
- Modify: `src/service/search.rs`

**Interfaces:**
- Consumes: `crate::domain::{Account, EvalEnv, LabelSchema, LabelValueType, Query, RESERVED_FIELDS}`、`crate::storage::cf`
- Produces: `EntryService::query` 内部构建 `EvalEnv`（账号映射 + 标签类型/格式映射）

- [ ] **Step 1: `EntryService::query` 构建求值环境**

在 `query` 里 `labels_map` 之后、`matched` 过滤之前插入：

```rust
// 账号解析只在查询树真的引用 CreatedBy / UpdatedBy 时才扫表。
let accounts: std::collections::HashMap<ulid::Ulid, (String, String)> =
    if query.contains_account_field() {
        let mut m = std::collections::HashMap::new();
        for (_, v) in self.store.scan_prefix(cf::ACCOUNTS, b"")? {
            let a: crate::domain::Account = bincode::deserialize(&v)?;
            m.insert(a.id, (a.name, a.email));
        }
        m
    } else {
        std::collections::HashMap::new()
    };
// 标签名 → (值类型, 时间格式)，供时间型标签的 >/< 比较取布局。
let schemas: std::collections::HashMap<String, (crate::domain::LabelValueType, Option<String>)> =
    self.store
        .scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())?
        .into_iter()
        .filter_map(|(_, v)| bincode::deserialize::<crate::domain::LabelSchema>(&v).ok())
        .map(|s| (s.name, (s.value_type, s.format)))
        .collect();
let account_of = |id: ulid::Ulid| accounts.get(&id).cloned();
let label_of = |name: &str| schemas.get(name).cloned();
```

过滤闭包改成：

```rust
let mut matched: Vec<Entry> = rows
    .into_iter()
    .filter(|e| {
        let labels = labels_of(&e.code);
        let text_ok = |kw: &str| match &text_hits {
            Some((keyword, set)) => kw == keyword && set.contains(&e.code),
            None => false,
        };
        let env = EvalEnv {
            text_hit: &text_ok,
            account_of: &account_of,
            label_of: &label_of,
        };
        query.evaluate(e, labels, &env)
    })
    .collect();
```

顶部 `use` 增加 `EvalEnv`。

- [ ] **Step 2: `SearchIndex::add_entry_doc` 数组值改为空格连接**

```rust
let label_text = labels
    .iter()
    .map(|l| {
        let v = match &l.value {
            crate::domain::LabelValue::EnumList(v) => v.join(" "),
            other => match other.to_json() {
                serde_json::Value::String(s) => s,
                o => o.to_string(),
            },
        };
        format!("{} {}", l.label_name, v)
    })
    .collect::<Vec<_>>()
    .join(" ");
```

> 当前实现把 `EnumList` 序列化成 `["a","b"]` 字符串，tantivy 分词后会产生噪声 token。

- [ ] **Step 3: 编译门 + 单测**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo test --lib`
Expected: `service::entry` 单测绿；`api/graphql.rs` 仍编译失败（Task 6 修）。

- [ ] **Step 4: 提交**

```bash
git add src/service/entry.rs src/service/search.rs
git commit -m "feat(service): wire account/label maps into query eval; index enum lists by element"
```

---

### Task 6: GraphQL 层 —— 新属性、账号字段、错误

**Files:**
- Modify: `src/api/graphql.rs`

**Interfaces:**
- Consumes: `crate::service::label::LabelSchemaInput`
- Produces:
  - `GqlLabelSchema { …, multi: bool, format: Option<String>, currency_symbol: Option<String>, unit: Option<String> }`
  - `GqlEntry { …, created_by_account: Option<GqlAccount>, updated_by_account: Option<GqlAccount> }`
  - `input LabelSchemaAttrsInput { multi, format, currencySymbol, unit }`
  - `createLabelSchema(…, attrs: LabelSchemaAttrsInput)` / `updateLabelSchema(…, attrs: LabelSchemaAttrsInput)`

- [ ] **Step 1: `GqlLabelSchema` 加字段**

```rust
#[derive(SimpleObject, Clone)]
pub struct GqlLabelSchema {
    name: String,
    title: String,
    value_type: String,
    enum_values: Vec<String>,
    color: Option<String>,
    value_colors: Json<serde_json::Value>,
    multi: bool,
    format: Option<String>,
    currency_symbol: Option<String>,
    unit: Option<String>,
}
```

`From<LabelSchema>` 里补 `multi: s.multi, format: s.format, currency_symbol: s.currency_symbol, unit: s.unit`。
（async-graphql 的 `SimpleObject` 默认把字段名转 camelCase，前端会看到 `currencySymbol`。）

- [ ] **Step 2: `LabelSchemaAttrsInput`**

```rust
#[derive(async_graphql::InputObject)]
#[graphql(rename_fields = "camelCase")]
pub struct LabelSchemaAttrsInput {
    multi: Option<bool>,
    format: Option<String>,
    currency_symbol: Option<String>,
    unit: Option<String>,
}

impl LabelSchemaAttrsInput {
    fn to_service(self, name: String, title: String, value_type: LabelValueType,
                  enum_values: Vec<String>, color: Option<String>,
                  value_colors: Vec<ValueColor>) -> crate::service::label::LabelSchemaInput {
        crate::service::label::LabelSchemaInput {
            name, title, value_type, enum_values,
            multi: self.multi.unwrap_or(false),
            format: self.format,
            currency_symbol: self.currency_symbol,
            unit: self.unit,
            color, value_colors,
        }
    }
}
```

- [ ] **Step 3: 两个 mutation 加 `attrs` 入参**

`create_label_schema` 签名追加 `attrs: LabelSchemaAttrsInput`（放在 `value_type` 之后、`color` 之前），实现体把原 8 个参数收进 `LabelSchemaInput` 再调 `create_schema`。`update_label_schema` 同理（`value_type` 不入参，传 `LabelValueType::Null` 占位，服务层会覆盖成库中类型）。

- [ ] **Step 4: `GqlEntry` 加账号字段**

```rust
#[derive(SimpleObject, Clone)]
pub struct GqlEntry {
    // …现有字段…
    created_by: ID,
    updated_by: ID,
    // …现有字段…
    /// 创建人 / 更新人账号；账号已删除则为 null。不动上面的 createdBy / updatedBy（ID）。
    created_by_account: Option<GqlAccount>,
    updated_by_account: Option<GqlAccount>,
    labels: Vec<GqlLabeling>,
}
```

`GqlEntry::new` 增加两个参数；`gql_entry` 填充：

```rust
fn gql_entry(gql: &GraphqlContext, entry: Entry, labels: Vec<Labeling>) -> GqlResult<GqlEntry> {
    let archived_at = gql.services.entry.archived_at(&entry.code)?;
    let created_by_account = gql.services.auth.find_by_id(entry.created_by)?.map(Into::into);
    let updated_by_account = gql.services.auth.find_by_id(entry.updated_by)?.map(Into::into);
    Ok(GqlEntry::new(
        entry, labels, archived_at, created_by_account, updated_by_account,
    ))
}
```

（`create_entry` / `update_entry` / `query_entries` / `archived_entries` / `entry` 都走 `gql_entry`，无需各自改动。）

- [ ] **Step 5: 编译门**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib`
Expected: PASS（后端整体编译通过）

- [ ] **Step 6: 提交**

```bash
git add src/api/graphql.rs
git commit -m "feat(api): expose label type attrs, entry account fields, reserved-name error"
```

---

### Task 7: 前端 —— GraphQL 客户端与求值器

**Files:**
- Modify: `src/frontend/graphql_client.rs`
- Modify: `src/frontend/query_eval.rs`

**Interfaces:**
- Consumes: 服务端新增字段（见 Task 6）
- Produces:
  - `pub struct AccountBrief { pub id: String, pub email: String, pub name: String }`
  - `Entry { …, created_by: String, updated_by: String, created_by_account: Option<AccountBrief>, updated_by_account: Option<AccountBrief> }`
  - `LabelSchema { …, multi: bool, format: Option<String>, currency_symbol: Option<String>, unit: Option<String> }`

- [ ] **Step 1: 客户端结构体加字段**

```rust
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBrief {
    pub id: String,
    pub email: String,
    pub name: String,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub created_at: String,
    pub updated_at: String,
    pub created_by: String,
    pub updated_by: String,
    #[serde(default)]
    pub created_by_account: Option<AccountBrief>,
    #[serde(default)]
    pub updated_by_account: Option<AccountBrief>,
    #[serde(default)]
    pub archived_at: Option<String>,
    pub labels: Vec<Labeling>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelSchema {
    pub name: String,
    pub title: String,
    pub value_type: String,
    pub enum_values: Vec<String>,
    pub color: Option<String>,
    #[serde(default)]
    pub value_colors: Value,
    #[serde(default)]
    pub multi: bool,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub currency_symbol: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
}
```

- [ ] **Step 2: 更新所有查询串**

定义常量，避免六处手改漏字段：

```rust
const ENTRY_FIELDS: &str = "code title detail createdAt updatedAt createdBy updatedBy \
     createdByAccount { id name email } updatedByAccount { id name email } archivedAt \
     labels { labelName value }";
const LABEL_SCHEMA_FIELDS: &str = "name title valueType enumValues color valueColors \
     multi format currencySymbol unit";
```

把 `entries` / `create_entry` / `entry` / `update_entry` / `archived_entries` / `query_entries` 里的字段列表换成 `{ENTRY_FIELDS}`；`label_schemas` / `create_label_schema` / `update_label_schema` 换成 `{LABEL_SCHEMA_FIELDS}`。用 `format!` 组装，例如：

```rust
pub async fn label_schemas(workspace_id: &str) -> Result<Vec<LabelSchema>, String> {
    let q = format!(
        "query($id: ID!) {{ labelSchemas(workspaceId: $id) {{ {LABEL_SCHEMA_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("labelSchemas").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 3: 两个 mutation 传 `attrs`**

```rust
pub async fn create_label_schema(
    workspace_id: &str, name: &str, title: &str, value_type: &str,
    enum_values: &[String], attrs: &Value, color: Option<&str>, value_colors: &Value,
) -> Result<LabelSchema, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $t: String!, $vt: String!, $ev: [String!]!, \
         $attrs: LabelSchemaAttrsInput!, $color: String, $vc: JSON!) {{ \
         createLabelSchema(workspaceId: $id, name: $n, title: $t, valueType: $vt, \
         enumValues: $ev, attrs: $attrs, color: $color, valueColors: $vc) \
         {{ {LABEL_SCHEMA_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({
        "id": workspace_id, "n": name, "t": title, "vt": value_type,
        "ev": enum_values, "attrs": attrs, "color": color, "vc": value_colors,
    })).await?;
    serde_json::from_value(data.get("createLabelSchema").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}
```

`update_label_schema` 同理（无 `$vt`）。`attrs` 由调用方组装为 `{"multi":…, "format":…, "currencySymbol":…, "unit":…}`。

- [ ] **Step 4: `query_eval.rs` 支持新字段**

`eval_cond` 的 `match field.as_str()` 扩展：

```rust
match field.as_str() {
    Some("updatedAt") => cmp_time(&entry.updated_at, op, want),
    Some("createdAt") => cmp_time(&entry.created_at, op, want),
    Some("code") => cmp_value(&Value::String(entry.code.clone()), op, want),
    Some("title") => cmp_value(&Value::String(entry.title.clone()), op, want),
    Some("detail") => cmp_value(&Value::String(entry.detail.clone()), op, want),
    Some("createdBy") => cmp_account(entry.created_by_account.as_ref(), op, want),
    Some("updatedBy") => cmp_account(entry.updated_by_account.as_ref(), op, want),
    Some("text") => eval_text(op, want, entry, labels),
    _ => false,
}
```

```rust
/// 内置 CreatedBy / UpdatedBy：显示名或邮箱任一命中即真，大小写不敏感。
fn cmp_account(acct: Option<&AccountBrief>, op: &str, want: Option<&Value>) -> bool {
    let Some(a) = acct else { return false };
    match op {
        "eq" | "ne" => {
            let Some(w) = want.and_then(Value::as_str) else { return false };
            let hit = a.name.eq_ignore_ascii_case(w) || a.email.eq_ignore_ascii_case(w);
            if op == "eq" { hit } else { !hit }
        }
        "contains" | "notContains" => {
            let Some(w) = want.and_then(Value::as_str) else { return false };
            let w = w.to_lowercase();
            let hit = a.name.to_lowercase().contains(&w) || a.email.to_lowercase().contains(&w);
            if op == "contains" { hit } else { !hit }
        }
        "in" | "notIn" => {
            let Some(list) = want.and_then(Value::as_array) else { return false };
            let hit = list.iter().any(|x| {
                x.as_str().is_some_and(|w| {
                    a.name.eq_ignore_ascii_case(w) || a.email.eq_ignore_ascii_case(w)
                })
            });
            if op == "in" { hit } else { !hit }
        }
        _ => false,
    }
}
```

- [ ] **Step 5: 多值集合语义 + 时间型标签比较**

`cmp_value` 按后端同构改写（`elem_strings` + 正负成对）；`Gt/Ge/Lt/Le` 分支加时间回退：

```rust
"gt" | "ge" | "lt" | "le" => {
    if let (Some(a), Some(b)) = (as_f64(got), as_f64(want)) {
        return match op { "gt" => a > b, "ge" => a >= b, "lt" => a < b, _ => a <= b };
    }
    // 时间型标签：两侧都能按时间戳解析时按时刻比较（默认布局与 RFC3339 都覆盖）。
    let (Some(a), Some(b)) = (got.as_str().and_then(parse_ts), want.as_str().and_then(parse_ts))
    else {
        return false;
    };
    match op { "gt" => a > b, "ge" => a >= b, "lt" => a < b, _ => a <= b }
}
```

`Eq` / `Ne` 保留数值优先、否则字符串集合语义。`resolve_label_color` 的 `value` 分支加数组包含：

```rust
Some(exact) if !exact.is_null() => match value {
    Value::Array(a) => a.iter().any(|x| x == exact),
    _ => exact == value,
},
```

- [ ] **Step 6: 更新 `query_eval` 的测试 `Entry` 构造**

`tests::entry()` 补 `created_by` / `updated_by` / `created_by_account` / `updated_by_account`（可给 `Some(AccountBrief{…})` 以便新增断言）。新增一条断言：

```rust
assert!(eval(
    &json!({"cond": {"field": "createdBy", "op": "eq", "value": "张三"}}),
    &e, &[]
));
```

- [ ] **Step 7: 编译门（wasm）**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: PASS

> 注意：此时 `settings.rs` / `label_editor.rs` / `workspace_main.rs` / `entry.rs` 仍按旧签名调用 `create_label_schema` / `update_label_schema`，会编译失败 —— 由 Task 8/9/10 修。若想让本任务独立通过，先做 Task 8 再回来跑这条门。

- [ ] **Step 8: 提交**

```bash
git add src/frontend/graphql_client.rs src/frontend/query_eval.rs
git commit -m "feat(frontend): carry label attrs and entry account fields; sync query eval"
```

---

### Task 8: 前端 —— 设置页与值类型显示名

**Files:**
- Modify: `src/frontend/components.rs`（`value_type_label`）
- Modify: `src/frontend/pages/settings.rs`

**Interfaces:**
- Consumes: `create_label_schema(… attrs …)`、`update_label_schema(… attrs …)`、`LabelSchema`（新字段）
- Produces: 设置页可选 5 种新类型；Enum 的「多选」勾选；时间型的「展示格式」；Currency 的符号 / 单位

- [ ] **Step 1: `components.rs::value_type_label`**

```rust
pub fn value_type_label(vt: &str) -> String {
    match vt {
        "null" => "Null",
        "boolean" => "Boolean",
        "integer" => "Integer",
        "float" => "Float",
        "string" => "String",
        "enum" => "Enum",
        "date" => "日期",
        "time" => "时间",
        "datetime" => "日期时间",
        "currency" => "金额",
        "email" => "邮箱",
        other => other,
    }
    .to_string()
}
```

- [ ] **Step 2: 新建表单加类型选项与属性输入**

类型下拉补 5 个 `<option>`（`date` / `time` / `datetime` / `currency` / `email`）。表单状态增加：

```rust
let new_multi = RwSignal::new(false);
let new_format = RwSignal::new(String::new());
let new_symbol = RwSignal::new(String::new());
let new_unit = RwSignal::new(String::new());
```

表单里按 `new_type.get()` 条件渲染（`enum` → 「多选」勾选；`date|time|datetime` → 「展示格式（Go 布局，如 2006-01-02）」占位提示；`currency` → 货币符号 / 单位）。提交时组装 attrs：

```rust
let vt_now = new_type.get();
let attrs = serde_json::json!({
    "multi": vt_now == "enum" && new_multi.get(),
    "format": non_empty(new_format.get()),
    "currencySymbol": non_empty(new_symbol.get()),
    "unit": non_empty(new_unit.get()),
});
match create_label_schema(&ws_id, &n, &t, &vt_now, &evals, &attrs, None,
                          &serde_json::json!([])).await { … }
```

（`fn non_empty(s: String) -> Option<String> { (!s.trim().is_empty()).then(|| s.trim().to_string()) }` 放文件底部。）

- [ ] **Step 3: `schema_row` 展示与编辑新属性**

在 `value_type` 与 `base_color` 之间插入「属性」列（或把新属性追加到「可选值 / 说明」列内），按类型渲染：

- `enum`：`type="checkbox"` 绑 `RwSignal::new(s.multi)`，标签「多选」。
- `date|time|datetime`：文本输入，占位 `2006-01-02 15:04:05`，初始值 `s.format`。
- `currency`：两个小输入（符号 / 单位），初始值 `s.currency_symbol` / `s.unit`。

保存时把三者一并放进 attrs 传给 `update_label_schema`：

```rust
let attrs = serde_json::json!({
    "multi": multi_input.get(),
    "format": non_empty(format_input.get()),
    "currencySymbol": non_empty(symbol_input.get()),
    "unit": non_empty(unit_input.get()),
});
spawn_local(async move {
    if let Err(e) = update_label_schema(&ws, &n, &t, &evals, &attrs, clr.as_deref(), &vcs).await {
        error.set(Some(e));
    }
    refresh.update(|x| *x += 1);
});
```

每个输入变更时 `dirty.set(true)`（沿用现有「无改动则保存置灰」的约定）。

- [ ] **Step 4: 编译门（wasm）**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: 只剩 `label_editor.rs` / `workspace_main.rs` / `entry.rs` 的报错（Task 9/10 修）

- [ ] **Step 5: 提交**

```bash
git add src/frontend/components.rs src/frontend/pages/settings.rs
git commit -m "feat(frontend): settings UI for new label types and enum multi"
```

---

### Task 9: 前端 —— 标签编辑器支持新类型与多选

**Files:**
- Modify: `src/frontend/label_editor.rs`
- Modify: `src/frontend/pages/workspace_main.rs`（`DraftLabel` / `LabelDraft` 覆盖新类型，保证新建与批量弹窗可用）

**Interfaces:**
- Consumes: `LabelSchema.multi` / `format` / `currency_symbol` / `unit`、`value_type_label`
- Produces: Enum 多选复选组（整体 `set_labeling` 一个数组值）；Date / Time / DateTime 原生选择器；Currency 数值 + 符号；Email 文本

- [ ] **Step 1: `LabelRow` 分支扩展**

在 `Some(s) if s.value_type == "enum"` 之前插入 `multi` 分支，并把现有 enum 分支保持给单选：

```rust
Some(s) if s.value_type == "enum" && s.multi => {
    let opts = s.enum_values.clone();
    let selected: std::collections::HashSet<String> = value
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    view! {
        <div class="multienum">
            {opts.into_iter().map(|o| {
                let checked = selected.contains(&o);
                let all = s.enum_values.clone();
                let cur = value.clone();
                let oc = o.clone();
                view! {
                    <label class="mut">
                        <input type="checkbox" prop:checked=checked on:change=move |ev| {
                            let mut set: Vec<String> = cur
                                .as_ref()
                                .and_then(|v| v.as_array())
                                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                                .unwrap_or_default();
                            if event_target_checked(&ev) {
                                if !set.contains(&oc) { set.push(oc.clone()); }
                            } else {
                                set.retain(|x| x != &oc);
                            }
                            // 清空即移除该标签，否则写数组（服务端按 multi schema 接受）。
                            if set.is_empty() {
                                on_remove.run(());
                            } else {
                                on_set.run(Value::Array(set.into_iter().map(Value::String).collect()));
                            }
                        } />
                        {display_enum_value(&o)}
                    </label>
                }
            }).collect::<Vec<_>>()}
        </div>
    }.into_any()
}
```

> `all` 变量在本分支未使用，删掉；`value.clone()` 需在 `LabelRow` 顶部保留一份（`value` 是 `Option<Value>`，已被 `current` 消费为 `&`，需 `let value_owned = value.clone();`）。

- [ ] **Step 2: 时间型分支**

```rust
Some(s) if matches!(s.value_type.as_str(), "date" | "time" | "datetime") => {
    let vt = s.value_type.clone();
    let layout = s.format.clone().unwrap_or_else(|| match vt.as_str() {
        "date" => crate::golayout::DATE_LAYOUT.to_string(),
        "time" => crate::golayout::TIME_LAYOUT.to_string(),
        _ => crate::golayout::DATETIME_LAYOUT.to_string(),
    });
    let native_ok = layout == crate::golayout::DATE_LAYOUT
        || layout == crate::golayout::TIME_LAYOUT
        || layout == crate::golayout::DATETIME_LAYOUT;
    if !native_ok {
        // 自定义布局：原生控件表达不了，退回文本输入。
        let input = RwSignal::new(current.clone());
        view! { <input class="inp" style="width:180px" prop:value=input
            on:input=move |ev| input.set(event_target_value(&ev)) /> }.into_any()
    } else {
        let input_type = match vt.as_str() { "date" => "date", "time" => "time", _ => "datetime-local" };
        let current_native = to_native(&vt, &current);
        view! {
            <input class="inp" type=input_type prop:value=current_native on:change=move |ev| {
                let v = event_target_value(&ev);
                if let Some(stored) = from_native(&vt, &v) {
                    on_set.run(Value::String(stored));
                }
            } />
        }.into_any()
    }
}
```

辅助函数（放 `label_editor.rs` 底部）：

```rust
/// 存储串（Go 默认布局）→ 原生控件的值。
fn to_native(vt: &str, s: &str) -> String {
    match vt {
        "datetime" => s.replacen(' ', "T", 1).get(..16).unwrap_or(s).to_string(),
        "time" => s.get(..5).unwrap_or(s).to_string(),
        _ => s.to_string(),
    }
}

/// 原生控件的值 → 存储串（补齐秒）。
fn from_native(vt: &str, s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    Some(match vt {
        "datetime" => format!("{}:00", s.replacen('T', " ", 1)).get(..19)?.to_string(),
        "time" => format!("{s}:00").get(..8)?.to_string(),
        _ => s.to_string(),
    })
}
```

- [ ] **Step 3: Currency / Email 分支**

```rust
Some(s) if s.value_type == "currency" => {
    let input = RwSignal::new(current.clone());
    let prefix = format!(
        "{}{}",
        s.currency_symbol.clone().unwrap_or_else(|| "¥".to_string()),
        s.unit.clone().map(|u| format!(" {u}")).unwrap_or_default()
    );
    view! {
        <div style="display:flex;gap:6px;align-items:center">
            <span class="mut">{prefix}</span>
            <input class="inp" style="width:110px" type="number" prop:value=input
                on:input=move |ev| input.set(event_target_value(&ev)) />
            <button class="btn" on:click=move |_| {
                if let Some(n) = input.get_untracked().parse::<f64>()
                    .ok().and_then(serde_json::Number::from_f64) {
                    on_set.run(Value::Number(n));
                }
            }>"设置"</button>
        </div>
    }.into_any()
}
Some(s) if s.value_type == "email" => {
    let input = RwSignal::new(current.clone());
    view! {
        <div style="display:flex;gap:6px">
            <input class="inp" style="width:180px" type="email" prop:value=input
                on:input=move |ev| input.set(event_target_value(&ev)) />
            <button class="btn" on:click=move |_| {
                let v = input.get_untracked();
                if v.contains('@') { on_set.run(Value::String(v)); }
            }>"设置"</button>
        </div>
    }.into_any()
}
```

把 Currency / Email 分支插在兜底 `Some(s) => …` 之前。

- [ ] **Step 4: `DraftLabel` / `LabelDraft` 覆盖新类型**

`DraftLabel::from_schema` 的 `value_type` 映射补分支：

```rust
value_type: match s.value_type.as_str() {
    "null" => "null", "enum" => "enum", "boolean" => "boolean",
    "integer" => "integer", "float" => "float",
    "date" => "date", "time" => "time", "datetime" => "datetime",
    "currency" => "currency", "email" => "email",
    _ => "string",
},
```

（`&'static str` 仍成立。）

`to_value` 补：

```rust
"date" | "time" | "datetime" | "email" => {
    let v = self.text.get_untracked();
    (!v.trim().is_empty()).then_some(Value::String(v))
}
"currency" => self.text.get_untracked().trim().parse::<f64>().ok()
    .and_then(serde_json::Number::from_f64).map(Value::Number),
```

`DraftLabel` 增加一个 `multi: bool` 字段（`from_schema` 填 `s.multi`），供 `LabelDraft` 渲染多选复选组——多选草稿用 `RwSignal<Vec<String>>`（新增 `many: RwSignal<Vec<String>>`）。

`LabelDraft` 新增 `"enum" 且 multi` 分支（复选组，写 `Value::Array`）；时间/金额/邮箱沿用兜底文本/数值输入即可（`input_type` 对 `"date"|"time"|"datetime"` 用原生类型）。

`LabelDraft` 新增 `column` 支持：`input_type` 计算改为

```rust
let input_type = match r.value_type {
    "string" | "email" => "text",
    "date" => "date",
    "time" => "time",
    "datetime" => "datetime-local",
    _ => "number",
};
```

- [ ] **Step 5: 编译门（wasm）**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: 只剩 `pages/entry.rs` 相关报错（若有）

- [ ] **Step 6: 提交**

```bash
git add src/frontend/label_editor.rs src/frontend/pages/workspace_main.rs
git commit -m "feat(frontend): label editor for multi enum, time, currency, email"
```

---

### Task 10: 前端 —— 表达式提示列表内置名与时间选择器

**Files:**
- Modify: `src/frontend/pages/workspace_main.rs`
- Modify: `style/main.css`（`.timepick` 浮层样式）

**Interfaces:**
- Consumes: `LabelSchema.value_type` / `format`、`expr_text`
- Produces: `/` 候选里恒定并入 7 个内置名（带中文展示名）；「键 + 比较运算符 + 空白」浮出原生时间控件

- [ ] **Step 1: 内置名常量与候选合并**

文件顶部（`TitleRule` 附近）：

```rust
/// 内置元数据关键字 → 中文展示名。恒定并入 `/` 候选。
const BUILTIN_FIELDS: [(&str, &str); 7] = [
    ("Code", "编码"),
    ("Title", "标题"),
    ("Detail", "详情"),
    ("CreatedBy", "创建人"),
    ("CreatedAt", "创建时间"),
    ("UpdatedBy", "更新人"),
    ("UpdatedAt", "更新时间"),
];
```

`items` 组装改为先放内置、再放本视图标签，按 name 去重：

```rust
let mut items: Vec<(String, String)> = BUILTIN_FIELDS
    .iter()
    .map(|(k, t)| (k.to_string(), t.to_string()))
    .collect();
for name in view_label_names.get() {
    if items.iter().any(|(n, _)| n == &name) {
        continue;
    }
    let title = schemas_now
        .iter()
        .find(|s| s.name == name)
        .map(|s| s.title.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| name.clone());
    items.push((name, title));
}
let items: Vec<(String, String)> = items
    .into_iter()
    .filter(|(n, t)| n.to_lowercase().contains(&frag) || t.to_lowercase().contains(&frag))
    .collect();
```

（其余渲染不变，`pick_label` 仍插入 key。）

- [ ] **Step 2: `detect_time_picker`**

文件底部：

```rust
#[derive(Clone, Copy, PartialEq)]
enum TimeKind {
    Date,
    Time,
    DateTime,
}

/// 光标前的最后一段若形如「键 + 比较运算符 + 结尾空白」（运算符后尚未写值），
/// 且键属于时间型标签或 CreatedAt / UpdatedAt，返回 (类型, 插入位置)。
/// 纯手写扫描，不引入 regex。
fn detect_time_picker(
    text: &str,
    kind_of: &dyn Fn(&str) -> Option<TimeKind>,
) -> Option<(TimeKind, usize)> {
    let trimmed = text.trim_end();
    if trimmed.len() == text.len() {
        return None; // 运算符后必须有空白，才说明「值还没写」
    }
    let op_len = if trimmed.ends_with(">=") || trimmed.ends_with("<=") || trimmed.ends_with("!=") {
        2
    } else if trimmed.ends_with('>') || trimmed.ends_with('<') {
        1
    } else {
        return None;
    };
    let key_end = trimmed.len() - op_len;
    let mut j = key_end;
    while j > 0 {
        let c = trimmed[..j].chars().next_back()?;
        if c.is_alphanumeric() || c == '_' || c == '-' {
            j -= c.len_utf8();
        } else {
            break;
        }
    }
    if j == key_end {
        return None;
    }
    let key = &trimmed[j..key_end];
    kind_of(key).map(|k| (k, text.len()))
}
```

- [ ] **Step 3: 时间控件浮层**

新增信号：

```rust
let time_pick = RwSignal::new(None::<(TimeKind, usize)>);
```

`expr_text` 的 `on:input` 里在 `hint_open` 之后追加：

```rust
let kind_of = |key: &str| -> Option<TimeKind> {
    if key.eq_ignore_ascii_case("CreatedAt") || key.eq_ignore_ascii_case("UpdatedAt") {
        return Some(TimeKind::DateTime);
    }
    schemas
        .get_untracked()
        .iter()
        .find(|s| s.name.eq_ignore_ascii_case(key))
        .and_then(|s| match s.value_type.as_str() {
            "date" => Some(TimeKind::Date),
            "time" => Some(TimeKind::Time),
            "datetime" => Some(TimeKind::DateTime),
            _ => None,
        })
};
time_pick.set(detect_time_picker(&v, &kind_of));
```

在 `lblhint` 浮层之后渲染控件：

```rust
{move || time_pick.get().map(|(kind, at)| {
    let input_type = match kind {
        TimeKind::Date => "date",
        TimeKind::Time => "time",
        TimeKind::DateTime => "datetime-local",
    };
    view! {
        <div class="timepick">
            <input type=input_type on:change=move |ev| {
                let raw = event_target_value(&ev);
                let formatted = match kind {
                    TimeKind::Date => raw,
                    TimeKind::Time => format!("{raw}:00"),
                    TimeKind::DateTime => format!("{}:00", raw.replacen('T', " ", 1)),
                };
                let cur = expr_text.get_untracked();
                // 时间值必须加引号，否则词法器会把它当数字解析。
                let head = cur.get(..at).unwrap_or(&cur).to_string();
                expr_text.set(format!("{head}\"{formatted}\" "));
                time_pick.set(None);
                hint_open.set(false);
            } />
        </div>
    }
})}
```

`style/main.css` 追加：

```css
.timepick { position: absolute; z-index: 30; margin-top: 4px; background: var(--panel);
    border: 1px solid var(--line); border-radius: 6px; padding: 6px; box-shadow: 0 6px 18px rgba(0,0,0,.18); }
```

（`.exprwrap` 需有 `position: relative`；若没有，一并补上。）

- [ ] **Step 4: 表达式语法帮助补内置名说明**

在 `exprdoc` 的表格后加一段：

```rust
<p class="mut">"内置元数据："<code>"Code"</code>" / "<code>"Title"</code>" / "<code>"Detail"</code>" / "
    <code>"CreatedBy"</code>" / "<code>"CreatedAt"</code>" / "<code>"UpdatedBy"</code>" / "
    <code>"UpdatedAt"</code>"（这些名字不可用作自定义标签名）。"</p>
```

- [ ] **Step 5: 编译门（wasm）**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: PASS（若 `entry.rs` 仍有报错，Task 11 修）

- [ ] **Step 6: 提交**

```bash
git add src/frontend/pages/workspace_main.rs style/main.css
git commit -m "feat(frontend): builtin keys in expression hints and time picker popover"
```

---

### Task 11: 前端 —— 详情元信息

**Files:**
- Modify: `src/frontend/pages/workspace_main.rs`（`EntryPanel`）
- Modify: `src/frontend/pages/entry.rs`
- Modify: `src/frontend/components.rs`（加 `fmt_datetime` 助手）

**Interfaces:**
- Consumes: `Entry.created_by_account` / `updated_by_account` / `created_at` / `updated_at` / `archived_at`、`crate::golayout`
- Produces: 两处详情都展示 编码 / 创建人 / 创建时间 / 更新人 / 更新时间（已归档再加归档时间）

- [ ] **Step 1: `components.rs` 加格式化助手**

```rust
/// RFC3339 → 默认展示格式 `2006-01-02 15:04:05`（按字符串切片，不引入 chrono）。
pub fn fmt_datetime(rfc: &str) -> String {
    let s = rfc.trim();
    match (s.get(..10), s.get(11..19)) {
        (Some(d), Some(t)) => format!("{d} {t}"),
        (Some(d), None) => d.to_string(),
        _ => s.to_string(),
    }
}
```

- [ ] **Step 2: `EntryPanel` 展示元信息**

在 `<div class="dhead">` 的 `</div>` 之后插入：

```rust
{move || data.get().and_then(|r| r.ok()).map(|e| {
    let by = |a: &Option<AccountBrief>| a.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| "—".to_string());
    view! {
        <div class="dmeta">
            <div><span class="mut">"编码"</span><span class="code">{e.code.clone()}</span></div>
            <div><span class="mut">"创建人"</span>{by(&e.created_by_account)}</div>
            <div><span class="mut">"创建时间"</span>{fmt_datetime(&e.created_at)}</div>
            <div><span class="mut">"更新人"</span>{by(&e.updated_by_account)}</div>
            <div><span class="mut">"更新时间"</span>{fmt_datetime(&e.updated_at)}</div>
            {e.archived_at.clone().map(|at| view! {
                <div><span class="mut">"归档时间"</span>{fmt_datetime(&at)}</div>
            })}
        </div>
    }
})}
```

`use` 增加 `AccountBrief`、`fmt_datetime`。

- [ ] **Step 3: `pages/entry.rs` 展示元信息**

在 `.entry-top` 面板之后加一块同样的 `.dmeta`（数据来自 `data` 里的 `Entry`）。`use` 增加 `AccountBrief`、`fmt_datetime`。

- [ ] **Step 4: `style/main.css` 加 `.dmeta` 样式**

```css
.dmeta { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    gap: 4px 16px; padding: 8px 12px; font-size: 12px; }
.dmeta .mut { margin-right: 6px; }
```

- [ ] **Step 5: 编译门（wasm）**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: PASS

- [ ] **Step 6: 提交**

```bash
git add src/frontend/components.rs src/frontend/pages/workspace_main.rs src/frontend/pages/entry.rs style/main.css
git commit -m "feat(frontend): show entry metadata in detail panel and fullscreen page"
```

---

### Task 12: 整体编译门与浏览器验收

**Files:** 无（只验证）

- [ ] **Step 1: 两个目标全绿**

Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib`
Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: 均 PASS

- [ ] **Step 2: 清库并重建（bincode 结构变了）**

Run: `make reset-data`
Run: `CARGO_BUILD_JOBS=1 /Users/wangxiaoyan/.cargo/bin/cargo build --lib`
（启动开发服务器由用户执行 `make dev`。）

- [ ] **Step 3: 浏览器验收清单（用户在浏览器确认）**

1. 设置页新建 Enum 标签勾「多选」，在 Entry 详情勾 2 个值 → 表格标签列显示 `A, B`；再用表达式 `标签 = "A"` 能命中。
2. 新建 Date / Time / DateTime 标签，表达式输入 `键 > `（含尾空格）→ 浮出原生控件；选中后插入带引号的时间串，回车能查询。
3. 新建 Currency 标签写入金额，`>` / `<` 比较数值生效。
4. 新建 Email 标签写入 `a@b.com`，写 `a@` 报「标签值不合法」。
5. Entry 详情（右侧面板 + 全屏页）显示编码 / 创建人 / 创建时间 / 更新人 / 更新时间。
6. 设置页尝试新建名为 `Title` 的标签 → 报「与内置元数据重名」。
7. 表达式 `/` 候选里出现 编码 / 标题 / 详情 / 创建人 / 创建时间 / 更新人 / 更新时间。
8. 表达式 `CreatedBy = "管理员显示名"` 与 `CreatedBy ~ "admin@x.io"` 都能命中。
9. 旧写法 `updated >= "2026-09-01"` 报错，改为 `UpdatedAt >= "2026-09-01"` 后生效。

- [ ] **Step 4: 记录缺陷**

发现的偏差追加到 `spec/20260912-缺陷记录.md`（不改本计划）。

---

## Self-Review

**Spec coverage**

| Spec 章节 | 落地任务 |
|---|---|
| §3.1 `LabelValueType` 5 型 | Task 2 Step 1 |
| §3.2 `LabelValue` 追加变体 + `from_json`/`to_json` | Task 2 Step 2–4 |
| §3.3 `LabelSchema` 新属性 + `with_attrs` | Task 2 Step 5 |
| §3.4 `Field` 新变体 + camelCase 序列化 | Task 3 Step 1 |
| §4.1 关键字与 `RESERVED_FIELDS`；移除 `created`/`updated` | Task 3 Step 1/4/5 |
| §4.2 `op_allowed` 扩展 | Task 3 Step 7 |
| §4.3 值解析与校验 | Task 3 Step 8–9 |
| §4.4 `to_expr` 规范名 + 往返用例 | Task 3 Step 6/12 |
| §5.1 多值集合语义（前后端） | Task 3 Step 10、Task 7 Step 5 |
| §5.2 时间 / 金额 / 邮箱比较 | Task 3 Step 11（`cmp_time_layout`）、Task 7 Step 5 |
| §5.3 CreatedBy / UpdatedBy 账号解析 | Task 3 Step 11、Task 5 Step 1 |
| §5.4 `EvalEnv` 求值环境 | Task 3 Step 3（**加了第三个闭包 `label_of`**，理由见该 Task 说明） |
| §6 `golayout` 共享模块 | Task 1 |
| §6.3 `parse_time` 扩展 | Task 3 Step 9 |
| §7 存储 / 服务 / GraphQL | Task 4、Task 5、Task 6 |
| §7 `LabelSchemaInput` | Task 4 Step 2–3 |
| §7 `SearchIndex::add_entry_doc` 数组空格连接 | Task 5 Step 2 |
| §7 `GqlEntry` 账号字段 | Task 6 Step 4 |
| §8.1 设置页新类型 / 属性 | Task 8 |
| §8.2 标签编辑器多选与新型控件 | Task 9 |
| §8.3 提示列表内置名 + 时间选择器 | Task 10 |
| §8.4 详情元信息（面板 + 全屏页） | Task 11 |
| §8.5 `query_eval` 同步 + 客户端结构体 | Task 7 |
| §9 错误处理（`LabelNameReserved` 等） | Task 4 Step 1、Task 3 Step 8 |
| §10 测试策略（不写后端新单测；旧单测同步） | 各 Task 的编译门；Task 3 Step 12、Task 4 Step 4 |
| §11 破坏性变更 / `rm -rf data` | Global Constraints；Task 12 Step 2 |

**Placeholder scan:** 无 TBD / TODO；每个改动步骤都给出了可直接落盘的代码。

**Type consistency:**
- `LabelSchemaInput` 字段名在 Task 4（定义）、Task 6（`to_service` 构造）、Task 8（前端 attrs JSON 键 `multi` / `format` / `currencySymbol` / `unit`）三处一致；GraphQL 侧 camelCase 由 `#[graphql(rename_fields = "camelCase")]` 保证。
- `EvalEnv` 三字段在 Task 3（定义）与 Task 5（构造处）一致。
- `default_layout` 在 Task 2 定义、Task 3 Step 8/11 使用。
- `ENTRY_FIELDS` / `LABEL_SCHEMA_FIELDS` 常量在 Task 7 定义并被六个查询串复用，字段名与 Task 6 的 `GqlEntry` / `GqlLabelSchema` 声明逐一对应。
- `TimeKind` / `detect_time_picker` 在 Task 10 内自洽。
