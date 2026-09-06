use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LabelValueType {
    Null,
    Boolean,
    Integer,
    Float,
    String,
    Enum,
}

impl LabelValueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            LabelValueType::Null => "null",
            LabelValueType::Boolean => "boolean",
            LabelValueType::Integer => "integer",
            LabelValueType::Float => "float",
            LabelValueType::String => "string",
            LabelValueType::Enum => "enum",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "null" => Some(LabelValueType::Null),
            "boolean" => Some(LabelValueType::Boolean),
            "integer" => Some(LabelValueType::Integer),
            "float" => Some(LabelValueType::Float),
            "string" => Some(LabelValueType::String),
            "enum" => Some(LabelValueType::Enum),
            _ => None,
        }
    }
}

/// 标签值，对应 LabelSchema.value_type。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LabelValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Enum(String),
}

impl LabelValue {
    /// 将 GraphQL 传入的 JSON 值按 schema 的 value_type 校验并转换为 LabelValue。
    pub fn from_json(value: &serde_json::Value, schema: &LabelSchema) -> Result<Self, AppError> {
        match schema.value_type {
            LabelValueType::Null => Ok(LabelValue::Null),
            LabelValueType::Boolean => value
                .as_bool()
                .map(LabelValue::Bool)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::Integer => value
                .as_i64()
                .map(LabelValue::Int)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::Float => value
                .as_f64()
                .map(LabelValue::Float)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::String => {
                // 多值场景：JSON 数组序列化为字符串存储。
                if let Some(arr) = value.as_array() {
                    let s = serde_json::to_string(arr)
                        .map_err(|e| AppError::Internal(e.to_string()))?;
                    Ok(LabelValue::String(s))
                } else {
                    value
                        .as_str()
                        .map(|s| LabelValue::String(s.to_string()))
                        .ok_or(AppError::InvalidLabelValue)
                }
            }
            LabelValueType::Enum => {
                let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                if !schema.enum_values.iter().any(|v| v == s) {
                    return Err(AppError::InvalidLabelValue);
                }
                Ok(LabelValue::Enum(s.to_string()))
            }
        }
    }

    /// 转为 JSON 值，用于 GraphQL 输出。
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            LabelValue::Null => serde_json::Value::Null,
            LabelValue::Bool(b) => serde_json::Value::Bool(*b),
            LabelValue::Int(i) => serde_json::Value::Number((*i).into()),
            LabelValue::Float(f) => serde_json::json!(f),
            LabelValue::String(s) => serde_json::Value::String(s.clone()),
            LabelValue::Enum(s) => serde_json::Value::String(s.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LabelSchema {
    pub workspace_id: Ulid,
    pub name: String,
    pub title: String,
    pub value_type: LabelValueType,
    pub enum_values: Vec<String>,
}

impl LabelSchema {
    pub fn new(
        workspace_id: Ulid,
        name: String,
        title: String,
        value_type: LabelValueType,
        enum_values: Vec<String>,
    ) -> Self {
        Self {
            workspace_id,
            name,
            title,
            value_type,
            enum_values,
        }
    }

    pub fn task(workspace_id: Ulid) -> Self {
        Self::new(
            workspace_id,
            "Task".to_string(),
            "任务".to_string(),
            LabelValueType::Enum,
            vec![
                "Open".to_string(),
                "InProgress".to_string(),
                "Done".to_string(),
                "Archived".to_string(),
            ],
        )
    }

    pub fn bug(workspace_id: Ulid) -> Self {
        Self::new(
            workspace_id,
            "Bug".to_string(),
            "缺陷".to_string(),
            LabelValueType::Enum,
            vec![
                "Open".to_string(),
                "Fixed".to_string(),
                "WontFix".to_string(),
                "Archived".to_string(),
            ],
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Labeling {
    pub entry_code: String,
    pub label_name: String,
    pub value: LabelValue,
    pub set_by: Ulid,
    pub set_at: DateTime<Utc>,
}

impl Labeling {
    pub fn new(entry_code: String, label_name: String, value: LabelValue, actor: Ulid) -> Self {
        Self {
            entry_code,
            label_name,
            value,
            set_by: actor,
            set_at: Utc::now(),
        }
    }
}
