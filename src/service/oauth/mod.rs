//! OAuth Provider 抽象 + 注册表。
//!
//! Spec 1 只实现 WeChat；Spec 2 加 Google/GitHub 时各加一个 provider struct
//! 实现本 trait，再注册到 `OAuthRegistry::new()` 即可。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::domain::OAuthProvider as DomainOAuthProvider;
use crate::error::AppError;

/// `provider_name` 字符串 → 领域 `OAuthProvider` enum。仅在 callback 入口处
/// 把供方名字转成内部 enum 用；故意不在 `OAuthProvider` trait 上 —— trait 已
/// 经是 registry 装配用的接口，再塞一个字符串解析会让 OAuthRegistry 的 trait
/// 抽象被 callback 层反向依赖污染。
///
/// 失败返回 `None`：调用方（`oauth_callback_common`）把这种情况翻成
/// `AppError::OAuthCallback("未知的 provider: …")`，因为 RFC 6749 不会回
/// 不认识的 provider_name，能走到这里基本是 URL 被篡改或客户端拼错。
///
/// 别名 `DomainOAuthProvider`：本文件 `OAuthProvider` 是 trait，与领域 enum
/// 同名会冲突。
pub fn provider_kind_from_str(s: &str) -> Option<DomainOAuthProvider> {
    match s {
        "wechat" => Some(DomainOAuthProvider::WeChat),
        "google" => Some(DomainOAuthProvider::Google),
        "github" => Some(DomainOAuthProvider::GitHub),
        _ => None,
    }
}

/// Provider 用 code 换回来的短期凭据。
/// 我们自己**不**调 refresh_token —— 每次扫码重新走完整 OAuth2 流程，
/// 与 builtin 的 72h session TTL 各管各的。
///
/// `email` / `display_name` 仅 Google / GitHub 会实际填充，微信保持 None。
/// callback 层用 email 做邮箱静默登录（spec §8），display_name 写到
/// IdentityBinding.display_name。`ExternalToken` 不进 bincode 持久化
///（只活在 callback 这一程），加字段不破坏存量数据。
#[derive(Debug, Clone)]
pub struct ExternalToken {
    pub external_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub email: Option<String>,
    pub display_name: Option<String>,
}

#[async_trait]
pub trait OAuthProvider: Send + Sync {
    /// 与 `OAuthProvider::as_str` 对齐："wechat" / "google" / "github"。
    fn name(&self) -> &'static str;
    /// `from_config` 已做配置校验；构造出来的实例 enabled 恒为 true。
    /// 保留这个方法是为了 trait 形状稳定（Spec 2 也许有「远程开关」之类）。
    fn enabled(&self) -> bool;
    /// 拼出「让浏览器跳过去」的 URL。
    fn authorization_url(&self, state: &str, redirect_uri: &str) -> String;
    /// 用 authorization_code 换 access_token + external_id。
    async fn exchange_code(&self, code: &str) -> Result<ExternalToken, AppError>;
}

pub struct OAuthRegistry {
    providers: HashMap<&'static str, Arc<dyn OAuthProvider>>,
}

impl OAuthRegistry {
    pub fn new(providers: Vec<Arc<dyn OAuthProvider>>) -> Self {
        let mut map = HashMap::new();
        for p in providers {
            map.insert(p.name(), p);
        }
        Self { providers: map }
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn OAuthProvider>> {
        self.providers.get(name).map(Arc::clone)
    }

    pub fn enabled_names(&self) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .providers
            .values()
            .filter(|p| p.enabled())
            .map(|p| p.name())
            .collect();
        v.sort();
        v
    }
}

pub mod github;
pub mod google;
pub mod url_guard;
pub mod wechat;
