use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum Role { System, User, Assistant, Tool }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: serde_json::Value,
    pub result_preview: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentMessage {
    pub id: Ulid,
    pub session_id: Ulid,
    pub role: Role,
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub tool_call_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSession {
    pub id: Ulid,
    pub user_id: Ulid,
    pub workspace_id: Ulid,
    pub title: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_message_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "domain", rename_all = "snake_case")]
pub enum SideEffect {
    Entry { action: EntryAction, id: String },
    Labeling { code: String },
    Comment { code: String },
    /// queryEntries 命中：前端把表达式写回 query_ast + expr_text
    ViewQueryReplace { query_expr: String },
    /// searchEntries 命中：前端把关键词写回 ad_hoc_text
    ViewSearchReplace { keyword: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EntryAction { Create, Update, Delete }

impl AgentSession {
    pub fn new(user_id: Ulid, workspace_id: Ulid) -> Self {
        let now = Utc::now();
        Self {
            id: Ulid::new(),
            user_id,
            workspace_id,
            title: String::new(),
            created_at: now,
            updated_at: now,
            last_message_at: None,
        }
    }
}

/// 每次 sendAgentMessage 调用携带的 turn-scoped 上下文。
/// runner 在每轮重新读取（不存进 cf::AGENT_SESSIONS），保证用户切视图后
/// 下一条消息的 context 反映新视图。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct AgentMessageContext {
    /// 当前打开的视图 id。None = 基础视图 / 未选视图。
    pub view_id: Option<String>,
    /// 用户在表格里勾选 / 打开的条目 code 列表。空 = 无选中。
    /// 上限 20 条（由 resolver 强制）。
    pub selected_entry_codes: Vec<String>,
}
