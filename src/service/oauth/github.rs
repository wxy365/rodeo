//! GitHub OAuth2（非 OpenID Connect，无 id_token）。
//! authorize:  https://github.com/login/oauth/authorize
//! token:      https://github.com/login/oauth/access_token
//! userinfo:   https://api.github.com/user
//! 邮箱备用:   https://api.github.com/user/emails
//! external_id = `id`（数字，转字符串）。
//!
//! GitHub OAuth App 的 token 响应只有 `access_token` —— 没有 `refresh_token`
//! 也没有 `expires_in`，所以 `ExternalToken.refresh_token` / `expires_at` 恒为 None。
//! 每次走完整 OAuth2 流程换新 token，与 builtin 的 72h session TTL 各管各的。
//!
//! GitHub 强制要求 `User-Agent` header，否则 `/user` 与 `/user/emails` 都回 403
//! secondary rate limit / 403 forbidden，所以每次请求都带上。

use serde::Deserialize;

use crate::config::GithubOAuthConfig;
use crate::error::AppError;
use crate::service::oauth::{ExternalToken, OAuthProvider as OAuthProviderTrait};

pub struct GithubProvider {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

impl GithubProvider {
    pub fn from_config(cfg: &GithubOAuthConfig) -> Result<Option<Self>, AppError> {
        // 任一必填项留空 = 未启用。和 google 一样，半配（client_id 配了但
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
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    id: serde_json::Value, // number；serde_json 拿到后转字符串
    login: String,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

#[async_trait::async_trait]
impl OAuthProviderTrait for GithubProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    fn enabled(&self) -> bool {
        true
    }

    fn authorization_url(&self, state: &str, redirect_uri: &str) -> String {
        format!(
            "https://github.com/login/oauth/authorize\
             ?client_id={client_id}\
             &redirect_uri={redirect_uri}\
             &scope=read:user+user:email\
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
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("code", code),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("redirect_uri", &self.redirect_uri),
            ])
            .send()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("github".to_string(), format!("token request failed: {e}"))
            })?
            .error_for_status()
            .map_err(|e| {
                AppError::OAuthWechat("github".to_string(), format!("token status: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("github".to_string(), format!("token decode: {e}"))
            })?;

        // 2. access_token → /user（取 id 作为 external_id，GitHub 强制 User-Agent）。
        let user: GithubUser = client
            .get("https://api.github.com/user")
            .bearer_auth(&token.access_token)
            .header("User-Agent", "rodeo")
            .send()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("github".to_string(), format!("user request failed: {e}"))
            })?
            .error_for_status()
            .map_err(|e| {
                AppError::OAuthWechat("github".to_string(), format!("user status: {e}"))
            })?
            .json()
            .await
            .map_err(|e| {
                AppError::OAuthWechat("github".to_string(), format!("user decode: {e}"))
            })?;

        // external_id：GitHub 的 id 是数字，serde_json::Value → String。
        let external_id = match &user.id {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            _ => {
                return Err(AppError::OAuthWechat(
                    "github".to_string(),
                    "GitHub user.id unexpected type".to_string(),
                ));
            }
        };

        // 3. email：user.email 为 null 时查 /user/emails 找 primary verified。
        // GitHub 允许用户在 web 上把 email 设成 private，导致 user.email 是 null
        // 但 user/emails 仍能返回 verified 列表 —— 这是 spec §8 邮箱静默登录
        // 依赖的兜底路径，必须有。
        let email = if let Some(e) = user.email.filter(|e| !e.is_empty()) {
            Some(e)
        } else {
            let emails: Vec<GithubEmail> = client
                .get("https://api.github.com/user/emails")
                .bearer_auth(&token.access_token)
                .header("User-Agent", "rodeo")
                .send()
                .await
                .map_err(|e| {
                    AppError::OAuthWechat(
                        "github".to_string(),
                        format!("emails request failed: {e}"),
                    )
                })?
                .error_for_status()
                .map_err(|e| {
                    AppError::OAuthWechat(
                        "github".to_string(),
                        format!("emails status: {e}"),
                    )
                })?
                .json()
                .await
                .map_err(|e| {
                    AppError::OAuthWechat(
                        "github".to_string(),
                        format!("emails decode: {e}"),
                    )
                })?;
            emails
                .into_iter()
                .find(|e| e.primary && e.verified)
                .map(|e| e.email)
        };

        // display_name 优先级：name（用户填的展示名） → login（账号 ID 兜底）。
        // 没有 name 时退到 login，callback 层用此写到 IdentityBinding.display_name。
        let display_name = user.name.clone().or(Some(user.login.clone()));

        Ok(ExternalToken {
            external_id,
            access_token: token.access_token,
            // GitHub OAuth App 不发 refresh_token / expires_in —— 见模块顶注释。
            refresh_token: None,
            expires_at: None,
            email,
            display_name,
        })
    }
}
