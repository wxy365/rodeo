//! Agent 消息与领域类型。
//!
//! 消息本体直接复用 `rig::message::Message` —— agent 的轮次编排、tool
//! call/result、序列化、provider 适配全部交给 rig 处理。Rodeo 私有字段
//! （`id` / `session_id` / `created_at`）由 `AgentMessageRecord` 在持久化层
//! 承载，与 Rig Message 一起落库。
//!
//! `rig::message::Message` 三个变体（参考 Context7）：
//! - `System { content: String }` — 系统提示
//! - `User { content: Vec<UserContent> }` — 用户消息 / 工具回执（`UserContent::tool_result`）
//! - `Assistant { id: Option<String>, content: Vec<AssistantContent> }` — 助手回复（`Text`/`ToolCall`/`Reasoning`）

pub use rig::message::{
    AssistantContent, Message as AgentMessage, ToolResult as AgentToolResult, ToolResultContent,
    UserContent,
};
pub use ulid::Ulid;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// DocStore 落库一行：Rig Message + Rodeo 元数据。
///
/// 把 Rig 的 `Message` 与 Rodeo 私有的 ULID / 时间戳捆成一个落库单元：
/// `id` 是消息本身的 row id，`session_id` 用于前缀扫描归属到会话，
/// `created_at` 走 session 列表的排序 +历史窗口裁剪。
///
/// 序列化走 `try_from` / `into` shim——`rig::message::Message` 及其嵌套
/// `AssistantContent` / `UserContent` 都带 `#[serde(tag = "...", ...)]`
/// 内部 tag，bincode 1.x 走 `deserialize_any` 读不回来（错误信息原文：
/// "Bincode does not support the serde::Deserializer::deserialize_any
/// method"）。`list_messages` 又是 `filter_map(|(_, v)| ... .ok())`
/// 静默吞掉失败的 row——结果就是「消息写入成功但 list_messages
/// 永远空」。Shim 把 `message` 字段单独编码成 JSON 字节，再让 bincode
/// 走正常的 tuple 编码。读侧失败会冒到上层而不是静默丢失。
#[derive(Debug, Clone)]
pub struct AgentMessageRecord {
    pub id: Ulid,
    pub session_id: Ulid,
    pub message: AgentMessage,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentMessageRecordShim {
    id: Ulid,
    session_id: Ulid,
    /// rig::Message 用 `#[serde(tag="role")]` 内部 tag，bincode 1.x
    /// 走 `deserialize_any` 读不回来。这里改成原始 JSON 字节
    /// ——bincode 对 `Vec<u8>` 友好（自带长度前缀），解码时再走 serde_json
    /// 还原回 `rig::Message`。
    message_json: Vec<u8>,
    created_at: DateTime<Utc>,
}

impl From<AgentMessageRecord> for AgentMessageRecordShim {
    fn from(r: AgentMessageRecord) -> Self {
        // `serde_json::to_vec` 对 rig::Message 一定成功（JSON 内部 tag
        // 友好）；在序列化落库路径上没有失败可能，没必要 `Result`。
        Self {
            id: r.id,
            session_id: r.session_id,
            message_json: serde_json::to_vec(&r.message)
                .expect("rig::Message 应当能 JSON 编码"),
            created_at: r.created_at,
        }
    }
}

impl TryFrom<AgentMessageRecordShim> for AgentMessageRecord {
    type Error = serde_json::Error;
    fn try_from(s: AgentMessageRecordShim) -> Result<Self, Self::Error> {
        Ok(Self {
            id: s.id,
            session_id: s.session_id,
            message: serde_json::from_slice(&s.message_json)?,
            created_at: s.created_at,
        })
    }
}

impl Serialize for AgentMessageRecord {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        AgentMessageRecordShim::from(self.clone()).serialize(s)
    }
}

impl<'de> Deserialize<'de> for AgentMessageRecord {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let shim = AgentMessageRecordShim::deserialize(d)?;
        Self::try_from(shim).map_err(serde::de::Error::custom)
    }
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
pub enum EntryAction {
    Create,
    Update,
    Delete,
}

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