//! 给单个条目写单个标签值。value 须符合该标签 schema 的 value_type：
//! "null" 标签传 `null`（或省略），"boolean"/"integer"/"float" 传对应 JSON 数字/布尔，
//! "string"/"enum"/"email"/"date"/"time"/"datetime" 传字符串，"currency" 传数字，
//! "account" 传账号 ULID 字符串；多选（schema.multi）传字符串数组。

use std::fmt;
use std::sync::Arc;

use rig::tool::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub struct SetLabelsTool {
    pub services: Arc<Services>,
    pub auth: AuthContext,
    pub workspace_id: Ulid,
}

#[derive(Deserialize)]
pub struct SetLabelsArgs {
    pub workspace_id: String,
    pub entry_code: String,
    pub label_name: String,
    /// 标签值，形态由 schema.value_type 决定。无值标签（"null"）传 `null` 或省略。
    #[serde(default)]
    pub value: Option<Value>,
}

#[derive(Serialize)]
pub struct SetLabelsOutput {
    pub entry_code: String,
    pub label_name: String,
    pub value: Value,
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
        "给单个条目写单个标签值。value 的 JSON 形态由对应 schema 的 value_type 决定（详见 label_schemas）：null 标签传 null；boolean/integer/float 传 JSON 数字或布尔；string/enum/email/date/time/datetime 传字符串；currency 传数字；account 传 ULID；多选类型传字符串数组。".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "workspace_id": {
                    "type": "string",
                    "description": "工作空间 ULID；必须与 agent session 的 workspace 一致"
                },
                "entry_code": {
                    "type": "string",
                    "description": "目标条目 16 位 base62 编码"
                },
                "label_name": {
                    "type": "string",
                    "description": "标签 schema 的 name（key）"
                },
                "value": {
                    "description": "标签值，形态由 schema.value_type 决定。无值（\"null\"）标签传 null 或省略。"
                }
            },
            "required": ["workspace_id", "entry_code", "label_name"]
        })
    }

    async fn call(
        &self,
        _ctx: &mut ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let ws = Ulid::from_string(&args.workspace_id)
            .map_err(|e| SetLabelsError::BadArgs(format!("workspace_id: {e}")))?;
        if ws != self.workspace_id {
            return Err(SetLabelsError::BadArgs(format!(
                "workspace_id 不匹配 session（session={}, args={}）",
                self.workspace_id, ws
            )));
        }
        // set_labeling 在服务端按 schema 校验 value；类型不符直接返回 InvalidQuery。
        let value = args.value.unwrap_or(Value::Null);
        let labeling = self
            .services
            .entry
            .set_labeling(
                self.auth.account_id,
                &args.entry_code,
                &args.label_name,
                &value,
            )
            .map_err(|e| SetLabelsError::Server(e.to_string()))?;
        // 回写后把 LabelValue 转回 JSON 形态；server 内部校验过 = 一定能反序列化。
        let value_json = serde_json::to_value(&labeling.value).unwrap_or(Value::Null);
        Ok(SetLabelsOutput {
            entry_code: labeling.entry_code,
            label_name: labeling.label_name,
            value: value_json,
        })
    }
}
