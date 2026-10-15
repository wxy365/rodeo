//! 删除条目上的单个标签。`remove_labeling` 是幂等的：原本没有这个标签时静默返回成功。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct RemoveLabelTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct RemoveLabelArgs {
    pub workspace_id: String,
    pub entry_code: String,
    pub label_name: String,
}

#[derive(Serialize)]
pub struct RemoveLabelOutput {
    pub entry_code: String,
    pub label_name: String,
    pub removed: bool,
}

pub enum RemoveLabelError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for RemoveLabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "remove_label bad args: {m}"),
            Self::Server(m) => write!(f, "remove_label server error: {m}"),
        }
    }
}

impl fmt::Debug for RemoveLabelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for RemoveLabelError {}

impl Tool for RemoveLabelTool {
    const NAME: &'static str = "remove_label";

    type Error = RemoveLabelError;
    type Args = RemoveLabelArgs;
    type Output = RemoveLabelOutput;

    fn description(&self) -> String {
        "删除单个条目上的单个标签（按 label_name 精确匹配）。条目上原本没有这个标签时静默返回成功（幂等）。".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；必须与 agent session 的 workspace 一致"
                },
                "entry_code": {
                    "type": "string",
                    "description": "目标条目 16 位 base62 编码"
                },
                "label_name": {
                    "type": "string",
                    "description": "要删除的标签 schema 的 name"
                }
            },
            "required": ["workspace_id", "entry_code", "label_name"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| RemoveLabelError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(RemoveLabelError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        // 服务端 remove_labeling 在目标不存在时静默成功；调用方拿不到区分。
        self.services
            .entry
            .remove_labeling(self.auth.account_id, &args.entry_code, &args.label_name)
            .map_err(|e| RemoveLabelError::Server(e.to_string()))?;
        Ok(RemoveLabelOutput {
            entry_code: args.entry_code,
            label_name: args.label_name,
            removed: true,
        })
    }
}
