//! `view_get` tool 骨架。Args/Outputs/Error 形态在后续 task 中按 spec §5 填充。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct ViewGetTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct ViewGetArgs {
    // TODO: 由后续 task 按 spec §5 字段填充
}

#[derive(Serialize)]
pub struct ViewGetOutput {
    // TODO: 由后续 task 按 spec §5 字段填充
}

pub enum ViewGetError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for ViewGetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "view_get bad args: {m}"),
            Self::Server(m) => write!(f, "view_get server error: {m}"),
        }
    }
}

impl fmt::Debug for ViewGetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for ViewGetError {}

impl Tool for ViewGetTool {
    const NAME: &'static str = "view_get";

    type Error = ViewGetError;
    type Args = ViewGetArgs;
    type Output = ViewGetOutput;

    fn description(&self) -> String {
        // TODO: 由后续 task 按 spec §5 替换为正式 description
        format!("view_get (TODO: 待后续 task 填实)")
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
        unimplemented!("view_get 将在后续 task 填实")
    }
}