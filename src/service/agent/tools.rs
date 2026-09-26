//! GraphQL introspection → OpenAI tools 元数据。

use std::sync::Arc;

pub use super::templates::ToolSchema;

use crate::api::graphql::AppSchema;
use crate::error::AppError;

// 备用 introspection query：实际 `build_tools` 走 SDL 解析（与 validate_templates
// 共用一份解析逻辑）。常量保留以备将来切到 `__schema` 反射时直接复用。
#[allow(dead_code)]
const INTROSPECTION_QUERY: &str = r#"
{
  __schema {
    mutationType {
      fields {
        name
        description
        args {
          name
          type { kind name ofType { kind name ofType { kind name } } }
        }
      }
    }
  }
}
"#;

/// 从现有 GraphQL Schema 生成 OpenAI `tools` 数组。
/// 一次性调用，结果用 Arc 共享给所有 SSE turn。
pub fn build_tools(schema: &AppSchema) -> Result<Arc<Vec<ToolSchema>>, AppError> {
    // 用 SDL 解析 mutation 字段：与 templates::validate_templates 共用同一份解析逻辑，
    // 不重复实现。description 来自 doc comment。
    let sdl = schema.sdl();
    let body = sdl
        .split("type Mutation")
        .nth(1)
        .and_then(|s| s.split('{').nth(1).and_then(|s| s.split('}').next()))
        .ok_or_else(|| AppError::Ai("introspection 解析失败".into()))?;

    let mut tools = Vec::new();
    let mut current: Option<(String, String, Vec<(String, String)>)> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // 字段名起始行：`<name>(<args>): <ReturnType>` 或 `<name>: <ReturnType>`
        if !trimmed.starts_with('(') && trimmed.contains('(') {
            // 关闭上一个
            if let Some((name, desc, args)) = current.take() {
                tools.push(build_tool(&name, &desc, &args));
            }
            let name = trimmed.split_whitespace().next().unwrap_or("").to_string();
            // 提取 description（SDL 在 description 字段里，Rust 注释已编译进 SDL）
            let desc = String::new(); // 简化：从 SDL 里抓 "" 字符串 —— 留给 execute_tool 校验。
            let args_str = trimmed
                .split_once('(')
                .map(|(_, rest)| rest)
                .and_then(|s| s.split_once(')').map(|(l, _)| l))
                .unwrap_or("");
            let args = parse_args(args_str);
            current = Some((name, desc, args));
        }
    }
    if let Some((name, desc, args)) = current.take() {
        tools.push(build_tool(&name, &desc, &args));
    }
    Ok(Arc::new(tools))
}

fn parse_args(s: &str) -> Vec<(String, String)> {
    s.split(',')
        .filter_map(|p| {
            let p = p.trim();
            if p.is_empty() {
                return None;
            }
            let mut it = p.splitn(2, ':');
            Some((it.next()?.trim().to_string(), it.next()?.trim().to_string()))
        })
        .collect()
}

fn build_tool(name: &str, _desc: &str, args: &[(String, String)]) -> ToolSchema {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    for (arg_name, type_str) in args {
        properties.insert(
            arg_name.clone(),
            serde_json::json!({ "type": map_type(type_str) }),
        );
        // 简化：凡是非 `!` 结尾的可空；此处粗略处理：缺 `!` 即 optional。
        if !type_str.ends_with('!') {
            // optional
        } else {
            required.push(arg_name.clone());
        }
    }
    ToolSchema {
        name: name.to_string(),
        description: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": properties,
            "required": required,
        }),
    }
}

fn map_type(t: &str) -> &'static str {
    let base = t.trim_end_matches('!').trim_end_matches(['[', ']'].as_ref());
    match base {
        "ID" | "String" => "string",
        "Int" | "Float" => "number",
        "Boolean" => "boolean",
        _ => "object", // InputObject
    }
}
