//! 删除条目下的指定评论。本工具以「作者本人」身份调用（can_moderate=false）：
//! 想删他人评论必须通过 GraphQL resolver，那条路径上才能取到 Maintainer+ 角色。
//! 删除会回收评论正文里被引用的内联图片附件。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct RemoveCommentTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct RemoveCommentArgs {
    pub workspace_id: String,
    pub entry_code: String,
    pub comment_id: String,
}

#[derive(Serialize)]
pub struct RemoveCommentOutput {
    pub entry_code: String,
    pub comment_id: String,
    pub removed: bool,
}

pub enum RemoveCommentError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for RemoveCommentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "remove_comment bad args: {m}"),
            Self::Server(m) => write!(f, "remove_comment server error: {m}"),
        }
    }
}

impl fmt::Debug for RemoveCommentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for RemoveCommentError {}

impl Tool for RemoveCommentTool {
    const NAME: &'static str = "remove_comment";

    type Error = RemoveCommentError;
    type Args = RemoveCommentArgs;
    type Output = RemoveCommentOutput;

    fn description(&self) -> String {
        "删除指定条目下的某条评论（按 comment_id 精确匹配）。本工具以作者本人身份调用（can_moderate=false）：能删自己的评论；删他人评论需走 GraphQL 的 deleteComment 接口。删除会回收该评论里被引用的内联图片附件。".to_string()
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
                    "description": "评论所属条目 16 位 base62 编码"
                },
                "comment_id": {
                    "type": "string",
                    "description": "评论 ULID 字符串"
                }
            },
            "required": ["workspace_id", "entry_code", "comment_id"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| RemoveCommentError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(RemoveCommentError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        let id = Ulid::from_string(&args.comment_id)
            .map_err(|e| RemoveCommentError::BadArgs(format!("comment_id: {e}")))?;
        // agent 工具无角色上下文，一律以 can_moderate=false 调；作者本人仍可删自己的。
        self.services
            .comment
            .delete(self.auth.account_id, &args.entry_code, id, false)
            .map_err(|e| RemoveCommentError::Server(e.to_string()))?;
        Ok(RemoveCommentOutput {
            entry_code: args.entry_code,
            comment_id: args.comment_id,
            removed: true,
        })
    }
}
