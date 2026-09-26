//! 工具模板表：把 GraphQL mutation 反射成 OpenAI tools 协议需要的元数据。
//!
//! 当前 task 仅放置 `ToolSchema` 结构体本身：`complete_stream` 必须能引用它。
//! 真正的反射逻辑（`build_tools` / `validate_templates`）在后续 task 实现。

use serde::Serialize;

/// 单个工具的 OpenAI 函数描述：名称、说明、参数 schema（JSON Schema 形态）。
#[derive(Debug, Clone, Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}
