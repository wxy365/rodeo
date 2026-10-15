//! `attachment_link` tool 骨架。Args/Outputs/Error 形态在后续 task 中按 spec §5 填充。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct AttachmentLinkTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct AttachmentLinkArgs {
    // TODO: 由后续 task 按 spec §5 字段填充
}

#[derive(Serialize)]
pub struct AttachmentLinkOutput {
    // TODO: 由后续 task 按 spec §5 字段填充
}

pub enum AttachmentLinkError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for AttachmentLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "attachment_link bad args: {m}"),
            Self::Server(m) => write!(f, "attachment_link server error: {m}"),
        }
    }
}

impl fmt::Debug for AttachmentLinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for AttachmentLinkError {}

impl Tool for AttachmentLinkTool {
    const NAME: &'static str = "attachment_link";

    type Error = AttachmentLinkError;
    type Args = AttachmentLinkArgs;
    type Output = AttachmentLinkOutput;

    fn description(&self) -> String {
        // TODO: 由后续 task 按 spec §5 替换为正式 description
        format!(
            "attachment_link (not yet implemented — hallucinated calls will return a tool error)"
        )
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
        Err(Self::Error::Server(
            "tool not yet implemented — will be filled by follow-up task".into(),
        ))
    }
}
