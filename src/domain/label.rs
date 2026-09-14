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
    // 新增：只能追加在末尾（bincode 位置编码）
    Date,
    Time,
    DateTime,
    Currency,
    Email,
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
            LabelValueType::Date => "date",
            LabelValueType::Time => "time",
            LabelValueType::DateTime => "datetime",
            LabelValueType::Currency => "currency",
            LabelValueType::Email => "email",
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
            "date" => Some(LabelValueType::Date),
            "time" => Some(LabelValueType::Time),
            "datetime" => Some(LabelValueType::DateTime),
            "currency" => Some(LabelValueType::Currency),
            "email" => Some(LabelValueType::Email),
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
    // 追加，勿插入到上方
    EnumList(Vec<String>),
    Date(String),
    Time(String),
    DateTime(String),
    Currency(f64),
    Email(String),
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
                if schema.multi {
                    let vals = match value {
                        serde_json::Value::Array(a) => a
                            .iter()
                            .map(|x| x.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                            .ok_or(AppError::InvalidLabelValue)?,
                        serde_json::Value::String(s) => vec![s.clone()],
                        _ => return Err(AppError::InvalidLabelValue),
                    };
                    if vals
                        .iter()
                        .any(|v| !schema.enum_values.iter().any(|e| e == v))
                    {
                        return Err(AppError::InvalidLabelValue);
                    }
                    Ok(LabelValue::EnumList(vals))
                } else {
                    let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                    if !schema.enum_values.iter().any(|v| v == s) {
                        return Err(AppError::InvalidLabelValue);
                    }
                    Ok(LabelValue::Enum(s.to_string()))
                }
            }
            LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime => {
                let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                let layout = schema
                    .format
                    .as_deref()
                    .unwrap_or_else(|| default_layout(schema.value_type));
                crate::golayout::parse(layout, s).ok_or(AppError::InvalidLabelValue)?;
                Ok(match schema.value_type {
                    LabelValueType::Date => LabelValue::Date(s.to_string()),
                    LabelValueType::Time => LabelValue::Time(s.to_string()),
                    _ => LabelValue::DateTime(s.to_string()),
                })
            }
            LabelValueType::Currency => value
                .as_f64()
                .map(LabelValue::Currency)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::Email => {
                let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                let mut parts = s.split('@');
                let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next())
                else {
                    return Err(AppError::InvalidLabelValue);
                };
                if local.is_empty() || domain.is_empty() {
                    return Err(AppError::InvalidLabelValue);
                }
                Ok(LabelValue::Email(s.to_string()))
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
            LabelValue::EnumList(v) => serde_json::Value::Array(
                v.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
            LabelValue::Date(s)
            | LabelValue::Time(s)
            | LabelValue::DateTime(s)
            | LabelValue::Email(s) => serde_json::Value::String(s.clone()),
            LabelValue::Currency(f) => serde_json::json!(f),
        }
    }
}

/// 时间型标签的默认展示布局（schema.format 缺省时使用）。
pub fn default_layout(vt: LabelValueType) -> &'static str {
    match vt {
        LabelValueType::Date => crate::golayout::DATE_LAYOUT,
        LabelValueType::Time => crate::golayout::TIME_LAYOUT,
        LabelValueType::DateTime => crate::golayout::DATETIME_LAYOUT,
        _ => "",
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelSchema {
    pub workspace_id: Ulid,
    pub name: String,
    pub title: String,
    pub value_type: LabelValueType,
    pub enum_values: Vec<String>,
    /// 基础色 `#rrggbb`，未配置为 None。
    #[serde(default)]
    pub color: Option<String>,
    /// 值 → 色映射；按顺序取首个命中。
    #[serde(default)]
    pub value_colors: Vec<ValueColor>,
    // 追加在末尾，全部 #[serde(default)]
    #[serde(default)]
    pub multi: bool, // 仅 Enum 有效
    #[serde(default)]
    pub format: Option<String>, // 时间型 = Go 布局；Currency 可留空
    #[serde(default)]
    pub currency_symbol: Option<String>, // 缺省 ¥（仅展示用）
    #[serde(default)]
    pub unit: Option<String>, // 如「元」「万」（仅展示用）
}

/// 标签值到颜色的映射规则。用普通 struct 而非 tagged enum：`LabelSchema` 以 bincode
/// 持久化，bincode 不支持 `deserialize_any`（internally-tagged enum 依赖它）。
/// 语义：`value` 为 `Some` → 枚举精确匹配；否则按 `min`（含）/`max`（不含）数值区间，None 无界。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueColor {
    pub color: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub value: Option<String>,
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
            color: None,
            value_colors: Vec::new(),
            multi: false,
            format: None,
            currency_symbol: None,
            unit: None,
        }
    }

    /// 链式设置基础色与值色。
    pub fn with_colors(mut self, color: Option<String>, value_colors: Vec<ValueColor>) -> Self {
        self.color = color;
        self.value_colors = value_colors;
        self
    }

    /// 链式设置类型属性，与 `with_colors` 并列。
    pub fn with_attrs(
        mut self,
        multi: bool,
        format: Option<String>,
        currency_symbol: Option<String>,
        unit: Option<String>,
    ) -> Self {
        self.multi = multi;
        self.format = format;
        self.currency_symbol = currency_symbol;
        self.unit = unit;
        self
    }

    /// 内置「任务」：无值标签，只有「打上 / 没打上」两种状态（表达式里写作 `Task` / `!Task`）。
    pub fn task(workspace_id: Ulid) -> Self {
        Self::new(
            workspace_id,
            "Task".to_string(),
            "任务".to_string(),
            LabelValueType::Null,
            Vec::new(),
        )
    }

    /// 内置「缺陷」：同 `task`，无值标签。
    pub fn bug(workspace_id: Ulid) -> Self {
        Self::new(
            workspace_id,
            "Bug".to_string(),
            "缺陷".to_string(),
            LabelValueType::Null,
            Vec::new(),
        )
    }
}

/// 标签值 → 颜色：Enum 精确匹配；数值按区间（左闭右开，首个命中）；未命中回退基础色。
pub fn resolve_color(schema: &LabelSchema, value: &serde_json::Value) -> Option<String> {
    for vc in &schema.value_colors {
        let hit = if let Some(want) = &vc.value {
            match value {
                serde_json::Value::Array(a) => a.iter().any(|x| x.as_str() == Some(want.as_str())),
                _ => value.as_str() == Some(want.as_str()),
            }
        } else if let Some(v) = value.as_f64() {
            vc.min.map_or(true, |lo| v >= lo) && vc.max.map_or(true, |hi| v < hi)
        } else {
            false
        };
        if hit {
            return Some(vc.color.clone());
        }
    }
    schema.color.clone()
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
