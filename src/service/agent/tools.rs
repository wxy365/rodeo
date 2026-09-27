//! GraphQL introspection → OpenAI tools 元数据。

use std::sync::Arc;

pub use super::templates::ToolSchema;

use crate::api::graphql::AppSchema;
use crate::error::AppError;
use tracing::info;

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
///
/// description 来自 mutation 字段前的 `///` doc comment：async-graphql 把
/// 注释编译进 SDL 时默认用 `"""..."""` 块包裹（见 export_sdl.rs 的
/// `write_description`：默认 `prefer_single_line_descriptions = false`）。
/// 这里把字段前的字符串字面量（多行）累积成 description 字符串。
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
    // 当前正在累积的 description 行（多行 `"\"\"\"...\"\"\""` 块）。遇到字段行时
    // 作为该字段的 description 使用，然后清空。
    let mut desc_lines: Vec<String> = Vec::new();
    // `false`=没在 description 里, `true`=正在 `"""..."""` 块内累积
    let mut in_desc = false;
    let mut current: Option<(String, String, Vec<(String, String)>)> = None;
    for line in body.lines() {
        let trimmed = line.trim();

        // 处理 `"""..."""` 块：开 / 收两个三引号都各占一行。
        if trimmed == "\"\"\"" {
            if in_desc {
                // 收：description 累积结束；等下一个字段行消费它。
                in_desc = false;
            } else {
                // 开：开始累积下一个字段的 description。
                desc_lines.clear();
                in_desc = true;
            }
            continue;
        }

        if in_desc {
            // 块内空白行也保留，让多行 description 有正确的换行。
            // 但末尾的纯空白行可以丢，避免产出 "\n\n..." 尾巴。
            if trimmed.is_empty() && desc_lines.last().map_or(true, |l| l.is_empty()) {
                continue;
            }
            desc_lines.push(trimmed.to_string());
            continue;
        }

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
            // 字段前的 description：累积的多行文本，去掉末尾空行再 join。
            let desc = join_desc_lines(&desc_lines);
            desc_lines.clear();
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
    // 生产期 sanity 检查：每个 tool 都应该有非空 description，否则 LLM 只能靠
    // name 猜 mutation 语义。空 description 计数会在日志里暴露出来，便于在
    // 启动期 / 重新加载 schema 时肉眼 / 监控告警抓出回归。
    let empty_desc: Vec<&str> = tools
        .iter()
        .filter(|t| t.description.is_empty())
        .map(|t| t.name.as_str())
        .collect();
    if empty_desc.is_empty() {
        info!(
            count = tools.len(),
            "build_tools: 全部 tool 的 description 已填充",
        );
        for t in tools.iter() {
            info!(name = %t.name, desc = %t.description, "tool description");
        }
    } else {
        info!(
            count = tools.len(),
            empty = empty_desc.len(),
            ?empty_desc,
            "build_tools: 部分 tool description 为空",
        );
    }
    Ok(Arc::new(tools))
}

/// 把 `"""..."""` 块内累积的多行 description 拼成单字符串：去掉尾部空行，
/// 中间用 `\n` 连接。空 description 块返回空字符串（不是 `" "` 或 `"\n"`）。
fn join_desc_lines(lines: &[String]) -> String {
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[..end].join("\n")
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

fn build_tool(name: &str, desc: &str, args: &[(String, String)]) -> ToolSchema {
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
        description: desc.to_string(),
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
