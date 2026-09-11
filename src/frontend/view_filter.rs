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

/// 判断查询是否可被芯片编辑器无损表示：
/// 顶层是 `{"and": [...]}`（每个元素都是 `{"cond": ...}`）、裸 `{"cond": ...}`，或空。
/// 含 OR / NOT / 嵌套结构的树不可被芯片表示，改写前必须防护。
pub fn is_flat(query: &Value) -> bool {
    match query {
        Value::Null => true,
        Value::Object(map) if map.is_empty() => true,
        Value::Object(map) if map.len() == 1 => {
            if let Some(arr) = query.get("and").and_then(Value::as_array) {
                arr.iter().all(|c| c.get("cond").is_some())
            } else {
                query.get("cond").is_some()
            }
        }
        _ => false,
    }
}

/// 把 ad-hoc 全文关键词并入查询：扁平时替换/追加 text 条件；否则整包一层 and 保留原树。
pub fn with_text(query: &Value, keyword: &str) -> Value {
    let keyword = keyword.trim();
    if is_flat(query) {
        let mut arr: Vec<Value> = conditions(query)
            .into_iter()
            .filter(|c| chip_of(c).map(|ch| !matches!(ch, CondChip::Text { .. })).unwrap_or(true))
            .collect();
        if !keyword.is_empty() {
            arr.push(chip_to_cond(&CondChip::Text { keyword: keyword.to_string() }));
        }
        json!({ "and": arr })
    } else if keyword.is_empty() {
        query.clone()
    } else {
        json!({ "and": [query.clone(), chip_to_cond(&CondChip::Text { keyword: keyword.to_string() })] })
    }
}

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

    #[test]
    fn is_flat_recognizes_chip_editable_shapes() {
        assert!(is_flat(&json!({"and": []})));
        assert!(is_flat(&json!({"and": [{"cond": {"field": "text", "op": "contains", "value": "x"}}]})));
        assert!(is_flat(&json!({"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Open"}})));
        assert!(is_flat(&Value::Null));
        assert!(is_flat(&json!({})));

        assert!(!is_flat(&json!({"or": [{"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Open"}}]})));
        assert!(!is_flat(&json!({"not": {"cond": {"field": "text", "op": "contains", "value": "x"}}})));
        assert!(!is_flat(&json!({"and": [{"or": [{"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Open"}}]}]})));
    }

    #[test]
    fn with_text_preserves_non_flat_or_tree() {
        let base = json!({"or": [
            {"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Open"}},
            {"cond": {"field": {"label": "Task"}, "op": "eq", "value": "Done"}}
        ]});
        assert!(!is_flat(&base));

        let q = with_text(&base, "检索");
        let arr = q["and"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        // 原 or 树被完整保留在第一个元素里，而不是被压平丢弃。
        assert!(arr[0].get("or").is_some());
        assert_eq!(arr[0]["or"].as_array().unwrap().len(), 2);
        assert_eq!(arr[1]["cond"]["field"], "text");

        // 空关键词：非扁平树原样返回，不改写。
        let unchanged = with_text(&base, "");
        assert_eq!(unchanged, base);
    }
}
