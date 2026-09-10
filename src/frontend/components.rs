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
        Value::Array(a) => serde_json::to_string(a).unwrap_or_default(),
        _ => v.to_string(),
    }
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

/// RFC3339 时间 → 简短 "YYYY-MM-DD HH:MM"。
pub fn short_time(at: &str) -> String {
    let s = at.replace('T', " ");
    s.chars().take(16).collect()
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
        _ => "变更",
    }
}

/// 值类型显示名。
pub fn value_type_label(vt: &str) -> String {
    match vt {
        "null" => "Null".to_string(),
        "boolean" => "Boolean".to_string(),
        "integer" => "Integer".to_string(),
        "float" => "Float".to_string(),
        "string" => "String".to_string(),
        "enum" => "Enum".to_string(),
        other => other.to_string(),
    }
}
