# 视图筛选与标签查询 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把工作空间的「视图」从占位做成可用闭环——命名视图（个人/共享）+ 标签/时间/全文查询条件 + 可配置表格列 + 排序分页，查询条件支持结构化芯片与表达式文本双模式。

**Architecture:** 查询条件统一为 `Query` AST（结构化即真相），带一套表达式语法可解析/反解析。过滤在服务层对 `entry.list(ws)` 结果做纯函数内存求值；全文走 Tantivy 得到候选 code 集后与结果求交。视图是「AST + 排序 + 列」的持久化实体。

**Tech Stack:** Rust / Leptos 0.8 (SSR + wasm) / async-graphql 7 / RocksDB / Tantivy 0.26 / bincode / chrono / ulid

**Spec:** `docs/superpowers/specs/2026-09-11-view-filter-design.md`

## Global Constraints

- 测试为 `#[cfg(test)] mod tests` 内联单元测试，风格与现有 `src/service/*.rs`、`src/storage/rocksdb.rs` 一致；运行 `cargo test`。
- 新增服务端依赖一律标 `optional = true` 并挂到 `ssr` feature 下（前端 wasm 构建不得引入 Tantivy）。
- `src/domain/*` 仅 `ssr` 可编译；wasm 前端**不得**引用 `domain` 类型。
- 错误消息面向用户、中文；客户端可修复的错误用专用变体（如 `InvalidQuery`），不加「内部错误:」前缀。
- 审计动作沿用已存在的 `AuditAction::ViewCreated / ViewUpdated / ViewDeleted`。
- 角色顺序：`OWNER > MAINTAINER > WORKER > READER`（`WorkspaceRole` 的 `Ord`）。
- 分页 `pageSize` 默认 20、上限 100；全文候选上限常量 `TEXT_CANDIDATE_LIMIT = 1000`。
- 表达式关键字大小写不敏感：`AND OR NOT in not in present absent text updated created`。

---

### Task 1: 领域层 —— 查询 AST、表达式语法与求值

**Files:**
- Create: `src/domain/query.rs`
- Modify: `src/domain/mod.rs`
- Test: `src/domain/query.rs`（内联 `mod tests`）

**Interfaces:**
- Consumes: `crate::domain::{Entry, LabelSchema, LabelValueType, Labeling}`、`crate::error::AppError`
- Produces:
  - `pub enum Query { And(Vec<Query>), Or(Vec<Query>), Not(Box<Query>), Cond(Condition) }`
  - `pub struct Condition { pub field: Field, pub op: Op, pub value: Option<serde_json::Value> }`
  - `pub enum Field { Label(String), UpdatedAt, CreatedAt, Text }`
  - `pub enum Op { Present, Absent, Eq, Ne, In, NotIn, Gt, Ge, Lt, Le, Contains, NotContains }`
  - `Query::all() -> Query`、`Query::parse(&str) -> Result<Query, AppError>`、`Query::to_expr(&self) -> String`
  - `Query::validate(&self, schemas: &[LabelSchema]) -> Result<(), AppError>`
  - `Query::contains_label(&self) -> bool`、`Query::contains_text(&self) -> bool`、`Query::first_text_keyword(&self) -> Option<String>`
  - `Query::evaluate(&self, entry: &Entry, labels: &[Labeling], text_hit: &dyn Fn(&str) -> bool) -> bool`

- [ ] **Step 1: 写失败测试（AST 序列化 + 求值语义 + 解析/反解析）**

在 `src/domain/query.rs` 末尾写：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ulid::Ulid;

    fn entry() -> Entry {
        Entry::new(Ulid::new(), "找回密码失败".to_string(), Ulid::new())
    }

    fn labeling(name: &str, value: serde_json::Value) -> Labeling {
        let lv = match value {
            serde_json::Value::Bool(b) => LabelValue::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() { LabelValue::Int(i) } else { LabelValue::Float(n.as_f64().unwrap()) }
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
            "present(Task)",
            "absent(Priority)",
            "Task in (\"Open\", \"Done\")",
            "updated >= \"2026-09-01\"",
            "text ~ \"检索\"",
            "Task = \"Open\" AND NOT present(Priority)",
            "(Task = \"Open\" OR Bug = \"Fixed\") AND updated >= \"2026-09-01\"",
        ];
        for src in cases {
            let q = Query::parse(src).unwrap_or_else(|e| panic!("parse {src} 失败: {e}"));
            let expr = q.to_expr();
            let again = Query::parse(&expr).unwrap_or_else(|e| panic!("re-parse {expr} 失败: {e}"));
            assert_eq!(q, again, "round-trip 不稳定: {src} -> {expr}");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(Query::parse("Task =").is_err());
        assert!(Query::parse("(Task = \"a\"").is_err());
        assert!(Query::parse("Task >< \"a\"").is_err());
    }

    #[test]
    fn evaluate_present_absent_and_missing() {
        let e = entry();
        let labels = vec![labeling("Task", serde_json::json!("Open"))];
        let never = |_: &str| false;

        let present = Query::Cond(Condition { field: Field::Label("Task".into()), op: Op::Present, value: None });
        assert!(present.evaluate(&e, &labels, &never));
        // 打标缺失时比较一律 false
        let missing_cmp = Query::Cond(Condition {
            field: Field::Label("Priority".into()), op: Op::Eq, value: Some(serde_json::json!("P0")),
        });
        assert!(!missing_cmp.evaluate(&e, &labels, &never));
        let absent = Query::Cond(Condition { field: Field::Label("Priority".into()), op: Op::Absent, value: None });
        assert!(absent.evaluate(&e, &labels, &never));
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
        let cond = |name: &str, op: Op, v: serde_json::Value| Query::Cond(Condition {
            field: Field::Label(name.into()), op, value: Some(v),
        });

        assert!(cond("Score", Op::Gt, serde_json::json!(5)).evaluate(&e, &labels, &never));
        assert!(!cond("Score", Op::Lt, serde_json::json!(5)).evaluate(&e, &labels, &never));
        assert!(cond("Title", Op::Contains, serde_json::json!("联调")).evaluate(&e, &labels, &never));
        assert!(cond("Task", Op::In, serde_json::json!(["Open", "Done"])).evaluate(&e, &labels, &never));
        assert!(!cond("Task", Op::In, serde_json::json!(["Done"])).evaluate(&e, &labels, &never));
    }

    #[test]
    fn evaluate_time_and_boolean_logic() {
        let e = entry();
        let never = |_: &str| false;
        let future = Query::Cond(Condition {
            field: Field::UpdatedAt, op: Op::Gt, value: Some(serde_json::json!("2099-01-01")),
        });
        assert!(!future.evaluate(&e, &[], &never));
        assert!(Query::Not(Box::new(future.clone())).evaluate(&e, &[], &never));
        assert!(Query::And(vec![]).evaluate(&e, &[], &never), "空 AND 恒真");
        assert!(!Query::Or(vec![]).evaluate(&e, &[], &never), "空 OR 恒假");
    }

    #[test]
    fn evaluate_text_uses_closure() {
        let e = entry();
        let q = Query::Cond(Condition {
            field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("检索")),
        });
        assert!(q.evaluate(&e, &[], &|kw| kw == "检索"));
        assert!(!q.evaluate(&e, &[], &|_| false));
    }

    #[test]
    fn validate_rejects_unknown_label_and_bad_op() {
        let schemas = vec![LabelSchema::new(
            Ulid::new(), "Score".into(), "分数".into(), LabelValueType::Integer, vec![],
        )];
        let unknown = Query::Cond(Condition {
            field: Field::Label("Nope".into()), op: Op::Present, value: None,
        });
        assert!(unknown.validate(&schemas).is_err());

        let bad_op = Query::Cond(Condition {
            field: Field::Label("Score".into()), op: Op::Contains, value: Some(serde_json::json!("x")),
        });
        assert!(bad_op.validate(&schemas).is_err(), "Integer 不支持 ~");

        let ok = Query::Cond(Condition {
            field: Field::Label("Score".into()), op: Op::Ge, value: Some(serde_json::json!(5)),
        });
        assert!(ok.validate(&schemas).is_ok());
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib domain::query`
Expected: 编译失败（`Query` 未定义）。

- [ ] **Step 3: 实现 AST + 求值**

在 `src/domain/query.rs` 顶部写：

```rust
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{Entry, LabelSchema, LabelValueType, Labeling};
use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Query {
    And(Vec<Query>),
    Or(Vec<Query>),
    Not(Box<Query>),
    Cond(Condition),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    pub field: Field,
    pub op: Op,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    Label(String),
    UpdatedAt,
    CreatedAt,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Op {
    Present,
    Absent,
    Eq,
    Ne,
    In,
    NotIn,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
    NotContains,
}

impl Query {
    pub fn all() -> Query {
        Query::And(Vec::new())
    }

    pub fn contains_label(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_label),
            Query::Not(q) => q.contains_label(),
            Query::Cond(c) => matches!(c.field, Field::Label(_)),
        }
    }

    pub fn contains_text(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_text),
            Query::Not(q) => q.contains_text(),
            Query::Cond(c) => matches!(c.field, Field::Text),
        }
    }

    /// 取第一个全文关键词（本轮 UI 只产生单个 text 条件）。
    pub fn first_text_keyword(&self) -> Option<String> {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().find_map(Query::first_text_keyword),
            Query::Not(q) => q.first_text_keyword(),
            Query::Cond(c) if matches!(c.field, Field::Text) => {
                c.value.as_ref().and_then(|v| v.as_str()).map(str::to_string)
            }
            Query::Cond(_) => None,
        }
    }

    pub fn validate(&self, schemas: &[LabelSchema]) -> Result<(), AppError> {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().try_for_each(|q| q.validate(schemas)),
            Query::Not(q) => q.validate(schemas),
            Query::Cond(c) => c.validate(schemas),
        }
    }

    pub fn evaluate(
        &self,
        entry: &Entry,
        labels: &[Labeling],
        text_hit: &dyn Fn(&str) -> bool,
    ) -> bool {
        match self {
            Query::And(v) => v.iter().all(|q| q.evaluate(entry, labels, text_hit)),
            Query::Or(v) => v.iter().any(|q| q.evaluate(entry, labels, text_hit)),
            Query::Not(q) => !q.evaluate(entry, labels, text_hit),
            Query::Cond(c) => c.evaluate(entry, labels, text_hit),
        }
    }
}

impl Condition {
    fn validate(&self, schemas: &[LabelSchema]) -> Result<(), AppError> {
        if let Field::Label(name) = &self.field {
            let schema = schemas
                .iter()
                .find(|s| &s.name == name)
                .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {name}")))?;
            if !op_allowed(schema.value_type, self.op) {
                return Err(AppError::InvalidQuery(format!(
                    "运算符 {:?} 不适用于标签 {name}（{:?}）",
                    self.op, schema.value_type
                )));
            }
        }
        Ok(())
    }

    fn evaluate(&self, entry: &Entry, labels: &[Labeling], text_hit: &dyn Fn(&str) -> bool) -> bool {
        match &self.field {
            Field::Text => self
                .value
                .as_ref()
                .and_then(|v| v.as_str())
                .map(text_hit)
                .unwrap_or(false),
            Field::UpdatedAt => cmp_time(&entry.updated_at, self.op, self.value.as_ref()),
            Field::CreatedAt => cmp_time(&entry.created_at, self.op, self.value.as_ref()),
            Field::Label(name) => match self.op {
                Op::Present => labels.iter().any(|l| &l.label_name == name),
                Op::Absent => !labels.iter().any(|l| &l.label_name == name),
                _ => match labels.iter().find(|l| &l.label_name == name) {
                    Some(l) => cmp_value(&l.value.to_json(), self.op, self.value.as_ref()),
                    None => false,
                },
            },
        }
    }
}

fn op_allowed(vt: LabelValueType, op: Op) -> bool {
    use LabelValueType::*;
    let existence = matches!(op, Op::Present | Op::Absent);
    let cmp = match vt {
        Null => false,
        Boolean => matches!(op, Op::Eq | Op::Ne),
        Integer | Float => matches!(op, Op::Eq | Op::Ne | Op::Gt | Op::Ge | Op::Lt | Op::Le),
        String | Enum => matches!(
            op,
            Op::Eq | Op::Ne | Op::Contains | Op::NotContains | Op::In | Op::NotIn
        ),
    };
    existence || cmp
}

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
}

fn cmp_value(got: &serde_json::Value, op: Op, want: Option<&serde_json::Value>) -> bool {
    let Some(want) = want else { return false };
    match op {
        Op::Eq => got == want,
        Op::Ne => got != want,
        Op::Gt | Op::Ge | Op::Lt | Op::Le => match (as_f64(got), as_f64(want)) {
            (Some(a), Some(b)) => match op {
                Op::Gt => a > b,
                Op::Ge => a >= b,
                Op::Lt => a < b,
                _ => a <= b,
            },
            _ => false,
        },
        Op::Contains | Op::NotContains => {
            let (Some(a), Some(b)) = (got.as_str(), want.as_str()) else { return false };
            let hit = a.to_lowercase().contains(&b.to_lowercase());
            if op == Op::Contains { hit } else { !hit }
        }
        Op::In | Op::NotIn => {
            let Some(list) = want.as_array() else { return false };
            let hit = list.iter().any(|x| x == got);
            if op == Op::In { hit } else { !hit }
        }
        Op::Present | Op::Absent => false,
    }
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|dt| dt.and_utc())
}

fn cmp_time(got: &DateTime<Utc>, op: Op, want: Option<&serde_json::Value>) -> bool {
    let Some(s) = want.and_then(|v| v.as_str()) else { return false };
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
```

- [ ] **Step 4: 实现表达式词法/语法分析与反解析**

在 `src/domain/query.rs` 追加：

```rust
#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Comma,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Tilde,
    NotTilde,
    And,
    Or,
    Not,
    In,
    Present,
    Absent,
    Text,
    Updated,
    Created,
    Ident(String),
    Str(String),
    Num(f64),
    Bool(bool),
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
            '(' => { out.push(Tok::LParen); i += 1; }
            ')' => { out.push(Tok::RParen); i += 1; }
            ',' => { out.push(Tok::Comma); i += 1; }
            '=' => { out.push(Tok::Eq); i += 1; }
            '>' => {
                if chars.get(i + 1) == Some(&'=') { out.push(Tok::Ge); i += 2; } else { out.push(Tok::Gt); i += 1; }
            }
            '<' => {
                if chars.get(i + 1) == Some(&'=') { out.push(Tok::Le); i += 2; } else { out.push(Tok::Lt); i += 1; }
            }
            '!' => match chars.get(i + 1) {
                Some('=') => { out.push(Tok::Ne); i += 2; }
                Some('~') => { out.push(Tok::NotTilde); i += 2; }
                _ => return Err(AppError::InvalidQuery("期望 != 或 !~".to_string())),
            },
            '~' => { out.push(Tok::Tilde); i += 1; }
            '"' | '\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                while i < chars.len() && chars[i] != quote {
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
                if c.is_ascii_digit() || (c == '-' && chars.get(i + 1).is_some_and(|n| n.is_ascii_digit())) {
                    let start = i;
                    i += 1;
                    while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '-') {
                        i += 1;
                    }
                    let text: String = chars[start..i].iter().collect();
                    let n = text.parse::<f64>().map_err(|_| AppError::InvalidQuery(format!("非法数字: {text}")))?;
                    out.push(Tok::Num(n));
                } else if c.is_alphanumeric() || c == '_' || c == '-' {
                    let start = i;
                    while i < chars.len()
                        && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '-' || chars[i] == '.')
                    {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    match word.to_ascii_lowercase().as_str() {
                        "and" => out.push(Tok::And),
                        "or" => out.push(Tok::Or),
                        "not" => out.push(Tok::Not),
                        "in" => out.push(Tok::In),
                        "present" => out.push(Tok::Present),
                        "absent" => out.push(Tok::Absent),
                        "text" => out.push(Tok::Text),
                        "updated" => out.push(Tok::Updated),
                        "created" => out.push(Tok::Created),
                        "true" => out.push(Tok::Bool(true)),
                        "false" => out.push(Tok::Bool(false)),
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
            Err(AppError::InvalidQuery(format!("期望 {t:?}，实际 {:?}", self.peek())))
        }
    }

    fn parse_or(&mut self) -> Result<Query, AppError> {
        let mut parts = vec![self.parse_and()?];
        while self.peek() == Some(&Tok::Or) {
            self.next();
            parts.push(self.parse_and()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Query::Or(parts) })
    }

    fn parse_and(&mut self) -> Result<Query, AppError> {
        let mut parts = vec![self.parse_unary()?];
        while self.peek() == Some(&Tok::And) {
            self.next();
            parts.push(self.parse_unary()?);
        }
        Ok(if parts.len() == 1 { parts.pop().unwrap() } else { Query::And(parts) })
    }

    fn parse_unary(&mut self) -> Result<Query, AppError> {
        if self.peek() == Some(&Tok::Not) {
            self.next();
            return Ok(Query::Not(Box::new(self.parse_unary()?)));
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
            Some(Tok::Present) => {
                self.next();
                self.eat(&Tok::LParen)?;
                let name = self.ident()?;
                self.eat(&Tok::RParen)?;
                Ok(Query::Cond(Condition { field: Field::Label(name), op: Op::Present, value: None }))
            }
            Some(Tok::Absent) => {
                self.next();
                self.eat(&Tok::LParen)?;
                let name = self.ident()?;
                self.eat(&Tok::RParen)?;
                Ok(Query::Cond(Condition { field: Field::Label(name), op: Op::Absent, value: None }))
            }
            Some(Tok::Text) => {
                self.next();
                let op = self.comparison_op()?;
                let v = self.scalar()?;
                Ok(Query::Cond(Condition { field: Field::Text, op, value: Some(v) }))
            }
            Some(Tok::Updated) | Some(Tok::Created) | Some(Tok::Ident(_)) => self.parse_condition(),
            other => Err(AppError::InvalidQuery(format!("无法解析: {other:?}"))),
        }
    }

    fn ident(&mut self) -> Result<String, AppError> {
        match self.next() {
            Some(Tok::Ident(s)) => Ok(s),
            other => Err(AppError::InvalidQuery(format!("期望标签名，实际 {other:?}"))),
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
            other => return Err(AppError::InvalidQuery(format!("期望运算符，实际 {other:?}"))),
        };
        Ok(op)
    }

    fn scalar(&mut self) -> Result<serde_json::Value, AppError> {
        match self.next() {
            Some(Tok::Str(s)) => Ok(serde_json::Value::String(s)),
            Some(Tok::Num(n)) => Ok(serde_json::Number::from_f64(n).map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)),
            Some(Tok::Bool(b)) => Ok(serde_json::Value::Bool(b)),
            Some(Tok::Ident(s)) => Ok(serde_json::Value::String(s)),
            other => Err(AppError::InvalidQuery(format!("期望值，实际 {other:?}"))),
        }
    }

    fn parse_condition(&mut self) -> Result<Query, AppError> {
        let field = match self.next() {
            Some(Tok::Updated) => Field::UpdatedAt,
            Some(Tok::Created) => Field::CreatedAt,
            Some(Tok::Ident(s)) => Field::Label(s),
            other => return Err(AppError::InvalidQuery(format!("期望字段，实际 {other:?}"))),
        };
        let op = self.comparison_op()?;
        let value = if self.peek() == Some(&Tok::LParen) {
            self.next();
            let mut items = vec![self.scalar()?];
            while self.peek() == Some(&Tok::Comma) {
                self.next();
                items.push(self.scalar()?);
            }
            self.eat(&Tok::RParen)?;
            serde_json::Value::Array(items)
        } else {
            self.scalar()?
        };
        Ok(Query::Cond(Condition { field, op, value: Some(value) }))
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
            return Err(AppError::InvalidQuery(format!("表达式尾部有多余内容: pos {}", p.pos)));
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
            Field::UpdatedAt => "updated".to_string(),
            Field::CreatedAt => "created".to_string(),
            Field::Text => "text".to_string(),
        };
        match self.op {
            Op::Present => format!("present({field})"),
            Op::Absent => format!("absent({field})"),
            _ => {
                let value = self.value.as_ref().map(expr_scalar).unwrap_or_default();
                format!("{field} {} {value}", expr_op(self.op))
            }
        }
    }
}
```

在 `src/domain/mod.rs` 末尾加：

```rust
pub mod query;
pub mod view;
pub use query::{Condition, Field, Op, Query};
pub use view::{SortField, SortSpec, View};
```

> 注意：`view.rs` 在 Task 2 创建。本步先只加 `pub mod query;` 与 `pub use query::{...};`，Task 2 再补 view 的行。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib domain::query`
Expected: PASS（8 个测试）。

- [ ] **Step 6: 提交**

```bash
git add src/domain/query.rs src/domain/mod.rs
git commit -m "feat(domain): add query AST with expression grammar and evaluator"
```

---

### Task 2: 领域层 —— View 模型与 InvalidQuery 错误

**Files:**
- Create: `src/domain/view.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/error.rs`
- Test: `src/domain/view.rs`、`src/error.rs`（内联 `mod tests`）

**Interfaces:**
- Consumes: `crate::domain::Query`（Task 1）
- Produces:
  - `pub struct View { pub id, pub workspace_id, pub name, pub query: Query, pub sort: SortSpec, pub columns: Vec<String>, pub is_shared, pub owner_id, pub created_at, pub updated_at }`
  - `pub struct SortSpec { pub field: SortField, pub desc: bool }`、`pub enum SortField { UpdatedAt, CreatedAt, Title }`
  - `SortSpec::default()` = `UpdatedAt desc`
  - `SortField::from_str(&str) -> Option<SortField>`、`SortField::as_str(&self) -> &'static str`
  - `AppError::InvalidQuery(String)`，`code() == "INVALID_QUERY"`

- [ ] **Step 1: 写失败测试**

`src/domain/view.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    #[test]
    fn sort_spec_default_is_updated_desc() {
        let s = SortSpec::default();
        assert_eq!(s.field, SortField::UpdatedAt);
        assert!(s.desc);
    }

    #[test]
    fn sort_field_str_roundtrip() {
        for f in [SortField::UpdatedAt, SortField::CreatedAt, SortField::Title] {
            assert_eq!(SortField::from_str(f.as_str()), Some(f));
        }
        assert_eq!(SortField::from_str("nope"), None);
    }

    #[test]
    fn view_roundtrips_through_bincode() {
        let v = View {
            id: Ulid::new(),
            workspace_id: Ulid::new(),
            name: "全部任务".into(),
            query: Query::all(),
            sort: SortSpec::default(),
            columns: vec!["Task".into(), "Priority".into()],
            is_shared: false,
            owner_id: Ulid::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let bytes = bincode::serialize(&v).unwrap();
        let back: View = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, v);
    }
}
```

`src/error.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn invalid_query_keeps_clean_message() {
        let e = AppError::InvalidQuery("标签不存在: Nope".to_string());
        assert_eq!(e.code(), "INVALID_QUERY");
        assert_eq!(e.to_string(), "标签不存在: Nope");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib domain::view error::tests::invalid_query`
Expected: 编译失败（`View` / `InvalidQuery` 未定义）。

- [ ] **Step 3: 实现**

`src/domain/view.rs` 顶部：

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::domain::Query;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub name: String,
    pub query: Query,
    pub sort: SortSpec,
    pub columns: Vec<String>,
    pub is_shared: bool,
    pub owner_id: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortSpec {
    pub field: SortField,
    pub desc: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self { field: SortField::UpdatedAt, desc: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortField {
    UpdatedAt,
    CreatedAt,
    Title,
}

impl SortField {
    pub fn as_str(&self) -> &'static str {
        match self {
            SortField::UpdatedAt => "updatedAt",
            SortField::CreatedAt => "createdAt",
            SortField::Title => "title",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "updatedAt" => Some(SortField::UpdatedAt),
            "createdAt" => Some(SortField::CreatedAt),
            "title" => Some(SortField::Title),
            _ => None,
        }
    }
}
```

`src/error.rs`：在 `enum AppError` 的 `LabelNameExists` 之后加

```rust
    #[error("{0}")]
    InvalidQuery(String),
```

`code()` 中加

```rust
            AppError::InvalidQuery(_) => "INVALID_QUERY",
```

`src/domain/mod.rs`：加 `pub mod view;` 与 `pub use view::{SortField, SortSpec, View};`

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib domain::view error::`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/domain/view.rs src/domain/mod.rs src/error.rs
git commit -m "feat(domain): add View model and InvalidQuery error"
```

---

### Task 3: 存储层 —— 新 CF、键编码与 labelings_by_workspace 维护

**Files:**
- Modify: `src/storage/rocksdb.rs`（`mod cf` + `ALL_CFS`）
- Modify: `src/storage/keys.rs`
- Modify: `src/service/entry.rs`（`set_labeling` / `remove_labeling` 同步索引）
- Test: `src/storage/rocksdb.rs`、`src/storage/keys.rs`、`src/service/entry.rs`（内联）

**Interfaces:**
- Produces:
  - `cf::VIEWS`、`cf::VIEWS_BY_WORKSPACE`、`cf::LABELINGS_BY_WORKSPACE`
  - `keys::view_key(id: Ulid) -> [u8; 16]`
  - `keys::view_by_workspace_key(ws: Ulid, id: Ulid) -> [u8; 32]`
  - `keys::labeling_by_workspace_key(ws: Ulid, code: &str, name: &str) -> Vec<u8>`
  - `EntryService::labelings_by_workspace(&self, ws: Ulid) -> Result<HashMap<String, Vec<Labeling>>, AppError>`

- [ ] **Step 1: 写失败测试**

`src/storage/keys.rs` 的 `mod tests` 内加：

```rust
    #[test]
    fn view_keys_encode_prefixes() {
        let ws = ulid::Ulid::new();
        let id = ulid::Ulid::new();
        assert_eq!(view_key(id).len(), 16);
        let k = view_by_workspace_key(ws, id);
        assert_eq!(k.len(), 32);
        assert!(k.starts_with(&ws.to_bytes()));

        let lk = labeling_by_workspace_key(ws, "CODE0001", "Task");
        assert!(lk.starts_with(&ws.to_bytes()));
        assert_eq!(&lk[16..], b"CODE0001Task");
    }
```

`src/service/entry.rs` 的 `mod tests` 内加：

```rust
    #[test]
    fn labeling_index_tracks_set_and_remove() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();

        let map = svc.labelings_by_workspace(ws_id).unwrap();
        assert_eq!(map.get(&e.code).map(|v| v.len()), Some(1));

        svc.remove_labeling(actor, &e.code, "Task").unwrap();
        let map = svc.labelings_by_workspace(ws_id).unwrap();
        assert!(map.get(&e.code).is_none(), "移除后索引必须清空");
        std::fs::remove_dir_all(&dir).ok();
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib storage::keys::tests::view_keys service::entry::tests::labeling_index`
Expected: 编译失败。

- [ ] **Step 3: 实现**

`src/storage/rocksdb.rs` 的 `mod cf` 内加：

```rust
    pub const VIEWS: &str = "views";
    pub const VIEWS_BY_WORKSPACE: &str = "views_by_workspace";
    pub const LABELINGS_BY_WORKSPACE: &str = "labelings_by_workspace";
```

`ALL_CFS` 加：

```rust
    cf::VIEWS,
    cf::VIEWS_BY_WORKSPACE,
    cf::LABELINGS_BY_WORKSPACE,
```

`src/storage/keys.rs` 末尾加：

```rust
/// 视图主键：16 字节 ulid。
pub fn view_key(id: Ulid) -> [u8; 16] {
    id.to_bytes()
}

/// (workspace_id, view_id) 复合键，32 字节。
pub fn view_by_workspace_key(workspace_id: Ulid, id: Ulid) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&workspace_id.to_bytes());
    key[16..].copy_from_slice(&id.to_bytes());
    key
}

/// (workspace_id, entry_code, label_name) 复合键，前缀扫描取整个 workspace 的打标。
pub fn labeling_by_workspace_key(workspace_id: Ulid, code: &str, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + code.len() + name.len());
    key.extend_from_slice(&workspace_id.to_bytes());
    key.extend_from_slice(code.as_bytes());
    key.extend_from_slice(name.as_bytes());
    key
}
```

`src/service/entry.rs`：`set_labeling` 在 push labeling 的 `BatchOp::put` 之后、`write_batch` 之前加：

```rust
        ops.push(BatchOp::put(
            cf::LABELINGS_BY_WORKSPACE,
            keys::labeling_by_workspace_key(entry.workspace_id, entry_code, label_name),
            &labeling,
        )?);
```

`remove_labeling` 在 `ops.push(BatchOp::delete(cf::LABELINGS, key));` 之后加：

```rust
        ops.push(BatchOp::delete(
            cf::LABELINGS_BY_WORKSPACE,
            keys::labeling_by_workspace_key(entry.workspace_id, entry_code, label_name),
        ));
```

在 `EntryService` impl 内加：

```rust
    /// 一次前缀扫描取回 workspace 内全部打标，按 entry_code 分组。
    pub fn labelings_by_workspace(
        &self,
        workspace_id: Ulid,
    ) -> Result<std::collections::HashMap<String, Vec<Labeling>>, AppError> {
        let rows = self
            .store
            .scan_prefix(cf::LABELINGS_BY_WORKSPACE, &workspace_id.to_bytes())?;
        let mut map: std::collections::HashMap<String, Vec<Labeling>> = std::collections::HashMap::new();
        for (_, v) in rows {
            let l: Labeling = bincode::deserialize(&v)?;
            map.entry(l.entry_code.clone()).or_default().push(l);
        }
        Ok(map)
    }
```

> `remove_labeling` 中 `entry` 变量已存在（函数开头 `let entry = self.get(...)?`）。

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib storage::keys service::entry`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add src/storage/rocksdb.rs src/storage/keys.rs src/service/entry.rs
git commit -m "feat(storage): add view and labeling-by-workspace column families"
```

---

### Task 4: 服务层 —— SearchIndex（Tantivy）

**Files:**
- Create: `src/service/search.rs`
- Modify: `Cargo.toml`（tantivy 依赖，ssr）
- Modify: `src/service/mod.rs`（`pub mod search;`）
- Test: `src/service/search.rs`（内联）

**Interfaces:**
- Produces:
  - `pub fn strip_rich_text(detail: &str) -> String`
  - `pub struct SearchIndex`
  - `SearchIndex::open(dir: &str) -> Result<Self, AppError>`
  - `SearchIndex::index_entry(&self, entry: &Entry, labels: &[Labeling]) -> Result<(), AppError>`
  - `SearchIndex::remove_entry(&self, code: &str) -> Result<(), AppError>`
  - `SearchIndex::search(&self, ws: Ulid, keyword: &str, limit: usize) -> Result<Vec<String>, AppError>`
  - `SearchIndex::num_docs(&self) -> u64`
  - `SearchIndex::backfill(&self, store: &DocStore) -> Result<usize, AppError>`
  - `pub const TEXT_CANDIDATE_LIMIT: usize = 1000;`

- [ ] **Step 1: 加依赖**

`Cargo.toml`：`[dependencies]` 末尾加

```toml
tantivy = { version = "0.26", optional = true, default-features = false, features = ["mmap", "lz4-compression", "stopwords"] }
```

`[features] ssr` 列表末尾加 `"dep:tantivy",`

- [ ] **Step 2: 写失败测试**

`src/service/search.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LabelValue, Labeling};
    use crate::storage::DocStore;
    use ulid::Ulid;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-search-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn strip_rich_text_extracts_delta_ops() {
        let delta = r#"{"ops":[{"insert":"复现步骤：\n"},{"insert":"1. 打标签"}]}"#;
        assert_eq!(strip_rich_text(delta), "复现步骤：\n1. 打标签");
        assert_eq!(strip_rich_text("纯文本"), "纯文本");
        assert_eq!(strip_rich_text(""), "");
    }

    #[test]
    fn indexes_and_searches_chinese_substring() {
        let dir = temp_dir("cjk");
        let idx = SearchIndex::open(&dir).unwrap();
        let ws = Ulid::new();
        let mut e = Entry::new(ws, "找回密码失败".to_string(), Ulid::new());
        e.detail = r#"{"ops":[{"insert":"用户反馈邮箱收不到验证码"}]}"#.to_string();
        idx.index_entry(&e, &[]).unwrap();

        let hits = idx.search(ws, "密码", 10).unwrap();
        assert_eq!(hits, vec![e.code.clone()], "中文子串必须命中");

        let hits = idx.search(ws, "验证码", 10).unwrap();
        assert_eq!(hits, vec![e.code.clone()], "详情正文必须可检索");
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_is_scoped_to_workspace_and_drops_deleted() {
        let dir = temp_dir("scope");
        let idx = SearchIndex::open(&dir).unwrap();
        let ws_a = Ulid::new();
        let ws_b = Ulid::new();
        let ea = Entry::new(ws_a, "共享词".to_string(), Ulid::new());
        let eb = Entry::new(ws_b, "共享词".to_string(), Ulid::new());
        idx.index_entry(&ea, &[]).unwrap();
        idx.index_entry(&eb, &[]).unwrap();

        assert_eq!(idx.search(ws_a, "共享", 10).unwrap(), vec![ea.code.clone()]);

        idx.remove_entry(&ea.code).unwrap();
        assert!(idx.search(ws_a, "共享", 10).unwrap().is_empty());
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn label_values_are_searchable() {
        let dir = temp_dir("labels");
        let idx = SearchIndex::open(&dir).unwrap();
        let ws = Ulid::new();
        let e = Entry::new(ws, "t".to_string(), Ulid::new());
        let l = Labeling::new(e.code.clone(), "Owner".to_string(), LabelValue::Enum("陈晨".to_string()), Ulid::new());
        idx.index_entry(&e, &[l]).unwrap();
        assert_eq!(idx.search(ws, "陈晨", 10).unwrap(), vec![e.code.clone()]);
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backfill_is_idempotent() {
        let dir = temp_dir("backfill");
        let store = DocStore::open(&dir).unwrap();
        let idx_dir = format!("{dir}/search");
        let idx = SearchIndex::open(&idx_dir).unwrap();
        let ws = Ulid::new();
        let mut e = Entry::new(ws, "回填目标".to_string(), Ulid::new());
        e.detail = String::new();
        store.put(cf::ENTRIES, e.code.as_bytes(), &e).unwrap();

        assert_eq!(idx.backfill(&store).unwrap(), 1);
        assert_eq!(idx.search(ws, "回填", 10).unwrap(), vec![e.code.clone()]);
        assert_eq!(idx.backfill(&store).unwrap(), 0, "已有文档时不再回填");
        drop(idx);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib service::search`
Expected: 编译失败（`SearchIndex` 未定义）。先确认 `cargo add` 拉到了 0.26：`cargo tree -p tantivy | head -1`。

- [ ] **Step 4: 实现**

`src/service/search.rs` 顶部：

```rust
use std::path::Path;
use std::sync::Mutex;

use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::{NgramTokenizer, TextAnalyzer};
use tantivy::{doc, Index, IndexReader, IndexWriter, Term};
use ulid::Ulid;

use crate::domain::{Entry, Labeling};
use crate::error::AppError;
use crate::storage::{cf, DocStore};

pub const TEXT_CANDIDATE_LIMIT: usize = 1000;
const TOKENIZER: &str = "cjk";

fn srch_err<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Storage(format!("检索索引错误: {e}"))
}

/// 从 Delta JSON 提取纯文本；非 Delta（历史纯文本）原样返回。
pub fn strip_rich_text(detail: &str) -> String {
    let d = detail.trim();
    if d.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(d) {
        let ops = v
            .get("ops")
            .and_then(|o| o.as_array())
            .cloned()
            .or_else(|| v.as_array().cloned());
        if let Some(ops) = ops {
            let mut out = String::new();
            for op in ops {
                if let Some(s) = op.get("insert").and_then(|i| i.as_str()) {
                    out.push_str(s);
                }
            }
            return out;
        }
    }
    d.to_string()
}

pub struct SearchIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    f_code: Field,
    f_ws: Field,
    f_title: Field,
    f_content: Field,
    f_labels: Field,
}

impl SearchIndex {
    pub fn open(dir: &str) -> Result<Self, AppError> {
        std::fs::create_dir_all(dir).map_err(|e| AppError::Storage(e.to_string()))?;
        let mut b = Schema::builder();
        let text = |b: &mut tantivy::schema::SchemaBuilder, name: &str| {
            let indexing = TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions);
            b.add_text_field(name, TextOptions::default().set_indexing_options(indexing))
        };
        let f_code = b.add_text_field("entry_code", STORED | STRING);
        let f_ws = b.add_text_field("workspace_id", STRING);
        let f_title = text(&mut b, "title");
        let f_content = text(&mut b, "content");
        let f_labels = text(&mut b, "labels");
        let schema = b.build();

        let index = match Index::open_in_dir(dir) {
            Ok(i) => i,
            Err(_) => Index::create_in_dir(Path::new(dir), schema.clone()).map_err(srch_err)?,
        };
        let analyzer = TextAnalyzer::builder(NgramTokenizer::new(1, 2, false).map_err(srch_err)?)
            .build();
        index.tokenizers().register(TOKENIZER, analyzer);

        let writer = index.writer_with_num_threads(1, 50_000_000).map_err(srch_err)?;
        let reader = index.reader().map_err(srch_err)?;

        Ok(Self { index, writer: Mutex::new(writer), reader, f_code, f_ws, f_title, f_content, f_labels })
    }

    pub fn index_entry(&self, entry: &Entry, labels: &[Labeling]) -> Result<(), AppError> {
        let label_text = labels
            .iter()
            .map(|l| {
                let v = match l.value.to_json() {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                format!("{} {}", l.label_name, v)
            })
            .collect::<Vec<_>>()
            .join(" ");

        let mut writer = self.writer.lock().map_err(|_| AppError::Internal("索引写锁中毒".into()))?;
        writer.delete_term(Term::from_field_text(self.f_code, &entry.code));
        writer
            .add_document(doc!(
                self.f_code => entry.code.clone(),
                self.f_ws => entry.workspace_id.to_string(),
                self.f_title => entry.title.clone(),
                self.f_content => strip_rich_text(&entry.detail),
                self.f_labels => label_text,
            ))
            .map_err(srch_err)?;
        writer.commit().map_err(srch_err)?;
        drop(writer);
        self.reader.reload().map_err(srch_err)?;
        Ok(())
    }

    pub fn remove_entry(&self, code: &str) -> Result<(), AppError> {
        let mut writer = self.writer.lock().map_err(|_| AppError::Internal("索引写锁中毒".into()))?;
        writer.delete_term(Term::from_field_text(self.f_code, code));
        writer.commit().map_err(srch_err)?;
        drop(writer);
        self.reader.reload().map_err(srch_err)?;
        Ok(())
    }

    pub fn search(&self, ws: Ulid, keyword: &str, limit: usize) -> Result<Vec<String>, AppError> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Ok(Vec::new());
        }
        let parser = QueryParser::for_index(
            &self.index,
            vec![self.f_title, self.f_content, self.f_labels],
        );
        let parsed = parser.parse_query(keyword).map_err(srch_err)?;
        let ws_query = TermQuery::new(
            Term::from_field_text(self.f_ws, &ws.to_string()),
            IndexRecordOption::Basic,
        );
        let combined = BooleanQuery::new(vec![
            (Occur::Must, parsed.box_clone()),
            (Occur::Must, Box::new(ws_query)),
        ]);

        let searcher = self.reader.searcher();
        let top = searcher
            .search(&combined, &TopDocs::with_limit(limit))
            .map_err(srch_err)?;
        let mut out = Vec::new();
        for (_score, addr) in top {
            let doc = searcher.doc::<tantivy::TantivyDocument>(addr).map_err(srch_err)?;
            if let Some(code) = doc.get_first(self.f_code).and_then(|v| v.as_str()) {
                out.push(code.to_string());
            }
        }
        Ok(out)
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// 索引为空时从 RocksDB 回填；已有文档则跳过（幂等）。
    pub fn backfill(&self, store: &DocStore) -> Result<usize, AppError> {
        if self.num_docs() > 0 {
            return Ok(0);
        }
        let rows = store.scan_prefix(cf::ENTRIES, b"")?;
        let mut count = 0;
        for (_, v) in rows {
            let entry: Entry = bincode::deserialize(&v)?;
            if entry.is_deleted() {
                continue;
            }
            let labels = store
                .scan_prefix(cf::LABELINGS, entry.code.as_bytes())?
                .into_iter()
                .map(|(_, lv)| bincode::deserialize::<Labeling>(&lv))
                .collect::<Result<Vec<_>, _>>()?;
            self.index_entry(&entry, &labels)?;
            count += 1;
        }
        Ok(count)
    }
}
```

`src/service/mod.rs` 加 `pub mod search;` 与 `pub use search::SearchIndex;`。

> 若 `parsed.box_clone()` 在 0.26 不存在，改用 `Box::new(parsed) as Box<dyn tantivy::query::Query>`；编译期若报错按此替换。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib service::search`
Expected: PASS（5 个测试）。若 `searcher.doc` 泛型报错，显式写 `searcher.doc::<tantivy::TantivyDocument>(addr)`。

- [ ] **Step 6: 提交**

```bash
git add Cargo.toml Cargo.lock src/service/search.rs src/service/mod.rs
git commit -m "feat(service): add Tantivy-backed SearchIndex with CJK ngram tokenizer"
```

---

### Task 5: 服务层 —— EntryService 查询编排与索引写入钩子

**Files:**
- Modify: `src/service/entry.rs`
- Test: `src/service/entry.rs`（内联）

**Interfaces:**
- Consumes: `crate::domain::{Query, SortSpec, SortField}`（Task 1/2）、`crate::service::search::{SearchIndex, TEXT_CANDIDATE_LIMIT}`（Task 4）
- Produces:
  - `pub struct PageInput { pub page: usize, pub page_size: usize }`（`Default` = `{1, 20}`，`normalized()` 把 `page≥1`、`page_size` 夹到 `1..=100`）
  - `pub struct QueryResult { pub items: Vec<(Entry, Vec<Labeling>)>, pub total: usize }`
  - `EntryService::with_search(store: Arc<DocStore>, search: Arc<SearchIndex>) -> Self`
  - `EntryService::query(&self, ws: Ulid, query: &Query, sort: &SortSpec, page: PageInput) -> Result<QueryResult, AppError>`

- [ ] **Step 1: 写失败测试**

`src/service/entry.rs` 的 `mod tests` 内加：

```rust
    use crate::domain::view::SortSpec;
    use crate::domain::query::{Condition, Field, Op, Query};

    fn temp_search() -> (String, std::sync::Arc<crate::service::search::SearchIndex>) {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-entry-search-{}", Ulid::new()));
        let dir = p.to_string_lossy().into_owned();
        let idx = std::sync::Arc::new(crate::service::search::SearchIndex::open(&dir).unwrap());
        (dir, idx)
    }

    #[test]
    fn query_filters_by_label_and_pages() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        for i in 0..5 {
            let e = svc.create(actor, ws_id, &format!("条目{i}")).unwrap();
            if i % 2 == 0 {
                svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();
            }
        }
        let q = Query::Cond(Condition {
            field: Field::Label("Task".into()), op: Op::Eq, value: Some(serde_json::json!("Open")),
        });
        let page = PageInput { page: 1, page_size: 2 };
        let r = svc.query(ws_id, &q, &SortSpec::default(), page).unwrap();
        assert_eq!(r.total, 3, "3 条被打了 Task=Open");
        assert_eq!(r.items.len(), 2, "第一页取 2 条");
        assert_eq!(r.items[0].1.len(), 1, "每行带上打标");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_fulltext_intersects_with_label_filter() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        let a = svc.create(actor, ws_id, "找回密码失败").unwrap();
        svc.update(actor, &a.code, &a.updated_at.to_rfc3339(), "找回密码失败", "验证码收不到").unwrap();
        let b = svc.create(actor, ws_id, "找回密码失败").unwrap();
        svc.set_labeling(actor, &a.code, "Task", &serde_json::json!("Open")).unwrap();

        let q = Query::And(vec![
            Query::Cond(Condition { field: Field::Label("Task".into()), op: Op::Eq, value: Some(serde_json::json!("Open")) }),
            Query::Cond(Condition { field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("密码")) }),
        ]);
        let r = svc.query(ws_id, &q, &SortSpec::default(), PageInput::default()).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].0.code, a.code, "b 未打标，应被过滤掉");
        let _ = b;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_sorts_by_title_desc_when_requested() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        for t in ["b", "a", "c"] {
            svc.create(actor, ws_id, t).unwrap();
        }
        let sort = SortSpec { field: SortField::Title, desc: false };
        let r = svc.query(ws_id, &Query::all(), &sort, PageInput::default()).unwrap();
        let titles: Vec<String> = r.items.into_iter().map(|(e, _)| e.title).collect();
        assert_eq!(titles, vec!["a", "b", "c"]);
        std::fs::remove_dir_all(&dir).ok();
    }
```

`setup()` 返回 `(String, Arc<DocStore>, EntryService, Ulid, Ulid)`，测试里用 `_svc` 接收原 svc、另建带 search 的 svc。

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib service::entry::tests::query`
Expected: 编译失败。

- [ ] **Step 3: 实现**

`src/service/entry.rs` 顶部 import 增加：

```rust
use std::sync::Arc;

use crate::domain::view::{SortField, SortSpec};
use crate::domain::Query;
use crate::service::search::{SearchIndex, TEXT_CANDIDATE_LIMIT};
```

`EntryService` 结构体改为：

```rust
pub struct EntryService {
    store: Arc<DocStore>,
    search: Option<Arc<SearchIndex>>,
}

#[derive(Debug, Clone, Copy)]
pub struct PageInput {
    pub page: usize,
    pub page_size: usize,
}

impl Default for PageInput {
    fn default() -> Self {
        Self { page: 1, page_size: 20 }
    }
}

impl PageInput {
    fn normalized(&self) -> (usize, usize) {
        let page = self.page.max(1);
        let page_size = self.page_size.clamp(1, 100);
        (page, page_size)
    }
}

pub struct QueryResult {
    pub items: Vec<(Entry, Vec<Labeling>)>,
    pub total: usize,
}
```

构造器：

```rust
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store, search: None }
    }

    pub fn with_search(store: Arc<DocStore>, search: Arc<SearchIndex>) -> Self {
        Self { store, search: Some(search) }
    }
```

索引钩子（私有）：

```rust
    fn reindex(&self, entry: &Entry) {
        let Some(search) = &self.search else { return };
        let labels = self.labelings(&entry.code).unwrap_or_default();
        if entry.is_deleted() {
            if let Err(e) = search.remove_entry(&entry.code) {
                tracing::warn!("移除检索索引失败 {}: {e}", entry.code);
            }
        } else if let Err(e) = search.index_entry(entry, &labels) {
            tracing::warn!("更新检索索引失败 {}: {e}", entry.code);
        }
    }
```

在 `create` / `update` / `soft_delete` 的 `self.store.write_batch(ops)?;` 之后、`Ok(...)` 之前插入 `self.reindex(&entry);`（`create` 用 `&entry`，`update` 用 `&entry`，`soft_delete` 用 `&entry`）。
在 `set_labeling` / `remove_labeling` 的 `write_batch` 之后插入：

```rust
        if let Ok(Some(e)) = self.get(entry_code) {
            self.reindex(&e);
        }
```

查询编排：

```rust
    pub fn query(
        &self,
        ws: Ulid,
        query: &Query,
        sort: &SortSpec,
        page: PageInput,
    ) -> Result<QueryResult, AppError> {
        let mut rows: Vec<Entry> = self.list(ws)?;

        if query.contains_label() {
            let map = self.labelings_by_workspace(ws)?;
            rows.retain(|e| query.evaluate(e, map.get(&e.code).map(Vec::as_slice).unwrap_or(&[]), &|_| false));
        }

        if let Some(keyword) = query.first_text_keyword() {
            let Some(search) = &self.search else {
                return Err(AppError::Internal("全文检索不可用".to_string()));
            };
            let hits: std::collections::HashSet<String> =
                search.search(ws, &keyword, TEXT_CANDIDATE_LIMIT)?.into_iter().collect();
            rows.retain(|e| {
                let labels = self.labelings(&e.code).unwrap_or_default();
                query.evaluate(e, &labels, &|kw| hits.iter().any(|c| c == &e.code) && kw == keyword)
            });
        } else if !query.contains_label() {
            // 纯时间条件或无条件的求值（labels 为空即可）
            rows.retain(|e| query.evaluate(e, &[], &|_| false));
        }

        sort_rows(&mut rows, sort);

        let total = rows.len();
        let (page, page_size) = page.normalized();
        let start = (page - 1) * page_size;
        let slice: Vec<Entry> = rows.into_iter().skip(start).take(page_size).collect();

        let mut items = Vec::with_capacity(slice.len());
        for e in slice {
            let labels = self.labelings(&e.code)?;
            items.push((e, labels));
        }
        Ok(QueryResult { items, total })
    }
```

> 上面的分支有重复求值；实现时统一为一次求值更清晰——先算出 `labels_map`（需要时）与 `text_hits`，再 `rows.retain(|e| query.evaluate(e, labels_of(e), &text_closure))`。`labels_of` 用 map 或逐条查。**最终实现必须写成这一种单次求值形式**，不得保留两遍 `retain`：

```rust
    pub fn query(&self, ws: Ulid, query: &Query, sort: &SortSpec, page: PageInput) -> Result<QueryResult, AppError> {
        let rows: Vec<Entry> = self.list(ws)?;

        let labels_map = if query.contains_label() {
            Some(self.labelings_by_workspace(ws)?)
        } else {
            None
        };
        let text_hits = match query.first_text_keyword() {
            Some(keyword) => {
                let search = self.search.as_ref().ok_or_else(|| AppError::Internal("全文检索不可用".to_string()))?;
                Some((keyword.clone(), search.search(ws, &keyword, TEXT_CANDIDATE_LIMIT)?.into_iter().collect::<std::collections::HashSet<_>>()))
            }
            None => None,
        };

        let mut matched: Vec<Entry> = rows
            .into_iter()
            .filter(|e| {
                let empty: Vec<Labeling> = Vec::new();
                let labels: &[Labeling] = labels_map.as_ref().and_then(|m| m.get(&e.code)).map(Vec::as_slice).unwrap_or(&empty);
                let text_ok = |kw: &str| match &text_hits {
                    Some((_, set)) => kw == text_hits.as_ref().unwrap().0 && set.contains(&e.code),
                    None => false,
                };
                query.evaluate(e, labels, &text_ok)
            })
            .collect();

        sort_rows(&mut matched, sort);
        let total = matched.len();
        let (page, page_size) = page.normalized();
        let slice: Vec<Entry> = matched.into_iter().skip((page - 1) * page_size).take(page_size).collect();

        let mut items = Vec::with_capacity(slice.len());
        for e in slice {
            let labels = match &labels_map {
                Some(m) => m.get(&e.code).cloned().unwrap_or_default(),
                None => self.labelings(&e.code)?,
            };
            items.push((e, labels));
        }
        Ok(QueryResult { items, total })
    }
```

（`text_ok` 闭包写成直接捕获 `text_hits` 与 `e.code` 的简洁形式；上面 `kw == text_hits.as_ref().unwrap().0` 的写法在实现时应简化为关键字相等判断。）

模块级排序助手：

```rust
fn sort_rows(rows: &mut [Entry], sort: &SortSpec) {
    rows.sort_by(|a, b| {
        let ord = match sort.field {
            SortField::UpdatedAt => a.updated_at.cmp(&b.updated_at),
            SortField::CreatedAt => a.created_at.cmp(&b.created_at),
            SortField::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        };
        if sort.desc { ord.reverse() } else { ord }
    });
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --lib service::entry`
Expected: PASS（含原有测试不回归）。

- [ ] **Step 5: 提交**

```bash
git add src/service/entry.rs
git commit -m "feat(service): add EntryService query orchestration and index hooks"
```

---

### Task 6: 服务层 —— ViewService 与 Services 装配

**Files:**
- Create: `src/service/view.rs`
- Modify: `src/service/mod.rs`
- Modify: `src/main.rs`
- Test: `src/service/view.rs`（内联）

**Interfaces:**
- Consumes: `crate::domain::{View, SortSpec, Query, AuditAction, AuditLog, LabelSchema}`、`crate::service::audit::audit_ops`、`crate::storage::{cf, keys, BatchOp, DocStore}`
- Produces:
  - `pub struct ViewService`
  - `ViewService::new(store: Arc<DocStore>) -> Self`
  - `ViewService::create(&self, actor: Ulid, ws: Ulid, name: &str, query: Query, sort: SortSpec, columns: Vec<String>, is_shared: bool) -> Result<View, AppError>`
  - `ViewService::list(&self, actor: Ulid, ws: Ulid) -> Result<Vec<View>, AppError>`
  - `ViewService::get(&self, id: Ulid) -> Result<Option<View>, AppError>`
  - `ViewService::update(&self, actor: Ulid, id: Ulid, name: &str, query: Query, sort: SortSpec, columns: Vec<String>, is_shared: bool) -> Result<View, AppError>`
  - `ViewService::delete(&self, actor: Ulid, id: Ulid) -> Result<(), AppError>`
  - `Services::new(store, config) -> Result<Services, AppError>`（新增 `search: Arc<SearchIndex>`、`view: ViewService`）

- [ ] **Step 1: 写失败测试**

`src/service/view.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::query::Query;
    use crate::service::WorkspaceService;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-view-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    fn setup() -> (String, Arc<DocStore>, ViewService, Ulid, Ulid) {
        let dir = temp_dir("setup");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let ws_svc = WorkspaceService::new(store.clone());
        let actor = Ulid::new();
        let ws = ws_svc.create(actor, "测试", None, "").unwrap();
        let svc = ViewService::new(store.clone());
        (dir, store, svc, ws.id, actor)
    }

    #[test]
    fn create_list_and_delete_with_audit() {
        let (dir, store, svc, ws, actor) = setup();
        let v = svc
            .create(actor, ws, "全部任务", Query::all(), SortSpec::default(), vec!["Task".into()], false)
            .unwrap();
        assert_eq!(v.name, "全部任务");
        assert!(!v.is_shared);

        let mine = svc.list(actor, ws).unwrap();
        assert_eq!(mine.len(), 1);

        // 他人看不到非共享视图
        assert!(svc.list(Ulid::new(), ws).unwrap().is_empty());

        // 共享视图对所有人可见
        let shared = svc
            .create(actor, ws, "看板", Query::all(), SortSpec::default(), vec![], true)
            .unwrap();
        assert_eq!(svc.list(Ulid::new(), ws).unwrap().len(), 1);
        assert_eq!(svc.list(Ulid::new(), ws).unwrap()[0].id, shared.id);

        svc.delete(actor, v.id).unwrap();
        assert!(svc.get(v.id).unwrap().is_none());

        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&crate::domain::AuditAction::ViewCreated));
        assert!(actions.contains(&crate::domain::AuditAction::ViewDeleted));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_rejects_unknown_column_and_bad_query() {
        let (dir, _store, svc, ws, actor) = setup();
        let bad_col = svc.create(actor, ws, "x", Query::all(), SortSpec::default(), vec!["Nope".into()], false);
        assert!(matches!(bad_col.unwrap_err(), AppError::InvalidQuery(_)));

        let bad_q = Query::Cond(crate::domain::Condition {
            field: crate::domain::Field::Label("Nope".into()),
            op: crate::domain::Op::Present,
            value: None,
        });
        assert!(matches!(
            svc.create(actor, ws, "x", bad_q, SortSpec::default(), vec![], false).unwrap_err(),
            AppError::InvalidQuery(_)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_changes_fields_and_audits() {
        let (dir, store, svc, ws, actor) = setup();
        let v = svc.create(actor, ws, "a", Query::all(), SortSpec::default(), vec![], false).unwrap();
        let u = svc
            .update(actor, v.id, "b", Query::all(), SortSpec::default(), vec!["Task".into()], true)
            .unwrap();
        assert_eq!(u.name, "b");
        assert!(u.is_shared);
        assert_eq!(u.columns, vec!["Task"]);

        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&crate::domain::AuditAction::ViewUpdated));
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --lib service::view`
Expected: 编译失败。

- [ ] **Step 3: 实现 ViewService**

`src/service/view.rs`：

```rust
use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, LabelSchema, Query, SortSpec, View};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct ViewService {
    store: Arc<DocStore>,
}

impl ViewService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    fn schemas(&self, ws: Ulid) -> Result<Vec<LabelSchema>, AppError> {
        let rows = self.store.scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())?;
        rows.into_iter()
            .map(|(_, v)| bincode::deserialize::<LabelSchema>(&v).map_err(Into::into))
            .collect()
    }

    fn validate(&self, ws: Ulid, name: &str, query: &Query, columns: &[String]) -> Result<(), AppError> {
        if name.trim().is_empty() {
            return Err(AppError::InvalidQuery("视图名称不能为空".to_string()));
        }
        let schemas = self.schemas(ws)?;
        for c in columns {
            if !schemas.iter().any(|s| &s.name == c) {
                return Err(AppError::InvalidQuery(format!("列引用了不存在的标签: {c}")));
            }
        }
        query.validate(&schemas)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        actor: Ulid,
        ws: Ulid,
        name: &str,
        query: Query,
        sort: SortSpec,
        columns: Vec<String>,
        is_shared: bool,
    ) -> Result<View, AppError> {
        self.validate(ws, name, &query, &columns)?;
        let now = Utc::now();
        let view = View {
            id: Ulid::new(),
            workspace_id: ws,
            name: name.trim().to_string(),
            query,
            sort,
            columns,
            is_shared,
            owner_id: actor,
            created_at: now,
            updated_at: now,
        };
        let audit = AuditLog::new(
            AuditAction::ViewCreated,
            actor,
            "view",
            &view.id.to_string(),
            Some(ws),
            None,
            Some(serde_json::to_string(&view).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::VIEWS, keys::view_key(view.id).to_vec(), &view)?);
        ops.push(BatchOp::put_raw(
            cf::VIEWS_BY_WORKSPACE,
            keys::view_by_workspace_key(ws, view.id).to_vec(),
            Vec::new(),
        ));
        self.store.write_batch(ops)?;
        Ok(view)
    }

    pub fn list(&self, actor: Ulid, ws: Ulid) -> Result<Vec<View>, AppError> {
        let rows = self.store.scan_prefix(cf::VIEWS_BY_WORKSPACE, &ws.to_bytes())?;
        let mut out = Vec::new();
        for (key, _) in rows {
            if key.len() < 32 {
                continue;
            }
            let id = Ulid::from_bytes(key[key.len() - 16..].try_into().unwrap());
            if let Some(v) = self.store.get::<View>(cf::VIEWS, &keys::view_key(id))? {
                if v.is_shared || v.owner_id == actor {
                    out.push(v);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get(&self, id: Ulid) -> Result<Option<View>, AppError> {
        self.store.get(cf::VIEWS, &keys::view_key(id))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        actor: Ulid,
        id: Ulid,
        name: &str,
        query: Query,
        sort: SortSpec,
        columns: Vec<String>,
        is_shared: bool,
    ) -> Result<View, AppError> {
        let mut view = self.get(id)?.ok_or(AppError::NotFound)?;
        self.validate(view.workspace_id, name, &query, &columns)?;
        let before = serde_json::to_string(&view).unwrap_or_default();
        view.name = name.trim().to_string();
        view.query = query;
        view.sort = sort;
        view.columns = columns;
        view.is_shared = is_shared;
        view.updated_at = Utc::now();
        let after = serde_json::to_string(&view).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::ViewUpdated,
            actor,
            "view",
            &id.to_string(),
            Some(view.workspace_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::VIEWS, keys::view_key(id).to_vec(), &view)?);
        self.store.write_batch(ops)?;
        Ok(view)
    }

    pub fn delete(&self, actor: Ulid, id: Ulid) -> Result<(), AppError> {
        let view = self.get(id)?.ok_or(AppError::NotFound)?;
        let audit = AuditLog::new(
            AuditAction::ViewDeleted,
            actor,
            "view",
            &id.to_string(),
            Some(view.workspace_id),
            Some(serde_json::to_string(&view).unwrap_or_default()),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::VIEWS, keys::view_key(id).to_vec()));
        ops.push(BatchOp::delete(
            cf::VIEWS_BY_WORKSPACE,
            keys::view_by_workspace_key(view.workspace_id, id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }
}
```

> `Ulid::from_bytes([u8;16])` 取 `key[16..]`（前 16 字节是 workspace）。上面 `key[key.len()-16..]` 等价；实现时用 `key[16..32]` 更直白。

- [ ] **Step 4: 装配 Services 与 main**

`src/service/mod.rs`：加 `pub mod view;`、`pub use view::ViewService;`，并把 `Services` 改为：

```rust
pub struct Services {
    pub store: Arc<DocStore>,
    pub config: Arc<Config>,
    pub auth: AuthService,
    pub workspace: WorkspaceService,
    pub entry: EntryService,
    pub label: LabelService,
    pub audit: AuditService,
    pub search: Arc<SearchIndex>,
    pub view: ViewService,
}

impl Services {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Result<Self, AppError> {
        let search = Arc::new(SearchIndex::open(&format!("{}/search", config.data_dir()))?);
        let services = Self {
            auth: AuthService::new(store.clone(), config.clone()),
            workspace: WorkspaceService::new(store.clone()),
            entry: EntryService::with_search(store.clone(), search.clone()),
            label: LabelService::new(store.clone()),
            audit: AuditService::new(store.clone()),
            view: ViewService::new(store.clone()),
            search,
            store,
            config,
        };
        services.search.backfill(&services.store)?;
        Ok(services)
    }
}
```

`src/service/mod.rs` 加 `use crate::error::AppError;` 与 `pub use search::SearchIndex;`。

`src/main.rs`：

```rust
    let services = Arc::new(Services::new(store.clone(), config.clone()).expect("初始化服务失败"));
```

- [ ] **Step 5: 运行测试 + 全量构建**

Run: `cargo test --lib service::view && cargo build`
Expected: PASS 且构建通过。

- [ ] **Step 6: 提交**

```bash
git add src/service/view.rs src/service/mod.rs src/main.rs
git commit -m "feat(service): add ViewService and wire search into Services"
```

---

### Task 7: API 层 —— GraphQL 视图与查询接口

**Files:**
- Modify: `src/api/graphql.rs`
- Test: 手工 GraphQL 调用（见 Task 11 端到端）；本任务以 `cargo build` 与既有测试为准

**Interfaces:**
- Consumes: `Services::{view, entry, label}`、`domain::{View, SortSpec, SortField, Query, Condition, Field, Op}`
- Produces（GraphQL 字段名）：
  - Query：`views(workspaceId)`, `view(id)`, `parseViewQuery(workspaceId, expr)`, `formatViewQuery(workspaceId, query)`, `queryEntries(workspaceId, query, sort, page)`
  - Mutation：`createView`, `updateView`, `deleteView`
  - 类型：`GqlView`, `GqlSortSpec`, `GqlEntryConnection`, input `SortInput`, `PageInput`

- [ ] **Step 1: 加类型映射**

`src/api/graphql.rs`，在 `GqlAuditLog` 之后加：

```rust
#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlSortSpec {
    field: String,
    desc: bool,
}

impl From<SortSpec> for GqlSortSpec {
    fn from(s: SortSpec) -> Self {
        Self { field: s.field.as_str().to_string(), desc: s.desc }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlView {
    id: ID,
    name: String,
    query: Json<serde_json::Value>,
    query_expr: String,
    sort: GqlSortSpec,
    columns: Vec<String>,
    is_shared: bool,
    owner_id: ID,
    created_at: String,
    updated_at: String,
}

impl GqlView {
    fn new(v: View) -> Self {
        let query = serde_json::to_value(&v.query).unwrap_or(serde_json::Value::Null);
        let query_expr = v.query.to_expr();
        Self {
            id: v.id.to_string().into(),
            name: v.name,
            query: Json(query),
            query_expr,
            sort: v.sort.into(),
            columns: v.columns,
            is_shared: v.is_shared,
            owner_id: v.owner_id.to_string().into(),
            created_at: v.created_at.to_rfc3339(),
            updated_at: v.updated_at.to_rfc3339(),
        }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlEntryConnection {
    items: Vec<GqlEntry>,
    total: i32,
    page: i32,
    page_size: i32,
}

#[derive(async_graphql::InputObject)]
pub struct SortInput {
    field: Option<String>,
    desc: Option<bool>,
}

impl SortInput {
    fn to_sort(&self) -> GqlResult<SortSpec> {
        let field = match &self.field {
            Some(f) => SortField::from_str(f)
                .ok_or_else(|| AppError::InvalidQuery(format!("未知排序字段: {f}")))?,
            None => SortField::UpdatedAt,
        };
        Ok(SortSpec { field, desc: self.desc.unwrap_or(true) })
    }
}

#[derive(async_graphql::InputObject)]
pub struct PageInput {
    page: Option<i32>,
    page_size: Option<i32>,
}

impl PageInput {
    fn to_page(&self) -> crate::service::entry::PageInput {
        crate::service::entry::PageInput {
            page: self.page.unwrap_or(1).max(1) as usize,
            page_size: self.page_size.unwrap_or(20).clamp(1, 100) as usize,
        }
    }
}

fn parse_query_json(value: Option<Json<serde_json::Value>>) -> GqlResult<Query> {
    match value {
        None => Ok(Query::all()),
        Some(Json(v)) if v.is_null() => Ok(Query::all()),
        Some(Json(v)) => serde_json::from_value(v)
            .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")).into()),
    }
}
```

`src/api/graphql.rs` 顶部 import 增加：

```rust
use crate::domain::{Query, SortField, SortSpec, View};
use crate::service::entry::PageInput as EntryPageInput;
```

> `SortSpec`/`GqlSortSpec` 结构体字段命名靠 `#[graphql(rename_fields = "camelCase")]` 暴露为 `pageSize` 等；`GqlEntry` 已用 `SimpleObject` 的默认 snake→camel，保持风格一致即可。若该属性在 async-graphql 7 不生效，改为字段逐个 `#[graphql(name = "pageSize")]`。

- [ ] **Step 2: 加 Query 解析器**

在 `impl Query` 内（`my_role` 之后）加：

```rust
    async fn views(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlView>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        Ok(gql.services.view.list(auth.account_id, ws)?.into_iter().map(GqlView::new).collect())
    }

    async fn view(&self, ctx: &Context<'_>, id: ID) -> GqlResult<Option<GqlView>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let view_id = parse_ulid(id.as_str())?;
        let Some(v) = gql.services.view.get(view_id)? else {
            return Ok(None);
        };
        gql.require_member(v.workspace_id)?;
        if !v.is_shared && v.owner_id != auth.account_id {
            return Err(AppError::Forbidden.into());
        }
        Ok(Some(GqlView::new(v)))
    }

    async fn parse_view_query(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        expr: String,
    ) -> GqlResult<Json<serde_json::Value>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let query = Query::parse(&expr)?;
        query.validate(&gql.services.label.list_schemas(ws)?)?;
        Ok(Json(serde_json::to_value(&query).unwrap_or(serde_json::Value::Null)))
    }

    async fn format_view_query(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        query: Json<serde_json::Value>,
    ) -> GqlResult<String> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let q: Query = serde_json::from_value(query.0)
            .map_err(|e| GqlResult::<()>::Err(AppError::InvalidQuery(format!("查询条件格式错误: {e}")).into()).unwrap_err())?;
        Ok(q.to_expr())
    }

    async fn query_entries(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        query: Option<Json<serde_json::Value>>,
        sort: Option<SortInput>,
        page: Option<PageInput>,
    ) -> GqlResult<GqlEntryConnection> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let q = parse_query_json(query)?;
        q.validate(&gql.services.label.list_schemas(ws)?)?;
        let sort = sort.map(|s| s.to_sort()).transpose()?.unwrap_or_default();
        let page = page.map(|p| p.to_page()).unwrap_or_default();
        let result = gql.services.entry.query(ws, &q, &sort, page)?;
        let items = result
            .items
            .into_iter()
            .map(|(e, labels)| GqlEntry::new(e, labels))
            .collect();
        Ok(GqlEntryConnection {
            items,
            total: result.total as i32,
            page: page.page as i32,
            page_size: page.page_size as i32,
        })
    }
```

> `format_view_query` 的 `from_value` 错误映射写成两行更清晰：
> ```rust
> let q: Query = serde_json::from_value(query.0)
>     .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")))?;
> ```
> 请用这一版，避免上面那行强行 unwrap 的写法。

把原 `entries` 解析器删除（前端改用 `queryEntries`）。

- [ ] **Step 3: 加 Mutation 解析器**

在 `impl Mutation` 内加：

```rust
    #[allow(clippy::too_many_arguments)]
    async fn create_view(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        query: Json<serde_json::Value>,
        sort: Option<SortInput>,
        columns: Vec<String>,
        is_shared: bool,
    ) -> GqlResult<GqlView> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws, if is_shared { WorkspaceRole::Maintainer } else { WorkspaceRole::Worker })?;
        let q: Query = serde_json::from_value(query.0)
            .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")))?;
        let sort = sort.map(|s| s.to_sort()).transpose()?.unwrap_or_default();
        let v = gql.services.view.create(auth.account_id, ws, &name, q, sort, columns, is_shared)?;
        Ok(GqlView::new(v))
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_view(
        &self,
        ctx: &Context<'_>,
        id: ID,
        name: String,
        query: Json<serde_json::Value>,
        sort: Option<SortInput>,
        columns: Vec<String>,
        is_shared: bool,
    ) -> GqlResult<GqlView> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let view_id = parse_ulid(id.as_str())?;
        let existing = gql.services.view.get(view_id)?.ok_or(AppError::NotFound)?;
        gql.require_member(existing.workspace_id)?;
        // 共享视图需 Maintainer；个人视图本人可改，他人需 Maintainer。
        let need = if is_shared || existing.is_shared || existing.owner_id != auth.account_id {
            WorkspaceRole::Maintainer
        } else {
            WorkspaceRole::Worker
        };
        gql.require_role(existing.workspace_id, need)?;
        let q: Query = serde_json::from_value(query.0)
            .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")))?;
        let sort = sort.map(|s| s.to_sort()).transpose()?.unwrap_or_default();
        let v = gql.services.view.update(auth.account_id, view_id, &name, q, sort, columns, is_shared)?;
        Ok(GqlView::new(v))
    }

    async fn delete_view(&self, ctx: &Context<'_>, id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let view_id = parse_ulid(id.as_str())?;
        let existing = gql.services.view.get(view_id)?.ok_or(AppError::NotFound)?;
        gql.require_member(existing.workspace_id)?;
        let need = if existing.is_shared || existing.owner_id != auth.account_id {
            WorkspaceRole::Maintainer
        } else {
            WorkspaceRole::Worker
        };
        gql.require_role(existing.workspace_id, need)?;
        gql.services.view.delete(auth.account_id, view_id)?;
        Ok(true)
    }
```

`use crate::service::entry::PageInput as EntryPageInput;` 若未用则删除；`PageInput` 保持为本文件内的 GraphQL input 结构体。

- [ ] **Step 4: 构建验证**

Run: `cargo build`
Expected: 通过。若 `Json` 的 InputObject 用法有出入，把 `query: Json<serde_json::Value>` 换成 `query: serde_json::Value`（async-graphql 的 `Json` 标量在参数位置可用）。

- [ ] **Step 5: 提交**

```bash
git add src/api/graphql.rs
git commit -m "feat(api): add view and entry query GraphQL resolvers"
```

---

### Task 8: 前端 —— graphql_client 扩展与 view_filter 助手

**Files:**
- Modify: `src/frontend/graphql_client.rs`
- Create: `src/frontend/view_filter.rs`
- Modify: `src/frontend/mod.rs`
- Test: `src/frontend/view_filter.rs`（内联；native 下运行）

**Interfaces:**
- Produces（`graphql_client`）：
  - `pub struct View { id, name, query: Value, query_expr, sort: ViewSort, columns, is_shared, owner_id }`
  - `pub struct ViewSort { field: String, desc: bool }`
  - `pub struct EntryPage { items: Vec<Entry>, total: i64, page: i64, page_size: i64 }`
  - `views(ws_id) -> Result<Vec<View>, String>`
  - `query_entries(ws_id, query: &Value, sort_field: &str, desc: bool, page: i64, page_size: i64) -> Result<EntryPage, String>`
  - `create_view(...) -> Result<View, String>`、`update_view(...) -> Result<View, String>`、`delete_view(id) -> Result<bool, String>`
  - `parse_view_query(ws_id, expr) -> Result<Value, String>`
- Produces（`view_filter`）：
  - `pub enum CondChip { Label { name, op, value }, Time { field, op, value }, Text { keyword } }`
  - `pub fn chips(query: &Value) -> Vec<CondChip>`
  - `pub fn build_query(chips: &[CondChip]) -> Value`
  - `pub fn with_text(query: &Value, keyword: &str) -> Value`

- [ ] **Step 1: 写 graphql_client 类型与函数**

在 `src/frontend/graphql_client.rs` 末尾加：

```rust
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSort {
    pub field: String,
    pub desc: bool,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: String,
    pub name: String,
    pub query: Value,
    pub query_expr: String,
    pub sort: ViewSort,
    pub columns: Vec<String>,
    pub is_shared: bool,
    pub owner_id: String,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPage {
    pub items: Vec<Entry>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
}
```

同文件末尾加请求函数：

```rust
const VIEW_FIELDS: &str = "id name query queryExpr sort { field desc } columns isShared ownerId";

pub async fn views(workspace_id: &str) -> Result<Vec<View>, String> {
    let q = format!("query($id: ID!) {{ views(workspaceId: $id) {{ {VIEW_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("views").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn query_entries(
    workspace_id: &str,
    query: &Value,
    sort_field: &str,
    desc: bool,
    page: i64,
    page_size: i64,
) -> Result<EntryPage, String> {
    let q = "query($id: ID!, $q: JSON, $s: SortInput, $p: PageInput) { \
        queryEntries(workspaceId: $id, query: $q, sort: $s, page: $p) { \
        items { code title detail updatedAt labels { labelName value } } total page pageSize } }";
    let data = graphql(
        q,
        json!({
            "id": workspace_id,
            "q": query,
            "s": { "field": sort_field, "desc": desc },
            "p": { "page": page, "pageSize": page_size },
        }),
    )
    .await?;
    serde_json::from_value(data.get("queryEntries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn create_view(
    workspace_id: &str,
    name: &str,
    query: &Value,
    sort_field: &str,
    desc: bool,
    columns: &[String],
    is_shared: bool,
) -> Result<View, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $q: JSON!, $s: SortInput, $c: [String!]!, $sh: Boolean!) {{ \
         createView(workspaceId: $id, name: $n, query: $q, sort: $s, columns: $c, isShared: $sh) {{ {VIEW_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id, "n": name, "q": query,
            "s": { "field": sort_field, "desc": desc }, "c": columns, "sh": is_shared,
        }),
    )
    .await?;
    serde_json::from_value(data.get("createView").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn update_view(
    id: &str,
    name: &str,
    query: &Value,
    sort_field: &str,
    desc: bool,
    columns: &[String],
    is_shared: bool,
) -> Result<View, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $q: JSON!, $s: SortInput, $c: [String!]!, $sh: Boolean!) {{ \
         updateView(id: $id, name: $n, query: $q, sort: $s, columns: $c, isShared: $sh) {{ {VIEW_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": id, "n": name, "q": query,
            "s": { "field": sort_field, "desc": desc }, "c": columns, "sh": is_shared,
        }),
    )
    .await?;
    serde_json::from_value(data.get("updateView").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn delete_view(id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { deleteView(id: $id) }",
        json!({ "id": id }),
    )
    .await?;
    Ok(data.get("deleteView").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn parse_view_query(workspace_id: &str, expr: &str) -> Result<Value, String> {
    let data = graphql(
        "query($id: ID!, $e: String!) { parseViewQuery(workspaceId: $id, expr: $e) }",
        json!({ "id": workspace_id, "e": expr }),
    )
    .await?;
    Ok(data.get("parseViewQuery").cloned().unwrap_or(Value::Null))
}
```

- [ ] **Step 2: 写失败测试（AST JSON 助手）**

`src/frontend/view_filter.rs`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chips_read_label_condition() {
        let q = json!({"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Open"}});
        let chips = chips(&q);
        assert_eq!(chips.len(), 1);
        assert!(matches!(&chips[0], CondChip::Label { name, op, .. } if name == "Task" && op == "eq"));
    }

    #[test]
    fn build_query_emits_and_tree() {
        let chips = vec![
            CondChip::Label { name: "Task".into(), op: "eq".into(), value: json!("Open") },
            CondChip::Text { keyword: "检索".into() },
        ];
        let q = build_query(&chips);
        assert_eq!(q["and"].as_array().unwrap().len(), 2);
        assert_eq!(q["and"][0]["cond"]["field"]["label"], "Task");
        assert_eq!(q["and"][1]["cond"]["field"], "text");
    }

    #[test]
    fn with_text_replaces_previous_text_condition() {
        let base = json!({"and": [
            {"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Open"}},
            {"cond": {"field": "text", "op": "contains", "value": "旧词"}}
        ]});
        let q = with_text(&base, "新词");
        let arr = q["and"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[1]["cond"]["value"], "新词");
    }

    #[test]
    fn with_text_on_empty_query_yields_single_text_cond() {
        let q = with_text(&json!({"and": []}), "词");
        assert_eq!(q["and"].as_array().unwrap().len(), 1);
    }
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --lib frontend::view_filter`
Expected: 编译失败。

- [ ] **Step 4: 实现 view_filter**

`src/frontend/view_filter.rs` 顶部：

```rust
use serde_json::{json, Value};

/// 筛选条上的一枚条件芯片。value 保持为 JSON，原样回写 AST。
#[derive(Debug, Clone, PartialEq)]
pub enum CondChip {
    Label { name: String, op: String, value: Value },
    Time { field: String, op: String, value: Value },
    Text { keyword: String },
}

fn conditions(query: &Value) -> Vec<Value> {
    if let Some(arr) = query.get("and").and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if query.get("cond").is_some() {
        return vec![query.clone()];
    }
    Vec::new()
}

fn chip_of(cond: &Value) -> Option<CondChip> {
    let c = cond.get("cond")?;
    let field = c.get("field")?;
    let op = c.get("op").and_then(|v| v.as_str()).unwrap_or("eq").to_string();
    let value = c.get("value").cloned().unwrap_or(Value::Null);
    if let Some(name) = field.get("label").and_then(|v| v.as_str()) {
        return Some(CondChip::Label { name: name.to_string(), op, value });
    }
    match field.as_str() {
        Some("text") => Some(CondChip::Text {
            keyword: value.as_str().unwrap_or("").to_string(),
        }),
        Some("updatedAt") | Some("createdAt") => Some(CondChip::Time {
            field: field.as_str().unwrap().to_string(),
            op,
            value,
        }),
        _ => None,
    }
}

pub fn chips(query: &Value) -> Vec<CondChip> {
    conditions(query).iter().filter_map(chip_of).collect()
}

fn chip_to_cond(chip: &CondChip) -> Value {
    match chip {
        CondChip::Label { name, op, value } => json!({
            "cond": { "field": { "label": name }, "op": op, "value": value }
        }),
        CondChip::Time { field, op, value } => json!({
            "cond": { "field": field, "op": op, "value": value }
        }),
        CondChip::Text { keyword } => json!({
            "cond": { "field": "text", "op": "contains", "value": keyword }
        }),
    }
}

pub fn build_query(chips: &[CondChip]) -> Value {
    let arr: Vec<Value> = chips.iter().map(chip_to_cond).collect();
    json!({ "and": arr })
}

/// 把 ad-hoc 全文关键词并入查询：替换已有的 text 条件，没有则追加。
pub fn with_text(query: &Value, keyword: &str) -> Value {
    let keyword = keyword.trim();
    let mut arr: Vec<Value> = conditions(query)
        .into_iter()
        .filter(|c| chip_of(c).map(|ch| !matches!(ch, CondChip::Text { .. })).unwrap_or(true))
        .collect();
    if !keyword.is_empty() {
        arr.push(chip_to_cond(&CondChip::Text { keyword: keyword.to_string() }));
    }
    json!({ "and": arr })
}
```

`src/frontend/mod.rs` 加 `pub mod view_filter;`。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --lib frontend::view_filter`
Expected: PASS。

- [ ] **Step 6: 提交**

```bash
git add src/frontend/graphql_client.rs src/frontend/view_filter.rs src/frontend/mod.rs
git commit -m "feat(frontend): add view graphql client and filter AST helpers"
```

---

### Task 9: 前端 —— 视图侧栏与视图 CRUD

**Files:**
- Modify: `src/frontend/pages/workspace_main.rs`
- Modify: `style/main.css`（新增 `.vchip-toggle` 等少量样式，可选）

**Interfaces:**
- Consumes: `graphql_client::{views, create_view, update_view, delete_view, View}`、`components::logged_out`
- Produces: `WorkspaceMain` 内新增信号 `view_list: RwSignal<Vec<View>>`、`active_view: RwSignal<Option<View>>`，以及 `load_views`/`select_view` 闭包；`WorkspaceSidebar` 签名改为接收上述信号与回调

- [ ] **Step 1: 侧栏渲染真实视图**

`workspace_main.rs` 顶部 import 增加：

```rust
use crate::frontend::graphql_client::{
    create_view, delete_view, update_view, views, View,
};
```

把 `WorkspaceSidebar` 组件替换为：

```rust
#[component]
fn WorkspaceSidebar(
    slug: String,
    name: RwSignal<String>,
    views: RwSignal<Vec<View>>,
    active: RwSignal<Option<String>>,
    on_select: Callback<String>,
    on_new: Callback<()>,
    on_delete: Callback<String>,
) -> impl IntoView {
    let list = move || views.get();
    let mine = move || list().into_iter().filter(|v| !v.is_shared).collect::<Vec<_>>();
    let shared = move || list().into_iter().filter(|v| v.is_shared).collect::<Vec<_>>();

    let row = move |v: View, shared_mark: bool| {
        let id = v.id.clone();
        let id_click = id.clone();
        let name = v.name.clone();
        let is_active = move || active.get().as_deref() == Some(id.as_str());
        let on_select = on_select;
        let on_delete = on_delete;
        let click_id = id.clone();
        let del_id = id.clone();
        view! {
            <div class=move || if is_active() { "it on" } else { "it" }
                 on:click=move |_| on_select.run(click_id.clone())>
                {if shared_mark { ic_share() } else { ic_folder() }}
                <span style="flex:1">{name}</span>
                <button class="ibtn" title="删除视图" on:click=move |ev| {
                    ev.stop_propagation();
                    on_delete.run(del_id.clone());
                }>"×"</button>
            </div>
        }
    };

    view! {
        <aside class="panel wside">
            <div style="padding:8px 12px;display:flex;gap:8px;align-items:center">
                <b>{move || name.get()}</b>
            </div>
            <div class="grp">"我的视图"</div>
            {move || mine().into_iter().map(|v| row(v, false)).collect::<Vec<_>>()}
            <div class="grp">"共享视图"</div>
            {move || shared().into_iter().map(|v| row(v, true)).collect::<Vec<_>>()}
            <div class="it" style="color:var(--ink3)" on:click=move |_| on_new.run(())>
                {ic_add()}"新建视图"
            </div>
            <div style="border-top:1px solid var(--line);margin-top:8px;padding-top:8px">
                <A href=format!("/{slug}/settings")>
                    <div class="it">{ic_setting()}"工作空间设置"</div>
                </A>
                <A href="/workspaces">
                    <div class="it">{ic_back()}"工作空间列表"</div>
                </A>
            </div>
        </aside>
    }
}
```

> Leptos 的 `Callback` 是 `Copy`，但闭包 `row` 会被多次调用——把 `row` 写成返回 `impl IntoView` 的 `move` 闭包时，捕获的 `on_select`/`on_delete` 需在每个分支重新 `let` 一份（已在上例中拷贝为局部变量）。若编译器报借用已移动，改用 `Callback` 的 `.run()` 前先 `let cb = on_select;`。

- [ ] **Step 2: 在 WorkspaceMain 中接线**

`WorkspaceMain` 顶部信号区加：

```rust
    let view_list = RwSignal::new(Vec::<View>::new());
    let active_view = RwSignal::new(None::<View>);
```

在数据加载 `Effect` 中，`data.set(Some(result))` 之前加：

```rust
            if let Ok((ref w, _, _)) = result {
                let ws_id = w.id.clone();
                let s2 = s.clone();
                spawn_local(async move {
                    if let Ok(list) = views(&ws_id).await {
                        if let Some(first) = list.first().cloned() {
                            active_view.set(Some(first));
                        }
                        view_list.set(list);
                    }
                    let _ = s2;
                });
            }
```

在 `view!` 中替换 `<WorkspaceSidebar slug=... name=... />` 为：

```rust
                <WorkspaceSidebar
                    slug=slug().to_string()
                    name=ws_name
                    views=view_list
                    active=Signal::derive(move || active_view.get().map(|v| v.id)).into()
                    on_select=Callback::new(move |id: String| {
                        if let Some(v) = view_list.get().into_iter().find(|v| v.id == id) {
                            active_view.set(Some(v));
                        }
                    })
                    on_new=Callback::new(move |_| { /* Task 9 Step 3 */ })
                    on_delete=Callback::new(move |id: String| {
                        spawn_local(async move {
                            let _ = delete_view(&id).await;
                            view_list.update(|l| l.retain(|v| v.id != id));
                            if active_view.get().map(|v| v.id) == Some(id.clone()) {
                                active_view.set(view_list.get().first().cloned());
                            }
                        });
                    })
                />
```

> `active` 的传参类型统一为 `RwSignal<Option<String>>`，把 `WorkspaceSidebar` 的 `active` 参数改成该类型即可（避免 `Signal::derive` 的额外转换）。

- [ ] **Step 3: 新建/重命名视图弹窗**

在 `WorkspaceMain` 加信号：

```rust
    let show_view_dialog = RwSignal::new(false);
    let view_name_input = RwSignal::new(String::new());
    let view_shared_input = RwSignal::new(false);
    let view_columns_input = RwSignal::new(String::new()); // 逗号分隔的标签 name
```

在 `view!` 的 `.page` 内末尾加：

```rust
            {move || if show_view_dialog.get() {
                let ws_id = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone());
                view! {
                    <div class="dmodal">
                        <div class="panel dmbox">
                            <h3>"新建视图"</h3>
                            <input class="inp" placeholder="视图名称" prop:value=view_name_input
                                on:input=move |ev| view_name_input.set(event_target_value(&ev)) />
                            <input class="inp" placeholder="展示为列的标签（逗号分隔，可空）" prop:value=view_columns_input
                                on:input=move |ev| view_columns_input.set(event_target_value(&ev)) />
                            <label><input type="checkbox" prop:checked=view_shared_input
                                on:change=move |ev| view_shared_input.set(event_target_checked(&ev)) />" 共享给工作空间"</label>
                            <div style="display:flex;gap:8px;justify-content:flex-end">
                                <button class="btn" on:click=move |_| show_view_dialog.set(false)>"取消"</button>
                                <button class="btn pri" on:click=move |_| {
                                    let Some(ws_id) = ws_id.clone() else { return };
                                    let name = view_name_input.get();
                                    let shared = view_shared_input.get();
                                    let cols: Vec<String> = view_columns_input.get().split(',')
                                        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                                    spawn_local(async move {
                                        if let Ok(v) = create_view(&ws_id, &name, &serde_json::json!({"and": []}),
                                            "updatedAt", true, &cols, shared).await {
                                            view_list.update(|l| l.push(v.clone()));
                                            active_view.set(Some(v));
                                            show_view_dialog.set(false);
                                        }
                                    });
                                }>"创建"</button>
                            </div>
                        </div>
                    </div>
                }.into_any()
            } else { view! { <div></div> }.into_any() }}
```

把 `on_new` 回调改为 `Callback::new(move |_| show_view_dialog.set(true))`。

CSS 追加到 `style/main.css` 末尾：

```css
.dmodal{position:fixed;inset:0;background:rgba(15,23,42,.35);display:flex;align-items:center;justify-content:center;z-index:50}
.dmbox{width:420px;padding:16px;display:flex;flex-direction:column;gap:10px}
.dmbox h3{margin:0}
```

- [ ] **Step 4: 构建验证（wasm）**

Run: `cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: 通过。

- [ ] **Step 5: 提交**

```bash
git add src/frontend/pages/workspace_main.rs style/main.css
git commit -m "feat(frontend): add view sidebar and view CRUD dialog"
```

---

### Task 10: 前端 —— 筛选芯片与表达式模式

**Files:**
- Modify: `src/frontend/pages/workspace_main.rs`
- Modify: `style/main.css`

**Interfaces:**
- Consumes: `view_filter::{chips, build_query, with_text, CondChip}`、`graphql_client::{parse_view_query, label_schemas, LabelSchema}`
- Produces: `WorkspaceMain` 内 `query_ast: RwSignal<Value>`、`expr_mode: RwSignal<bool>`、`expr_text: RwSignal<String>`、`ad_hoc_text: RwSignal<String>`

- [ ] **Step 1: 加筛选条 UI**

`workspace_main.rs` 加信号：

```rust
    let query_ast = RwSignal::new(serde_json::json!({ "and": [] }));
    let expr_mode = RwSignal::new(false);
    let expr_text = RwSignal::new(String::new());
    let ad_hoc_text = RwSignal::new(String::new());
    let new_cond_label = RwSignal::new(String::new());
```

把现有 `<div class="filters">…</div>`（第 101–105 行）替换为：

```rust
                    <div class="filters">
                        {move || if expr_mode.get() {
                            view! {
                                <input class="inp" style="flex:1" placeholder="Task = \"Open\" AND present(Priority)"
                                    prop:value=expr_text
                                    on:input=move |ev| expr_text.set(event_target_value(&ev)) />
                                <button class="btn" on:click=move |_| {
                                    let Some(ws_id) = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone()) else { return };
                                    let expr = expr_text.get();
                                    spawn_local(async move {
                                        match parse_view_query(&ws_id, &expr).await {
                                            Ok(ast) => { query_ast.set(ast); expr_mode.set(false); error.set(None); }
                                            Err(e) => error.set(Some(e)),
                                        }
                                    });
                                }>"应用"</button>
                                <button class="btn" on:click=move |_| {
                                    expr_mode.set(false);
                                    expr_text.set(crate::frontend::graphql_client::View { query: query_ast.get(), ..default_view() }.query.to_string());
                                }>"取消"</button>
                            }.into_any()
                        } else {
                            view! {
                                {move || chips(&query_ast.get()).into_iter().map(|c| chip_view(c, query_ast)).collect::<Vec<_>>()}
                                <select class="inp" style="width:130px" prop:value=new_cond_label
                                    on:change=move |ev| new_cond_label.set(event_target_value(&ev))>
                                    <option value="">"＋ 条件"</option>
                                    {move || schemas.get().into_iter().map(|s| view! {
                                        <option value=s.name.clone()>{s.title.clone()}</option>
                                    }).collect::<Vec<_>>()}
                                </select>
                                <button class="btn" on:click=move |_| {
                                    let name = new_cond_label.get();
                                    if name.is_empty() { return; }
                                    let mut cs = chips(&query_ast.get());
                                    cs.push(CondChip::Label { name, op: "present".into(), value: serde_json::Value::Null });
                                    query_ast.set(build_query(&cs));
                                    new_cond_label.set(String::new());
                                }>"添加"</button>
                                <button class="btn" on:click=move |_| {
                                    let ast = query_ast.get();
                                    // 表达式文本由服务端格式化，避免前端复刻语法
                                    let Some(ws_id) = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone()) else { return };
                                    spawn_local(async move {
                                        if let Ok(s) = crate::frontend::graphql_client::format_view_query_auth(&ws_id, &ast).await {
                                            expr_text.set(s);
                                        }
                                        expr_mode.set(true);
                                    });
                                }>"表达式"</button>
                            }.into_any()
                        }}
                        <span style="margin-left:auto" class="mut">{move || format!("排序：{}", sort_label())}</span>
                    </div>
```

`default_view()` 无实际用途——上面「取消」分支改为：

```rust
                                <button class="btn" on:click=move |_| expr_mode.set(false)>"取消"</button>
```

`chip_view` 与 `sort_label` 作为 `WorkspaceMain` 外的模块级辅助：

```rust
fn chip_view(chip: CondChip, ast: RwSignal<serde_json::Value>) -> impl IntoView {
    let label = match &chip {
        CondChip::Label { name, op, value } => format!("{name} {op} {}", value_to_string(value)),
        CondChip::Time { field, op, value } => format!("{field} {op} {}", value_to_string(value)),
        CondChip::Text { keyword } => format!("全文：「{keyword}」"),
    };
    let target = chip.clone();
    view! {
        <span class="chip sel">
            {label}
            <button class="ibtn" title="移除" on:click=move |_| {
                let mut cs = chips(&ast.get());
                cs.retain(|c| c != &target);
                ast.set(build_query(&cs));
            }>"×"</button>
        </span>
    }
}

fn sort_label() -> String {
    "更新时间 ↓".to_string()
}
```

> 需要 `use crate::frontend::view_filter::{build_query, chips, with_text, CondChip};` 与 `use crate::frontend::components::value_to_string;`。`format_view_query_auth` 是 Task 8 未提供的新函数——**在 `graphql_client.rs` 补一个**：

```rust
pub async fn format_view_query_auth(workspace_id: &str, query: &Value) -> Result<String, String> {
    let data = graphql(
        "query($id: ID!, $q: JSON!) { formatViewQuery(workspaceId: $id, query: $q) }",
        json!({ "id": workspace_id, "q": query }),
    )
    .await?;
    Ok(data.get("formatViewQuery").and_then(|v| v.as_str()).unwrap_or("").to_string())
}
```

命名为 `format_view_query` 即可（不必带 `_auth` 后缀，前端无此类命名约定）。

- [ ] **Step 2: 顶栏搜索框接通（ad-hoc 全文）**

把 `<input placeholder="搜索本视图（即将上线）" disabled />` 替换为：

```rust
                            <input placeholder="搜索本视图，可与过滤组合" prop:value=ad_hoc_text
                                on:input=move |ev| ad_hoc_text.set(event_target_value(&ev))
                                on:keydown=move |ev| {
                                    if ev.key() == "Enter" {
                                        let mut ast = query_ast.get();
                                        ast = with_text(&ast, &ad_hoc_text.get());
                                        query_ast.set(ast);
                                        refresh_view.update(|n| *n += 1);
                                    }
                                } />
```

新增 `let refresh_view = RwSignal::new(0u32);`。

- [ ] **Step 3: 用 queryEntries 驱动表格**

把现有数据加载里对 `entries(&ws.id)` 的调用替换为：

```rust
                let ast = {
                    let base = active_view.get().map(|v| v.query).unwrap_or(serde_json::json!({"and": []}));
                    with_text(&base, &ad_hoc_text.get())
                };
                let sort_field = active_view.get().map(|v| v.sort.field).unwrap_or_else(|| "updatedAt".to_string());
                let sort_desc = active_view.get().map(|v| v.sort.desc).unwrap_or(true);
                let page = query_entries(&ws.id, &ast, &sort_field, sort_desc, 1, 100).await?;
                let items = page.items;
```

并把 `data` 的类型保持 `(Workspace, Vec<Entry>, Vec<LabelSchema>)` 不变（`page.items` 即 `Vec<Entry>`）。Effect 的依赖追加 `refresh_view.get()` 与 `query_ast.get()` 与 `active_view.get()`。

> 分页参数在 Task 11 改为真实分页；本步先用 `page=1, page_size=100` 跑通。

- [ ] **Step 4: 构建验证 + 提交**

Run: `cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: 通过。

```bash
git add src/frontend/pages/workspace_main.rs src/frontend/graphql_client.rs style/main.css
git commit -m "feat(frontend): add filter chips and expression query mode"
```

---

### Task 11: 前端 —— 动态列、排序与分页

**Files:**
- Modify: `src/frontend/pages/workspace_main.rs`
- Modify: `style/main.css`

**Interfaces:**
- Consumes: `View.columns`、`components::label_chip_class`、`graphql_client::query_entries`

- [ ] **Step 1: 表格动态列**

把 `EntryTable` 组件改为接收列配置：

```rust
#[component]
fn EntryTable(
    data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>>,
    schemas: RwSignal<Vec<LabelSchema>>,
    selected: RwSignal<String>,
    columns: RwSignal<Vec<String>>,
    sort_field: RwSignal<String>,
    sort_desc: RwSignal<bool>,
    on_sort: Callback<String>,
) -> impl IntoView {
    let rows = move || data.get().and_then(|r| r.ok()).map(|(_, e, _)| e).unwrap_or_default();
    let cols = move || columns.get();

    view! {
        <table class="tbl">
            <thead><tr>
                <th>"Code"</th>
                <th on:click=move |_| on_sort.run("title".to_string())>"标题"</th>
                {move || cols().iter().map(|name| {
                    let n = name.clone();
                    let title = schemas.get().into_iter().find(|s| s.name == n).map(|s| s.title).unwrap_or(n.clone());
                    let n2 = n.clone();
                    view! { <th on:click=move |_| on_sort.run(n2.clone())>{title}</th> }
                }).collect::<Vec<_>>()}
                <th on:click=move |_| on_sort.run("updatedAt".to_string())>"更新时间"</th>
            </tr></thead>
            <tbody>
                {move || rows().into_iter().map(|e| {
                    let code = e.code.clone();
                    let code2 = code.clone();
                    let is_sel = move || selected.get() == code2;
                    let labels = e.labels.clone();
                    let cols_now = cols();
                    view! {
                        <tr class=move || if is_sel() { "sel" } else { "" }
                            on:click=move |_| selected.set(code.clone())>
                            <td class="code">{e.code.clone()}</td>
                            <td>{e.title.clone()}</td>
                            {cols_now.iter().map(|name| {
                                let lv = labels.iter().find(|l| &l.label_name == name).map(|l| l.value.clone());
                                match lv {
                                    Some(v) => {
                                        let s = value_to_string(&v);
                                        let cls = label_chip_class(name, &s);
                                        view! { <td><span class=format!("chip {cls}")>{display_enum_value(&s)}</span></td> }.into_any()
                                    }
                                    None => view! { <td class="mut">"—"</td> }.into_any(),
                                }
                            }).collect::<Vec<_>>()}
                            <td class="mut">{short_time(&e.updated_at)}</td>
                        </tr>
                    }
                }).collect::<Vec<_>>()}
            </tbody>
        </table>
    }
}
```

在 `WorkspaceMain` 加 `let page_signal = RwSignal::new(1i64);` `let page_size = 20i64;` `let total_signal = RwSignal::new(0i64);`，把 `<EntryTable …/>` 调用改为传入 `columns=Signal::derive(move || active_view.get().map(|v| v.columns).unwrap_or_default()).into()`、`sort_field`、`sort_desc`、`on_sort`。

- [ ] **Step 2: 真实分页**

Effect 中的 `query_entries(..., 1, 100)` 改为 `query_entries(&ws.id, &ast, &sort_field, sort_desc, page_signal.get(), page_size)`，并在成功分支加 `total_signal.set(page.total);`。

表格下加 pager：

```rust
                            <div class="pager">
                                <button on:click=move |_| page_signal.update(|p| *p = (*p - 1).max(1))>"‹"</button>
                                {move || {
                                    let pages = ((total_signal.get() + page_size - 1) / page_size).max(1);
                                    (1..=pages.min(9)).map(|p| {
                                        let cur = page_signal.get();
                                        view! {
                                            <button class=if p == cur { "on" } else { "" }
                                                on:click=move |_| page_signal.set(p)>{p}</button>
                                        }
                                    }).collect::<Vec<_>>()
                                }}
                                <button on:click=move |_| page_signal.update(|p| *p += 1)>"›"</button>
                                <span class="mut">{move || format!("共 {} 条", total_signal.get())}</span>
                            </div>
```

- [ ] **Step 3: 排序切换**

`on_sort` 回调：

```rust
                    on_sort=Callback::new(move |field: String| {
                        let cur_field = active_view.get().map(|v| v.sort.field).unwrap_or_default();
                        let cur_desc = active_view.get().map(|v| v.sort.desc).unwrap_or(true);
                        let desc = if field == cur_field { !cur_desc } else { true };
                        if let Some(mut v) = active_view.get() {
                            v.sort.field = field;
                            v.sort.desc = desc;
                            active_view.set(Some(v));
                        }
                        page_signal.set(1);
                    })
```

> 排序变更只改本地 `active_view`（未落库）；「保存视图」由 Task 9 的 update_view 触发（在弹窗里加「保存当前条件」按钮，或复选后自动保存）。本任务先在筛选条右侧加一个「保存视图」按钮：

```rust
                                <button class="btn" on:click=move |_| {
                                    let Some(v) = active_view.get() else { return };
                                    let ast = query_ast.get();
                                    let cols = v.columns.clone();
                                    let shared = v.is_shared;
                                    let id = v.id.clone();
                                    let name = v.name.clone();
                                    let field = v.sort.field.clone();
                                    let desc = v.sort.desc;
                                    spawn_local(async move {
                                        if let Ok(saved) = update_view(&id, &name, &ast, &field, desc, &cols, shared).await {
                                            view_list.update(|l| {
                                                if let Some(slot) = l.iter_mut().find(|x| x.id == saved.id) { *slot = saved.clone(); }
                                            });
                                            active_view.set(Some(saved));
                                        }
                                    });
                                }>"保存视图"</button>
```

- [ ] **Step 4: 构建验证 + 提交**

Run: `cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown && cargo build`
Expected: 均通过。

```bash
git add src/frontend/pages/workspace_main.rs style/main.css
git commit -m "feat(frontend): add dynamic columns, sorting and pagination"
```

---

### Task 12: 端到端验证与收尾

**Files:**
- Modify: `docs/superpowers/specs/2026-09-11-view-filter-design.md`（仅在实现与设计有偏差时更新）

- [ ] **Step 1: 全量测试与构建**

Run: `cargo test && cargo build && cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown`
Expected: 全部通过。

- [ ] **Step 2: 启动开发服务器（用户浏览器验收）**

先清空开发数据（新 CF 与索引目录，bincode 不兼容）：`rm -rf data`
Run: `make dev`

浏览器验收清单：
1. 登录 `admin@local` / `Admin12345`，进入工作空间。
2. 侧栏应出现「我的视图 / 共享视图」；新建一个视图（含共享勾选），侧栏即时出现。
3. 筛选条「＋ 条件」添加 `Task`，切成 `present(Task)`，表格随之变化。
4. 点「表达式」，输入 `Task = "Open" AND updated >= "2026-01-01"`，应用后 chips 与表格一致。
5. 顶栏输入中文关键词回车，能命中详情/标签中的中文（含中文标题、中文标签值）。
6. 视图配置输入列（如 `Task,Priority`）后表格出现对应列；列头点击切换排序。
7. 分页页码可点击、`共 N 条` 与过滤结果一致。
8. 「保存视图」后刷新页面，视图与条件仍在。

- [ ] **Step 3: 更新 spec（如实现有偏差）并提交**

```bash
git add docs/superpowers/specs/2026-09-11-view-filter-design.md
git commit -m "docs(spec): sync view-filter design with implementation"
```

---

## 自审结论

- **Spec 覆盖**：§3 领域模型/语法/求值 → Task 1/2；§4 存储 → Task 3；§5.1 Tantivy → Task 4；§5.2 查询编排 → Task 5；§5.3 ViewService + 权限落点 → Task 6（角色判定在 Task 7 的 GraphQL 层，与既有代码一致）；§6 GraphQL → Task 7；§7 前端 → Task 8/9/10/11；§8 错误处理 → Task 2 的 `InvalidQuery` + Task 7 校验；§9 测试 → 各任务内联；§10 破坏性变更 → Task 12 的 `rm -rf data`。
- **类型一致性**：`Query`/`Condition`/`Field`/`Op`、`SortSpec`/`SortField`、`PageInput`/`QueryResult`、`View`、`SearchIndex` 的签名在各任务间保持一致；GraphQL 的 `queryEntries`/`views`/`createView`… 与前端 `query_entries`/`views`/`create_view`… 一一对应。
- **占位符扫描**：无 TBD/TODO；每个代码步骤含可粘贴代码。Task 5 中出现的两遍 `retain` 草稿已在同一步用「最终实现必须写成单次求值」明确替换，执行时以第二版为准。
- **已知风险**：Tantivy 0.26 的 `BooleanQuery::new`/`searcher.doc` 泛型、`#[graphql(rename_fields)]` 属性名在个别版本上可能有出入——Task 4/7 各含一条「若报错按此替换」的兜底说明。
