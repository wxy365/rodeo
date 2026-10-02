//! OAuth Provider 抽象 + 注册表。
//!
//! Spec 1 只实现 WeChat；Spec 2 加 Google/GitHub 时各加一个 provider struct
//! 实现本 trait，再注册到 `OAuthRegistry::new()` 即可。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::error::AppError;

/// Provider 用 code 换回来的短期凭据。
/// 我们自己**不**调 refresh_token —— 每次扫码重新走完整 OAuth2 流程，
/// 与 builtin 的 72h session TTL 各管各的。
#[derive(Debug, Clone)]
pub struct ExternalToken {
    pub external_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
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
