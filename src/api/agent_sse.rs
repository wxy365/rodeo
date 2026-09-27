//! SSE 端点 `/api/agent/turns/<turn_id>/stream`：把 `SessionTurns` 的
//! `broadcast::Sender<AgentEvent>` 转成 `text/event-stream`。
//!
//! 客户端：前端 `new EventSource('/api/agent/turns/<turn_id>/stream')`，每条
//! `AgentEvent` 被序列化成一行 `data: {...}\n\n`，`Done` / `Error` 之后再额外
//! 发一条 `event: close\ndata: end` 通知前端 `source.close()`。
//!
//! **鉴权**：复用现有 cookie 约定——浏览器登录后 Set-Cookie 写入 `jwt=...`，
//! 这里从请求头里取、和 `src/api/graphql.rs` 的 `extract_auth` 一致。不复用
//! `extract_auth` 是因为它是私有的；本文件自带一份等价的 4 行解析。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::header::COOKIE;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::Stream;
use ulid::Ulid;

use crate::domain::agent_events::AgentEvent;
use crate::error::AppError;
use crate::service::Services;

/// Agent SSE 流：把 `broadcast::Receiver<AgentEvent>` 转成 Axum 的 `Sse<Stream>`。
///
/// 流程：
/// 1. 解析 `jwt` cookie → `verify_token` → `AuthContext`；
/// 2. `turn_id` → `session_id`（注册表里查归属）→ 校验当前账号拥有这个 session；
/// 3. `subscribe(turn_id)` 拿 receiver，包成 `async_stream::stream!` 喂给 Axum。
///
/// 终止路径（与 Task 6 进度文档一致）：`run_turn` 跑到 `finish(turn_id)` 时把
/// `Sender` drop，receiver 端 `recv()` 返回 `Err(Closed)` → 流结束。`OnDrop` 里
/// 那次 cancel 是「对称式保险」，并不真的能跑到 runner（`cancel_token_for` 与
/// `start_or_replace` 之间存在替换），无副作用，但也不指望它生效。
pub async fn agent_stream_handler(
    State(services): State<Arc<Services>>,
    Path(turn_id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let auth = auth_from_cookie(&services, &headers)?;
    let turn_id = parse_turn_id(&turn_id)?;
    let session_id = services
        .agent_turns
        .session_of(turn_id)
        .ok_or(AppError::NotFound)?;
    services
        .agent
        .get_session(auth.account_id, session_id)?
        .ok_or(AppError::NotFound)?;

    // cancel 是「对称式」：实际靠 broadcast 关闭触发流结束，这里只是
    // 跟着 drop 跑一次 cancel()。保留是为了和 brief / 后续重构预期对齐。
    let cancel = services.agent_turns.cancel_token(turn_id);
    let mut rx = services.agent_turns.subscribe(turn_id)?;
    let cancel_for_drop = cancel.clone();

    let stream = async_stream::stream! {
        struct OnDrop<F: FnOnce()>(Option<F>);
        impl<F: FnOnce()> Drop for OnDrop<F> {
            fn drop(&mut self) { if let Some(f) = self.0.take() { f(); } }
        }
        let _on_drop = OnDrop(Some(move || cancel_for_drop.cancel()));

        loop {
            match rx.recv().await {
                Ok(ev) => {
                    let json = serde_json::to_string(&ev)
                        .unwrap_or_else(|_| r#"{"type":"error","message":"encode"}"#.to_string());
                    yield Ok(Event::default().data(json));
                    if matches!(ev, AgentEvent::Done { .. } | AgentEvent::Error { .. }) {
                        // 显式 close 帧，让前端 source.close()。
                        yield Ok(Event::default().event("close").data("end"));
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
        // 真正退出由上面的 `Err(_)` 分支走到（broadcast 关闭）。`cancel` 自身
        // 不等待：它从不被 select，仅通过 OnDrop 在流被 drop 时跑一次。
        let _ = cancel;
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

fn auth_from_cookie(
    services: &Arc<Services>,
    headers: &HeaderMap,
) -> Result<crate::service::AuthContext, AppError> {
    let token = cookie_value(headers, "jwt").ok_or(AppError::Unauthorized)?;
    services.auth.verify_token(&token)
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(COOKIE)?.to_str().ok()?;
    for pair in cookie.split(';') {
        let mut kv = pair.trim().splitn(2, '=');
        if kv.next() == Some(name) {
            return kv.next().map(|s| s.trim().to_string());
        }
    }
    None
}

fn parse_turn_id(s: &str) -> Result<Ulid, AppError> {
    Ulid::from_string(s).map_err(|e| AppError::InvalidQuery(format!("无效的 turn_id: {e}")))
}