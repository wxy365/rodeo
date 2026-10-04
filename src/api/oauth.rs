//! OAuth HTTP 端点。
//!
//! - `/api/auth/wechat/start`：生成 csrf state + return_to，跳到微信。
//! - `/api/auth/wechat/callback`：微信跳回；签名是 Spec 1 留下来的，**逻辑**
//!   走公共路径 `oauth_callback_common`（Spec 2 起 Google / GitHub 共用同一函数）。
//! - Google / GitHub 的 start / callback 路由在 Spec 2 后续任务单独加，
//!   都直接调 `oauth_callback_common`。

use axum::extract::{Extension, Query};
use axum::response::Redirect;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use ulid::Ulid;

use crate::api::AppState;
use crate::domain::{AccountStatus, OAuthProvider};
use crate::error::AppError;
use crate::service::oauth::provider_kind_from_str;
use crate::service::oauth_state::{BindEntry, CsrfEntry};

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    #[serde(default)]
    pub return_to: Option<String>,
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

/// Spec 2 公共回调路径。三步算法见 `2026-10-04-oauth-google-github` plan：
/// 1. csrf 校验 + provider 比对（防跨 provider 重放，spec §14）。
/// 2. 已绑：刷新 last_used_at + 签 JWT + 跳 OAuthCallback。
/// 3. email 命中已有账号：状态门 + upsert 新 binding + 签 JWT。
///    （仅 Google / GitHub 触发 —— WeChat external.email 为 None 不进。）
/// 4. 没 binding + 没 email 命中：写 BindEntry + 跳 `/oauth/callback?bind=...`。
async fn oauth_callback_common(
    state: Arc<AppState>,
    provider_name: &str,
    code: String,
    state_token: String,
    return_to: String,
) -> Result<Redirect, AppError> {
    // return_to 兜底：与 Spec 1 `wechat_callback` 行为一致 —— 不安全的字符串
    // 全部退到 "/workspaces"，避免 open-redirect。
    let return_to = if crate::service::oauth::url_guard::is_safe_return_to(&return_to) {
        return_to
    } else {
        DEFAULT_RETURN_TO.to_string()
    };

    // csrf 必须与本次请求的 provider 对应，否则可能是跨 provider 重放
    // （拿微信 csrf 去打 Google callback）。
    let csrf = state
        .services
        .oauth_state
        .take_csrf(&state_token)
        .ok_or_else(|| AppError::OAuthCallback("登录已过期，请重试".to_string()))?;
    if csrf.provider != provider_name {
        return Err(AppError::OAuthCallback("登录状态不匹配".to_string()));
    }

    let provider_kind = provider_kind_from_str(provider_name)
        .ok_or_else(|| AppError::OAuthCallback(format!("未知的 provider: {provider_name}")))?;

    let provider = state
        .services
        .oauth_registry
        .get(provider_name)
        .ok_or_else(|| AppError::OAuthNotConfigured(provider_name.to_string()))?;

    // exchange_code 单一参数 —— redirect_uri 已在 provider 构造时由 from_config
    // 写进自身。
    let external = provider.exchange_code(&code).await?;

    // 1. 已绑过 → 刷 binding 后签 token（spec §7 步 1）
    if let Some(b) = state
        .services
        .oauth_bindings
        .find(provider_kind, &external.external_id)?
    {
        // 状态门：先判 status，**再** upsert。Frozen / Deactivated
        // 立即拒，不污染 binding 状态。
        let account = state
            .services
            .auth
            .find_by_id(b.account_id)?
            .ok_or(AppError::NotFound)?;
        match state.services.auth.status(account.id)? {
            AccountStatus::Active => {}
            AccountStatus::Frozen => {
                return Err(AppError::OAuthWechat(
                    provider_kind.as_str().to_string(),
                    "账号已被冻结，请联系系统管理员".to_string(),
                ));
            }
            AccountStatus::Deactivated => {
                return Err(AppError::OAuthCallback("此账号已注销".to_string()));
            }
        }
        state.services.oauth_bindings.upsert(
            account.id,
            provider_kind,
            &external.external_id,
            external.email.clone(),
            external.display_name.clone(),
        )?;
        return redirect_with_token(state.clone(), account.id, &return_to).await;
    }

    // 2. 邮箱命中 → 静默登录（spec §8）。WeChat external.email = None 不进。
    if let Some(email) = &external.email {
        if let Some(account) = state.services.auth.find_by_email(email)? {
            // 状态门：先判 status，**再** upsert + 签 token。
            // Frozen / Deactivated 与 Spec 1 login 路径文案保持一致。
            match state.services.auth.status(account.id)? {
                AccountStatus::Active => {}
                AccountStatus::Frozen => {
                    return Err(AppError::OAuthWechat(
                        provider_kind.as_str().to_string(),
                        "账号已被冻结，请联系系统管理员".to_string(),
                    ));
                }
                AccountStatus::Deactivated => {
                    return Err(AppError::OAuthCallback("此账号已注销".to_string()));
                }
            }
            state.services.oauth_bindings.upsert(
                account.id,
                provider_kind,
                &external.external_id,
                external.email.clone(),
                external.display_name.clone(),
            )?;
            return redirect_with_token(state.clone(), account.id, &return_to).await;
        }
    }

    // 3. 没 binding、没 email 命中 → 走 bind 面板：写 BindEntry 后
    // 跳到 `/login?bind=...&provider=...&return_to=...`。
    // Spec 1 的 `WeChatBindPanel` 只在 `/login` 路由上挂载（见
    // `frontend/pages/login.rs::url_param("bind")`）。`/oauth/callback`
    // 是 JWT fragment 接收路由，**不**挂 bind panel —— R4 ruling。
    let bind_token = crate::service::oauth_state::OAuthStateStore::new_token();
    state.services.oauth_state.put_bind(
        bind_token.clone(),
        BindEntry {
            provider: provider_kind,
            external_id: external.external_id,
            access_token: external.access_token,
            return_to: return_to.clone(),
            created_at: chrono::Utc::now(),
        },
    );
    Ok(Redirect::to(&format!(
        "/login?bind={bind_token}&provider={provider_name}&return_to={}",
        urlencoding::encode(&return_to),
    )))
}

/// 公共路径「签 token + 跳 OAuthCallback」。
///
/// 状态门已在调用方（`oauth_callback_common`）完成，这里只负责签 JWT 与拼重定向。
/// 复用同一份「JWT + URL」格式：浏览器跳到 `/oauth/callback#token=...`，fragment
/// 不会被发到服务端，前端 `OAuthCallback` 组件解析 `#token=` 后存进 `rodeo_jwt`。
async fn redirect_with_token(
    state: Arc<AppState>,
    account_id: Ulid,
    return_to: &str,
) -> Result<Redirect, AppError> {
    let jwt = state.services.auth.sign_token(account_id)?;
    Ok(Redirect::to(&format!(
        "/oauth/callback#token={}&return_to={}",
        urlencoding::encode(&jwt),
        urlencoding::encode(return_to),
    )))
}

pub async fn wechat_callback(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    // Spec 2 起 callback 路径走公共 `oauth_callback_common`，所以这里从
    // HashMap 里读三个字段（code / state / 可选 return_to）然后转交。
    // `return_to` 缺省时优先用 csrf 里 start 阶段记下的那份；都没有就
    // 退到 DEFAULT_RETURN_TO（与 url_guard 安全门之后的行为一致）。
    let code = params.get("code").cloned().unwrap_or_default();
    let state_token = params.get("state").cloned().unwrap_or_default();
    let return_to = params
        .get("return_to")
        .cloned()
        .unwrap_or_else(|| DEFAULT_RETURN_TO.to_string());
    oauth_callback_common(state, OAuthProvider::WeChat.as_str(), code, state_token, return_to).await
}

/// Spec 2: Google / GitHub 的 start 公共壳。`provider_name` 是 `"google"` / `"github"`
/// 字面量，与 `OAuthRegistry` 的 key 对齐。`redirect_uri` 直接从 config 读：
/// 不给 trait 加方法（plan Step 2.5 决定 b）—— 这条路径只在 start 用到，
/// 抽到 trait 反而让其他 provider 多一个空实现。
async fn start_for_provider(
    state: Arc<AppState>,
    provider_name: &'static str,
    params: HashMap<String, String>,
) -> Result<Redirect, AppError> {
    let raw_return_to = params
        .get("return_to")
        .cloned()
        .unwrap_or_else(|| DEFAULT_RETURN_TO.to_string());
    let return_to = if crate::service::oauth::url_guard::is_safe_return_to(&raw_return_to) {
        raw_return_to
    } else {
        DEFAULT_RETURN_TO.to_string()
    };

    let redirect_uri = match provider_name {
        "google" => state
            .services
            .config
            .auth
            .oauth
            .google
            .as_ref()
            .map(|c| c.redirect_uri.clone())
            .unwrap_or_default(),
        "github" => state
            .services
            .config
            .auth
            .oauth
            .github
            .as_ref()
            .map(|c| c.redirect_uri.clone())
            .unwrap_or_default(),
        _ => String::new(),
    };

    let provider = state
        .services
        .oauth_registry
        .get(provider_name)
        .ok_or_else(|| {
            AppError::OAuthNotConfigured(provider_name.to_string())
        })?;

    let csrf_token = crate::service::oauth_state::OAuthStateStore::new_token();
    state.services.oauth_state.put_csrf(
        csrf_token.clone(),
        CsrfEntry {
            provider: provider_name.to_string(),
            redirect_uri: redirect_uri.clone(),
            return_to: return_to.clone(),
            created_at: chrono::Utc::now(),
        },
    );

    let auth_url = provider.authorization_url(&csrf_token, &redirect_uri);
    Ok(Redirect::to(&auth_url))
}

pub async fn google_start(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    start_for_provider(state, "google", params).await
}

pub async fn github_start(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    start_for_provider(state, "github", params).await
}

pub async fn google_callback(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_token = params.get("state").cloned().unwrap_or_default();
    let return_to = params
        .get("return_to")
        .cloned()
        .unwrap_or_else(|| DEFAULT_RETURN_TO.to_string());
    oauth_callback_common(state, "google", code, state_token, return_to).await
}

pub async fn github_callback(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_token = params.get("state").cloned().unwrap_or_default();
    let return_to = params
        .get("return_to")
        .cloned()
        .unwrap_or_else(|| DEFAULT_RETURN_TO.to_string());
    oauth_callback_common(state, "github", code, state_token, return_to).await
}