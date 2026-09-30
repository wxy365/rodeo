//! 在当前工作空间里新建一个条目（仅设置标题，正文留空）。
//! 与 GraphQL `Mutation::create_entry` 对齐：写库、自动生成 code、推进 updated_at。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct CreateEntryTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct CreateEntryArgs {
    pub workspace_id: String,
    pub title: String,
}

#[derive(Serialize)]
pub struct CreateEntryOutput {
    pub code: String,
    pub title: String,
    pub updated_at: String,
}

pub enum CreateEntryError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for CreateEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "create_entry bad args: {m}"),
            Self::Server(m) => write!(f, "create_entry server error: {m}"),
        }
    }
}

impl fmt::Debug for CreateEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for CreateEntryError {}

impl Tool for CreateEntryTool {
    const NAME: &'static str = "create_entry";

    type Error = CreateEntryError;
    type Args = CreateEntryArgs;
    type Output = CreateEntryOutput;

    fn description(&self) -> String {
        "在当前工作空间里新建一个条目。仅写标题，正文留空；返回新条目的 code（16位 base62 全局唯一编码，后续所有接口都以此为标识）、标题与更新时间。".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；必须与 agent session 的 workspace 一致"
                },
                "title": {
                    "type": "string",
                    "description": "条目标题。trim 后不能为空；最长受 entry.title 字段长度约束。"
                }
            },
            "required": ["workspace_id", "title"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| CreateEntryError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(CreateEntryError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        let entry = self
            .services
            .entry
            .create(self.auth.account_id, ws, &args.title)
            .map_err(|e| CreateEntryError::Server(e.to_string()))?;
        Ok(CreateEntryOutput {
            code: entry.code,
            title: entry.title,
            updated_at: entry.updated_at.to_rfc3339(),
        })
    }
}