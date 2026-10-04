use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// OAuth 提供方。bincode 按 variant 位置编码，**追加新 provider 只能放末尾**：
/// 本轮先声明全部三个 variant 是为了让 `as_byte` 编号从 day 1 就稳定
/// （WeChat=1, Google=2, GitHub=3），Spec 2 实现 Google/GitHub 时不破坏存量绑定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OAuthProvider {
    WeChat,
    Google,
    GitHub,
}

impl OAuthProvider {
    pub fn as_byte(self) -> u8 {
        match self {
            Self::WeChat => 1,
            Self::Google => 2,
            Self::GitHub => 3,
        }
    }

    /// 未知 byte 返回 None —— 列族里读出读不懂的值时，调用方自己决定回退策略
    /// （一般是当作「无绑定」处理，让用户重走一次 OAuth 流程）。
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::WeChat),
            2 => Some(Self::Google),
            3 => Some(Self::GitHub),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WeChat => "wechat",
            Self::Google => "google",
            Self::GitHub => "github",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityBinding {
    pub account_id: Ulid,
    pub provider: OAuthProvider,
    pub external_id: String,
    /// 微信返回的 userinfo 不含邮箱，本字段为 None。
    /// Google/GitHub 在 Spec 2 接入，会回填 Some。
    pub email: Option<String>,
    /// 微信：None。Google/GitHub：Some(provider 返回的 display_name)。
    pub display_name: Option<String>,
    pub bound_at: DateTime<Utc>,
    pub last_used_at: DateTime<Utc>,
}
