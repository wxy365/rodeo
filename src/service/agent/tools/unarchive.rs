//! 取消条目归档：删 `ENTRIES_ARCHIVED` 列族里的标记、重新进入基础视图与全文检索。
//! 幂等：未归档时直接返回成功。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct UnarchiveTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct UnarchiveArgs {
    pub workspace_id: String,
    pub code: String,
}

#[derive(Serialize)]
pub struct UnarchiveOutput {
    pub code: String,
    pub unarchived: bool,
}

pub enum UnarchiveError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for UnarchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "unarchive bad args: {m}"),
            Self::Server(m) => write!(f, "unarchive server error: {m}"),
        }
    }
}

impl fmt::Debug for UnarchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for UnarchiveError {}

impl Tool for UnarchiveTool {
    const NAME: &'static str = "unarchive";

    type Error = UnarchiveError;
    type Args = UnarchiveArgs;
    type Output = UnarchiveOutput;

    fn description(&self) -> String {
        "取消条目归档（删 ENTRIES_ARCHIVED 列族里的标记）。条目重新进入基础视图与全文检索；未归档时静默返回成功（幂等）。".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；必须与 agent session 的 workspace 一致"
                },
                "code": {
                    "type": "string",
                    "description": "条目 16 位 base62 编码"
                }
            },
            "required": ["workspace_id", "code"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| UnarchiveError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(UnarchiveError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        self.services
            .entry
            .unarchive(self.auth.account_id, &args.code)
            .map_err(|e| UnarchiveError::Server(e.to_string()))?;
        Ok(UnarchiveOutput {
            code: args.code,
            unarchived: true,
        })
    }
}