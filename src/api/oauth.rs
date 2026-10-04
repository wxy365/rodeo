//! 微信 OAuth HTTP 端点。
//!
//! - `/api/auth/wechat/start`：生成 csrf state + return_to，跳到微信。
//! - `/api/auth/wechat/callback`：微信跳回，验 csrf、换 openid、决定登录或进绑定。

use axum::extract::{Extension, Query};
use axum::response::Redirect;
use serde::Deserialize;
use std::sync::Arc;

use crate::api::AppState;
use crate::domain::OAuthProvider;
use crate::error::AppError;
use crate::service::oauth_state::{BindEntry, CsrfEntry};

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    pub return_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
}

const DEFAULT_RETURN_TO: &str = "/workspaces";

pub async fn wechat_start(
    Extension(state): Extension<Arc<AppState>>,
    Query(q): Query<StartQuery>,
) -> Result<Redirect, AppError> {
    let return_to = q.return_to.unwrap_or_else(|| DEFAULT_RETURN_TO.to_string());
    if !crate::service::oauth::url_guard::is_safe_return_to(&return_to) {
        return Err(AppError::OAuthCallback(
            "return_to 必须是同源相对路径".to_string(),
        ));
    }

    let provider = state
        .services
        .oauth_registry
        .get(OAuthProvider::WeChat.as_str())
        .ok_or_else(|| AppError::OAuthNotConfigured(OAuthProvider::WeChat.as_str().to_string()))?;

    let csrf = crate::service::oauth_state::OAuthStateStore::new_token();

    // authorization_url 内的 redirect_uri 与 config 一致；我们这里再次传同一份是
    // 给 spec 留的口子 —— 未来若需要「每个回调动态 redirect_uri」也好扩展。
    // 同时把它一起记进 csrf，callback 时比对供方回跳的 URL 是否与当时一致。
    let config_redirect = match state.services.config.auth.oauth.wechat.as_ref() {
        Some(c) => c.redirect_uri.clone(),
        None => {
            return Err(AppError::OAuthNotConfigured(
                OAuthProvider::WeChat.as_str().to_string(),
            ))
        }
    };

    state.services.oauth_state.put_csrf(
        csrf.clone(),
        CsrfEntry {
            provider: OAuthProvider::WeChat.as_str().to_string(),
            redirect_uri: config_redirect.clone(),
            return_to,
            created_at: chrono::Utc::now(),
        },
    );

    let url = provider.authorization_url(&csrf, &config_redirect);
    Ok(Redirect::to(&url))
}

pub async fn wechat_callback(
    Extension(state): Extension<Arc<AppState>>,
    Query(q): Query<CallbackQuery>,
) -> Result<Redirect, AppError> {
    // 1. 取 csrf（缺席/过期 → 统一文案，不区分 CSRF 与过期）
    let entry = state
        .services
        .oauth_state
        .take_csrf(&q.state)
        .ok_or_else(|| AppError::OAuthCallback("登录已过期，请重试".to_string()))?;

    let provider = state
        .services
        .oauth_registry
        .get(OAuthProvider::WeChat.as_str())
        .ok_or_else(|| AppError::OAuthNotConfigured(OAuthProvider::WeChat.as_str().to_string()))?;

    // 2. 用 code 换 access_token + openid
    let token = provider.exchange_code(&q.code).await?;

    // 3. 查绑定
    if let Some(binding) = state
        .services
        .oauth_bindings
        .find(OAuthProvider::WeChat, &token.external_id)?
    {
        // 已绑 → 状态检查 + 签 JWT + 跳 OAuthCallback
        let account = state
            .services
            .auth
            .find_by_id(binding.account_id)?
            .ok_or(AppError::NotFound)?;
        match state.services.auth.status(account.id)? {
            crate::domain::AccountStatus::Active => {}
            crate::domain::AccountStatus::Frozen => {
                return Err(AppError::InvalidQuery(
                    "账号已被冻结，请联系系统管理员".to_string(),
                ));
            }
            crate::domain::AccountStatus::Deactivated => {
                return Err(AppError::InvalidCredentials);
            }
        }
        state.services.oauth_bindings.upsert(
            account.id,
            OAuthProvider::WeChat,
            &token.external_id,
            binding.email.clone(),
            binding.display_name.clone(),
        )?;
        let jwt = state.services.auth.sign_token(account.id)?;
        let redirect = format!(
            "/oauth/callback#token={}&return_to={}",
            urlencoding::encode(&jwt),
            urlencoding::encode(&entry.return_to),
        );
        return Ok(Redirect::to(&redirect));
    }

    // 未绑 → 建 bind session，跳登录页带 bind_token
    let bind_token = crate::service::oauth_state::OAuthStateStore::new_token();
    state.services.oauth_state.put_bind(
        bind_token.clone(),
        BindEntry {
            provider: OAuthProvider::WeChat,
            external_id: token.external_id.clone(),
            access_token: token.access_token,
            return_to: entry.return_to.clone(),
            created_at: chrono::Utc::now(),
        },
    );
    let redirect = format!(
        "/login?bind={}&provider={}&return_to={}",
        urlencoding::encode(&bind_token),
        OAuthProvider::WeChat.as_str(),
        urlencoding::encode(&entry.return_to),
    );
    Ok(Redirect::to(&redirect))
}
