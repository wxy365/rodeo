//! 客户端查询求值器与颜色解析（纯函数，不依赖 `domain`）。
//!
//! 后端 `Query` AST（externally-tagged camelCase JSON）与 `ValueColor` 配置
//! 都以前端 `serde_json::Value` 承载，在浏览器里对 `Entry` + `Labeling`
//! 求值，用于标题着色与服务端查询结果的本地复核。

use serde_json::Value;

use crate::frontend::graphql_client::{Entry, Labeling};

// ---------- 查询求值 ----------

/// 对一行 `entry`（携带 `labels`）求值后端下发的 `Query` JSON。
///
/// 支持 `{"and":[…]}` / `{"or":[…]}` / `{"not":{…}}` / `{"cond":{…}}`。
pub fn eval(query: &Value, entry: &Entry, labels: &[Labeling]) -> bool {
    if let Some(list) = query.get("and").and_then(Value::as_array) {
        return list.iter().all(|q| eval(q, entry, labels));
    }
    if let Some(list) = query.get("or").and_then(Value::as_array) {
        return list.iter().any(|q| eval(q, entry, labels));
    }
    if let Some(inner) = query.get("not") {
        return !eval(inner, entry, labels);
    }
    if let Some(cond) = query.get("cond") {
        return eval_cond(cond, entry, labels);
    }
    false
}

fn eval_cond(cond: &Value, entry: &Entry, labels: &[Labeling]) -> bool {
    let Some(field) = cond.get("field") else {
        return false;
    };
    let op = cond.get("op").and_then(Value::as_str).unwrap_or("");
    let want = cond.get("value");

    if let Some(name) = field.get("label").and_then(Value::as_str) {
        return eval_label(name, op, want, labels);
    }
    match field.as_str() {
        Some("updatedAt") => cmp_time(&entry.updated_at, op, want),
        Some("createdAt") => cmp_time(&entry.created_at, op, want),
        Some("text") => eval_text(op, want, entry, labels),
        _ => false,
    }
}

fn eval_label(name: &str, op: &str, want: Option<&Value>, labels: &[Labeling]) -> bool {
    let found = labels.iter().find(|l| l.label_name == name);
    match op {
        "present" => found.is_some(),
        "absent" => found.is_none(),
        _ => match found {
            Some(l) => cmp_value(&l.value, op, want),
            None => false,
        },
    }
}

fn cmp_value(got: &Value, op: &str, want: Option<&Value>) -> bool {
    let Some(want) = want else {
        return false;
    };
    match op {
        "eq" => json_eq(got, want),
        "ne" => !json_eq(got, want),
        "gt" | "ge" | "lt" | "le" => match (as_f64(got), as_f64(want)) {
            (Some(a), Some(b)) => match op {
                "gt" => a > b,
                "ge" => a >= b,
                "lt" => a < b,
                _ => a <= b,
            },
            _ => false,
        },
        "contains" | "notContains" => {
            let (Some(a), Some(b)) = (got.as_str(), want.as_str()) else {
                return false;
            };
            let hit = contains_ci(a, &b.to_lowercase());
            if op == "contains" {
                hit
            } else {
                !hit
            }
        }
        "in" | "notIn" => {
            let Some(list) = want.as_array() else {
                return false;
            };
            let hit = list.iter().any(|x| x == got);
            if op == "in" {
                hit
            } else {
                !hit
            }
        }
        _ => false,
    }
}

fn eval_text(op: &str, want: Option<&Value>, entry: &Entry, labels: &[Labeling]) -> bool {
    let Some(kw) = want.and_then(Value::as_str) else {
        return false;
    };
    let needle = kw.to_lowercase();
    let mut hit = contains_ci(&entry.code, &needle)
        || contains_ci(&entry.title, &needle)
        || contains_ci(&entry.detail, &needle);
    if !hit {
        hit = labels.iter().any(|l| {
            label_text(&l.value).is_some_and(|t| contains_ci(&t, &needle))
        });
    }
    match op {
        "contains" => hit,
        "notContains" => !hit,
        _ => false,
    }
}

fn label_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn contains_ci(hay: &str, needle_lower: &str) -> bool {
    hay.to_lowercase().contains(needle_lower)
}

/// 与后端一致的相等语义：两边都可转数值时按数值，否则按 JSON 相等。
fn json_eq(a: &Value, b: &Value) -> bool {
    match (as_f64(a), as_f64(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn cmp_time(got: &str, op: &str, want: Option<&Value>) -> bool {
    let Some(s) = want.and_then(Value::as_str) else {
        return false;
    };
    let (Some(a), Some(b)) = (parse_ts(got), parse_ts(s)) else {
        return false;
    };
    match op {
        "eq" => a == b,
        "ne" => a != b,
        "gt" => a > b,
        "ge" => a >= b,
        "lt" => a < b,
        "le" => a <= b,
        _ => false,
    }
}

/// 解析 RFC3339 或 `YYYY-MM-DD`（视作 00:00 UTC）为 Unix 秒。
fn parse_ts(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut secs = days_from_civil(year, month, day) * 86_400;

    if let Some(rest) = rest {
        let hh: i64 = rest.get(0..2)?.parse().ok()?;
        let mm: i64 = rest.get(3..5)?.parse().ok()?;
        let ss: i64 = rest.get(6..8).and_then(|x| x.parse().ok()).unwrap_or(0);
        if hh > 23 || mm > 59 || ss > 60 {
            return None;
        }
        secs += hh * 3600 + mm * 60 + ss;
        // 时区后缀：Z 或 ±hh:mm（出现在秒之后）。
        let tail_start = rest.len().min(8);
        if let Some(rel) = rest[tail_start..].find(['Z', '+', '-']) {
            let idx = tail_start + rel;
            let sign = rest.as_bytes()[idx];
            if sign == b'+' || sign == b'-' {
                let off = &rest[idx + 1..];
                let oh: i64 = off.get(0..2).and_then(|x| x.parse().ok()).unwrap_or(0);
                let om: i64 = off.get(3..5).and_then(|x| x.parse().ok()).unwrap_or(0);
                let val = oh * 3600 + om * 60;
                secs -= if sign == b'+' { val } else { -val };
            }
        }
    }
    Some(secs)
}

/// Howard Hinnant `days_from_civil`：公历日期 → 距 1970-01-01 的天数。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ---------- 颜色解析 ----------

/// 按 `value_colors` 规则解析标签值色；未命中回退 `base`。
///
/// `value_colors` 为 `[{color,min,max,value}]`：`value` 存在则精确匹配；
/// 否则按数值区间 `min`（含）/ `max`（不含），`null` 表示无界；首个命中。
pub fn resolve_label_color(base: Option<&Value>, value_colors: &Value, value: &Value) -> Option<String> {
    if let Some(list) = value_colors.as_array() {
        for vc in list {
            let matched = match vc.get("value") {
                Some(exact) if !exact.is_null() => exact == value,
                _ => match (value.as_f64(), vc.get("min").and_then(Value::as_f64), vc.get("max").and_then(Value::as_f64)) {
                    (Some(v), min, max) => {
                        min.map_or(true, |lo| v >= lo) && max.map_or(true, |hi| v < hi)
                    }
                    _ => false,
                },
            };
            if matched {
                if let Some(c) = vc.get("color").and_then(Value::as_str) {
                    return Some(c.to_string());
                }
            }
        }
    }
    base.and_then(Value::as_str).map(str::to_string)
}

/// 按 `rules`（`[{query,color}]`）顺序求值，返回首个命中规则的颜色。
pub fn title_color(rules: &Value, entry: &Entry, labels: &[Labeling]) -> Option<String> {
    let list = rules.as_array()?;
    for rule in list {
        let matched = rule
            .get("query")
            .map(|q| eval(q, entry, labels))
            .unwrap_or(false);
        if matched {
            if let Some(c) = rule.get("color").and_then(Value::as_str) {
                return Some(c.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry() -> Entry {
        Entry {
            code: "E1".into(),
            title: "登录失败".into(),
            detail: "找回密码报错".into(),
            created_at: "2026-08-01T12:00:00Z".into(),
            updated_at: "2026-09-01T12:00:00Z".into(),
            archived_at: None,
            labels: vec![
                labeling("Task", json!("Open")),
                labeling("Score", json!(75)),
            ],
        }
    }

    fn labeling(name: &str, value: Value) -> Labeling {
        Labeling {
            label_name: name.into(),
            value,
        }
    }

    #[test]
    fn eval_boolean_and_label_ops() {
        let e = entry();
        let ls = e.labels.clone();
        let q = json!({"and": [
            {"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Open"}},
            {"not": {"cond": {"field": {"label": "Score"}, "op": "lt", "value": 60}}}
        ]});
        assert!(eval(&q, &e, &ls));
        assert!(eval(
            &json!({"cond": {"field": {"label": "Priority"}, "op": "absent", "value": null}}),
            &e,
            &ls
        ));
        // 缺失标签的比较一律 false
        assert!(!eval(
            &json!({"cond": {"field": {"label": "Priority"}, "op": "eq", "value": "P0"}}),
            &e,
            &ls
        ));
        // 空 AND 恒真，空 OR 恒假
        assert!(eval(&json!({"and": []}), &e, &ls));
        assert!(!eval(&json!({"or": []}), &e, &ls));
    }

    #[test]
    fn eval_time_and_text() {
        let e = entry();
        assert!(eval(
            &json!({"cond": {"field": "updatedAt", "op": "ge", "value": "2026-09-01"}}),
            &e,
            &[]
        ));
        assert!(!eval(
            &json!({"cond": {"field": "updatedAt", "op": "lt", "value": "2026-09-01"}}),
            &e,
            &[]
        ));
        assert!(eval(
            &json!({"cond": {"field": "text", "op": "contains", "value": "密码"}}),
            &e,
            &[]
        ));
        assert!(eval(
            &json!({"cond": {"field": "text", "op": "contains", "value": "open"}}),
            &e,
            &e.labels
        ));
        // Code 也纳入全文检索（大小写不敏感）。
        assert!(eval(
            &json!({"cond": {"field": "text", "op": "contains", "value": "e1"}}),
            &e,
            &[]
        ));
        assert!(!eval(
            &json!({"cond": {"field": "text", "op": "contains", "value": "e9"}}),
            &e,
            &[]
        ));
    }

    #[test]
    fn resolve_and_title_colors() {
        let vcs = json!([
            {"color": "#f00", "min": null, "max": 60.0, "value": null},
            {"color": "#0f0", "min": 60.0, "max": null, "value": null}
        ]);
        assert_eq!(
            resolve_label_color(Some(&json!("#999")), &vcs, &json!(30)).as_deref(),
            Some("#f00")
        );
        assert_eq!(
            resolve_label_color(Some(&json!("#999")), &vcs, &json!(60)).as_deref(),
            Some("#0f0")
        );
        // 未命中回退 base
        assert_eq!(
            resolve_label_color(Some(&json!("#999")), &vcs, &json!("x")).as_deref(),
            Some("#999")
        );
        let enum_colors = json!([{"color": "#00f", "min": null, "max": null, "value": "Open"}]);
        assert_eq!(
            resolve_label_color(None, &enum_colors, &json!("Open")).as_deref(),
            Some("#00f")
        );

        let e = entry();
        let rules = json!([
            {"query": {"cond": {"field": {"label": "Score"}, "op": "ge", "value": 90}}, "color": "#0f0"},
            {"query": {"and": []}, "color": "#888"}
        ]);
        assert_eq!(title_color(&rules, &e, &e.labels).as_deref(), Some("#888"));
    }
}
