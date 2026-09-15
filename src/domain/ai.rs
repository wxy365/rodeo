use serde::{Deserialize, Serialize};

/// 「名称 + 提示词」条目。场景与语气共用这一种形状：名称是用户在界面上选的东西，
/// 提示词是拼进 system 消息的那段文字。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NamedPrompt {
    pub name: String,
    pub prompt: String,
}

/// 工作空间级的 AI 总结配置。整体以 bincode 存进 `cf::WORKSPACE_AI`，
/// 键是 workspace id——所以结构体一旦落过库，新增字段必须带 `#[serde(default)]`。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceAiConfig {
    pub scenarios: Vec<NamedPrompt>,
    pub tones: Vec<NamedPrompt>,
}
