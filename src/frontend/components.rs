use leptos::prelude::*;
use serde_json::Value;

/// 圆形头像，取首字符展示。
#[component]
pub fn Avatar(#[prop(into)] text: String, #[prop(optional)] large: bool) -> impl IntoView {
    let cls = if large { "av lg" } else { "av" };
    let ch = text.chars().next().unwrap_or('?').to_string();
    view! { <span class=cls>{ch}</span> }
}

/// 标签值 → 展示字符串。
pub fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        // 多值标签（如多选 Enum）按「逗号 + 空格」拼接各元素，而不是吐 JSON。
        Value::Array(a) => a.iter().map(value_to_string).collect::<Vec<_>>().join(", "),
        _ => v.to_string(),
    }
}

/// date / time / datetime 三类中，`format` 缺省或恰为默认 Go 布局时，原生 HTML
/// 控件就能表达该值；只有自定义布局才需要退回文本输入。
pub fn is_native_time_layout(vt: &str, format: Option<&str>) -> bool {
    let layout = format.unwrap_or(match vt {
        "date" => crate::golayout::DATE_LAYOUT,
        "time" => crate::golayout::TIME_LAYOUT,
        _ => crate::golayout::DATETIME_LAYOUT,
    });
    layout == crate::golayout::DATE_LAYOUT
        || layout == crate::golayout::TIME_LAYOUT
        || layout == crate::golayout::DATETIME_LAYOUT
}

/// 原生控件的值 → 存储串（Go 默认布局：时间类补齐秒、datetime 去 `T` 换空格）。
/// 空串返回 `None`，由调用方决定是「不写」还是「移除」。
pub fn from_native(vt: &str, s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    Some(match vt {
        "datetime" => format!("{}:00", s.replacen('T', " ", 1)).get(..19)?.to_string(),
        "time" => format!("{s}:00").get(..8)?.to_string(),
        _ => s.to_string(),
    })
}

/// 内置枚举值 → 友好展示（InProgress → In progress 等）。
pub fn display_enum_value(v: &str) -> String {
    match v {
        "InProgress" => "In progress".to_string(),
        "WontFix" => "Wont fix".to_string(),
        other => other.to_string(),
    }
}

/// Task/Bug 等状态值 → chip 颜色类。
pub fn status_chip(value: &str) -> &'static str {
    match value {
        "InProgress" => "c-doing",
        "Done" | "Fixed" => "c-done",
        "WontFix" | "Archived" => "c-wont",
        "Open" => "c-open",
        _ => "c-open",
    }
}

/// 优先级值 → chip 颜色类。
pub fn priority_chip(value: &str) -> &'static str {
    match value {
        "P0" | "P1" => "c-p0",
        "P2" => "c-p2",
        "P3" => "c-p3",
        _ => "c-open",
    }
}

/// 依据 label_name 选择状态/优先级 chip 类。
pub fn label_chip_class(label_name: &str, value: &str) -> &'static str {
    if label_name.eq_ignore_ascii_case("priority") || label_name.eq_ignore_ascii_case("优先级") {
        priority_chip(value)
    } else {
        status_chip(value)
    }
}

/// 角色 → chip 颜色类。
pub fn role_chip_class(role: &str) -> &'static str {
    match role.to_ascii_lowercase().as_str() {
        "owner" => "c-done",
        "maintainer" => "c-doing",
        "worker" => "c-open",
        "reader" => "dim",
        _ => "dim",
    }
}

/// 角色显示名。
pub fn role_label(role: &str) -> String {
    match role.to_ascii_lowercase().as_str() {
        "owner" => "Owner".to_string(),
        "maintainer" => "Maintainer".to_string(),
        "worker" => "Worker".to_string(),
        "reader" => "Reader".to_string(),
        other => other.to_string(),
    }
}

/// 未登录（浏览器端无 token，或非浏览器环境一律视为未登录）。
pub fn logged_out() -> bool {
    if cfg!(target_arch = "wasm32") {
        crate::frontend::graphql_client::get_token().is_none()
    } else {
        true
    }
}

/// RFC3339 → 本地时区的 (年, 月, 日, 时, 分, 秒)；解析失败返回 `None`。
///
/// 服务端的时间戳一律是 UTC（`Utc::now().to_rfc3339()`），直接切字符串会把 UTC 当本地时间展示。
/// 本地时区偏移只有浏览器知道，所以只有 wasm 端能换算；SSR 阶段这些时间戳还没取到，
/// 走下面各自的切片回退。
#[cfg(target_arch = "wasm32")]
fn local_parts(rfc: &str) -> Option<(u32, u32, u32, u32, u32, u32)> {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_str(rfc.trim()));
    if js_sys::Date::get_time(&d).is_nan() {
        return None;
    }
    Some((
        js_sys::Date::get_full_year(&d),
        js_sys::Date::get_month(&d) + 1,
        js_sys::Date::get_date(&d),
        js_sys::Date::get_hours(&d),
        js_sys::Date::get_minutes(&d),
        js_sys::Date::get_seconds(&d),
    ))
}

#[cfg(not(target_arch = "wasm32"))]
fn local_parts(_rfc: &str) -> Option<(u32, u32, u32, u32, u32, u32)> {
    None
}

/// RFC3339 时间 → 本地时区 "YYYY-MM-DD HH:MM"。
pub fn short_time(at: &str) -> String {
    if let Some((y, mo, d, h, mi, _)) = local_parts(at) {
        return format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}");
    }
    let s = at.replace('T', " ");
    s.chars().take(16).collect()
}

/// RFC3339 → 本地时区的 `2006-01-02 15:04:05`。
pub fn fmt_datetime(rfc: &str) -> String {
    if let Some((y, mo, d, h, mi, s)) = local_parts(rfc) {
        return format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}");
    }
    let s = rfc.trim();
    match (s.get(..10), s.get(11..19)) {
        (Some(d), Some(t)) => format!("{d} {t}"),
        (Some(d), None) => d.to_string(),
        _ => s.to_string(),
    }
}

/// 审计 action → 中文标签。
pub fn action_label(action: &str) -> &'static str {
    match action {
        "EntryCreated" => "创建",
        "EntryUpdated" => "更新详情",
        "EntryDeleted" => "删除",
        "LabelingSet" => "设置标签",
        "LabelingRemoved" => "移除标签",
        "LabelSchemaCreated" => "创建标签定义",
        "LabelSchemaUpdated" => "更新标签定义",
        "LabelSchemaDeleted" => "删除标签定义",
        "ViewCreated" => "创建视图",
        "ViewUpdated" => "更新视图",
        "ViewDeleted" => "删除视图",
        "EntryArchived" => "归档",
        "EntryUnarchived" => "取消归档",
        "MemberInvited" => "邀请成员",
        "MemberJoined" => "接受邀请",
        "InviteDeclined" => "拒绝邀请",
        "InviteRevoked" => "撤销邀请",
        "RoleChanged" => "变更成员角色",
        "MemberRemoved" => "移除成员",
        "WorkspaceUpdated" => "更新工作空间",
        "WorkspaceDeleted" => "删除工作空间",
        "WorkspaceRestored" => "恢复工作空间",
        "WorkspaceCreated" => "创建工作空间",
        "RuleCreated" => "创建规则",
        "RuleUpdated" => "更新规则",
        "RuleDeleted" => "删除规则",
        "RuleApplied" => "规则触发",
        _ => "变更",
    }
}

/// 审计 before/after 快照 → 一句话「具体改了什么」。
///
/// 两个快照都是 JSON 对象字符串（`serde_json::to_string` 出来的资源快照）。创建 / 删除
/// 各只有一边，写成「创建「标题」」；两边都在时逐字段对比，只报实际变化的字段。
pub fn audit_change(before: Option<&str>, after: Option<&str>) -> String {
    let parse = |s: Option<&str>| -> Option<Value> {
        let s = s?;
        serde_json::from_str::<Value>(s).ok()
    };
    let b = parse(before);
    let a = parse(after);
    let bobj = b.as_ref().and_then(Value::as_object);
    let aobj = a.as_ref().and_then(Value::as_object);
    match (bobj, aobj) {
        (None, None) => "—".to_string(),
        (None, Some(ao)) => format!("创建「{}」", subject(ao)),
        (Some(bo), None) => format!("删除「{}」", subject(bo)),
        (Some(bo), Some(ao)) => diff(bo, ao),
    }
}

/// 资源快照 → 一个能指代它的短标签。
fn subject(o: &serde_json::Map<String, Value>) -> String {
    for k in ["title", "name", "label_name", "value", "email", "account_id"] {
        if let Some(v) = o.get(k) {
            let s = show(v);
            if s != "—" {
                return s;
            }
        }
    }
    "（无标题）".to_string()
}

fn diff(b: &serde_json::Map<String, Value>, a: &serde_json::Map<String, Value>) -> String {
    let null = Value::Null;
    // 先按 after 的字段顺序，再补上只存在于 before 的字段（被移除的字段）。
    let mut keys: Vec<&String> = a.keys().collect();
    for k in b.keys() {
        if !a.contains_key(k) {
            keys.push(k);
        }
    }
    let mut parts = Vec::new();
    for k in keys {
        let bv = b.get(k).unwrap_or(&null);
        let av = a.get(k).unwrap_or(&null);
        if bv == av {
            continue;
        }
        parts.push(format!("{}: {} → {}", field_label(k), show(bv), show(av)));
    }
    if parts.is_empty() {
        "无字段变化".to_string()
    } else {
        parts.join("；")
    }
}

/// 快照字段名 → 中文；未在映射表里的字段直接沿用原名。
fn field_label(k: &str) -> String {
    match k {
        "title" => "标题",
        "detail" => "详情",
        "name" => "名称",
        "slug" => "地址",
        "description" => "描述",
        "deleted_at" => "删除时间",
        "value" => "值",
        "label_name" => "标签",
        "value_type" => "值类型",
        "enum_values" => "可选值",
        "color" => "颜色",
        "value_colors" => "值颜色",
        "query" => "条件",
        "columns" => "列",
        "sort" => "排序",
        "is_shared" => "共享",
        "title_colors" => "标题颜色规则",
        "role" => "角色",
        "account_id" => "成员",
        other => other,
    }
    .to_string()
}

/// 展示一个 JSON 值：字符串去掉引号，其余保持紧凑 JSON；过长则截断。
fn show(v: &Value) -> String {
    match v {
        Value::Null => "—".to_string(),
        Value::String(s) => clip(s),
        other => clip(&other.to_string()),
    }
}

fn clip(s: &str) -> String {
    let one_line = s.replace('\n', " ");
    if one_line.chars().count() <= 40 {
        one_line
    } else {
        let mut t: String = one_line.chars().take(40).collect();
        t.push('…');
        t
    }
}

/// 值类型显示名。
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_change_reports_only_changed_fields() {
        let b = r#"{"title":"旧","detail":"同","value":"Open"}"#;
        let a = r#"{"title":"新","detail":"同","value":"Open"}"#;
        assert_eq!(audit_change(Some(b), Some(a)), "标题: 旧 → 新");
    }

    #[test]
    fn audit_change_summarizes_create_and_delete() {
        assert_eq!(
            audit_change(None, Some(r#"{"title":"条目0"}"#)),
            "创建「条目0」"
        );
        assert_eq!(
            audit_change(Some(r#"{"name":"看板"}"#), None),
            "删除「看板」"
        );
        assert_eq!(audit_change(None, None), "—");
    }

    #[test]
    fn audit_change_marks_added_and_removed_fields() {
        let b = r#"{"a":1}"#;
        let a = r#"{"b":2}"#;
        assert_eq!(audit_change(Some(b), Some(a)), "b: — → 2；a: 1 → —");
    }
}
