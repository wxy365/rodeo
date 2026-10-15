//! 在当前工作空间里做全文检索（Tantivy 关键词搜索）。与 GraphQL `Query::search_entries`
//! 对齐：跳过已删除与已归档条目，keyword 可空时返回空集。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct SearchEntriesTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct SearchEntriesArgs {
    pub workspace_id: String,
    pub keyword: String,
    #[serde(default)]
    pub limit: Option<i32>,
}

#[derive(Serialize)]
pub struct SearchEntriesOutput {
    pub items: Vec<Value>,
    pub total: i64,
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
        "在工作空间内做 Tantivy 全文检索（关键词命中标题 / 正文 / 评论）。返回包含 keyword 的条目列表，跳过软删除与已归档条目；keyword 为空时返回空集。limit 默认 20，最大 100。".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；必须与 agent session 的 workspace 一致"
                },
                "keyword": {
                    "type": "string",
                    "description": "检索关键词；空字符串返回空集"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "返回条数上限，默认 20，clamp 1..=100"
                }
            },
            "required": ["workspace_id", "keyword"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| SearchEntriesError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(SearchEntriesError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        let limit = args.limit.unwrap_or(20).clamp(1, 100) as usize;
        let keyword = args.keyword.trim();
        if keyword.is_empty() {
            return Ok(SearchEntriesOutput {
                items: Vec::new(),
                total: 0,
            });
        }
        let search = self
            .services
            .entry
            .search_available()
            .ok_or_else(|| SearchEntriesError::Server("全文检索不可用".to_string()))?;
        let codes = search
            .search(ws, keyword, limit)
            .map_err(|e| SearchEntriesError::Server(e.to_string()))?;
        let mut items: Vec<Value> = Vec::with_capacity(codes.len());
        for code in codes {
            let Some(entry) = self
                .services
                .entry
                .get(&code)
                .map_err(|e| SearchEntriesError::Server(e.to_string()))?
            else {
                continue;
            };
            // 跳过软删除与归档：与 query_entries 行为一致。
            if entry.is_deleted() {
                continue;
            }
            let is_archived = self
                .services
                .entry
                .is_archived(&code)
                .map_err(|e| SearchEntriesError::Server(e.to_string()))?;
            if is_archived {
                continue;
            }
            items.push(serde_json::to_value(&entry).unwrap_or(Value::Null));
        }
        let total = items.len() as i64;
        Ok(SearchEntriesOutput { items, total })
    }
}
