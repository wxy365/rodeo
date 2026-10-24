//! 列出工作空间内的全部标签定义（schema）。调用方拿到 schema 后才能在 set_labels
//! 里给条目写对应类型的取值。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
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
    pub schemas: Vec<LabelSchemaBrief>,
}

/// 标签定义的精简形态——只保留模型写表达式 / 打标签时真正要看的字段。
///
/// 直接吐 `LabelSchema` 的完整 JSON（含 workspace_id / color / value_colors /
/// links / default_value）有两个问题：token 多，且模型要在一堆元数据里找
/// 「这个标签叫什么、值域是什么」。这里裁成 6 个字段，`value_type` 走
/// `LabelValueType::as_str()` 的小写形态（enum / null / account / …）——
/// 比 serde 默认的 `Enum` / `Null` 更好认。
#[derive(Serialize)]
pub struct LabelSchemaBrief {
    /// 标签 key——表达式 / set_labels 里用的就是它。
    pub name: String,
    /// 展示名（中文标题），给人看的。
    pub title: String,
    /// 值类型：null | boolean | integer | float | string | enum | date | time |
    /// datetime | currency | email | account。
    pub value_type: String,
    /// 枚举取值（仅 enum 类型非空）——表达式里比较的值必须来自这里。
    pub enum_values: Vec<String>,
    /// 是否多值（多值标签才能用 has / is）。
    pub multi: bool,
    /// 时间型标签的格式（date / time / datetime 才有）。
    pub format: Option<String>,
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
        concat!(
            "列出当前工作空间内的全部标签定义（schema）。每个 schema 包含 name（标签 key——",
            "写表达式 / set_labels 时都用这个）、title（中文名）、value_type（取值类型）、",
            "enum_values（枚举可选值）、multi（是否多值）。",
            "写 query_entries 的表达式或调用 set_labels 前先查一次：标签名必须与 name 完全一致，",
            "比较的值必须来自 enum_values。"
        )
        .to_string()
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
            .map(|s| LabelSchemaBrief {
                name: s.name,
                title: s.title,
                value_type: s.value_type.as_str().to_string(),
                enum_values: s.enum_values,
                multi: s.multi,
                format: s.format,
            })
            .collect();
        Ok(LabelSchemasOutput { schemas })
    }
}
