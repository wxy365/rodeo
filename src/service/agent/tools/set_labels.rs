//! `set_labels` tool 骨架。Args/Outputs/Error 形态在后续 task 中按 spec §5 填充。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct SetLabelsTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct SetLabelsArgs {
    // TODO: 由后续 task 按 spec §5 字段填充
}

#[derive(Serialize)]
pub struct SetLabelsOutput {
    // TODO: 由后续 task 按 spec §5 字段填充
}

pub enum SetLabelsError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for SetLabelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "set_labels bad args: {m}"),
            Self::Server(m) => write!(f, "set_labels server error: {m}"),
        }
    }
}

impl fmt::Debug for SetLabelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for SetLabelsError {}

impl Tool for SetLabelsTool {
    const NAME: &'static str = "set_labels";

    type Error = SetLabelsError;
    type Args = SetLabelsArgs;
    type Output = SetLabelsOutput;

    fn description(&self) -> String {
        // TODO: 由后续 task 按 spec §5 替换为正式 description
        format!("set_labels (TODO: 待后续 task 填实)")
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
        unimplemented!("set_labels 将在后续 task 填实")
    }
}