//! 按 query 表达式列出工作空间内条目。query 字段是 JSON AST，
//! 形态参考 domain::Query 枚举（cond/and/or/not 嵌套）。
//! 不传 query 等价全选。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use crate::domain::query::Query;
use crate::domain::view::{SortField, SortKey, SortSpec};
use crate::service::entry::PageInput as EntryPageInput;
use crate::service::{AuthContext, Services};

/// 与 `api::SortInput` 字段一致；`api::SortInput` 是 async-graphql InputObject，
/// 字段私有且没有公开访问器，不能跨模块使用。这里定义 tool-local 版本。
#[derive(Deserialize)]
pub struct SortInput {
    pub field: Option<String>,
    pub desc: Option<bool>,
}

/// 与 `api::PageInput` 字段一致（理由同上）。
#[derive(Deserialize)]
pub struct PageInput {
    pub page: Option<i32>,
    pub page_size: Option<i32>,
}

pub struct QueryEntriesTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct QueryEntriesArgs {
    pub workspace_id: String,
    #[serde(default)]
    pub query: Option<Query>,
    #[serde(default)]
    pub sorts: Option<Vec<SortInput>>,
    #[serde(default)]
    pub page: Option<PageInput>,
    #[serde(default)]
    pub limit: Option<i32>,
}

#[derive(Serialize)]
pub struct QueryEntriesOutput {
    pub items: Vec<Value>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub label_names: Vec<String>,
}

pub enum QueryEntriesError {
    BadArgs(String),
    Server(String),
}

impl fmt::Display for QueryEntriesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadArgs(m) => write!(f, "query_entries bad args: {m}"),
            Self::Server(m) => write!(f, "query_entries server error: {m}"),
        }
    }
}

impl fmt::Debug for QueryEntriesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for QueryEntriesError {}

/// 与 GraphQL resolver 中 `to_sort` 等价：入参缺省（None / 空数组）落回默认排序；
/// 内置字段名走 `SortField::from_str`，其余视作标签名（`SortField::Label`）。
fn build_sort_spec(sorts: Option<Vec<SortInput>>) -> SortSpec {
    let keys: Vec<SortKey> = sorts
        .unwrap_or_default()
        .into_iter()
        .map(|s| {
            // SortInput 字段未导出，这里按 JSON 形态手工读取。
            let field_json = s.field;
            let desc_json = s.desc;
            let field = match field_json {
                None => SortField::UpdatedAt,
                Some(f) => SortField::from_str(&f).unwrap_or_else(|| SortField::Label(f)),
            };
            SortKey {
                field,
                desc: desc_json.unwrap_or(true),
            }
        })
        .collect();
    if keys.is_empty() {
        SortSpec::default()
    } else {
        SortSpec { keys }
    }
}

impl Tool for QueryEntriesTool {
    const NAME: &'static str = "query_entries";

    type Error = QueryEntriesError;
    type Args = QueryEntriesArgs;
    type Output = QueryEntriesOutput;

    fn description(&self) -> String {
        "按 query 表达式列出工作空间内条目。query 字段是 JSON AST，形态参考 domain::Query 枚举（cond/and/or/not 嵌套）。不传 query 等价全选。".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；必须与 agent session 的 workspace 一致"
                },
                "query": {
                    "description": "查询表达式 JSON AST（cond/and/or/not 嵌套）。null 或省略 = 全选。",
                    "oneOf": [
                        { "type": "null" },
                        { "$ref": "#/$defs/Query" }
                    ]
                },
                "sorts": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "field": { "type": "string", "description": "排序字段：updatedAt | createdAt | title | createdBy | updatedBy | 标签名" },
                            "desc": { "type": "boolean", "description": "是否倒序（默认 true）" }
                        }
                    },
                    "description": "排序链。空数组或缺省 = 默认按 updatedAt 降序。"
                },
                "page": {
                    "type": "object",
                    "properties": {
                        "page": { "type": "integer", "minimum": 1, "description": "页码（默认 1）" },
                        "page_size": { "type": "integer", "minimum": 1, "maximum": 100, "description": "每页条数（默认 20，clamp 1..=100）" }
                    },
                    "description": "分页参数。"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "便捷上限；当前版本仅做语义占位，实际仍以 page.page_size 为准。"
                }
            },
            "required": ["workspace_id"],
            "$defs": {
                "Query": {
                    "type": "object",
                    "properties": {
                        "and": { "type": "array", "items": { "$ref": "#/$defs/Query" } },
                        "or": { "type": "array", "items": { "$ref": "#/$defs/Query" } },
                        "not": { "$ref": "#/$defs/Query" },
                        "cond": {
                            "type": "object",
                            "properties": {
                                "field": { "type": "string" },
                                "op": { "type": "string" },
                                "value": {}
                            },
                            "required": ["field", "op"]
                        }
                    }
                }
            }
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| QueryEntriesError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(QueryEntriesError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        let query = args.query.unwrap_or_else(Query::all);
        let schemas = self
            .services
            .label
            .list_schemas(ws)
            .map_err(|e| QueryEntriesError::Server(e.to_string()))?;
        query
            .validate(&schemas)
            .map_err(|e| QueryEntriesError::BadArgs(e.to_string()))?;
        let sort = build_sort_spec(args.sorts);
        let page = args.page.map(to_entry_page).unwrap_or_default();
        let r = self
            .services
            .entry
            .query(ws, &query, &sort, page)
            .map_err(|e| QueryEntriesError::Server(e.to_string()))?;
        Ok(QueryEntriesOutput {
            items: r
                .items
                .into_iter()
                .map(|(e, _)| serde_json::to_value(&e).unwrap_or(Value::Null))
                .collect(),
            total: r.total as i64,
            page: page.page as i64,
            page_size: page.page_size as i64,
            label_names: r.label_names,
        })
    }
}

/// 与 GraphQL resolver 中 `PageInput::to_page` 等价：缺省 1/20，clamp 1..=100。
fn to_entry_page(p: PageInput) -> EntryPageInput {
    EntryPageInput {
        page: p.page.unwrap_or(1).max(1) as usize,
        page_size: p.page_size.unwrap_or(20).clamp(1, 100) as usize,
    }
}
