# 标签事件自动化 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让标签写入成为事件源，工作空间内可配置「触发条件 DSL + 触发动作」的自动化规则，规则动作与主写入同批原子提交。

**Architecture:** 领域层新增事件与规则模型；`Query` 文法追加 `$label` / `$old` / `$new` 事件字段；`RuleEngine` 以「分层批处理 + 内存后置状态 overlay」求值，把规则动作的 ops 交给 `EntryService` 与用户写入拼成**一个** `write_batch`；新列族存规则，GraphQL + 设置页提供配置界面。

**Tech Stack:** Rust（async-graphql / rocksdb / bincode / chrono）、Leptos 0.8（SSR + WASM hydrate）。

**Spec:** `docs/superpowers/specs/2026-09-16-event-automation-design.md`

## Global Constraints

- **编译门禁**：每个任务收尾跑 `make check`（= `cargo check` + `cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown`）。两个目标都必须通过。
- **单测范围（用户明确要求）**：**只有** `src/service/rule.rs` 允许新增 `#[cfg(test)] mod tests`。其他任何模块都不新增单测。既有测试不得改坏（`cargo test` 全绿）。
- **bincode 兼容**：`Field` / `AuditAction` 等按变体序号编码的枚举，新变体**只能追加在末尾**，绝不插入中间。存量数据不得反序列化失败。
- **新增列族必须注册进 `src/storage/rocksdb.rs` 的 `ALL_CFS`**，否则 `DB::open_cf` 报错。
- **注释用中文**，只写「为什么」，不写「做了什么」。文件头部的既有注释风格照抄。
- **表达式语法用 AND / OR / NOT**，不用「且 / 或 / 非」（用户明确要求）。
- 既有写入语义不变：用户直接写标签一律落库、一律写审计（即使值没变）。去重只作用于引擎产生的写入。

---

### Task 1: 领域模型与存储

**Files:**
- Create: `src/domain/rule.rs`
- Modify: `src/domain/view.rs`（`query_json` 提升为 `pub(crate)`）
- Modify: `src/domain/mod.rs`
- Modify: `src/storage/keys.rs`
- Modify: `src/storage/rocksdb.rs`

**Interfaces:**
- Consumes: `crate::domain::{LabelValue, Query}`（既有）
- Produces:
  - `domain::rule::{LabelEvent, AutomationRule, RuleAction, ActionTarget, LabelWrite, WriteOp, ValueSource}`
  - `keys::rule_key(id: Ulid) -> [u8; 16]`、`keys::rule_by_workspace_key(ws: Ulid, id: Ulid) -> [u8; 32]`
  - `cf::AUTOMATION_RULES`、`cf::AUTOMATION_RULES_BY_WORKSPACE`

- [ ] **Step 1: 新建 `src/domain/rule.rs`**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::domain::view::query_json;
use crate::domain::{LabelValue, Query};

/// 规则整体以 bincode 落库，而 bincode 不支持 `deserialize_any`，
/// `serde_json::Value` 直接编码就会「写得进去、读不出来」。
/// 与 `domain::view::query_json` 同一套办法：先转成 JSON 字符串再交给 bincode。
mod raw_json {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T, S>(value: &T, ser: S) -> Result<S::Ok, S::Error>
    where
        T: Serialize,
        S: Serializer,
    {
        let s = serde_json::to_string(value).map_err(serde::ser::Error::custom)?;
        ser.serialize_str(&s)
    }

    pub fn deserialize<'de, T, D>(de: D) -> Result<T, D::Error>
    where
        T: serde::de::DeserializeOwned,
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        serde_json::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// 一次标签变更。新增时 `old` 为 `None`，删除时 `new` 为 `None`。
/// 不落库——事件是请求内的瞬时结构，审计另有 `AuditLog` 承载。
#[derive(Debug, Clone, PartialEq)]
pub struct LabelEvent {
    pub workspace_id: Ulid,
    pub entry_code: String,
    pub label_name: String,
    pub old: Option<LabelValue>,
    pub new: Option<LabelValue>,
    /// 触发者。规则动作产生的事件里仍是触发者——规则不是主体，没有自己的身份。
    pub actor: Ulid,
    /// 0 = 用户直接写入；1..=3 = 规则动作产生的写入。
    pub level: u8,
}

/// 写入动作。`Set` 涵盖新增 / 修改 / upsert——底层都是 `Labeling` 的覆盖写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriteOp {
    Set,
    Remove,
}

/// 动作写入值的来源。`Literal` 之外都无法在保存时静态校验类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueSource {
    Literal(#[serde(with = "raw_json")] serde_json::Value),
    /// 当前时间，按目标标签 schema 的 format 渲染。一次请求内只取一个时刻。
    Now,
    /// 事件的新值（$new）。
    New,
    /// 事件的旧值（$old）。
    Old,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelWrite {
    pub label_name: String,
    pub op: WriteOp,
    /// `Remove` 时为 `None`，`Set` 时必填。
    pub value: Option<ValueSource>,
}

/// 动作作用到哪些条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionTarget {
    /// 事件源条目。
    EventSource,
    /// 表达式圈定（工作空间内、排除已删除与已归档，口径同视图列表）。
    Query(#[serde(with = "query_json")] Query),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAction {
    pub target: ActionTarget,
    pub writes: Vec<LabelWrite>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRule {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub name: String,
    pub enabled: bool,
    /// 触发条件 AST。文本由 `Query::to_expr()` 反推，不存副本，避免两份真相漂移。
    #[serde(with = "query_json")]
    pub trigger: Query,
    pub action: RuleAction,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AutomationRule {
    pub fn new(
        workspace_id: Ulid,
        name: String,
        enabled: bool,
        trigger: Query,
        action: RuleAction,
        actor: Ulid,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Ulid::new(),
            workspace_id,
            name,
            enabled,
            trigger,
            action,
            created_by: actor,
            created_at: now,
            updated_at: now,
        }
    }
}
```

- [ ] **Step 2: 把 `src/domain/view.rs` 的 `query_json` 提为 `pub(crate)`，并在 `src/domain/mod.rs` 注册新模块**

`query_json` 现在只服务 `View`，`rule.rs` 要复用它，所以第 10 行 `mod query_json {` 改成：

```rust
pub(crate) mod query_json {
```

**不要**把这段 serde helper 复制一份到 `rule.rs` 之外的另一处地方——`ValueSource::Literal` 用的 `raw_json` 已经在本文件里，两者职责不同（一个序列化 `Query`，一个序列化任意 JSON），共用同一份反而会互相牵制。

在 `src/domain/mod.rs` 的 `pub mod query;` 之后加 `pub mod rule;`（模块按字母序，`rule` 排在 `query` 与 `view` 之间），并在 re-export 区加：

```rust
pub use rule::{ActionTarget, AutomationRule, LabelEvent, LabelWrite, RuleAction, ValueSource, WriteOp};
```

- [ ] **Step 3: 在 `src/storage/keys.rs` 加规则键**

追加到 `view_by_workspace_key` 之后：

```rust
/// 规则主键：16 字节 ulid。
pub fn rule_key(id: Ulid) -> [u8; 16] {
    id.to_bytes()
}

/// (workspace_id, rule_id) 复合键，32 字节。
pub fn rule_by_workspace_key(workspace_id: Ulid, id: Ulid) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&workspace_id.to_bytes());
    key[16..].copy_from_slice(&id.to_bytes());
    key
}
```

- [ ] **Step 4: 在 `src/storage/rocksdb.rs` 的 `cf` 模块与 `ALL_CFS` 注册列族**

`cf::WORKSPACE_AI` 之后追加：

```rust
    /// 自动化规则：rule id → `AutomationRule`（bincode）。
    pub const AUTOMATION_RULES: &str = "automation_rules";
    /// 工作空间下的规则索引：(workspace_id, rule_id) → 空值，供前缀扫描。
    pub const AUTOMATION_RULES_BY_WORKSPACE: &str = "automation_rules_by_workspace";
```

`ALL_CFS` 末尾（`cf::WORKSPACE_AI,` 之后）追加：

```rust
    cf::AUTOMATION_RULES,
    cf::AUTOMATION_RULES_BY_WORKSPACE,
```

- [ ] **Step 5: 编译校验**

Run: `make check`
Expected: 两个目标都 `Finished`，无 warning 新增。

- [ ] **Step 6: 提交**

```bash
git add src/domain/rule.rs src/domain/view.rs src/domain/mod.rs src/storage/keys.rs src/storage/rocksdb.rs
git commit -m "feat(domain): add automation rule model and rule column families"
```

---

### Task 2: 事件字段 DSL 与规则 CRUD 服务

**Files:**
- Modify: `src/domain/query.rs`
- Create: `src/service/rule.rs`
- Modify: `src/service/mod.rs`
- Modify: `src/service/entry.rs`（仅 `EvalEnv` 构造点补 `event: None`）

**Interfaces:**
- Consumes: Task 1 的 `domain::rule::*`
- Produces:
  - `Query::validate_for_rule(&self, schemas: &[LabelSchema], allow_event: bool) -> Result<(), AppError>`
  - `Query::contains_event_field(&self) -> bool`
  - `EvalEnv { text_hit, account_of, label_of, event: Option<&'a LabelEvent> }`
  - `RuleService::{new, list, get, create, update, delete, parse_trigger, schemas}`
  - `RuleService::parse_trigger(&self, ws: Ulid, expr: &str) -> Result<Query, AppError>`
  - `RuleService::schemas(&self, ws: Ulid) -> Result<Vec<LabelSchema>, AppError>`（`pub(crate)`，Task 3 的引擎复用）

- [ ] **Step 1: 先写失败的单测（`src/service/rule.rs` 新建，只放测试模块）**

`Field` 的事件变体与求值此时还不存在，测试必然编译失败——这正是要用它来钉住接口。

```rust
//! 自动化规则：DSL 校验、CRUD、以及把规则动作接进标签写入的引擎。

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::rule::LabelEvent;
    use crate::domain::{Entry, LabelSchema, LabelValue, LabelValueType, Labeling, Query};
    use ulid::Ulid;

    fn schema(name: &str, vt: LabelValueType) -> LabelSchema {
        LabelSchema {
            workspace_id: Ulid::new(),
            name: name.into(),
            title: name.into(),
            value_type: vt,
            enum_values: vec!["InProgress".into(), "Finished".into()],
            color: None,
            value_colors: Vec::new(),
            multi: false,
            format: None,
            currency_symbol: None,
            unit: None,
        }
    }

    fn schemas() -> Vec<LabelSchema> {
        vec![
            schema("Status", LabelValueType::Enum),
            schema("FinishedAt", LabelValueType::DateTime),
            schema("Priority", LabelValueType::Integer),
        ]
    }

    fn entry() -> Entry {
        Entry::new(Ulid::new(), "找回密码失败".to_string(), Ulid::new())
    }

    fn event(old: Option<LabelValue>, new: Option<LabelValue>) -> LabelEvent {
        LabelEvent {
            workspace_id: Ulid::new(),
            entry_code: "E1".into(),
            label_name: "Status".into(),
            old,
            new,
            actor: Ulid::new(),
            level: 0,
        }
    }

    /// 在给定事件下求值一个触发条件。
    fn matches(expr: &str, ev: &LabelEvent, labels: &[Labeling]) -> bool {
        let q = Query::parse(expr).expect("表达式应能解析");
        let e = entry();
        let schemas = schemas();
        // 具名闭包与 `src/domain/query.rs` 既有测试同形；内联 `&|_| false` 会因
        // 参数类型无法推断而编译失败。
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let label_of = |n: &str| {
            schemas
                .iter()
                .find(|s| s.name == n)
                .map(|s| (s.value_type, s.format.clone()))
        };
        let env = EvalEnv {
            text_hit: &never,
            account_of: &no_acct,
            label_of: &label_of,
            event: Some(ev),
        };
        q.evaluate(&e, labels, &env)
    }

    #[test]
    fn event_fields_match_label_and_new_value() {
        let ev = event(Some(LabelValue::Enum("InProgress".into())), Some(LabelValue::Enum("Finished".into())));
        assert!(matches(r#"$label = "Status" AND $new = "Finished""#, &ev, &[]));
        assert!(matches(r#"$label = "Status" AND $old = "InProgress""#, &ev, &[]));
        assert!(!matches(r#"$new = "Aborted""#, &ev, &[]));
    }

    #[test]
    fn absent_sugar_distinguishes_insert_and_delete() {
        let added = event(None, Some(LabelValue::Enum("Finished".into())));
        let removed = event(Some(LabelValue::Enum("Finished".into())), None);
        assert!(matches("!$old", &added, &[]));
        assert!(!matches("!$new", &added, &[]));
        assert!(matches("!$new", &removed, &[]));
        assert!(!matches("!$old", &removed, &[]));
    }

    #[test]
    fn event_value_compares_with_the_changed_labels_own_layout() {
        // FinishedAt 是 DateTime 且未配 format，走 default_layout，
        // 事件值 "2026-09-16 10:00:00" 与字面量比较按时刻而非字符串。
        let ev = LabelEvent {
            label_name: "FinishedAt".into(),
            old: None,
            new: Some(LabelValue::DateTime("2026-09-16 10:00:00".into())),
            ..event(None, None)
        };
        assert!(matches(r#"$new > "2026-09-16 09:00:00""#, &ev, &[]));
        assert!(!matches(r#"$new > "2026-09-16 11:00:00""#, &ev, &[]));
    }

    #[test]
    fn entry_conditions_see_the_event_source_entry() {
        let ev = event(None, None);
        let labels = vec![Labeling::new("E1".into(), "Priority".into(), LabelValue::Int(5), Ulid::new())];
        assert!(matches("Priority >= 3", &ev, &labels));
        assert!(!matches("Priority >= 9", &ev, &labels));
    }

    #[test]
    fn to_expr_round_trips_event_fields() {
        for expr in [
            r#"$label = "Status" AND $new = "Finished""#,
            "!$old",
            r#"$new > "2026-09-16 09:00:00""#,
        ] {
            let q = Query::parse(expr).unwrap();
            let again = Query::parse(&q.to_expr()).expect("to_expr 的输出必须能再次解析");
            assert_eq!(q, again, "表达式往返失败: {expr}");
        }
    }

    #[test]
    fn views_reject_event_fields_and_rules_reject_text() {
        let schemas = schemas();
        let ev_field = Query::parse(r#"$label = "Status""#).unwrap();
        assert!(ev_field.validate(&schemas).is_err(), "视图不得使用事件字段");
        assert!(ev_field.validate_for_rule(&schemas, true).is_ok(), "触发条件允许事件字段");
        assert!(
            ev_field.validate_for_rule(&schemas, false).is_err(),
            "动作目标是对条目的过滤，不得使用事件字段"
        );
        let text = Query::parse(r#"text ~ "报错""#).unwrap();
        assert!(text.validate_for_rule(&schemas, true).is_err(), "规则不得使用全文条件");
    }

    #[test]
    fn unknown_event_field_is_rejected_with_readable_message() {
        let err = Query::parse("$nope = 1").unwrap_err();
        assert!(err.to_string().contains("nope"), "错误信息应指出未知字段: {err}");
    }
}
```

为了让这个文件先能编过，文件顶部必须引入 `EvalEnv`：

```rust
use crate::domain::EvalEnv;
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib service::rule 2>&1 | head -30`
Expected: 编译失败——`Field::EventLabel` 等变体不存在、`EvalEnv` 没有 `event` 字段、`validate_for_rule` 未定义。

- [ ] **Step 3: `src/domain/query.rs`——追加 `Field` 变体**

`Field` 枚举末尾（`UpdatedBy,` 之后）追加：

```rust
    // 追加在末尾：事件字段，仅规则触发条件可用
    EventLabel, // $label
    EventOld,   // $old
    EventNew,   // $new
```

- [ ] **Step 4: `src/domain/query.rs`——`EvalEnv` 加 `event`**

```rust
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
}
```

文件顶部 use 追加 `LabelEvent`：`use crate::domain::{Entry, LabelEvent, LabelSchema, LabelValueType, Labeling};`

同一文件里 `EvalEnv` 的既有构造点（`#[cfg(test)] mod tests` 内的辅助函数）补 `event: None`。

- [ ] **Step 5: `src/service/entry.rs`——补既有 `EvalEnv` 构造点**

`EntryService::query` 里的 `EvalEnv { text_hit, account_of, label_of }` 加一个字段：

```rust
                let env = EvalEnv {
                    text_hit: &text_ok,
                    account_of: &account_of,
                    label_of: &label_of,
                    event: None,
                };
```

- [ ] **Step 6: `src/domain/query.rs`——词法器识别 `$label` / `$old` / `$new`**

`Tok` 枚举末尾追加 `EventField(Field),`（`Tok` 是私有枚举、不进库，位置无所谓）。

`lex` 的 `match c` 里，在 `'~' => { ... }` 之后、`'"' | '\''` 之前插入：

```rust
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
                    _ => {
                        return Err(AppError::InvalidQuery(format!("未知事件字段: ${word}")))
                    }
                }
                i = j;
            }
```

`tok_label` 的 `Some(Tok::Builtin(f))` 之后补一行：

```rust
        Some(Tok::EventField(f)) => canonical_name(f).to_string(),
```

- [ ] **Step 7: `src/domain/query.rs`——解析、规范名、格式化**

`canonical_name` 追加三个分支（放在 `Field::Text => "text",` 之后）：

```rust
        Field::EventLabel => "$label",
        Field::EventOld => "$old",
        Field::EventNew => "$new",
```

`parse_primary` 的 `Some(Tok::Builtin(_)) | Some(Tok::Ident(_))` 分支改成：

```rust
            Some(Tok::Builtin(_)) | Some(Tok::Ident(_)) | Some(Tok::EventField(_)) => {
                self.parse_condition()
            }
```

`parse_condition` 开头的字段解析改成：

```rust
        let field = match self.next() {
            Some(Tok::Builtin(f)) => f,
            Some(Tok::EventField(f)) => f,
            Some(Tok::Ident(s)) => Field::Label(s),
            other => {
                return Err(AppError::InvalidQuery(format!(
                    "期望字段，实际 {}",
                    tok_label(other.as_ref())
                )))
            }
        };
```

既有 `if let Field::Label(_) = &field { ... }` 存在性语法糖分支**保持不变**，紧随其后新增事件字段的同类分支：

```rust
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
                Some(Tok::Eq | Tok::Ne | Tok::Gt | Tok::Ge | Tok::Lt | Tok::Le | Tok::Tilde | Tok::NotTilde | Tok::In | Tok::Not)
            ) {
                return Ok(Query::Cond(Condition { field, op: Op::Present, value: None }));
            }
        }
```

`Condition::to_expr` 的 field 推导已是 `other => canonical_name(other).to_string()`，无需改动——事件字段因此自然格式化回 `$label` / `$old` / `$new`。

- [ ] **Step 8: `src/domain/query.rs`——求值分支**

`Condition::evaluate` 的 `match &self.field` 末尾（`Field::Label(name) => { ... }` 之后）追加：

```rust
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
                if matches!(self.field, Field::EventOld | Field::EventNew)
                    && !matches!(self.field, Field::EventLabel)
                {
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
```

简化提示：上面两个 `matches!` 条件重复，实现时直接写成
`if matches!(self.field, Field::EventOld | Field::EventNew) { if let Some((vt, fmt)) = ... }` 即可。

文件顶部 use 补 `LabelValue`：`use crate::domain::{Entry, LabelEvent, LabelSchema, LabelValue, LabelValueType, Labeling};`

- [ ] **Step 9: `src/domain/query.rs`——校验入口**

`Query::contains_text` 之后新增：

```rust
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
```

`Query::validate` 开头（`let mut kws = HashSet::new();` 之前）加：

```rust
        if self.contains_event_field() {
            return Err(AppError::InvalidQuery(
                "事件字段（$label / $old / $new）只能用在自动化规则的触发条件里".to_string(),
            ));
        }
```

`Query::validate` 之后新增规则专用入口：

```rust
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
        // 复用视图那套逐字段校验；它含「拒绝事件字段」，故先把事件字段摘掉再校验。
        let stripped = strip_event_fields(self);
        stripped.validate(schemas)
    }
```

`strip_event_fields` 作为自由函数放在 `Query::validate_for_rule` 附近：

```rust
/// 把事件字段条件替换成恒真的 `Query::Cond(Condition { field: Field::Code, op: Op::Present, value: None })`？
fn strip_event_fields(q: &Query) -> Query { ... }
```

**不要用上面这个方案**——它给校验引入了一个假条件。改用更直接的做法：把「事件字段的条件」在配置校验时当作**已通过**处理，只校验其余条件。实现方式是把 `Condition::validate` 拆出「事件字段早返回」：

```rust
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
        match &self.field {
            ... 既有内容保持不变 ...
        }
    }
}
```

这样 `validate_for_rule` 直接调 `self.validate(schemas)`，但 `validate` 顶部那句「拒绝事件字段」会让 `allow_event = true` 的场景失败——把该检查从 `Query::validate` 移到需要它的调用方即可。最终形态：

```rust
    pub fn validate(&self, schemas: &[LabelSchema]) -> Result<(), AppError> {
        if self.contains_event_field() {
            return Err(AppError::InvalidQuery(
                "事件字段（$label / $old / $new）只能用在自动化规则的触发条件里".to_string(),
            ));
        }
        self.validate_inner(schemas)
    }

    /// 规则专用校验：触发条件允许事件字段，动作目标不允许；两者都禁止全文条件。
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
        let mut kws = HashSet::new();
        collect_text_keywords(self, &mut kws);
        if kws.len() > 1 {
            return Err(AppError::InvalidQuery(
                "本轮仅支持单个全文条件".to_string(),
            ));
        }
        match self {
            Query::And(v) | Query::Or(v) => v.iter().try_for_each(|q| q.validate_inner(schemas)),
            Query::Not(q) => q.validate_inner(schemas),
            Query::Cond(c) => c.validate(schemas),
        }
    }
```

把既有 `Query::validate` 的函数体整体改名为 `validate_inner`（`Query::And/Or/Not` 递归调用也一并改成 `validate_inner`），再按上面补两个新入口。既有调用方（`service/view.rs`、`api/graphql.rs` 的 `parse_view_query`）继续调 `validate`，行为不变。

- [ ] **Step 10: 运行测试确认 DSL 部分通过**

Run: `cargo test --lib service::rule 2>&1 | tail -20`
Expected: 6 个 DSL 测试全部 PASS（CRUD 相关的测试还没有，本步只跑这一个模块）。

- [ ] **Step 11: `src/service/rule.rs`——补 `RuleService`**

在测试模块之前加入实现（同文件，`use` 合并到文件顶部）：

```rust
use std::sync::Arc;

use ulid::Ulid;

use crate::domain::rule::{ActionTarget, AutomationRule, LabelWrite, RuleAction, ValueSource, WriteOp};
use crate::domain::{AuditAction, AuditLog, LabelSchema, LabelValue, Query};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 每个工作空间的规则数上限。规则求值是同步的，且目标表达式要扫全表，
/// 数量失控会拖慢每一次打标。
pub const MAX_RULES_PER_WORKSPACE: usize = 50;

const RESOURCE_TYPE: &str = "rule";

/// 规则的增删改查与保存时校验。执行不在这里——见 `RuleEngine`。
pub struct RuleService {
    store: Arc<DocStore>,
}

impl RuleService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    pub fn list(&self, ws: Ulid) -> Result<Vec<AutomationRule>, AppError> {
        let rows = self
            .store
            .scan_prefix(cf::AUTOMATION_RULES_BY_WORKSPACE, &ws.to_bytes())?;
        let mut out = Vec::new();
        for (key, _) in rows {
            // 复合键为 (workspace_id, rule_id)，各 16 字节。
            if key.len() < 32 {
                continue;
            }
            let id = Ulid::from_bytes(key[16..32].try_into().unwrap());
            match self.store.get::<AutomationRule>(cf::AUTOMATION_RULES, &keys::rule_key(id)) {
                Ok(Some(r)) => out.push(r),
                Ok(None) => {}
                // 读不出来的记录（旧编码残留）不该拖垮整个规则列表：跳过并记一笔。
                Err(e) => tracing::warn!("跳过无法读取的规则 {id}: {e}"),
            }
        }
        // 按创建顺序（Ulid 时间有序）排列，求值顺序因此可预期。
        out.sort_by_key(|r| r.id);
        Ok(out)
    }

    pub fn get(&self, id: Ulid) -> Result<Option<AutomationRule>, AppError> {
        self.store.get(cf::AUTOMATION_RULES, &keys::rule_key(id))
    }

    /// 解析 + 按规则规则校验触发条件，供编辑器实时校验。
    pub fn parse_trigger(&self, ws: Ulid, expr: &str) -> Result<Query, AppError> {
        let q = Query::parse(expr)?;
        q.validate_for_rule(&self.schemas(ws)?, true)?;
        Ok(q)
    }

    pub fn create(
        &self,
        actor: Ulid,
        ws: Ulid,
        name: &str,
        enabled: bool,
        trigger_expr: &str,
        target_event_source: bool,
        target_expr: Option<&str>,
        writes: Vec<LabelWrite>,
    ) -> Result<AutomationRule, AppError> {
        if self.list(ws)?.len() >= MAX_RULES_PER_WORKSPACE {
            return Err(AppError::InvalidQuery(format!(
                "自动化规则数量已达上限（{MAX_RULES_PER_WORKSPACE} 条）"
            )));
        }
        let (trigger, action) = self.build(
            ws,
            name,
            trigger_expr,
            target_event_source,
            target_expr,
            writes,
        )?;
        let rule = AutomationRule::new(ws, name.trim().to_string(), enabled, trigger, action, actor);
        let audit = AuditLog::new(
            AuditAction::RuleCreated,
            actor,
            RESOURCE_TYPE,
            &rule.id.to_string(),
            Some(ws),
            None,
            Some(serde_json::to_string(&rule).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::AUTOMATION_RULES, keys::rule_key(rule.id).to_vec(), &rule)?);
        ops.push(BatchOp::put_raw(
            cf::AUTOMATION_RULES_BY_WORKSPACE,
            keys::rule_by_workspace_key(ws, rule.id).to_vec(),
            Vec::new(),
        ));
        self.store.write_batch(ops)?;
        Ok(rule)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        actor: Ulid,
        id: Ulid,
        name: &str,
        enabled: bool,
        trigger_expr: &str,
        target_event_source: bool,
        target_expr: Option<&str>,
        writes: Vec<LabelWrite>,
    ) -> Result<AutomationRule, AppError> {
        let mut rule = self.get(id)?.ok_or(AppError::NotFound)?;
        let (trigger, action) = self.build(
            rule.workspace_id,
            name,
            trigger_expr,
            target_event_source,
            target_expr,
            writes,
        )?;
        let before = serde_json::to_string(&rule).unwrap_or_default();
        rule.name = name.trim().to_string();
        rule.enabled = enabled;
        rule.trigger = trigger;
        rule.action = action;
        rule.updated_at = chrono::Utc::now();
        let audit = AuditLog::new(
            AuditAction::RuleUpdated,
            actor,
            RESOURCE_TYPE,
            &id.to_string(),
            Some(rule.workspace_id),
            Some(before),
            Some(serde_json::to_string(&rule).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::AUTOMATION_RULES, keys::rule_key(id).to_vec(), &rule)?);
        self.store.write_batch(ops)?;
        Ok(rule)
    }

    pub fn delete(&self, actor: Ulid, id: Ulid) -> Result<(), AppError> {
        let rule = self.get(id)?.ok_or(AppError::NotFound)?;
        let audit = AuditLog::new(
            AuditAction::RuleDeleted,
            actor,
            RESOURCE_TYPE,
            &id.to_string(),
            Some(rule.workspace_id),
            Some(serde_json::to_string(&rule).unwrap_or_default()),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::AUTOMATION_RULES, keys::rule_key(id).to_vec()));
        ops.push(BatchOp::delete(
            cf::AUTOMATION_RULES_BY_WORKSPACE,
            keys::rule_by_workspace_key(rule.workspace_id, id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 解析并校验一次提交，返回规范化的触发条件与动作。
    fn build(
        &self,
        ws: Ulid,
        name: &str,
        trigger_expr: &str,
        target_event_source: bool,
        target_expr: Option<&str>,
        writes: Vec<LabelWrite>,
    ) -> Result<(Query, RuleAction), AppError> {
        // 设计文档 §6.4 第 1 条：规则名非空。放在这里而不是 create/update 各自开头，
        // 是为了让「保存时的全部校验」只有 build 这一个入口。
        if name.trim().is_empty() {
            return Err(AppError::InvalidQuery("规则名不能为空".to_string()));
        }
        let schemas = self.schemas(ws)?;
        let trigger = Query::parse(trigger_expr)?;
        trigger.validate_for_rule(&schemas, true)?;
        if writes.is_empty() {
            return Err(AppError::InvalidQuery("规则至少要有一个标签动作".to_string()));
        }
        let target = if target_event_source {
            ActionTarget::EventSource
        } else {
            let expr = target_expr.unwrap_or("").trim();
            if expr.is_empty() {
                return Err(AppError::InvalidQuery(
                    "动作目标为「表达式圈定」时必须填写表达式".to_string(),
                ));
            }
            let q = Query::parse(expr)?;
            q.validate_for_rule(&schemas, false)?;
            ActionTarget::Query(q)
        };
        for w in &writes {
            let schema = schemas
                .iter()
                .find(|s| s.name == w.label_name)
                .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {}", w.label_name)))?;
            match (w.op, &w.value) {
                (WriteOp::Remove, Some(_)) => {
                    return Err(AppError::InvalidQuery(format!(
                        "删除动作不能带值（标签 {}）",
                        w.label_name
                    )))
                }
                (WriteOp::Remove, None) => {}
                (WriteOp::Set, None) => {
                    return Err(AppError::InvalidQuery(format!(
                        "写入动作必须指定值来源（标签 {}）",
                        w.label_name
                    )))
                }
                (WriteOp::Set, Some(ValueSource::Literal(v))) => {
                    // 字面量不合法在保存时就报出来，不留到运行时才发现。
                    LabelValue::from_json(v, schema).map_err(|_| {
                        AppError::InvalidQuery(format!(
                            "标签 {} 的值不合法: {v}",
                            w.label_name
                        ))
                    })?;
                }
                (WriteOp::Set, Some(_)) => {}
            }
        }
        Ok((
            trigger,
            RuleAction {
                target,
                writes,
            },
        ))
    }

    pub(crate) fn schemas(&self, ws: Ulid) -> Result<Vec<LabelSchema>, AppError> {
        let mut out = Vec::new();
        for (_, v) in self.store.scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())? {
            if let Ok(s) = bincode::deserialize::<LabelSchema>(&v) {
                out.push(s);
            }
        }
        Ok(out)
    }
```

- [ ] **Step 12: 审计变体**

`src/domain/audit.rs` 的 `AuditAction` **末尾**（`InviteRevoked,` 之后）追加：

```rust
    RuleCreated,
    RuleUpdated,
    RuleDeleted,
    RuleApplied,
```

`src/frontend/components.rs` 的 `action_label()` 加四行文案：

```rust
        "RuleCreated" => "创建规则",
        "RuleUpdated" => "更新规则",
        "RuleDeleted" => "删除规则",
        "RuleApplied" => "规则触发",
```

- [ ] **Step 13: `src/service/mod.rs`——注册服务**

```rust
pub mod rule;
```

```rust
pub use rule::RuleService;
```

**只导出 `RuleService`**——`RuleEngine` 到 Task 3 才存在，此处一并导出会编译失败。Task 3 收尾时再把这一行改成 `pub use rule::{RuleEngine, RuleService};`。

`Services` 结构体加字段 `pub rule: RuleService,`，`Services::new` 里加：

```rust
            rule: RuleService::new(store.clone()),
```

（`RuleEngine` 此处先不接——Task 4 才动 `EntryService`。）

- [ ] **Step 14: 编译并跑测试**

Run: `cargo test --lib service::rule 2>&1 | tail -20 && make check`
Expected: 6 个测试 PASS；两个目标编译通过。

- [ ] **Step 15: 提交**

```bash
git add src/domain/query.rs src/domain/audit.rs src/domain/mod.rs src/service/rule.rs src/service/mod.rs src/service/entry.rs src/frontend/components.rs
git commit -m "feat(domain): add \$label/\$old/\$new event fields and rule service"
```

---

### Task 3: 规则执行引擎

**Files:**
- Modify: `src/service/rule.rs`
- Modify: `src/service/entry.rs`（抽出 `list_entries` 与 `labeling_ops` 两个自由函数，供引擎共用）
- Modify: `src/service/mod.rs`（`pub use rule::RuleService;` → `pub use rule::{RuleEngine, RuleService};`）

**Interfaces:**
- Consumes: Task 1/2 的全部类型；`service::entry::{list_entries, labeling_ops}`；`RuleService::{new, list, schemas}`
- Produces:
  - `service::entry::list_entries(store: &DocStore, ws: Ulid) -> Result<Vec<Entry>, AppError>`
  - `service::entry::labeling_ops(ws: Ulid, code: &str, label_name: &str, value: Option<&LabelValue>, actor: Ulid, before: Option<&Labeling>) -> Result<Vec<BatchOp>, AppError>`
  - `service::rule::{RuleEngine, StagedWrite, RulePlan, USER_LEVEL, MAX_LEVEL}`
  - `RuleEngine::plan(&self, ws: Ulid, user_writes: &[StagedWrite]) -> Result<Option<RulePlan>, AppError>`

- [ ] **Step 1: `src/service/entry.rs`——抽出 `list_entries`**

把 `EntryService::list` 的函数体搬进自由函数，方法改为薄封装：

```rust
/// 工作空间内的「在视图内」条目：不含已删除，也不含已归档。
/// 抽成自由函数是为了让 `RuleEngine` 的目标圈定与视图列表口径完全一致——
/// 规则引擎不能依赖 `EntryService`（会形成循环依赖）。
pub fn list_entries(store: &DocStore, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
    let prefix = workspace_id.to_bytes();
    let rows = store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &prefix)?;
    let mut entries = Vec::new();
    for (key, _) in rows {
        if key.len() <= 16 {
            continue;
        }
        let code = std::str::from_utf8(&key[16..]).unwrap_or("").to_string();
        if let Some(e) = store.get::<Entry>(cf::ENTRIES, code.as_bytes())? {
            let archived = store.get_raw(cf::ENTRIES_ARCHIVED, code.as_bytes())?.is_some();
            if !e.is_deleted() && !archived {
                entries.push(e);
            }
        }
    }
    entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(entries)
}
```

方法体：

```rust
    pub fn list(&self, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
        list_entries(&self.store, workspace_id)
    }
```

`is_archived` 读法确认：若既有实现不是 `store.get_raw(cf::ENTRIES_ARCHIVED, code.as_bytes())?.is_some()`，就照它的实现写进 `list_entries`，别改语义。

- [ ] **Step 2: `src/service/entry.rs`——抽出 `labeling_ops`**

```rust
/// 构造「写 / 删一个标签」的全套 ops：Labeling 两条（主键 + 工作空间索引）+ 一条标签审计。
/// `before` 由调用方给定：`EntryService` 从库里读，`RuleEngine` 从内存后置状态读——
/// 引擎的写入尚未提交，读库拿到的是前像。
pub fn labeling_ops(
    ws: Ulid,
    code: &str,
    label_name: &str,
    value: Option<&LabelValue>,
    actor: Ulid,
    before: Option<&Labeling>,
) -> Result<Vec<BatchOp>, AppError> {
    match value {
        Some(lv) => {
            let labeling = Labeling::new(code.to_string(), label_name.to_string(), lv.clone(), actor);
            let audit = AuditLog::new(
                AuditAction::LabelingSet,
                actor,
                "labeling",
                code,
                Some(ws),
                before.map(|l| serde_json::to_string(l).unwrap_or_default()),
                Some(serde_json::to_string(&labeling).unwrap_or_default()),
            );
            let mut ops = audit_ops(&audit)?;
            ops.push(BatchOp::put(
                cf::LABELINGS,
                keys::labeling_key(code, label_name),
                &labeling,
            )?);
            ops.push(BatchOp::put(
                cf::LABELINGS_BY_WORKSPACE,
                keys::labeling_by_workspace_key(ws, code, label_name),
                &labeling,
            )?);
            Ok(ops)
        }
        None => {
            let audit = AuditLog::new(
                AuditAction::LabelingRemoved,
                actor,
                "labeling",
                code,
                Some(ws),
                before.map(|l| serde_json::to_string(l).unwrap_or_default()),
                None,
            );
            let mut ops = audit_ops(&audit)?;
            ops.push(BatchOp::delete(cf::LABELINGS, keys::labeling_key(code, label_name)));
            ops.push(BatchOp::delete(
                cf::LABELINGS_BY_WORKSPACE,
                keys::labeling_by_workspace_key(ws, code, label_name),
            ));
            Ok(ops)
        }
    }
}
```

`set_labeling` / `set_labelings` / `remove_labeling` 改为调用它，行为与现有实现逐字节等价（同样的 ops 顺序、同样的审计）。`set_labeling` 仍返回它构造的 `Labeling`，因此在方法内保留一次 `Labeling::new` 用于返回值：

```rust
        let before = self
            .store
            .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(entry_code, label_name))?;
        let mut ops = labeling_ops(
            entry.workspace_id,
            entry_code,
            label_name,
            Some(&lv),
            actor,
            before.as_ref(),
        )?;
        let labeling = Labeling::new(entry_code.to_string(), label_name.to_string(), lv, actor);
        self.store.write_batch(ops)?;
        ...
```

（`ops` 里那条 `Labeling` 与返回值是两个等价副本，落库的是 ops 里那个；`Labeling::new` 只多取一次 `Utc::now()`，`set_at` 可能差几微秒。若在意，把 `labeling_ops` 改成接收已构造好的 `Labeling` 引用。实现时二选一，保持返回值与落库值一致即可。）

- [ ] **Step 3: 写引擎的单测**

追加到 `src/service/rule.rs` 的 `mod tests`（新开一个 `mod engine` 子模块，复用上面的 `schema` / `schemas` 辅助函数）：

```rust
    mod engine {
        use super::*;
        use crate::service::entry::{list_entries, labeling_ops};
        use crate::service::rule::RuleEngine;
        use crate::service::EntryService;
        use crate::storage::DocStore;
        use std::sync::Arc;

        /// 建一个只有 schema + 条目的临时工作空间。
        fn setup() -> (String, Arc<DocStore>, Ulid, Ulid) {
            let mut dir = std::env::temp_dir();
            dir.push(format!("rodeo-rule-test-{}", Ulid::new()));
            let path = dir.to_string_lossy().to_string();
            let store = Arc::new(DocStore::open(&path).unwrap());
            let ws = Ulid::new();
            let actor = Ulid::new();
            for s in schemas() {
                let mut s = s;
                s.workspace_id = ws;
                store.put(cf::LABEL_SCHEMAS, &keys::label_schema_key(ws, &s.name), &s).unwrap();
            }
            (path, store, ws, actor)
        }

        /// 建一条「Status 变 Finished 就写 FinishedAt = now」的规则。
        fn rule_set_finished_at(store: &Arc<DocStore>, ws: Ulid, actor: Ulid) -> AutomationRule {
            let svc = RuleService::new(store.clone());
            svc.create(
                actor,
                ws,
                "完成时间",
                true,
                r#"$label = "Status" AND $new = "Finished""#,
                true,
                None,
                vec![LabelWrite {
                    label_name: "FinishedAt".into(),
                    op: WriteOp::Set,
                    value: Some(ValueSource::Now),
                }],
            )
            .unwrap()
        }

        fn set(store: &Arc<DocStore>, ws: Ulid, code: &str, name: &str, lv: LabelValue, actor: Ulid) {
            let before = store.get::<Labeling>(cf::LABELINGS, &keys::labeling_key(code, name)).unwrap();
            let ops = labeling_ops(ws, code, name, Some(&lv), actor, before.as_ref()).unwrap();
            store.write_batch(ops).unwrap();
        }

        fn get(store: &Arc<DocStore>, code: &str, name: &str) -> Option<LabelValue> {
            store
                .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(code, name))
                .unwrap()
                .map(|l| l.value)
        }

        /// 走一遍「用户写入 + 引擎 plan + 同批提交」的真实路径。
        fn apply(
            store: &Arc<DocStore>,
            ws: Ulid,
            actor: Ulid,
            writes: &[StagedWrite],
        ) {
            for w in writes {
                let before = store
                    .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(&w.entry_code, &w.label_name))
                    .unwrap();
                let ops = labeling_ops(ws, &w.entry_code, &w.label_name, w.value.as_ref(), w.actor, before.as_ref())
                    .unwrap();
                store.write_batch(ops).unwrap();
            }
            let engine = RuleEngine::new(store.clone());
            // 注意：plan 必须在提交之前算，否则读到的 before 是新值。
            let plan = engine.plan(ws, writes).unwrap();
            if let Some(p) = plan {
                store.write_batch(p.ops).unwrap();
            }
        }

        #[test]
        fn status_finished_writes_finished_at() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            rule_set_finished_at(&store, ws, actor);
            apply(&store, ws, actor, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let got = get(&store, &entry.code, "FinishedAt");
            assert!(got.is_some(), "Status=Finished 应触发 FinishedAt 写入");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn rewrites_with_unchanged_value_do_not_fire() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            rule_set_finished_at(&store, ws, actor);
            // 先真的写一次 Finished，让规则触发。
            apply(&store, ws, actor, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let first = get(&store, &entry.code, "FinishedAt");
            // 再把同一个值写一遍：值没变，不产生事件，规则不该重跑。
            apply(&store, ws, actor, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            assert_eq!(get(&store, &entry.code, "FinishedAt"), first, "重复写同值不得刷新 FinishedAt");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn cascade_stops_at_max_level() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            // A: 任意 Status 写入 → Priority = 1；B: Priority 落在范围内 → Priority = 2 …
            // 用 Priority 自身做自触发的链，验证三层到顶后停下（而不是无限循环）。
            let svc = RuleService::new(store.clone());
            svc.create(actor, ws, "P+A", true, "Priority", true, None, vec![LabelWrite {
                label_name: "Priority".into(), op: WriteOp::Set,
                value: Some(ValueSource::Literal(serde_json::json!(1))),
            }]).unwrap();
            svc.create(actor, ws, "P+B", true, "Priority", true, None, vec![LabelWrite {
                label_name: "Priority".into(), op: WriteOp::Set,
                value: Some(ValueSource::Literal(serde_json::json!(2))),
            }]).unwrap();
            // 同层两条规则都写 Priority，后者覆盖前者，因此只应该发生有限轮写入。
            apply(&store, ws, actor, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Priority".into(),
                value: Some(LabelValue::Int(0)),
                actor,
            }]);
            assert_eq!(get(&store, &entry.code, "Priority"), Some(LabelValue::Int(2)));
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn rule_audit_records_trigger_and_writes() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            rule_set_finished_at(&store, ws, actor);
            apply(&store, ws, actor, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let audits = crate::service::AuditService::new(store.clone()).list(ws, 100).unwrap();
            let applied = audits
                .iter()
                .find(|a| a.action == AuditAction::RuleApplied)
                .expect("规则命中应写一条 RuleApplied");
            let after = applied.after.as_deref().unwrap_or("");
            assert!(after.contains("FinishedAt"), "审计应记录规则写了哪些标签: {after}");
            std::fs::remove_dir_all(&dir).ok();
        }
    }
```

- [ ] **Step 4: 运行测试确认失败**

Run: `cargo test --lib service::rule 2>&1 | grep -E "^error|not found|FAILED" | head -20`
Expected: 编译失败——`RuleEngine`、`StagedWrite` 未定义。

- [ ] **Step 5: 实现引擎**

追加到 `src/service/rule.rs`（在 `RuleService` 之后）：

```rust
// ---------- 执行引擎 ----------

/// 用户直接写入产生的事件层级。
pub const USER_LEVEL: u8 = 0;
/// 规则动作最多再触发几轮（level 1..=MAX_LEVEL）。到顶后静默截断。
pub const MAX_LEVEL: u8 = 3;

/// 一次待提交的标签变更。`value = None` 表示删除。
#[derive(Debug, Clone)]
pub struct StagedWrite {
    pub entry_code: String,
    pub label_name: String,
    pub value: Option<LabelValue>,
    pub actor: Ulid,
}

/// 引擎算出的补充批次：规则动作的写入 + 它们的审计，外加需要 reindex 的条目。
/// 与用户写入拼成**一个** `write_batch` 提交，任一步失败整体回滚。
pub struct RulePlan {
    pub ops: Vec<BatchOp>,
    pub affected: Vec<String>,
}

/// 标签后置状态：条目 → (标签名 → Labeling)。规则求值看的是「写入之后」的状态，
/// 而写入尚未提交，因此引擎自己维护一份内存视图。
struct Overlay {
    labels: std::collections::HashMap<String, std::collections::HashMap<String, Labeling>>,
}

impl Overlay {
    fn load(store: &DocStore, ws: Ulid) -> Result<Self, AppError> {
        let mut labels: std::collections::HashMap<String, std::collections::HashMap<String, Labeling>> =
            std::collections::HashMap::new();
        for (_, v) in store.scan_prefix(cf::LABELINGS_BY_WORKSPACE, &ws.to_bytes())? {
            if let Ok(l) = bincode::deserialize::<Labeling>(&v) {
                labels
                    .entry(l.entry_code.clone())
                    .or_default()
                    .insert(l.label_name.clone(), l);
            }
        }
        Ok(Self { labels })
    }

    fn get(&self, code: &str, name: &str) -> Option<&Labeling> {
        self.labels.get(code).and_then(|m| m.get(name))
    }

    fn value_of(&self, code: &str, name: &str) -> Option<LabelValue> {
        self.get(code, name).map(|l| l.value.clone())
    }

    fn labels_of(&self, code: &str) -> Vec<Labeling> {
        self.labels
            .get(code)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    fn apply(&mut self, w: &StagedWrite) {
        let entry = self.labels.entry(w.entry_code.clone()).or_default();
        match &w.value {
            Some(v) => {
                entry.insert(
                    w.label_name.clone(),
                    Labeling::new(w.entry_code.clone(), w.label_name.clone(), v.clone(), w.actor),
                );
            }
            None => {
                entry.remove(&w.label_name);
            }
        }
    }
}

/// 工作空间标签 schema 的按名索引。
struct Schemas {
    by_name: std::collections::HashMap<String, LabelSchema>,
}

impl Schemas {
    fn new(list: Vec<LabelSchema>) -> Self {
        Self {
            by_name: list.into_iter().map(|s| (s.name.clone(), s)).collect(),
        }
    }

    fn get(&self, name: &str) -> Option<&LabelSchema> {
        self.by_name.get(name)
    }

    fn label_of(&self, name: &str) -> Option<(crate::domain::LabelValueType, Option<String>)> {
        self.by_name.get(name).map(|s| (s.value_type, s.format.clone()))
    }
}

pub struct RuleEngine {
    store: Arc<DocStore>,
}

impl RuleEngine {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    /// 给定一次请求内的用户标签写入，算出规则动作要补的全部 ops。
    /// 没有启用规则时返回 `Ok(None)`——常态写入因此不为规则付出任何代价。
    /// **必须在用户写入提交之前调用**：`before` 取自库里的前像。
    pub fn plan(&self, ws: Ulid, user_writes: &[StagedWrite]) -> Result<Option<RulePlan>, AppError> {
        let rules: Vec<AutomationRule> = RuleService::new(self.store.clone())
            .list(ws)?
            .into_iter()
            .filter(|r| r.enabled)
            .collect();
        if rules.is_empty() {
            return Ok(None);
        }

        let schemas = Schemas::new(RuleService::new(self.store.clone()).schemas(ws)?);
        let candidates = list_entries(&self.store, ws)?;
        let mut overlay = Overlay::load(&self.store, ws)?;
        let accounts = if rules.iter().any(|r| r.trigger.contains_account_field()) {
            self.accounts()?
        } else {
            std::collections::HashMap::new()
        };

        // level 0：用户写入全部落库（现有语义不变），但只有真正变化的成为事件。
        let mut events = collect_diff(ws, &overlay, user_writes, USER_LEVEL);
        let mut affected: Vec<String> = user_writes.iter().map(|w| w.entry_code.clone()).collect();
        for w in user_writes {
            overlay.apply(w);
        }

        let now = chrono::Utc::now();
        let mut ops: Vec<BatchOp> = Vec::new();
        let mut level = USER_LEVEL;

        while level < MAX_LEVEL && !events.is_empty() {
            let next = level + 1;
            // (规则下标, 写入)：下标用于把最终生效的写入归给产出它的规则。
            let mut staged: Vec<(usize, StagedWrite)> = Vec::new();
            let mut fired: Vec<usize> = Vec::new();

            for (ri, rule) in rules.iter().enumerate() {
                let matched: Vec<&LabelEvent> = events
                    .iter()
                    .filter(|ev| self.trigger_matches(rule, ev, &overlay, &schemas, &accounts))
                    .collect();
                if matched.is_empty() {
                    continue;
                }
                fired.push(ri);
                // 圈定结果在层内可复用：层内 overlay 冻结，目标集合不会变。
                let target_codes = match &rule.action.target {
                    ActionTarget::EventSource => Vec::new(),
                    ActionTarget::Query(q) => {
                        self.match_entries(q, &candidates, &overlay, &schemas, &accounts)
                    }
                };
                for ev in &matched {
                    let codes: Vec<String> = match &rule.action.target {
                        ActionTarget::EventSource => vec![ev.entry_code.clone()],
                        ActionTarget::Query(_) => target_codes.clone(),
                    };
                    for code in codes {
                        for w in &rule.action.writes {
                            let Some(value) =
                                resolve_value(rule, w, ev, &schemas, now)?
                            else {
                                // $old 遇上新增、$new 遇上删除：该事件不产生这条写入。
                                // 这不是错误——删除标签的操作不该因为规则引用旧值而失败。
                                continue;
                            };
                            staged.push((
                                ri,
                                StagedWrite {
                                    entry_code: code.clone(),
                                    label_name: w.label_name.clone(),
                                    value,
                                    actor: ev.actor,
                                },
                            ));
                        }
                    }
                }
            }

            if staged.is_empty() {
                break;
            }
            // 同层同 (entry, label)：后者覆盖前者；与层初值相同的丢弃（级联的安全阀）。
            let collapsed = collapse(&overlay, &staged);
            if collapsed.is_empty() {
                break;
            }

            let mut next_events = Vec::new();
            for (_, w) in &collapsed {
                let before = overlay.get(&w.entry_code, &w.label_name).cloned();
                ops.extend(labeling_ops(
                    ws,
                    &w.entry_code,
                    &w.label_name,
                    w.value.as_ref(),
                    w.actor,
                    before.as_ref(),
                )?);
                next_events.push(LabelEvent {
                    workspace_id: ws,
                    entry_code: w.entry_code.clone(),
                    label_name: w.label_name.clone(),
                    old: before.map(|l| l.value),
                    new: w.value.clone(),
                    actor: w.actor,
                    level: next,
                });
                overlay.apply(w);
                affected.push(w.entry_code.clone());
            }

            for ri in fired {
                let own: Vec<(&StagedWrite, bool)> = staged
                    .iter()
                    .filter(|(r, _)| *r == ri)
                    .map(|(_, w)| {
                        let applied = collapsed.iter().any(|(cr, cw)| {
                            *cr == ri
                                && cw.entry_code == w.entry_code
                                && cw.label_name == w.label_name
                        });
                        (w, applied)
                    })
                    .collect();
                let audit = rule_audit(&rules[ri], next, &events, &own);
                ops.extend(audit_ops(&audit)?);
            }

            events = next_events;
            level = next;
        }

        affected.sort();
        affected.dedup();
        Ok(Some(RulePlan { ops, affected }))
    }

    fn accounts(
        &self,
    ) -> Result<std::collections::HashMap<Ulid, (String, String)>, AppError> {
        let mut m = std::collections::HashMap::new();
        for (_, v) in self.store.scan_prefix(cf::ACCOUNTS, b"")? {
            let a: crate::domain::Account = bincode::deserialize(&v)?;
            m.insert(a.id, (a.name, a.email));
        }
        Ok(m)
    }

    /// 事件源条目：触发条件里的条目字段求的是它的写入后状态。
    /// 条目可能已被归档 / 软删除（列表口径里没有），此时直接读库补上，不静默跳过。
    fn entry_of(&self, code: &str) -> Result<Option<Entry>, AppError> {
        self.store.get::<Entry>(cf::ENTRIES, code.as_bytes())
    }

    fn trigger_matches(
        &self,
        rule: &AutomationRule,
        ev: &LabelEvent,
        overlay: &Overlay,
        schemas: &Schemas,
        accounts: &std::collections::HashMap<Ulid, (String, String)>,
    ) -> bool {
        let Ok(Some(entry)) = self.entry_of(&ev.entry_code) else {
            return false;
        };
        let labels = overlay.labels_of(&ev.entry_code);
        let env = crate::domain::EvalEnv {
            text_hit: &|_| false,
            account_of: &|id| accounts.get(&id).cloned(),
            label_of: &|n| schemas.label_of(n),
            event: Some(ev),
        };
        rule.trigger.evaluate(&entry, &labels, &env)
    }

    fn match_entries(
        &self,
        q: &Query,
        candidates: &[Entry],
        overlay: &Overlay,
        schemas: &Schemas,
        accounts: &std::collections::HashMap<Ulid, (String, String)>,
    ) -> Vec<String> {
        candidates
            .iter()
            .filter(|e| {
                let labels = overlay.labels_of(&e.code);
                let env = crate::domain::EvalEnv {
                    text_hit: &|_| false,
                    account_of: &|id| accounts.get(&id).cloned(),
                    label_of: &|n| schemas.label_of(n),
                    event: None,
                };
                q.evaluate(e, &labels, &env)
            })
            .map(|e| e.code.clone())
            .collect()
    }
}
```

配套的自由函数：

```rust
/// 按 (entry, label) 归并一层内的写入，返回真正发生变化的那些事件。
fn collect_diff(ws: Ulid, overlay: &Overlay, writes: &[StagedWrite], level: u8) -> Vec<LabelEvent> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    // 逆序遍历：同一个 key 只保留最后一次写入，但保持首次出现的顺序。
    let mut last: std::collections::HashMap<(String, String), &StagedWrite> =
        std::collections::HashMap::new();
    for w in writes {
        last.insert((w.entry_code.clone(), w.label_name.clone()), w);
    }
    for w in writes {
        let key = (w.entry_code.clone(), w.label_name.clone());
        if !seen.insert(key.clone()) {
            continue;
        }
        let w = last[&key];
        let old = overlay.value_of(&w.entry_code, &w.label_name);
        if old != w.value {
            out.push(LabelEvent {
                workspace_id: ws,
                entry_code: w.entry_code.clone(),
                label_name: w.label_name.clone(),
                old,
                new: w.value.clone(),
                actor: w.actor,
                level,
            });
        }
    }
    out
}

/// 同层归并：同 (entry, label) 后者覆盖前者；与层初值相同者丢弃。
fn collapse(overlay: &Overlay, staged: &[(usize, StagedWrite)]) -> Vec<(usize, StagedWrite)> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut last: std::collections::HashMap<(String, String), (usize, StagedWrite)> =
        std::collections::HashMap::new();
    for (ri, w) in staged {
        let key = (w.entry_code.clone(), w.label_name.clone());
        if !last.contains_key(&key) {
            order.push(key.clone());
        }
        last.insert(key, (*ri, w.clone()));
    }
    let mut out = Vec::new();
    for key in order {
        let (ri, w) = last.remove(&key).unwrap();
        if overlay.value_of(&w.entry_code, &w.label_name) != w.value {
            out.push((ri, w));
        }
    }
    out
}

/// 算出动作要写入的值。`Ok(None)` = 该事件不产生这条写入（$old 无旧值 / $new 无新值）。
fn resolve_value(
    rule: &AutomationRule,
    w: &LabelWrite,
    ev: &LabelEvent,
    schemas: &Schemas,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<LabelValue>, AppError> {
    if w.op == WriteOp::Remove {
        return Ok(None);
    }
    let Some(schema) = schemas.get(&w.label_name) else {
        return Err(AppError::RuleFailed(format!(
            "规则「{}」写标签「{}」失败：标签不存在",
            rule.name, w.label_name
        )));
    };
    let bad = |why: &str| {
        AppError::RuleFailed(format!(
            "规则「{}」写标签「{}」失败：{why}",
            rule.name, w.label_name
        ))
    };
    match w.value.as_ref() {
        Some(ValueSource::Literal(v)) => Ok(Some(
            LabelValue::from_json(v, schema).map_err(|_| bad("字面量的值不合法"))?,
        )),
        Some(ValueSource::Now) => Ok(Some(now_value(schema, now).map_err(|e| bad(&e))?)),
        // $old / $new 按目标标签的类型重新校验：跨类型转发（如把枚举值写进整数标签）
        // 在运行时被拦下，整体回滚，不留半成品。
        Some(ValueSource::New) => match &ev.new {
            Some(v) => Ok(Some(
                LabelValue::from_json(&v.to_json(), schema).map_err(|_| bad("事件的新值与目标标签类型不兼容"))?,
            )),
            None => Ok(None),
        },
        Some(ValueSource::Old) => match &ev.old {
            Some(v) => Ok(Some(
                LabelValue::from_json(&v.to_json(), schema).map_err(|_| bad("事件的旧值与目标标签类型不兼容"))?,
            )),
            None => Ok(None),
        },
        None => Err(bad("写入动作缺少值来源")),
    }
}

/// `now` 只对时间型标签有意义：按 schema 的布局渲染成存储串。
fn now_value(schema: &LabelSchema, now: chrono::DateTime<chrono::Utc>) -> Result<LabelValue, String> {
    use crate::domain::LabelValueType::*;
    use chrono::{Datelike, Timelike};
    if !matches!(schema.value_type, Date | Time | DateTime) {
        return Err("now 只能写入日期 / 时间 / 日期时间型标签".to_string());
    }
    let layout = schema
        .format
        .as_deref()
        .unwrap_or_else(|| crate::domain::label::default_layout(schema.value_type));
    let t = crate::golayout::YmdHms {
        year: now.year(),
        month: now.month(),
        day: now.day(),
        hour: now.hour(),
        minute: now.minute(),
        second: now.second(),
    };
    let s = crate::golayout::format(layout, t);
    LabelValue::from_json(&serde_json::Value::String(s), schema)
        .map_err(|_| "当前时间无法按标签的布局渲染".to_string())
}

/// 一条 RuleApplied 审计：规则本次命中了哪些事件、它算了哪些写入、哪些真正生效。
fn rule_audit(
    rule: &AutomationRule,
    level: u8,
    events: &[LabelEvent],
    writes: &[(&StagedWrite, bool)],
) -> AuditLog {
    let triggers: Vec<serde_json::Value> = events
        .iter()
        .map(|ev| {
            serde_json::json!({
                "entryCode": ev.entry_code,
                "labelName": ev.label_name,
                "old": ev.old.as_ref().map(LabelValue::to_json),
                "new": ev.new.as_ref().map(LabelValue::to_json),
            })
        })
        .collect();
    let writes: Vec<serde_json::Value> = writes
        .iter()
        .map(|(w, applied)| {
            serde_json::json!({
                "entryCode": w.entry_code,
                "labelName": w.label_name,
                "value": w.value.as_ref().map(LabelValue::to_json),
                "applied": applied,
            })
        })
        .collect();
    let after = serde_json::json!({
        "ruleId": rule.id.to_string(),
        "ruleName": rule.name,
        "level": level,
        "triggers": triggers,
        "writes": writes,
    });
    // actor 用规则创建者：RuleApplied 是规则自身的动作记录，与触发者无关
    // （被规则写入的标签另有 LabelingSet 审计，那里记的是触发者）。
    AuditLog::new(
        AuditAction::RuleApplied,
        rule.created_by,
        RESOURCE_TYPE,
        &rule.id.to_string(),
        Some(rule.workspace_id),
        None,
        Some(after.to_string()),
    )
}
```

`src/domain/label.rs` 的 `default_layout` 已是 `pub`；`crate::domain::label::default_layout` 可达（`domain/mod.rs` 也 re-export 了 `default_layout`，直接用 `crate::domain::default_layout` 亦可）。

- [ ] **Step 6: 运行测试**

Run: `cargo test --lib service::rule 2>&1 | tail -25`
Expected: 全部 PASS（6 个 DSL 测试 + 4 个引擎测试）。

- [ ] **Step 7: 提交**

```bash
git add src/service/rule.rs src/service/entry.rs src/service/mod.rs
git commit -m "feat(service): add layered rule engine with post-state overlay"
```

---

### Task 4: 接线到标签写入路径

**Files:**
- Modify: `src/service/entry.rs`
- Modify: `src/error.rs`

**Interfaces:**
- Consumes: Task 3 的 `RuleEngine::{new, plan}`、`StagedWrite`
- Produces: `AppError::RuleFailed(String)`（code `RULE_FAILED`）；`EntryService` 的三个写入方法在此之后会自动执行规则

- [ ] **Step 1: `src/error.rs` 加变体**

`AppError` 末尾（`Ai(String),` 之后）追加：

```rust
    #[error("{0}")]
    RuleFailed(String),
```

`code()` 末尾加：

```rust
            AppError::RuleFailed(_) => "RULE_FAILED",
```

- [ ] **Step 2: `EntryService` 持有引擎**

```rust
pub struct EntryService {
    store: Arc<DocStore>,
    search: Option<Arc<SearchIndex>>,
    rules: RuleEngine,
}

impl EntryService {
    pub fn new(store: Arc<DocStore>) -> Self {
        let rules = RuleEngine::new(store.clone());
        Self { store, search: None, rules }
    }

    pub fn with_search(store: Arc<DocStore>, search: Arc<SearchIndex>) -> Self {
        let rules = RuleEngine::new(store.clone());
        Self { store, search: Some(search), rules }
    }
```

use 补 `use crate::service::rule::{RuleEngine, StagedWrite};`。

- [ ] **Step 3: `set_labeling` 接入**

在 `self.store.write_batch(ops)?;` **之前**插入。注意 `plan` 必须在提交前算（`before` 取自库里的前像）：

```rust
        let staged = StagedWrite {
            entry_code: entry_code.to_string(),
            label_name: label_name.to_string(),
            value: Some(lv.clone()),
            actor,
        };
        // 规则动作与用户写入拼成同一批：任一步失败整体回滚，不留半成品。
        if let Some(plan) = self.rules.plan(entry.workspace_id, std::slice::from_ref(&staged))? {
            ops.extend(plan.ops);
        }
        self.store.write_batch(ops)?;
```

- [ ] **Step 4: `set_labelings` 接入**

在 `let written = entries.len() * resolved.len();` 之后、`self.store.write_batch(ops)?;` 之前插入：

```rust
        let staged: Vec<StagedWrite> = entries
            .iter()
            .flat_map(|e| {
                resolved.iter().map(move |(name, lv)| StagedWrite {
                    entry_code: e.code.clone(),
                    label_name: name.clone(),
                    value: Some(lv.clone()),
                    actor,
                })
            })
            .collect();
        if let Some(plan) = self.rules.plan(ws_id, &staged)? {
            ops.extend(plan.ops);
        }
```

- [ ] **Step 5: `remove_labeling` 接入**

在 `self.store.write_batch(ops)?;` 之前插入：

```rust
        let staged = StagedWrite {
            entry_code: entry_code.to_string(),
            label_name: label_name.to_string(),
            value: None,
            actor,
        };
        if let Some(plan) = self.rules.plan(entry.workspace_id, std::slice::from_ref(&staged))? {
            ops.extend(plan.ops);
        }
```

- [ ] **Step 6: 三个方法的 reindex 覆盖规则写入的条目**

`set_labeling` / `remove_labeling` 现有的 `if let Ok(Some(e)) = self.get(entry_code) { self.reindex(&e); }` 改成先收集 `affected`：

```rust
        let mut affected = vec![entry_code.to_string()];
        // ...（plan 之后）
        if let Some(plan) = &plan {
            affected.extend(plan.affected.iter().cloned());
        }
        self.store.write_batch(ops)?;
        affected.sort();
        affected.dedup();
        for code in &affected {
            if let Ok(Some(e)) = self.get(code) {
                self.reindex(&e);
            }
        }
```

`plan` 需要在 `write_batch` 之后仍可用，故用 `let plan = self.rules.plan(...)?;` 绑定一次，再 `if let Some(p) = &plan { ops.extend(p.ops.clone()) }`——或者把 `plan.ops` 用 `extend` 消费、把 `plan.affected` 先取出来：

```rust
        let plan = self.rules.plan(entry.workspace_id, std::slice::from_ref(&staged))?;
        let mut affected = vec![entry_code.to_string()];
        if let Some(p) = plan {
            affected.extend(p.affected);
            ops.extend(p.ops);
        }
        self.store.write_batch(ops)?;
        affected.sort();
        affected.dedup();
        for code in &affected {
            if let Ok(Some(e)) = self.get(code) {
                self.reindex(&e);
            }
        }
```

`set_labelings` 同理：`affected` 初始为 `entries.iter().map(|e| e.code.clone())`。

- [ ] **Step 7: 编译 + 跑既有测试**

Run: `cargo test --lib 2>&1 | tail -15 && make check`
Expected: 既有测试（含 `set_labeling` / `labelings_by_workspace` 等）全绿，两个目标编译通过。

- [ ] **Step 8: 提交**

```bash
git add src/service/entry.rs src/error.rs
git commit -m "feat(service): run automation rules in the label write transaction"
```

---

### Task 5: GraphQL

**Files:**
- Modify: `src/api/graphql.rs`

**Interfaces:**
- Consumes: `RuleService::{list, get, create, update, delete, parse_trigger}`、`domain::rule::*`
- Produces: 查询 `automationRules` / `parseRuleTrigger`；变更 `createAutomationRule` / `updateAutomationRule` / `deleteAutomationRule`

- [ ] **Step 1: 输出类型与入参类型**

放在 `GqlView` 之后：

```rust
#[derive(SimpleObject, Clone)]
pub struct GqlRuleWrite {
    label_name: String,
    op: String,
    /// literal / now / new / old
    value_kind: String,
    value: Json<serde_json::Value>,
}

#[derive(SimpleObject, Clone)]
pub struct GqlAutomationRule {
    id: ID,
    name: String,
    enabled: bool,
    trigger_expr: String,
    target_event_source: bool,
    /// 目标为「事件源条目」时为空串。
    target_expr: String,
    writes: Vec<GqlRuleWrite>,
    created_at: String,
    updated_at: String,
}

impl From<AutomationRule> for GqlAutomationRule {
    fn from(r: AutomationRule) -> Self {
        let trigger_expr = r.trigger.to_expr();
        let (target_event_source, target_expr) = match &r.action.target {
            ActionTarget::EventSource => (true, String::new()),
            ActionTarget::Query(q) => (false, q.to_expr()),
        };
        let writes = r
            .action
            .writes
            .iter()
            .map(|w| {
                let (kind, value) = match &w.value {
                    Some(ValueSource::Literal(v)) => ("literal", v.clone()),
                    Some(ValueSource::Now) => ("now", serde_json::Value::Null),
                    Some(ValueSource::New) => ("new", serde_json::Value::Null),
                    Some(ValueSource::Old) => ("old", serde_json::Value::Null),
                    None => ("", serde_json::Value::Null),
                };
                GqlRuleWrite {
                    label_name: w.label_name.clone(),
                    op: match w.op {
                        WriteOp::Set => "set".to_string(),
                        WriteOp::Remove => "remove".to_string(),
                    },
                    value_kind: kind.to_string(),
                    value: Json(value),
                }
            })
            .collect();
        Self {
            id: r.id.to_string().into(),
            name: r.name,
            enabled: r.enabled,
            trigger_expr,
            target_event_source,
            target_expr,
            writes,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }
    }
}

#[derive(InputObject)]
pub struct RuleWriteInput {
    label_name: String,
    op: String,
    /// literal / now / new / old
    value_kind: String,
    value: Option<Json<serde_json::Value>>,
}

/// 入参 → 领域写入。op / valueKind 的字符串在这里收敛成枚举，非法值给可读错误。
fn to_rule_writes(inputs: Vec<RuleWriteInput>) -> GqlResult<Vec<LabelWrite>> {
    let mut out = Vec::with_capacity(inputs.len());
    for i in inputs {
        let op = match i.op.as_str() {
            "set" => WriteOp::Set,
            "remove" => WriteOp::Remove,
            other => {
                return Err(AppError::InvalidQuery(format!("未知的标签操作: {other}")).into())
            }
        };
        let value = match op {
            WriteOp::Remove => None,
            WriteOp::Set => {
                let kind = match i.value_kind.as_str() {
                    "literal" => ValueSource::Literal(i.value.map(|j| j.0).unwrap_or(serde_json::Value::Null)),
                    "now" => ValueSource::Now,
                    "new" => ValueSource::New,
                    "old" => ValueSource::Old,
                    // 缺省即字面量：前端下拉未选时不该报错，值本身仍会被校验。
                    "" => ValueSource::Literal(i.value.map(|j| j.0).unwrap_or(serde_json::Value::Null)),
                    other => {
                        return Err(
                            AppError::InvalidQuery(format!("未知的值来源: {other}")).into()
                        )
                    }
                };
                Some(kind)
            }
        };
        out.push(LabelWrite {
            label_name: i.label_name,
            op,
            value,
        });
    }
    Ok(out)
}
```

- [ ] **Step 2: Query 字段**

`Query` 的 `parse_view_query` 之后追加：

```rust
    /// 规则列表：成员即可读。返回顺序即求值顺序（创建顺序）。
    async fn automation_rules(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
    ) -> GqlResult<Vec<GqlAutomationRule>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        Ok(gql
            .services
            .rule
            .list(ws)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// 解析并按规则规则校验触发条件，供编辑器实时校验。
    async fn parse_rule_trigger(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        expr: String,
    ) -> GqlResult<Json<serde_json::Value>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let q = gql.services.rule.parse_trigger(ws, &expr)?;
        Ok(Json(serde_json::to_value(&q).unwrap_or(serde_json::Value::Null)))
    }
```

- [ ] **Step 3: Mutation 字段**

`Mutation` 的 `delete_view` 附近追加：

```rust
    /// 新建规则（Maintainer+）。返回落库后的规则，前端据此刷新列表。
    #[allow(clippy::too_many_arguments)]
    async fn create_automation_rule(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        enabled: bool,
        trigger_expr: String,
        target_event_source: bool,
        target_expr: Option<String>,
        writes: Vec<RuleWriteInput>,
    ) -> GqlResult<GqlAutomationRule> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws, WorkspaceRole::Maintainer)?;
        let rule = gql.services.rule.create(
            auth.account_id,
            ws,
            &name,
            enabled,
            &trigger_expr,
            target_event_source,
            target_expr.as_deref(),
            to_rule_writes(writes)?,
        )?;
        Ok(rule.into())
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_automation_rule(
        &self,
        ctx: &Context<'_>,
        id: ID,
        name: String,
        enabled: bool,
        trigger_expr: String,
        target_event_source: bool,
        target_expr: Option<String>,
        writes: Vec<RuleWriteInput>,
    ) -> GqlResult<GqlAutomationRule> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let rule_id = parse_ulid(id.as_str())?;
        let existing = gql.services.rule.get(rule_id)?.ok_or(AppError::NotFound)?;
        gql.require_role(existing.workspace_id, WorkspaceRole::Maintainer)?;
        let rule = gql.services.rule.update(
            auth.account_id,
            rule_id,
            &name,
            enabled,
            &trigger_expr,
            target_event_source,
            target_expr.as_deref(),
            to_rule_writes(writes)?,
        )?;
        Ok(rule.into())
    }

    async fn delete_automation_rule(&self, ctx: &Context<'_>, id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let rule_id = parse_ulid(id.as_str())?;
        let existing = gql.services.rule.get(rule_id)?.ok_or(AppError::NotFound)?;
        gql.require_role(existing.workspace_id, WorkspaceRole::Maintainer)?;
        gql.services.rule.delete(auth.account_id, rule_id)?;
        Ok(true)
    }
```

- [ ] **Step 4: 编译**

Run: `make check`
Expected: 通过。若 async-graphql 报 `target_expr: Option<String>` 与简单对象字段同名冲突，把 resolver 参数名保持 `target_expr` 即可（GraphQL 侧自动 camelCase 成 `targetExpr`）。

- [ ] **Step 5: 手工验证 GraphQL 可用**

Run: `make serve`（另开终端），浏览器登录后打开 `<slug>/settings`，控制台里跑：

```js
await fetch('/graphql', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ query: 'mutation($id:ID!,$n:String!,$e:Boolean!,$t:String!,$tes:Boolean!,$w:[RuleWriteInput!]!){ createAutomationRule(workspaceId:$id,name:$n,enabled:$e,triggerExpr:$t,targetEventSource:$tes,writes:$w){ id name triggerExpr writes { labelName op valueKind } } }', variables: { id: '<workspaceId>', n: '完成时间', e: true, t: '$label = "Status" AND $new = "Finished"', tes: true, w: [{ labelName: 'FinishedAt', op: 'set', valueKind: 'now' }] } }) }).then(r => r.json())
```

Expected: 返回带 `id` 的规则对象，无 `errors`。

- [ ] **Step 6: 提交**

```bash
git add src/api/graphql.rs
git commit -m "feat(api): expose automation rule CRUD over GraphQL"
```

---

### Task 6: 前端（客户端 + 设置页「自动化」标签页）

**Files:**
- Modify: `src/frontend/graphql_client.rs`
- Create: `src/frontend/automation_tab.rs`
- Modify: `src/frontend/mod.rs`
- Modify: `src/frontend/pages/settings.rs`

**Interfaces:**
- Consumes: Task 5 的 GraphQL 字段
- Produces: `graphql_client::{automation_rules, create_automation_rule, update_automation_rule, delete_automation_rule, parse_rule_trigger, AutomationRule, RuleWrite}`；`automation_tab::AutomationTab(...) -> impl IntoView`

- [ ] **Step 1: `graphql_client.rs` 加模型与函数**

模型放在 `View` 之后：

```rust
// ---------- 自动化规则（AutomationRule） ----------

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleWrite {
    pub label_name: String,
    pub op: String,
    pub value_kind: String,
    pub value: Value,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger_expr: String,
    pub target_event_source: bool,
    pub target_expr: String,
    pub writes: Vec<RuleWrite>,
    pub created_at: String,
    pub updated_at: String,
}
```

函数：

```rust
const RULE_FIELDS: &str = "id name enabled triggerExpr targetEventSource targetExpr \
     writes { labelName op valueKind value } createdAt updatedAt";

pub async fn automation_rules(workspace_id: &str) -> Result<Vec<AutomationRule>, String> {
    let q = format!("query($id: ID!) {{ automationRules(workspaceId: $id) {{ {RULE_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("automationRules").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn parse_rule_trigger(workspace_id: &str, expr: &str) -> Result<Value, String> {
    let data = graphql(
        "query($id: ID!, $e: String!) { parseRuleTrigger(workspaceId: $id, expr: $e) }",
        json!({ "id": workspace_id, "e": expr }),
    )
    .await?;
    Ok(data.get("parseRuleTrigger").cloned().unwrap_or(Value::Null))
}

/// `writes` 是 `[{labelName, op, valueKind, value}]`。
pub async fn create_automation_rule(
    workspace_id: &str,
    name: &str,
    enabled: bool,
    trigger_expr: &str,
    target_event_source: bool,
    target_expr: Option<&str>,
    writes: &Value,
) -> Result<AutomationRule, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $e: Boolean!, $t: String!, $tes: Boolean!, $te: String, $w: [RuleWriteInput!]!) {{ \
         createAutomationRule(workspaceId: $id, name: $n, enabled: $e, triggerExpr: $t, \
         targetEventSource: $tes, targetExpr: $te, writes: $w) {{ {RULE_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({ "id": workspace_id, "n": name, "e": enabled, "t": trigger_expr,
                "tes": target_event_source, "te": target_expr, "w": writes }),
    )
    .await?;
    serde_json::from_value(data.get("createAutomationRule").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn update_automation_rule(
    id: &str,
    name: &str,
    enabled: bool,
    trigger_expr: &str,
    target_event_source: bool,
    target_expr: Option<&str>,
    writes: &Value,
) -> Result<AutomationRule, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $e: Boolean!, $t: String!, $tes: Boolean!, $te: String, $w: [RuleWriteInput!]!) {{ \
         updateAutomationRule(id: $id, name: $n, enabled: $e, triggerExpr: $t, \
         targetEventSource: $tes, targetExpr: $te, writes: $w) {{ {RULE_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({ "id": id, "n": name, "e": enabled, "t": trigger_expr,
                "tes": target_event_source, "te": target_expr, "w": writes }),
    )
    .await?;
    serde_json::from_value(data.get("updateAutomationRule").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn delete_automation_rule(id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { deleteAutomationRule(id: $id) }",
        json!({ "id": id }),
    )
    .await?;
    Ok(data
        .get("deleteAutomationRule")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}
```

- [ ] **Step 2: 新建 `src/frontend/automation_tab.rs`**

```rust
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{json, Value};

use super::graphql_client::{
    automation_rules, create_automation_rule, delete_automation_rule, parse_rule_trigger,
    update_automation_rule, AutomationRule, LabelSchema,
};

/// 编辑表单里的一行动作。信号放在行内，删除行时随之释放。
#[derive(Clone)]
struct DraftWrite {
    label_name: RwSignal<String>,
    op: RwSignal<String>,
    value_kind: RwSignal<String>,
    value: RwSignal<String>,
}

impl DraftWrite {
    fn new(schema: &LabelSchema) -> Self {
        // 值的输入按标签类型给初始形态：枚举给下拉、布尔给开关在本轮不做，
        // 统一用文本框，值来源为「字面量」时按 JSON 解析（字符串可直接写）。
        Self {
            label_name: RwSignal::new(schema.name.clone()),
            op: RwSignal::new("set".to_string()),
            value_kind: RwSignal::new("literal".to_string()),
            value: RwSignal::new(String::new()),
        }
    }

    fn to_input(&self) -> Value {
        let kind = self.value_kind.get_untracked();
        let raw = self.value.get_untracked();
        let value = if kind == "literal" {
            // 纯数字 / true / false / null 按 JSON 解析，其余当字符串。
            serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw))
        } else {
            Value::Null
        };
        json!({
            "labelName": self.label_name.get_untracked(),
            "op": self.op.get_untracked(),
            "valueKind": kind,
            "value": value,
        })
    }
}

/// 设置页的「自动化」标签页。`can_manage` 为 false 时只读。
pub fn AutomationTab(
    ws_id: Signal<String>,
    schemas: Signal<Vec<LabelSchema>>,
    can_manage: bool,
    on_changed: Callback<()>,
) -> impl IntoView {
    let rules = RwSignal::new(Vec::<AutomationRule>::new());
    let error = RwSignal::new(None::<String>);
    let loaded = RwSignal::new(false);

    // 编辑态：None = 未打开表单。
    let editing_id = RwSignal::new(None::<String>);
    let form_open = RwSignal::new(false);
    let f_name = RwSignal::new(String::new());
    let f_enabled = RwSignal::new(true);
    let f_trigger = RwSignal::new(String::new());
    let f_event_source = RwSignal::new(true);
    let f_target = RwSignal::new(String::new());
    let f_writes = RwSignal::new(Vec::<DraftWrite>::new());
    let busy = RwSignal::new(false);
    let form_error = RwSignal::new(None::<String>);

    let load = move || {
        let Some(id) = ws_id.try_get().map(|s| s.to_string()) else { return };
        if id.is_empty() {
            return;
        }
        spawn_local(async move {
            match automation_rules(&id).await {
                Ok(list) => {
                    rules.set(list);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
            loaded.set(true);
        });
    };
    Effect::new_sync(move |_| {
        let _ = ws_id.get();
        load();
    });

    let open_new = move |_| {
        let list = schemas.get_untracked();
        editing_id.set(None);
        f_name.set(String::new());
        f_enabled.set(true);
        f_trigger.set(format!("$label = \"{}\" AND $new = \"\"", list.first().map(|s| s.name.clone()).unwrap_or_default()));
        f_event_source.set(true);
        f_target.set(String::new());
        f_writes.set(vec![DraftWrite::new(&list[0])]);
        form_error.set(None);
        form_open.set(true);
    };

    let open_edit = Callback::new(move |rule: AutomationRule| {
        let list = schemas.get_untracked();
        editing_id.set(Some(rule.id.clone()));
        f_name.set(rule.name.clone());
        f_enabled.set(rule.enabled);
        f_trigger.set(rule.trigger_expr.clone());
        f_event_source.set(rule.target_event_source);
        f_target.set(rule.target_expr.clone());
        let drafts: Vec<DraftWrite> = rule
            .writes
            .iter()
            .map(|w| {
                let d = DraftWrite {
                    label_name: RwSignal::new(w.label_name.clone()),
                    op: RwSignal::new(w.op.clone()),
                    value_kind: RwSignal::new(if w.value_kind.is_empty() {
                        "literal".to_string()
                    } else {
                        w.value_kind.clone()
                    }),
                    value: RwSignal::new(match &w.value {
                        Value::Null => String::new(),
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }),
                };
                d
            })
            .collect();
        f_writes.set(if drafts.is_empty() {
            vec![DraftWrite::new(&list[0])]
        } else {
            drafts
        });
        form_error.set(None);
        form_open.set(true);
    });

    let add_write = move |_| {
        let list = schemas.get_untracked();
        if let Some(first) = list.first() {
            f_writes.update(|w| w.push(DraftWrite::new(first)));
        }
    };

    let check_trigger = move |_| {
        let Some(id) = ws_id.get_untracked_str() else { return };
        let expr = f_trigger.get_untracked();
        spawn_local(async move {
            match parse_rule_trigger(&id, &expr).await {
                Ok(_) => form_error.set(None),
                Err(e) => form_error.set(Some(e)),
            }
        });
    };

    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let Some(id) = ws_id.get_untracked_str() else { return };
        let name = f_name.get_untracked();
        if name.trim().is_empty() {
            form_error.set(Some("请填写规则名称".to_string()));
            return;
        }
        let writes: Vec<Value> = f_writes.get_untracked().iter().map(|w| w.to_input()).collect();
        let writes = Value::Array(writes);
        let target = f_target.get_untracked();
        let target_arg = (!f_event_source.get_untracked()).then_some(target.as_str());
        let editing = editing_id.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let result = match &editing {
                Some(rid) => {
                    update_automation_rule(
                        rid,
                        &name,
                        f_enabled.get_untracked(),
                        &f_trigger.get_untracked(),
                        f_event_source.get_untracked(),
                        target_arg,
                        &writes,
                    )
                    .await
                }
                None => {
                    create_automation_rule(
                        &id,
                        &name,
                        f_enabled.get_untracked(),
                        &f_trigger.get_untracked(),
                        f_event_source.get_untracked(),
                        target_arg,
                        &writes,
                    )
                    .await
                }
            };
            busy.set(false);
            match result {
                Ok(_) => {
                    form_open.set(false);
                    form_error.set(None);
                    load();
                    on_changed.run(());
                }
                Err(e) => form_error.set(Some(e)),
            }
        });
    };

    let remove = Callback::new(move |rid: String| {
        spawn_local(async move {
            match delete_automation_rule(&rid).await {
                Ok(_) => {
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    view! {
        <h2>"自动化规则"</h2>
        <p class="mut">
            "标签被写入时触发。触发条件用 $label / $old / $new 引用本次变更，"
            "也可以直接用标签名（如 Priority >= 3）判断事件源条目的写入后状态。"
        </p>
        {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
        {move || {
            if !loaded.get() {
                view! { <div class="empty">"加载中…"</div> }.into_any()
            } else if rules.get().is_empty() {
                view! { <div class="empty">"还没有规则"</div> }.into_any()
            } else {
                view! {
                    <table class="tbl">
                        <thead>
                            <tr>
                                <th>"名称"</th>
                                <th>"触发条件"</th>
                                <th>"动作"</th>
                                <th>"启用"</th>
                                <th></th>
                            </tr>
                        </thead>
                        <tbody>
                            {rules
                                .get()
                                .into_iter()
                                .map(|r| {
                                    let id = r.id.clone();
                                    let open = r.clone();
                                    let enabled = r.enabled;
                                    view! {
                                        <tr>
                                            <td>{r.name.clone()}</td>
                                            <td class="mono">{r.trigger_expr.clone()}</td>
                                            <td>
                                                {format!(
                                                    "{} → {}",
                                                    if r.target_event_source { "事件源条目" } else { "表达式圈定" },
                                                    r.writes
                                                        .iter()
                                                        .map(|w| format!("{}({})", w.label_name, w.op))
                                                        .collect::<Vec<_>>()
                                                        .join("、"),
                                                )}
                                            </td>
                                            <td>{if enabled { "是" } else { "否" }}</td>
                                            <td>
                                                {if can_manage {
                                                    view! {
                                                        <button class="btn sm" on:click=move |_| open_edit.run(open.clone())>
                                                            "编辑"
                                                        </button>
                                                        <button class="btn sm dgr" on:click=move |_| remove.run(id.clone())>
                                                            "删除"
                                                        </button>
                                                    }.into_any()
                                                } else {
                                                    view! { <span class="mut">"—"</span> }.into_any()
                                                }}
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect_view()}
                        </tbody>
                    </table>
                }.into_any()
            }
        }}
        {if can_manage {
            view! {
                <button class="btn pri" on:click=open_new style="align-self:flex-start">
                    "新建规则"
                </button>
            }.into_any()
        } else {
            view! { <p class="mut">"仅 Maintainer 及以上可编辑"</p> }.into_any()
        }}

        {move || {
            if !form_open.get() {
                return view! { <div></div> }.into_any();
            }
            let schema_list = schemas.get();
            view! {
                <div class="dmodal on">
                    <form class="dmbox stack" on:submit=submit>
                        <h3>{move || if editing_id.get().is_some() { "编辑规则" } else { "新建规则" }}</h3>
                        {move || form_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <label class="fld">
                            <span>"名称"</span>
                            <input class="inp" prop:value=f_name
                                on:input=move |ev| f_name.set(event_target_value(&ev)) />
                        </label>
                        <label class="fld">
                            <span>"启用"</span>
                            <input type="checkbox" prop:checked=f_enabled
                                on:change=move |ev| f_enabled.set(event_target_checked(&ev)) />
                        </label>
                        <label class="fld">
                            <span>"触发条件"</span>
                            <textarea class="inp mono" rows="3" prop:value=f_trigger
                                on:input=move |ev| f_trigger.set(event_target_value(&ev))></textarea>
                        </label>
                        <div class="mut" style="font-size:12px">
                            "可用关键字：" <code>"$label"</code> "（本次变更的标签名）、"
                            <code>"$old"</code> "（旧值，新增时用 " <code>"!$old"</code> "）、"
                            <code>"$new"</code> "（新值，删除时用 " <code>"!$new"</code> "）。"
                        </div>
                        <div style="display:flex;gap:8px;flex-wrap:wrap">
                            {schema_list
                                .iter()
                                .map(|s| {
                                    let name = s.name.clone();
                                    view! {
                                        <button type="button" class="btn sm"
                                            on:click=move |_| {
                                                let cur = f_trigger.get_untracked();
                                                f_trigger.set(format!("{cur} {name}"));
                                            }>
                                            {s.name.clone()}
                                        </button>
                                    }
                                })
                                .collect_view()}
                            <button type="button" class="btn sm" on:click=check_trigger>"校验"</button>
                        </div>
                        <label class="fld">
                            <span>"动作目标"</span>
                            <select class="inp" prop:value=move || if f_event_source.get() { "event" } else { "query" }
                                on:change=move |ev| f_event_source.set(event_target_value(&ev) == "event")>
                                <option value="event">"事件源条目"</option>
                                <option value="query">"表达式圈定"</option>
                            </select>
                        </label>
                        {move || {
                            if f_event_source.get() {
                                return view! { <div></div> }.into_any();
                            }
                            view! {
                                <label class="fld">
                                    <span>"圈定表达式"</span>
                                    <input class="inp mono" prop:value=f_target
                                        on:input=move |ev| f_target.set(event_target_value(&ev)) />
                                </label>
                            }.into_any()
                        }}
                        <div class="fld">
                            <span>"标签动作"</span>
                            {move || {
                                f_writes
                                    .get()
                                    .into_iter()
                                    .map(|w| {
                                        let opts: Vec<(String, String)> = schemas
                                            .get()
                                            .into_iter()
                                            .map(|s| (s.name.clone(), s.title.clone()))
                                            .collect();
                                        let is_remove = w.op.get() == "remove";
                                        let is_literal = w.value_kind.get() == "literal";
                                        view! {
                                            <div style="display:flex;gap:6px;align-items:center;margin:4px 0">
                                                <select class="inp" style="width:160px"
                                                    prop:value=move || w.label_name.get()
                                                    on:change=move |ev| w.label_name.set(event_target_value(&ev))>
                                                    {opts
                                                        .iter()
                                                        .map(|(n, t)| view! { <option value=n.clone()>{t.clone()}</option> })
                                                        .collect_view()}
                                                </select>
                                                <select class="inp" style="width:110px"
                                                    prop:value=move || w.op.get()
                                                    on:change=move |ev| w.op.set(event_target_value(&ev))>
                                                    <option value="set">"写入"</option>
                                                    <option value="remove">"删除"</option>
                                                </select>
                                                {(!is_remove)
                                                    .then(|| {
                                                        view! {
                                                            <select class="inp" style="width:130px" prop:value=move || w.value_kind.get()
                                                                on:change=move |ev| w.value_kind.set(event_target_value(&ev))>
                                                                <option value="literal">"固定值"</option>
                                                                <option value="now">"当前时间"</option>
                                                                <option value="new">"事件新值"</option>
                                                                <option value="old">"事件旧值"</option>
                                                            </select>
                                                            <input class="inp" style="width:160px"
                                                                prop:value=move || w.value.get()
                                                                disabled=!is_literal
                                                                on:input=move |ev| w.value.set(event_target_value(&ev)) />
                                                        }.into_any()
                                                    })}
                                                <button type="button" class="btn sm dgr"
                                                    on:click=move |_| f_writes.update(|ws| ws.retain(|x| x.label_name != w.label_name))>
                                                    "移除"
                                                </button>
                                            </div>
                                        }
                                    })
                                    .collect_view()
                            }}
                            <button type="button" class="btn sm" on:click=add_write>"＋ 添加动作"</button>
                        </div>
                        <div style="display:flex;gap:8px">
                            <button class="btn pri" type="submit" disabled=move || busy.get()>"保存"</button>
                            <button class="btn" type="button" on:click=move |_| form_open.set(false)>"取消"</button>
                        </div>
                    </form>
                </div>
            }.into_any()
        }}
    }
}
```

`ws_id.get_untracked_str()` 不是标准 API——实现时改成读 `Signal<String>`：

```rust
let id_now = move || ws_id.try_get().map(|s| s.to_string()).unwrap_or_default();
```
用它取当前工作空间 id（`try_get` 在 leptos 0.8 可用；若不可用则把 `ws_id` 改成 `RwSignal<String>` 并用 `get_untracked()`）。

`DraftWrite` 的「移除」按钮用 `label_name` 做判据，同一标签多行时会一次删掉多行——实现时改成按下标删除：`f_writes.update(|ws| { ws.remove(idx); })`。

- [ ] **Step 3: `src/frontend/mod.rs` 注册模块**

```rust
pub mod automation_tab;
```

- [ ] **Step 4: `settings.rs` 挂上标签页**

侧栏「视图共享」之后加一项：

```rust
                    <div class="it" class:on=move || tab.get() == "automation" on:click=move |_| tab.set("automation".into())>
                        {ic_history()}"自动化"
                    </div>
```

数据加载处把规则一并拉回来（在 `Ok::<_, String>((ws, role, schemas, logs, members, invite_list, view_list))` 之外**不要**加进元组——`AutomationTab` 自己加载，避免改动那个 7 元组的所有解构点）。

`tab.get() == "audit"` 分支之后加：

```rust
                            } else if cur_tab == "automation" {
                                let ws_id_signal = Signal::derive(move || {
                                    _ws.id.clone()
                                });
                                let schemas_signal = Signal::derive(move || schemas.clone());
                                let refresh_cb = Callback::new(move |_| refresh.update(|x| *x += 1));
                                view! {
                                    {AutomationTab(ws_id_signal, schemas_signal, can_manage, refresh_cb)}
                                }.into_any()
                            } else if cur_tab == "audit" {
```

注意 `schemas` 在那个 `match` 分支里是按值拿到的（`Some(Ok((_ws, role, schemas, logs, member_list, invite_list, view_list)))`），闭包捕获需要 `move` + 克隆，实现时按编译器提示调整（把 `schemas.clone()` 存进 `Signal::derive`，`_ws.id.clone()` 同理）。

- [ ] **Step 5: 编译**

Run: `make check`
Expected: 两个目标都通过。wasm 目标尤其要过——本任务大部分代码只在 wasm 下编译。

- [ ] **Step 6: 浏览器实测**

Run: `make serve`（或 `make dev`）

验收清单（全部要过）：

1. 设置页出现「自动化」标签页；非 Maintainer 只读。
2. 新建规则「完成时间」：触发条件 `$label = "Status" AND $new = "Finished"`，目标「事件源条目」，动作 `FinishedAt = 当前时间`。保存成功。
3. 在表格里把某条目的 `Status` 设为 `Finished` → 详情里立刻出现 `FinishedAt`，值约等于当前时间。
4. 再把 `Status` 重复设为 `Finished` → `FinishedAt` **不**刷新（值不产生变化即不触发）。
5. 从 `InProgress` 改成 `Finished` → `FinishedAt` 刷新为新的当前时间。
6. 目标改为「表达式圈定」`Status = "Finished"`，动作 `Priority = 1`（字面量）→ 触发后所有满足表达式的条目都拿到 `Priority`。
7. 加一条 `remove` 动作的规则，验证标签被删掉。
8. 触发条件里写非法表达式（如 `$nope = 1`、`text ~ "x"`）→ 保存/校验报可读错误，规则不被保存。
9. 审计页出现「规则触发」记录；被规则写入的标签另有「设置标签」记录。
10. 浏览器控制台无 page error。

- [ ] **Step 7: 提交**

```bash
git add src/frontend/graphql_client.rs src/frontend/automation_tab.rs src/frontend/mod.rs src/frontend/pages/settings.rs
git commit -m "feat(frontend): add automation rule tab to workspace settings"
```

---

## 收尾检查

- [ ] `cargo test` 全绿（既有测试 + `service::rule` 的新测试）
- [ ] `make check` 两个目标通过
- [ ] `cargo fmt` 后无额外改动（仓库既有风格）
- [ ] 设计文档 `docs/superpowers/specs/2026-09-16-event-automation-design.md` 与本计划已对齐：
      §8 补了「`$old` / `$new` 无值时跳过该条写入」，§5 补了「`Query` / `serde_json::Value`
      必须走 JSON 字符串进 bincode」。**这两条已由写计划时回写完毕**，实施过程中若再发现差异，同样回写。
