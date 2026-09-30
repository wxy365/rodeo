//! `remove_comment` tool 骨架。Args/Outputs/Error 形态在后续 task 中按 spec §5 填充。

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
    // TODO: 由后续 task 按 spec §5 字段填充
}

#[derive(Serialize)]
pub struct RemoveCommentOutput {
    // TODO: 由后续 task 按 spec §5 字段填充
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
        // TODO: 由后续 task 按 spec §5 替换为正式 description
        format!("remove_comment (TODO: 待后续 task 填实)")
    }

    fn parameters(&self) -> serde_json::Value {
        // TODO: 由后续 task 按 spec §5 替换为正式 JSON Schema
        serde_json::json!({"type": "object", "properties": {}, "required": []})
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        unimplemented!("remove_comment 将在后续 task 填实")
    }
}