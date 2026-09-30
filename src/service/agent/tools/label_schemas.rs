//! 列出工作空间内的全部标签定义（schema）。调用方拿到 schema 后才能在 set_labels
//! 里给条目写对应类型的取值。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct LabelSchemasTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct LabelSchemasArgs {
    pub workspace_id: String,
}

#[derive(Serialize)]
pub struct LabelSchemasOutput {
    pub schemas: Vec<Value>,
}

pub enum LabelSchemasError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for LabelSchemasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "label_schemas bad args: {m}"),
            Self::Server(m) => write!(f, "label_schemas server error: {m}"),
        }
    }
}

impl fmt::Debug for LabelSchemasError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for LabelSchemasError {}

impl Tool for LabelSchemasTool {
    const NAME: &'static str = "label_schemas";

    type Error = LabelSchemasError;
    type Args = LabelSchemasArgs;
    type Output = LabelSchemasOutput;

    fn description(&self) -> String {
        "列出当前工作空间内的全部标签定义（schema）。每个 schema 包含 name（写标签时用这个 key）、title、value_type（决定 set_labels 里 value 的 JSON 形态）、enum_values / color / multi 等元数据。调用 set_labels 前先查一次，避免给 schema 不存在的标签或类型不匹配的值。".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；必须与 agent session 的 workspace 一致"
                }
            },
            "required": ["workspace_id"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| LabelSchemasError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(LabelSchemasError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        let rows = self
            .services
            .label
            .list_schemas(ws)
            .map_err(|e| LabelSchemasError::Server(e.to_string()))?;
        let schemas = rows
            .into_iter()
            .map(|s| serde_json::to_value(&s).unwrap_or(Value::Null))
            .collect();
        Ok(LabelSchemasOutput { schemas })
    }
}