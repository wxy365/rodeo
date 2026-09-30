//! `search_entries` tool 骨架。Args/Outputs/Error 形态在后续 task 中按 spec §5 填充。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct SearchEntriesTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct SearchEntriesArgs {
    // TODO: 由后续 task 按 spec §5 字段填充
}

#[derive(Serialize)]
pub struct SearchEntriesOutput {
    // TODO: 由后续 task 按 spec §5 字段填充
}

pub enum SearchEntriesError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for SearchEntriesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "search_entries bad args: {m}"),
            Self::Server(m) => write!(f, "search_entries server error: {m}"),
        }
    }
}

impl fmt::Debug for SearchEntriesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for SearchEntriesError {}

impl Tool for SearchEntriesTool {
    const NAME: &'static str = "search_entries";

    type Error = SearchEntriesError;
    type Args = SearchEntriesArgs;
    type Output = SearchEntriesOutput;

    fn description(&self) -> String {
        // TODO: 由后续 task 按 spec §5 替换为正式 description
        format!("search_entries (TODO: 待后续 task 填实)")
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
        unimplemented!("search_entries 将在后续 task 填实")
    }
}