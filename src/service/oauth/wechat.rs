//! 微信开放平台「网站应用」扫码登录。
//!
//! 标准 OAuth2 QR Connect：start 路由拼 `open.weixin.qq.com/connect/qrconnect?...`
//! 重定向过去，微信展示 QR，用户扫码后微信跳回我们的 `/api/auth/wechat/callback`。
//! 回调里用 code 换 access_token + openid。
//!
//! 注意：URL 末尾的 `#wechat_redirect` 是微信约定 —— 不带它微信会报警告。

use std::time::Duration;

use chrono::{DateTime, Utc};

use super::{ExternalToken, OAuthProvider};
use crate::config::WeChatOAuthConfig;
use crate::domain::OAuthProvider;
use crate::error::AppError;

pub struct WeChatProvider {
    pub app_id: String,
    pub app_secret: String,
    pub redirect_uri: String,
    pub http: reqwest::Client,
}

impl WeChatProvider {
    pub fn from_config(cfg: &WeChatOAuthConfig) -> Result<Option<Self>, AppError> {
        let app_id = cfg.app_id.trim();
        let app_secret = cfg.app_secret.trim();
        let redirect_uri = cfg.redirect_uri.trim();

        // app_id 留空 = 未启用；其余字段必须同时为空，否则报错（半配 = 配置错误）。
        if app_id.is_empty() {
            if !app_secret.is_empty() {
                return Err(AppError::Internal(
                    "auth.oauth.wechat.app_id 留空时不能单独配置 app_secret".to_string(),
                ));
            }
            if !redirect_uri.is_empty() {
                return Err(AppError::Internal(
                    "auth.oauth.wechat.app_id 留空时不能单独配置 redirect_uri".to_string(),
                ));
            }
            return Ok(None);
        }

        if app_secret.is_empty() {
            return Err(AppError::Internal(
                "auth.oauth.wechat.app_id 已配置但 app_secret 留空".to_string(),
            ));
        }
        if redirect_uri.is_empty() {
            return Err(AppError::Internal(
                "auth.oauth.wechat.app_id 已配置但 redirect_uri 留空".to_string(),
            ));
        }

        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| AppError::Internal(format!("reqwest client 初始化失败: {e}")))?;

        Ok(Some(Self {
            app_id: app_id.to_string(),
            app_secret: app_secret.to_string(),
            redirect_uri: redirect_uri.to_string(),
            http,
        }))
    }
}

#[async_trait::async_trait]
impl OAuthProvider for WeChatProvider {
    fn name(&self) -> &'static str {
        OAuthProvider::WeChat.as_str()
    }

    fn enabled(&self) -> bool {
        true
    }

    fn authorization_url(&self, state: &str, redirect_uri: &str) -> String {
        // redirect_uri 二次 URL-encode —— 微信在文档里强调回调 URL 必须是 urlencoded，
        // 即使配置里已写完整 URL，再 encode 一遍不会出错。
        let redirect_enc = urlencoding::encode(redirect_uri);
        format!(
            "https://open.weixin.qq.com/connect/qrconnect\
             ?appid={app_id}\
             &redirect_uri={redirect_enc}\
             &response_type=code\
             &scope=snsapi_login\
             &state={state}\
             #wechat_redirect",
            app_id = urlencoding::encode(&self.app_id),
            redirect_enc = redirect_enc,
            state = urlencoding::encode(state),
        )
    }

    async fn exchange_code(&self, code: &str) -> Result<ExternalToken, AppError> {
        let url = format!(
            "https://api.weixin.qq.com/sns/oauth2/access_token\
             ?appid={app_id}\
             &secret={secret}\
             &code={code}\
             &grant_type=authorization_code",
            app_id = urlencoding::encode(&self.app_id),
            secret = urlencoding::encode(&self.app_secret),
            code = urlencoding::encode(code),
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("微信 token 端点请求失败: {e}")))?;
        // 微信 4xx/5xx 也回 200 + JSON errcode，所以不靠 status 判断，按 body 解。
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AppError::Internal(format!("微信 token 端点响应解析失败: {e}")))?;

        let errcode = body.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
        if errcode != 0 {
            let errmsg = body
                .get("errmsg")
                .and_then(|v| v.as_str())
                .unwrap_or("(no errmsg)")
                .to_string();
            return Err(AppError::OAuthWechat(errcode.to_string(), errmsg));
        }

        let external_id = body
            .get("openid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AppError::Internal("微信 token 响应缺 openid".to_string()))?
            .to_string();
        let access_token = body
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AppError::Internal("微信 token 响应缺 access_token".to_string())
            })?
            .to_string();
        let refresh_token = body
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(7200);
        let expires_at: DateTime<Utc> = Utc::now() + chrono::Duration::seconds(expires_in);

        Ok(ExternalToken {
            external_id,
            access_token,
            refresh_token,
            expires_at: Some(expires_at),
        })
    }
}
