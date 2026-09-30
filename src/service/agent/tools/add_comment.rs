//! 在条目下发表评论。同时会推进该条目的 updated_at（让条目回到列表最前）。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct AddCommentTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct AddCommentArgs {
    pub workspace_id: String,
    pub entry_code: String,
    /// 评论正文（Quill Delta JSON 字符串）。空字符串 / 仅空白会被服务端拒绝；
    /// 只含图片嵌入的也算合法内容。
    pub body: String,
}

#[derive(Serialize)]
pub struct AddCommentOutput {
    pub id: String,
    pub entry_code: String,
    pub body: String,
    pub created_at: String,
}

pub enum AddCommentError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for AddCommentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "add_comment bad args: {m}"),
            Self::Server(m) => write!(f, "add_comment server error: {m}"),
        }
    }
}

impl fmt::Debug for AddCommentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for AddCommentError {}

impl Tool for AddCommentTool {
    const NAME: &'static str = "add_comment";

    type Error = AddCommentError;
    type Args = AddCommentArgs;
    type Output = AddCommentOutput;

    fn description(&self) -> String {
        "在指定条目下发表评论。正文是 Quill Delta JSON 字符串，与 Entry.detail 同构；同时会推进该条目的 updated_at。空内容或仅空白会被服务端拒绝；只含图片嵌入的算合法内容。".to_string()
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
                "body": {
                    "type": "string",
                    "description": "评论正文（Quill Delta JSON 字符串）"
                }
            },
            "required": ["workspace_id", "entry_code", "body"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| AddCommentError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(AddCommentError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        // comment.create 需要 author_name 给 mention 通知 / 审计用；空串也走得通，
        // 这里仍然取一次真名以保证通知预览与用户实际姓名一致。
        let name = self
            .services
            .auth
            .find_by_id(self.auth.account_id)
            .map_err(|e| AddCommentError::Server(e.to_string()))?
            .map(|a| a.name)
            .unwrap_or_default();
        let c = self
            .services
            .comment
            .create(self.auth.account_id, &name, &args.entry_code, &args.body)
            .map_err(|e| AddCommentError::Server(e.to_string()))?;
        Ok(AddCommentOutput {
            id: c.id.to_string(),
            entry_code: c.entry_code,
            body: c.body,
            created_at: c.created_at.to_rfc3339(),
        })
    }
}