//! Google OAuth2 + OpenID Connect。
//! authorize:  https://accounts.google.com/o/oauth2/v2/auth
//! token:      https://oauth2.googleapis.com/token
//! userinfo:   https://openidconnect.googleapis.com/v1/userinfo
//! external_id = `sub`（OIDC 标准的稳定账号 ID）。
//!
//! email / display_name 在 callback 层（api/oauth.rs）从 userinfo 二次拉取后
//! 写到 `IdentityBinding`，**不在 `ExternalToken` 里**——`ExternalToken` 在
//! Spec 1 里只有外部凭据字段（external_id/access_token/refresh_token/expires_at），
//! 与 IdentityBinding 的「账号档案字段」分层。如果后续想把 email/display_name
//! 提前到 provider 层再加进 ExternalToken。

use serde::Deserialize;

use crate::config::GoogleOAuthConfig;
use crate::error::AppError;
use crate::service::oauth::{ExternalToken, OAuthProvider as OAuthProviderTrait};

pub struct GoogleProvider {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

impl GoogleProvider {
    pub fn from_config(cfg: &GoogleOAuthConfig) -> Result<Option<Self>, AppError> {
        // 任一必填项留空 = 未启用。和 wechat 一样，半配（client_id 配了但
        // secret 没配）视作未启用而不是配置错误：避免「正在配的过程中」启动崩。
        if cfg.client_id.trim().is_empty() || cfg.client_secret.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
            redirect_uri: cfg.redirect_uri.clone(),
        }))
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct Userinfo {
    sub: String,
    email: Option<String>,
    name: Option<String>,
}

#[async_trait::async_trait]
impl OAuthProviderTrait for GoogleProvider {
    fn name(&self) -> &'static str {
        "google"
    }

    fn enabled(&self) -> bool {
        true
    }

    fn authorization_url(&self, state: &str, redirect_uri: &str) -> String {
        // Google 接受 URL-encoded redirect_uri；调用方传入的 redirect_uri 通常已经
        // 是 config 里的「裸 URL」，所以这里再 encode 一次是必须的（多次 encode 也无副作用）。
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth\
             ?client_id={client_id}\
             &redirect_uri={redirect_uri}\
             &response_type=code\
             &scope=openid+email+profile\
             &state={state}",
            client_id = urlencoding::encode(&self.client_id),
            redirect_uri = urlencoding::encode(redirect_uri),
            state = urlencoding::encode(state),
        )
    }

    async fn exchange_code(&self, code: &str) -> Result<ExternalToken, AppError> {
        let client = reqwest::Client::new();

        // 1. code → access_token
        let token: TokenResponse = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("code", code),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("redirect_uri", &self.redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("google".to_string(), format!("token request failed: {e}"))
            })?
            .error_for_status()
            .map_err(|e| {
                AppError::OAuthWechat("google".to_string(), format!("token status: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("google".to_string(), format!("token decode: {e}"))
            })?;

        // 2. access_token → userinfo（取 sub 作为 external_id）。
        // Google 4xx/5xx 也回 JSON error body，先 error_for_status 把网络错提前；
        // 这里不取 email/name —— 它们由 callback 层另行调用 userinfo 后写入
        // IdentityBinding，避免在 ExternalToken 上加字段（bincode 结构变更）。
        let info: Userinfo = client
            .get("https://openidconnect.googleapis.com/v1/userinfo")
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("google".to_string(), format!("userinfo failed: {e}"))
            })?
            .error_for_status()
            .map_err(|e| {
                AppError::OAuthWechat("google".to_string(), format!("userinfo status: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("google".to_string(), format!("userinfo decode: {e}"))
            })?;

        let expires_at = token
            .expires_in
            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));

        Ok(ExternalToken {
            external_id: info.sub,
            access_token: token.access_token,
            refresh_token: token.refresh_token,
            expires_at,
            email: info.email,
            display_name: info.name,
        })
    }
}