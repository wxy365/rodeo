//! 归档条目：写入 `ENTRIES_ARCHIVED` 列族、移出基础视图与全文检索。数据保留，
//! 可由 `unarchive` 工具取消归档（幂等：已归档时直接返回成功）。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct ArchiveTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct ArchiveArgs {
    pub workspace_id: String,
    pub code: String,
}

#[derive(Serialize)]
pub struct ArchiveOutput {
    pub code: String,
    pub archived: bool,
}

pub enum ArchiveError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "archive bad args: {m}"),
            Self::Server(m) => write!(f, "archive server error: {m}"),
        }
    }
}

impl fmt::Debug for ArchiveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ArchiveError {}

impl Tool for ArchiveTool {
    const NAME: &'static str = "archive";

    type Error = ArchiveError;
    type Args = ArchiveArgs;
    type Output = ArchiveOutput;

    fn description(&self) -> String {
        "归档条目（写入 ENTRIES_ARCHIVED 列族）。条目与打标全部保留，移出基础视图与全文检索；可由 unarchive 取消归档（幂等）。已软删除的条目不可归档（返回 NotFound）。".to_string()
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
            .map_err(|e| ArchiveError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(ArchiveError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        self.services
            .entry
            .archive(self.auth.account_id, &args.code)
            .map_err(|e| ArchiveError::Server(e.to_string()))?;
        Ok(ArchiveOutput {
            code: args.code,
            archived: true,
        })
    }
}
