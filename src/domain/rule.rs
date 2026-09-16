use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::domain::view::query_json;
use crate::domain::{LabelValue, Query};

/// 规则整体以 bincode 落库，而 bincode 不支持 `deserialize_any`，
/// `serde_json::Value` 直接编码就会「写得进去、读不出来」。
/// 与 `domain::view::query_json` 同一套办法：先转成 JSON 字符串再交给 bincode。
mod raw_json {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T, S>(value: &T, ser: S) -> Result<S::Ok, S::Error>
    where
        T: Serialize,
        S: Serializer,
    {
        let s = serde_json::to_string(value).map_err(serde::ser::Error::custom)?;
        ser.serialize_str(&s)
    }

    pub fn deserialize<'de, T, D>(de: D) -> Result<T, D::Error>
    where
        T: serde::de::DeserializeOwned,
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        serde_json::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// 一次标签变更。新增时 `old` 为 `None`，删除时 `new` 为 `None`。
/// 不落库——事件是请求内的瞬时结构，审计另有 `AuditLog` 承载。
#[derive(Debug, Clone, PartialEq)]
pub struct LabelEvent {
    pub workspace_id: Ulid,
    pub entry_code: String,
    pub label_name: String,
    pub old: Option<LabelValue>,
    pub new: Option<LabelValue>,
    /// 触发者。规则动作产生的事件里仍是触发者——规则不是主体，没有自己的身份。
    pub actor: Ulid,
    /// 0 = 用户直接写入；1..=3 = 规则动作产生的写入。
    pub level: u8,
}

/// 写入动作。`Set` 涵盖新增 / 修改 / upsert——底层都是 `Labeling` 的覆盖写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WriteOp {
    Set,
    Remove,
}

/// 动作写入值的来源。`Literal` 之外都无法在保存时静态校验类型。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValueSource {
    Literal(#[serde(with = "raw_json")] serde_json::Value),
    /// 当前时间，按目标标签 schema 的 format 渲染。一次请求内只取一个时刻。
    Now,
    /// 事件的新值（$new）。
    New,
    /// 事件的旧值（$old）。
    Old,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelWrite {
    pub label_name: String,
    pub op: WriteOp,
    /// `Remove` 时为 `None`，`Set` 时必填。
    pub value: Option<ValueSource>,
}

/// 动作作用到哪些条目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionTarget {
    /// 事件源条目。
    EventSource,
    /// 表达式圈定（工作空间内、排除已删除与已归档，口径同视图列表）。
    Query(#[serde(with = "query_json")] Query),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleAction {
    pub target: ActionTarget,
    pub writes: Vec<LabelWrite>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRule {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub name: String,
    pub enabled: bool,
    /// 触发条件 AST。文本由 `Query::to_expr()` 反推，不存副本，避免两份真相漂移。
    #[serde(with = "query_json")]
    pub trigger: Query,
    pub action: RuleAction,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AutomationRule {
    pub fn new(
        workspace_id: Ulid,
        name: String,
        enabled: bool,
        trigger: Query,
        action: RuleAction,
        actor: Ulid,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Ulid::new(),
            workspace_id,
            name,
            enabled,
            trigger,
            action,
            created_by: actor,
            created_at: now,
            updated_at: now,
        }
    }
}
