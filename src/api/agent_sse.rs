//! SSE 端点 `/api/agent/turns/<turn_id>/stream`：把 `SessionTurns` 的
//! `broadcast::Sender<AgentEvent>` 转成 `text/event-stream`。
//!
//! 客户端：前端 `new EventSource('/api/agent/turns/<turn_id>/stream')`，每条
//! `AgentEvent` 被序列化成一行 `data: {...}\n\n`，`Done` / `Error` 之后再额外
//! 发一条 `event: close\ndata: end` 通知前端 `source.close()`。
//!
//! **鉴权**：复用 `src/api/graphql.rs` 的 `extract_auth`——cookie 优先、
//! `Authorization: Bearer` 头 fallback。`new EventSource(url)` 是浏览器硬性
//! 无法设自定义 header，因此 token 走 cookie；其它客户端（curl / 服务端对
//! 调）则走 Bearer 头。两条路径收敛在同一处，对齐 spec §12「SSE 与 Mutation
//! 都走同一份 auth_from_cookie」。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, Path};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures_util::Stream;
use ulid::Ulid;

use crate::api::AppState;
use crate::domain::agent_events::AgentEvent;
use crate::error::AppError;

/// 把 `AppError` 映射成 SSE 接入前的拒绝响应——只覆盖鉴权/查找/解析路径，
/// 映射表刻意保守（Internal 一律 500），不与 GraphQL 错误流耦合；其它路径
/// （Agent 业务失败）会先通过 run_turn 的 `AgentEvent::Error` 推送给客户端，
/// 不会落到这里。
///
/// 写在本文件而不是 `error.rs`：本任务限定只能改 `agent_sse.rs` / `mod.rs`
/// / `main.rs` 三个文件，跨模块加 trait 实现会破坏隔离。SSE 路由独有的需求
/// 不值得污染通用错误类型。
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            AppError::Unauthorized | AppError::InvalidCredentials => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::InvalidQuery(_) => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (status, self.code()).into_response()
    }
}

/// Agent SSE 流：把 `broadcast::Receiver<AgentEvent>` 转成 Axum 的 `Sse<Stream>`。
///
/// 流程：
/// 1. 复用 `extract_auth` 解析 cookie / Bearer 头 → `verify_token` → `AuthContext`；
/// 2. `turn_id` → `session_id`（注册表里查归属）→ 校验当前账号拥有这个 session；
/// 3. `subscribe(turn_id)` 拿 receiver，包成 `async_stream::stream!` 喂给 Axum。
///
/// 终止路径（与 Task 6 进度文档一致）：`run_turn` 跑到 `finish(turn_id)` 时把
/// `Sender` drop，receiver 端 `recv()` 返回 `Err(Closed)` → 流结束。`OnDrop` 里
/// 那次 cancel 是「对称式保险」，并不真的能跑到 runner（`cancel_token_for` 与
/// `start_or_replace` 之间存在替换），无副作用，但也不指望它生效。
pub async fn agent_stream_handler(
    Extension(state): Extension<Arc<AppState>>,
    Path(turn_id): Path<String>,
    headers: HeaderMap,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AppError> {
    let auth = crate::api::graphql::extract_auth(state.services.as_ref(), &headers)
        .ok_or(AppError::Unauthorized)?;
    let turn_id = parse_turn_id(&turn_id)?;
    let session_id = state
        .services
        .agent_turns
        .session_of(turn_id)
        .ok_or(AppError::NotFound)?;
    state
        .services
        .agent
        .get_session(auth.account_id, session_id)?
        .ok_or(AppError::NotFound)?;

    // cancel 是「对称式」：实际靠 broadcast 关闭触发流结束，这里只是
    // 跟着 drop 跑一次 cancel()。保留是为了和 brief / 后续重构预期对齐。
    let cancel = state.services.agent_turns.cancel_token(turn_id);
    let mut rx = state.services.agent_turns.subscribe(turn_id)?;
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

fn parse_turn_id(s: &str) -> Result<Ulid, AppError> {
    Ulid::from_string(s).map_err(|e| AppError::InvalidQuery(format!("无效的 turn_id: {e}")))
}
