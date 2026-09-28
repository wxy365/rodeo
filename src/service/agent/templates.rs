//! Agent 调用的 mutation 模板表 + `ToolSchema` 元数据类型。
//!
//! 与 `tools.rs::build_tools` 共用同一个名字集合：启动期 `validate_templates`
//! 会断言两者一致。任何漂移都在启动期 panic。
//!
//! 模板以 OpenAI 风格字符串给出：参数类型来自 `inputs`，variables 由调用方注入。
//! 不要让 agent 客户端拼字符串 query —— 模板即白名单。

use serde::Serialize;

/// 单个工具的 OpenAI 函数描述：名称、说明、参数 schema（JSON Schema 形态）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// 服务端 mutation 模板表。key = mutation 名（GraphQL 字段名），
/// value = 完整的 mutation 文档字符串（带 `$input` 变量定义）。
pub static TEMPLATES: &[(&str, &str)] = &[
    (
        "createEntry",
        "mutation($input: CreateEntryInput!) { createEntry(input: $input) { id code title } }",
    ),
    (
        "updateEntry",
        "mutation($input: UpdateEntryInput!) { updateEntry(input: $input) { id code title updatedAt } }",
    ),
    (
        "deleteEntry",
        "mutation($id: ID!) { deleteEntry(id: $id) }",
    ),
    (
        "setLabeling",
        "mutation($entryCode: String!, $name: String!, $value: JSON!) { setLabeling(entryCode: $entryCode, name: $name, value: $value) { id } }",
    ),
    (
        "removeLabeling",
        "mutation($entryCode: String!, $name: String!) { removeLabeling(entryCode: $entryCode, name: $name) }",
    ),
    (
        "createComment",
        "mutation($input: CommentInput!) { createComment(input: $input) { id } }",
    ),
    (
        "createLabelSchema",
        "mutation($workspaceId: ID!, $attrs: LabelSchemaAttrsInput!) { createLabelSchema(workspaceId: $workspaceId, attrs: $attrs) { name } }",
    ),
    (
        "updateLabelSchema",
        "mutation($workspaceId: ID!, $name: String!, $attrs: LabelSchemaAttrsInput!) { updateLabelSchema(workspaceId: $workspaceId, name: $name, attrs: $attrs) { name } }",
    ),
    (
        "createView",
        "mutation($workspaceId: ID!, $input: ViewInput!) { createView(workspaceId: $workspaceId, input: $input) { id name } }",
    ),
    (
        "updateView",
        "mutation($id: ID!, $input: ViewInput!) { updateView(id: $id, input: $input) { id name } }",
    ),
    (
        "deleteView",
        "mutation($id: ID!) { deleteView(id: $id) }",
    ),
];

use crate::api::graphql::AppSchema;
use crate::error::AppError;

/// 启动期校验：TEMPLATES 里的每个 mutation 名都在 schema 中——防止 templates 引用
/// 已删/重命名的 mutation。
///
/// 只做单方向（templates ⊆ schema）。反向（schema ⊆ templates）是有意不检查的——
/// agent 工具表是 GraphQL mutation 的子集，新加 mutation 不会自动升级为 tool。
///
/// 解析逻辑与 `tools::build_tools` 对齐：跳过 `"""..."""` description 块，
/// 只挑 `<name>(<args>): <ReturnType>` 的字段行、取首段标识符（字母数字下划线）
/// 作为 mutation 名。
pub fn validate_templates(schema: &AppSchema) -> Result<(), AppError> {
    let sdl = schema.sdl();
    let body = sdl
        .split("type Mutation")
        .nth(1)
        .and_then(|s| s.split('{').nth(1).and_then(|s| s.split('}').next()))
        .ok_or_else(|| AppError::Ai("introspection 解析失败".into()))?;

    let mut schema_names: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut in_desc = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed == "\"\"\"" {
            in_desc = !in_desc;
            continue;
        }
        if in_desc || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // 字段行：`<name>(<args>): <ReturnType>`
        if !trimmed.starts_with('(') && trimmed.contains('(') {
            if let Some(name) = first_identifier(trimmed) {
                schema_names.insert(name.to_string());
            }
        }
    }
    let template_names: Vec<&str> = TEMPLATES.iter().map(|(n, _)| *n).collect();
    let missing: Vec<&&str> = template_names
        .iter()
        .filter(|n| !schema_names.contains(**n))
        .collect();
    if !missing.is_empty() {
        return Err(AppError::Ai(format!(
            "agent templates 引用了 schema 中不存在的 mutation: {:?}",
            missing
        )));
    }
    Ok(())
}

/// 取 trimmed 行首的 identifier（字母数字下划线），跳过任何前导字符。
/// `createEntry(input: CreateEntryInput!): Entry!` → `createEntry`。
/// 返回 None 如果行首没有合法 identifier。`tools.rs::build_tools` 复用同款，
/// 解析同一份 SDL——单点维护避免两个模块各自实现同一 helper 又漂移。
pub(super) fn first_identifier(s: &str) -> Option<&str> {
    let start = s.find(|c: char| c.is_alphanumeric() || c == '_')?;
    let rest = &s[start..];
    let end = rest
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(&rest[..end])
    }
}

// brief 注释里提到 `pub type Schema = AppSchema;` 的别名形态；为避免与
// `async_graphql::Schema` 重名，这里改用 `SchemaAlias`。调用方一律使用 `AppSchema`。
#[allow(dead_code)]
pub type SchemaAlias = AppSchema;

