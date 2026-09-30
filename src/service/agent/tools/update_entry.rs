//! `update_entry` tool 骨架。Args/Outputs/Error 形态在后续 task 中按 spec §5 填充。

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
    // TODO: 由后续 task 按 spec §5 字段填充
}

#[derive(Serialize)]
pub struct UpdateEntryOutput {
    // TODO: 由后续 task 按 spec §5 字段填充
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
        // TODO: 由后续 task 按 spec §5 替换为正式 description
        format!("update_entry (TODO: 待后续 task 填实)")
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
        unimplemented!("update_entry 将在后续 task 填实")
    }
}