//! 按标签表达式列出工作空间内条目。`expr` 是表达式**字符串**（与视图的
//! 表达式输入框、与 `Query::parse` 的语法完全一致），服务端解析成 AST 后执行。
//! 不传 expr 等价全选。
//!
//! 为什么是字符串而不是 JSON AST：AST 形态（`{"cond":{"field":{"label":"X"},
//! "op":"eq","value":"Y"}}`）要模型自己猜对 `Field` / `Op` 的 serde 外部标签
//! 编码——猜错的代价是「工具反复报参数错 → 回合数耗尽 → 没有最终答案」。
//! 表达式字符串是用户看得懂的语法，模型可以直接抄视图里的表达式。

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
///
/// 标量字段用宽松反序列化（见 `de_str_opt` / `de_bool_opt`）——模型偶尔把
/// 字符串字段写成数字、把布尔写成「true」这种字符串，serde 严格模式会让整次
/// 工具调用失败，而失败会让模型反复重试。
#[derive(Deserialize)]
pub struct SortInput {
    #[serde(default, deserialize_with = "de_str_opt")]
    pub field: Option<String>,
    #[serde(default, deserialize_with = "de_bool_opt")]
    pub desc: Option<bool>,
}

/// 宽松字符串：非字符串（数字 / 布尔 / null）当作「没给」。
fn de_str_opt<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) => Some(s),
        _ => None,
    })
}

/// 宽松布尔：接受 true / false / 「true」/「false」/ 1 / 0；其它当作「没给」。
fn de_bool_opt<'de, D>(d: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::Bool(b)) => Some(b),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        Some(Value::Number(n)) => n.as_i64().map(|i| i != 0),
        _ => None,
    })
}

pub struct QueryEntriesTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct QueryEntriesArgs {
    /// 工作空间 ULID。**可省**——省略时用 session 自己的工作空间。
    /// 给了就必须与 session 一致（跨工作空间查询不在 agent 的权限范围内）。
    pub workspace_id: Option<String>,
    /// 标签表达式字符串。空 / 省略 = 全选。
    #[serde(default)]
    pub expr: Option<String>,
    #[serde(default)]
    pub sorts: Option<Vec<SortInput>>,
    /// 页码，从 1 开始。
    #[serde(default, deserialize_with = "de_int_opt")]
    pub page: Option<i32>,
    /// 每页条数（1..=100）。
    #[serde(default, deserialize_with = "de_int_opt")]
    pub page_size: Option<i32>,
    /// `page_size` 的别名——模型更爱写 limit。给出时以它为准。
    #[serde(default, deserialize_with = "de_int_opt")]
    pub limit: Option<i32>,
}

/// 宽松整数：接受数字，也接受「100」这种数字字符串；null / 其它类型当作「没给」。
///
/// 模型经常把整数字段写成字符串（MiniMax 的 tool call 尤其如此），也会把
/// 可选参数显式写成 null。serde 默认严格——一个类型不合就把整次工具调用判成
/// 「failed to parse tool arguments」，而参数错会让模型反复重试同一参数，
/// 正是「回合耗尽、没有最终答案」的成因。这里一律放宽：能解析成数字就用，
/// 解析不出就退回默认分页。
fn de_int_opt<'de, D>(d: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let v = Option::<Value>::deserialize(d)?;
    Ok(match v {
        None => None,
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|i| i as i32)
            .or_else(|| n.as_f64().map(|f| f as i32)),
        Some(Value::String(s)) => s.trim().parse::<i32>().ok(),
        Some(_) => None,
    })
}

#[derive(Serialize)]
pub struct QueryEntriesOutput {
    pub items: Vec<EntryBrief>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    pub label_names: Vec<String>,
}

/// 条目的精简形态——模型读得懂、token 也省。
///
/// 之前直接吐 `Entry` 的完整 JSON：带 `detail`（富文本正文，可能很长）、
/// `workspace_id`、created_by / updated_by 的 ULID，却没有**标签取值**——
/// 模型拿到一堆标题，却看不出每条的状态是什么（「未开始」还是「已完成」），
/// 只能复述过滤条件、编不出「你还有哪几条未完成」的答案。
#[derive(Serialize)]
pub struct EntryBrief {
    /// 条目编码（后续 update / set_labels 等工具用它定位）。
    pub code: String,
    pub title: String,
    /// 标签取值：标签名 → 值（无值标签是 null）。回答「哪些是未完成」靠它。
    pub labels: serde_json::Map<String, Value>,
    /// 正文摘要（截断到 DETAIL_PREVIEW_CHARS 字）。
    pub detail_preview: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// 正文摘要的截断长度。与 `service::mention::preview` 用同一个量级——
/// 够模型判断「这条讲了什么」，又不至于把上下文撑爆。
const DETAIL_PREVIEW_CHARS: usize = 400;

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
        // 表达式语法直接抄 domain::query 的文法速查表——模型能照着示例拼。
        // 之前这里写的是「JSON AST，形态参考 domain::Query 枚举」，模型无从
        // 猜出 Field/Op 的 serde 外部标签编码，于是反复报「unknown variant」。
        concat!(
            "按标签表达式列出工作空间内的条目。expr 是表达式字符串（与视图的表达式输入框同一套语法），",
            "服务端解析后执行。\n",
            "语法：\n",
            "- 标签字段直接写标签名（区分大小写）；内置字段：Code / Title / Detail / CreatedBy / ",
            "CreatedAt / UpdatedBy / UpdatedAt / text\n",
            "- 比较：= != > >= < <= ；~ 是「包含」（大小写不敏感）\n",
            "- 集合：in (a, b) / not in (a, b) / has (a, b) / is (a, b)（has / is 仅多值标签可用）\n",
            "- 逻辑：AND / OR / NOT 必须大写，括号分组\n",
            "- 存在性：只写标签名即「该标签存在」；!标签名 即「该标签不存在」\n",
            "示例：Status = \"未开始\"、Status != \"已完成\"、Task AND Status = \"进行中\"、",
            "Assignee has (\"alice@x.io\")\n",
            "不传 expr = 全部条目。查之前先用 label_schemas 确认可用的标签名与取值。"
        )
        .to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；可省略（默认 = session 的工作空间），给了必须与 session 一致。"
                },
                "expr": {
                    "type": "string",
                    "description": "标签表达式字符串。示例：Status = \"未开始\" AND Task。省略或空串 = 全部条目。"
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
                    "type": "integer",
                    "minimum": 1,
                    "description": "页码，从 1 开始（默认 1）。"
                },
                "page_size": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "每页条数（默认 20，上限 100）。结果里的 total 是命中总数。"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "page_size 的别名；给出时以它为准。"
                }
            }
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        // workspace_id 可省——省略即 session 的工作空间。给了就必须一致。
        let ws = match args.workspace_id.as_deref() {
            None | Some("") => self.workspace_id,
            Some(raw) => Ulid::from_string(raw)
                .map_err(|e| QueryEntriesError::BadArgs(format!("workspace_id: {e}")))?,
        };
        if ws != self.workspace_id {
            return Err(QueryEntriesError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        let query = match args.expr.as_deref().map(str::trim) {
            None | Some("") => Query::all(),
            Some(expr) => Query::parse(expr)
                .map_err(|e| QueryEntriesError::BadArgs(format!("expr 表达式无效: {e}")))?,
        };
        let schemas = self
            .services
            .label
            .list_schemas(ws)
            .map_err(|e| QueryEntriesError::Server(e.to_string()))?;
        query
            .validate(&schemas)
            .map_err(|e| QueryEntriesError::BadArgs(e.to_string()))?;
        let sort = build_sort_spec(args.sorts);
        let page = to_entry_page(args.page, args.page_size, args.limit);
        let r = self
            .services
            .entry
            .query(ws, &query, &sort, page)
            .map_err(|e| QueryEntriesError::Server(e.to_string()))?;
        Ok(QueryEntriesOutput {
            items: r
                .items
                .into_iter()
                .map(|(e, labels)| EntryBrief {
                    code: e.code,
                    title: e.title,
                    labels: labels
                        .into_iter()
                        .map(|l| (l.label_name, l.value.to_json()))
                        .collect(),
                    detail_preview: crate::service::mention::preview(
                        &e.detail,
                        DETAIL_PREVIEW_CHARS,
                    ),
                    created_at: e.created_at,
                    updated_at: e.updated_at,
                })
                .collect(),
            total: r.total as i64,
            page: page.page as i64,
            page_size: page.page_size as i64,
            label_names: r.label_names,
        })
    }
}

/// 与 GraphQL resolver 中 `PageInput::to_page` 等价：缺省 1/20，clamp 1..=100。
///
/// 三个来源合成一页大小：`limit`（模型爱写的别名）> `page_size` > 缺省 20。
/// 之前 page_size 藏在 `page` 对象里、模型却把它写在顶层——serde 静默忽略未知
/// 字段，于是模型要的 100 条变成了 20 条，它还会照着 20 条报数。
fn to_entry_page(page: Option<i32>, page_size: Option<i32>, limit: Option<i32>) -> EntryPageInput {
    let size = limit.or(page_size).unwrap_or(20).clamp(1, 100) as usize;
    EntryPageInput {
        page: page.unwrap_or(1).max(1) as usize,
        page_size: size,
    }
}
