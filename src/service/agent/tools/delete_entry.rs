//! 软删除条目：写入 `deleted_at`，数据保留，可由 archive/unarchive 处理归档但
//! 不可恢复删除状态。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct DeleteEntryTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct DeleteEntryArgs {
    pub workspace_id: String,
    pub code: String,
}

#[derive(Serialize)]
pub struct DeleteEntryOutput {
    pub code: String,
    pub deleted: bool,
}

pub enum DeleteEntryError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for DeleteEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "delete_entry bad args: {m}"),
            Self::Server(m) => write!(f, "delete_entry server error: {m}"),
        }
    }
}

impl fmt::Debug for DeleteEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for DeleteEntryError {}

impl Tool for DeleteEntryTool {
    const NAME: &'static str = "delete_entry";

    type Error = DeleteEntryError;
    type Args = DeleteEntryArgs;
    type Output = DeleteEntryOutput;

    fn description(&self) -> String {
        "软删除条目（写 deleted_at）。数据保留，移出基础视图与全文检索；归档（archive）后可用 unarchive 复活，但本删除不可撤销。".to_string()
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
            .map_err(|e| DeleteEntryError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(DeleteEntryError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        let actor_name = self
            .services
            .auth
            .account_name(self.auth.account_id)
            .map_err(|e| DeleteEntryError::Server(e.to_string()))?
            .unwrap_or_default();
        self.services
            .entry
            .soft_delete(self.auth.account_id, &actor_name, &args.code)
            .map_err(|e| DeleteEntryError::Server(e.to_string()))?;
        Ok(DeleteEntryOutput {
            code: args.code,
            deleted: true,
        })
    }
}