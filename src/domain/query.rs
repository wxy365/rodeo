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
        Op::Eq => match (as_f64(got), as_f64(want)) {
            (Some(a), Some(b)) => a == b,
            _ => got == want,
        },
        Op::Ne => match (as_f64(got), as_f64(want)) {
            (Some(a), Some(b)) => a != b,
            _ => got != want,
        },
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Entry, LabelSchema, LabelValue, LabelValueType, Labeling};
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
            r#"Title = "他说 \"你好\"""#,
        ];
        for src in cases {
            let q = Query::parse(src).unwrap_or_else(|e| panic!("parse {src} 失败: {e}"));
            let expr = q.to_expr();
            let again = Query::parse(&expr).unwrap_or_else(|e| panic!("re-parse {expr} 失败: {e}"));
            assert_eq!(q, again, "round-trip 不稳定: {src} -> {expr}");
        }
    }

    #[test]
    fn parse_integer_eq_matches_integer_label() {
        let e = entry();
        let labels = vec![labeling("Score", serde_json::json!(7))];
        let never = |_: &str| false;
        assert!(Query::parse("Score = 7").unwrap().evaluate(&e, &labels, &never));
        assert!(!Query::parse("Score != 7").unwrap().evaluate(&e, &labels, &never));
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
