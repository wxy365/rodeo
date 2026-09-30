//! 编辑条目（标题与正文）。`expected_updated_at` 走乐观并发：与库里最新
//! `updated_at` 不一致时返回 ConflictDetected，避免覆盖并发修改。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct UpdateEntryTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct UpdateEntryArgs {
    pub workspace_id: String,
    pub code: String,
    /// 当前条目的 `updated_at`（RFC3339）。与库中最新值不一致会被服务端拒绝。
    pub expected_updated_at: String,
    pub title: String,
    #[serde(default)]
    pub detail: String,
}

#[derive(Serialize)]
pub struct UpdateEntryOutput {
    pub code: String,
    pub title: String,
    pub updated_at: String,
}

pub enum UpdateEntryError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for UpdateEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "update_entry bad args: {m}"),
            Self::Server(m) => write!(f, "update_entry server error: {m}"),
        }
    }
}

impl fmt::Debug for UpdateEntryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for UpdateEntryError {}

impl Tool for UpdateEntryTool {
    const NAME: &'static str = "update_entry";

    type Error = UpdateEntryError;
    type Args = UpdateEntryArgs;
    type Output = UpdateEntryOutput;

    fn description(&self) -> String {
        "编辑条目（标题与正文）。expected_updated_at 用于乐观并发：与库中最新 updated_at 不一致时服务端拒绝（避免覆盖他人改动）。如果之前没有 read，建议先 query_entries 拿最新 updated_at。".to_string()
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
                },
                "expected_updated_at": {
                    "type": "string",
                    "description": "调用方拿到的上一版 updated_at（RFC3339）；乐观并发校验"
                },
                "title": {
                    "type": "string",
                    "description": "新标题；trim 后不能为空"
                },
                "detail": {
                    "type": "string",
                    "description": "新正文（Quill Delta JSON 字符串）。省略/空串 = 正文留空。"
                }
            },
            "required": ["workspace_id", "code", "expected_updated_at", "title"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| UpdateEntryError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(UpdateEntryError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        // 服务端 update 调用取 author_name 做审计 / 通知；本工具不暴露该字段，
        // 用空串占位——通知路径仍走 @姓名 解析，不依赖这个名字。
        let name = self
            .services
            .auth
            .find_by_id(self.auth.account_id)
            .map_err(|e| UpdateEntryError::Server(e.to_string()))?
            .map(|a| a.name)
            .unwrap_or_default();
        let updated = self
            .services
            .entry
            .update(
                self.auth.account_id,
                &name,
                &args.code,
                &args.expected_updated_at,
                &args.title,
                &args.detail,
            )
            .map_err(|e| UpdateEntryError::Server(e.to_string()))?;
        Ok(UpdateEntryOutput {
            code: updated.code,
            title: updated.title,
            updated_at: updated.updated_at.to_rfc3339(),
        })
    }
}