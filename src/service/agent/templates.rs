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
        "createLabeling",
        "mutation($input: LabelingInput!) { createLabeling(input: $input) { id } }",
    ),
    (
        "updateLabeling",
        "mutation($input: LabelingInput!) { updateLabeling(input: $input) { id } }",
    ),
    (
        "deleteLabeling",
        "mutation($entryCode: String!, $name: String!) { deleteLabeling(entryCode: $entryCode, name: $name) }",
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

/// 启动期校验：schema 里每个 mutation 都在 TEMPLATES；TEMPLATES 里每个名字都在 schema。
pub fn validate_templates(schema: &AppSchema) -> Result<(), AppError> {
    // 用 introspection 取 schema 里的所有 mutation 名
    let sdl = schema.sdl();
    // 简易正则：抓 `type Mutation { ... }` 块里的字段名
    // 这里用 SDL 而非 __schema 是因为 build_tools 也要做同样的解析，避免两套解析。
    let mutation_block = sdl
        .split("type Mutation")
        .nth(1)
        .and_then(|s| s.split('{').nth(1).and_then(|s| s.split('}').next()))
        .ok_or_else(|| AppError::Ai("introspection 解析失败".into()))?;
    let schema_names: std::collections::HashSet<&str> = mutation_block
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .collect();
    let template_names: std::collections::HashSet<&str> =
        TEMPLATES.iter().map(|(n, _)| *n).collect();

    let missing_in_template: Vec<&&str> = schema_names.difference(&template_names).collect();
    let missing_in_schema: Vec<&&str> = template_names.difference(&schema_names).collect();
    if !missing_in_template.is_empty() || !missing_in_schema.is_empty() {
        let mut msg = String::from("agent templates 与 GraphQL schema 不一致: ");
        if !missing_in_template.is_empty() {
            msg.push_str(&format!(
                "schema 有但 templates 缺: {:?}; ",
                missing_in_template
            ));
        }
        if !missing_in_schema.is_empty() {
            msg.push_str(&format!(
                "templates 有但 schema 缺: {:?}",
                missing_in_schema
            ));
        }
        return Err(AppError::Ai(msg));
    }
    Ok(())
}

// brief 注释里提到 `pub type Schema = AppSchema;` 的别名形态；为避免与
// `async_graphql::Schema` 重名，这里改用 `SchemaAlias`。调用方一律使用 `AppSchema`。
#[allow(dead_code)]
pub type SchemaAlias = AppSchema;

