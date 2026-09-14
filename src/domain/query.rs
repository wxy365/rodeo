use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

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

    pub fn contains_account_field(&self) -> bool {
        match self {
            Query::And(v) | Query::Or(v) => v.iter().any(Query::contains_account_field),
            Query::Not(q) => q.contains_account_field(),
            Query::Cond(c) => matches!(c.field, Field::CreatedBy | Field::UpdatedBy),
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
        // 引擎只按 first_text_keyword 检索，多个不同全文关键词会得出错误结果。
        let mut kws = HashSet::new();
        collect_text_keywords(self, &mut kws);
        if kws.len() > 1 {
            return Err(AppError::InvalidQuery(
                "本轮仅支持单个全文条件".to_string(),
            ));
        }
        match self {
            Query::And(v) | Query::Or(v) => v.iter().try_for_each(|q| q.validate(schemas)),
            Query::Not(q) => q.validate(schemas),
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
}

impl Condition {
    fn validate(&self, schemas: &[LabelSchema]) -> Result<(), AppError> {
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
                                return Err(AppError::InvalidQuery(
                                    "比较值须为字符串".to_string(),
                                ));
                            }
                        }
                    }
                }
            }
            Field::Label(name) => {
                let schema = schemas
                    .iter()
                    .find(|s| &s.name == name)
                    .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {name}")))?;
                if !op_allowed(schema.value_type, self.op) {
                    return Err(AppError::InvalidQuery(format!(
                        "运算符 {} 不适用于标签 {name}（{}）",
                        op_label(self.op),
                        type_label(schema.value_type)
                    )));
                }
                // in / not in 的候选值必须落在 enum_values 内。
                if matches!(self.op, Op::In | Op::NotIn) && schema.value_type == LabelValueType::Enum
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
                            return Err(AppError::InvalidQuery(
                                "比较值须为字符串".to_string(),
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

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
                            return cmp_time_layout(
                                &l.value.to_json(),
                                layout,
                                self.op,
                                self.value.as_ref(),
                            );
                        }
                    }
                    cmp_value(&l.value.to_json(), self.op, self.value.as_ref())
                }
            },
        }
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
    }
}

/// 字段的规范名：内置字段给出关键字原文，Label 无规范名（返回空串）。
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

fn as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
}

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
    Bang,
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
                _ => { out.push(Tok::Bang); i += 1; }
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
        Some(Tok::And) => "AND".to_string(),
        Some(Tok::Or) => "OR".to_string(),
        Some(Tok::Not) => "NOT".to_string(),
        Some(Tok::In) => "in".to_string(),
        Some(Tok::Text) => "text".to_string(),
        Some(Tok::Builtin(f)) => canonical_name(f).to_string(),
        Some(Tok::Ident(s)) => s.clone(),
        Some(Tok::Str(s)) => s.clone(),
        Some(Tok::Num(n)) => n.to_string(),
        Some(Tok::Bool(b)) => b.to_string(),
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
        if self.peek() == Some(&Tok::Bang) {
            self.next();
            // `!标签名` 是存在性取反的语法糖，直接落成缺席条件（等价于旧的 absent()）；
            // 其余形式按普通一元否定处理，`!x` 即 `NOT x`。
            return Ok(match self.parse_primary()? {
                Query::Cond(Condition { field: Field::Label(name), op: Op::Present, value: None }) => {
                    Query::Cond(Condition { field: Field::Label(name), op: Op::Absent, value: None })
                }
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
                Ok(Query::Cond(Condition { field: Field::Text, op, value: Some(v) }))
            }
            Some(Tok::Builtin(_)) | Some(Tok::Ident(_)) => self.parse_condition(),
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
            Some(Tok::Num(n)) => Ok(serde_json::Number::from_f64(n).map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)),
            Some(Tok::Bool(b)) => Ok(serde_json::Value::Bool(b)),
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
            Some(Tok::Ident(s)) => Field::Label(s),
            other => {
                return Err(AppError::InvalidQuery(format!(
                    "期望字段，实际 {}",
                    tok_label(other.as_ref())
                )))
            }
        };
        if let Field::Label(_) = &field {
            if self.peek() == Some(&Tok::LParen) {
                return Err(AppError::InvalidQuery(
                    "不再支持 present()/absent()：标签名单独出现即表示「存在」，前缀 ! 表示「不存在」"
                        .to_string(),
                ));
            }
            // 标签名后不接运算符时是存在性判断：`Task` 等价于旧的 present(Task)。
            if !matches!(
                self.peek(),
                Some(Tok::Eq | Tok::Ne | Tok::Gt | Tok::Ge | Tok::Lt | Tok::Le | Tok::Tilde | Tok::NotTilde | Tok::In | Tok::Not)
            ) {
                return Ok(Query::Cond(Condition { field, op: Op::Present, value: None }));
            }
        }
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
            other => canonical_name(other).to_string(),
        };
        match self.op {
            Op::Present => field,
            Op::Absent => format!("!{field}"),
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
        assert_eq!(Query::parse("Title ~ \"登录\"").unwrap().to_expr(), "Title ~ \"登录\"");
    }

    #[test]
    fn parse_integer_eq_matches_integer_label() {
        let e = entry();
        let labels = vec![labeling("Score", serde_json::json!(7))];
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let env = EvalEnv { text_hit: &never, account_of: &no_acct, label_of: &no_label };
        assert!(Query::parse("Score = 7").unwrap().evaluate(&e, &labels, &env));
        assert!(!Query::parse("Score != 7").unwrap().evaluate(&e, &labels, &env));
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
        let env = EvalEnv { text_hit: &never, account_of: &no_acct, label_of: &no_label };

        let present = Query::parse("Task").unwrap();
        assert!(present.evaluate(&e, &labels, &env));
        let absent = Query::parse("!Task").unwrap();
        assert!(!absent.evaluate(&e, &labels, &env));
        assert!(Query::parse("!Priority").unwrap().evaluate(&e, &labels, &env));

        // 语法糖落成与旧函数等价的条件，且格式化后仍可回读。
        assert_eq!(Query::parse("Task").unwrap().to_expr(), "Task");
        assert_eq!(Query::parse("!Task").unwrap().to_expr(), "!Task");
        // `!` 后接比较时退化为普通取反。
        assert!(Query::parse("!(Task = \"Done\")").unwrap().evaluate(&e, &labels, &env));
        // 组合表达式中存在性判断不再需要括号。
        assert!(Query::parse("Task AND !Priority").unwrap().evaluate(&e, &labels, &env));
    }

    #[test]
    fn removed_present_and_absent_functions_are_rejected() {
        for src in ["present(Task)", "absent(Priority)", "Task AND present(Priority)"] {
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
        let env = EvalEnv { text_hit: &never, account_of: &no_acct, label_of: &no_label };

        let present = Query::Cond(Condition { field: Field::Label("Task".into()), op: Op::Present, value: None });
        assert!(present.evaluate(&e, &labels, &env));
        // 打标缺失时比较一律 false
        let missing_cmp = Query::Cond(Condition {
            field: Field::Label("Priority".into()), op: Op::Eq, value: Some(serde_json::json!("P0")),
        });
        assert!(!missing_cmp.evaluate(&e, &labels, &env));
        let absent = Query::Cond(Condition { field: Field::Label("Priority".into()), op: Op::Absent, value: None });
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
        let env = EvalEnv { text_hit: &never, account_of: &no_acct, label_of: &no_label };
        let cond = |name: &str, op: Op, v: serde_json::Value| Query::Cond(Condition {
            field: Field::Label(name.into()), op, value: Some(v),
        });

        assert!(cond("Score", Op::Gt, serde_json::json!(5)).evaluate(&e, &labels, &env));
        assert!(!cond("Score", Op::Lt, serde_json::json!(5)).evaluate(&e, &labels, &env));
        assert!(cond("Title", Op::Contains, serde_json::json!("联调")).evaluate(&e, &labels, &env));
        assert!(cond("Task", Op::In, serde_json::json!(["Open", "Done"])).evaluate(&e, &labels, &env));
        assert!(!cond("Task", Op::In, serde_json::json!(["Done"])).evaluate(&e, &labels, &env));
    }

    #[test]
    fn evaluate_time_and_boolean_logic() {
        let e = entry();
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let env = EvalEnv { text_hit: &never, account_of: &no_acct, label_of: &no_label };
        let future = Query::Cond(Condition {
            field: Field::UpdatedAt, op: Op::Gt, value: Some(serde_json::json!("2099-01-01")),
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
            field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("检索")),
        });
        let hit = |kw: &str| kw == "检索";
        let miss = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let no_label = |_: &str| None;
        let hit_env = EvalEnv { text_hit: &hit, account_of: &no_acct, label_of: &no_label };
        let miss_env = EvalEnv { text_hit: &miss, account_of: &no_acct, label_of: &no_label };
        assert!(q.evaluate(&e, &[], &hit_env));
        assert!(!q.evaluate(&e, &[], &miss_env));
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

    #[test]
    fn validate_text_only_contains() {
        let schemas: Vec<LabelSchema> = vec![];
        let ok = Query::Cond(Condition {
            field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("检索")),
        });
        assert!(ok.validate(&schemas).is_ok());

        for bad_op in [Op::Eq, Op::Ne, Op::NotContains, Op::In] {
            let q = Query::Cond(Condition {
                field: Field::Text, op: bad_op, value: Some(serde_json::json!("检索")),
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
            Ulid::new(), "Task".into(), "任务".into(), LabelValueType::Enum,
            vec!["Open".into(), "Done".into()],
        )];
        let ok = Query::Cond(Condition {
            field: Field::Label("Task".into()), op: Op::In,
            value: Some(serde_json::json!(["Open", "Done"])),
        });
        assert!(ok.validate(&schemas).is_ok());

        let out_of_range = Query::Cond(Condition {
            field: Field::Label("Task".into()), op: Op::In,
            value: Some(serde_json::json!(["Open", "Nope"])),
        });
        assert!(matches!(out_of_range.validate(&schemas), Err(AppError::InvalidQuery(_))));

        let not_array = Query::Cond(Condition {
            field: Field::Label("Task".into()), op: Op::NotIn,
            value: Some(serde_json::json!("Open")),
        });
        assert!(matches!(not_array.validate(&schemas), Err(AppError::InvalidQuery(_))));
    }

    #[test]
    fn validate_rejects_invalid_time_format() {
        let schemas: Vec<LabelSchema> = vec![];
        let bad = Query::Cond(Condition {
            field: Field::UpdatedAt, op: Op::Ge, value: Some(serde_json::json!("2026-13-40")),
        });
        assert!(matches!(bad.validate(&schemas), Err(AppError::InvalidQuery(_))));

        let ok_date = Query::Cond(Condition {
            field: Field::CreatedAt, op: Op::Ge, value: Some(serde_json::json!("2026-09-01")),
        });
        assert!(ok_date.validate(&schemas).is_ok());

        let ok_rfc = Query::Cond(Condition {
            field: Field::UpdatedAt, op: Op::Le,
            value: Some(serde_json::json!("2026-09-01T00:00:00Z")),
        });
        assert!(ok_rfc.validate(&schemas).is_ok());
    }

    #[test]
    fn validate_rejects_multiple_text_keywords() {
        let schemas: Vec<LabelSchema> = vec![];
        let two = Query::And(vec![
            Query::Cond(Condition {
                field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("a")),
            }),
            Query::Cond(Condition {
                field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("b")),
            }),
        ]);
        assert!(matches!(two.validate(&schemas), Err(AppError::InvalidQuery(_))));

        // 相同关键词只算一个，仍然允许。
        let same = Query::And(vec![
            Query::Cond(Condition {
                field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("a")),
            }),
            Query::Cond(Condition {
                field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("a")),
            }),
        ]);
        assert!(same.validate(&schemas).is_ok());

        let nested = Query::Not(Box::new(Query::Cond(Condition {
            field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("x")),
        })));
        let nested_two = Query::And(vec![
            nested,
            Query::Cond(Condition {
                field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("y")),
            }),
        ]);
        assert!(matches!(nested_two.validate(&schemas), Err(AppError::InvalidQuery(_))));
    }
}
