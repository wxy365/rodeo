//! Axum WebSocket upgrade handler + 单连接生命周期。
//!
//! JWT 鉴权必须在 upgrade 之前完成——浏览器 `new WebSocket(url)` 走 query param
//! 是唯一能传 token 的路径，且不能在 WS 内发「鉴权失败」（spec §2 决策）。
//! 鉴权失败直接返回 401，不开 WS。

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message as AxumMsg, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Query};
use axum::response::IntoResponse;
use serde::Deserialize;
use ulid::Ulid;

use crate::api::AppState;
use super::message::WsMessage;

/// 心跳周期。客户端不需要回包；服务端判活靠「写 Pong 时是否 BrokenPipe」。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
pub struct WsParams {
    pub token: Option<String>,
}

/// `GET /api/ws?token=<jwt>` —— 鉴权通过后开 WS。
pub async fn ws_handler(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<WsParams>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let token = match params.token.as_deref() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return (axum::http::StatusCode::UNAUTHORIZED, "missing token").into_response();
        }
    };
    let auth = match state.services.auth.verify_token(&token) {
        Ok(a) => a,
        Err(_) => {
            return (axum::http::StatusCode::UNAUTHORIZED, "invalid token").into_response();
        }
    };
    let user_id = auth.account_id;
    ws.on_upgrade(move |socket| connection_task(socket, state, user_id))
}

/// 单条连接生命周期：
/// 1. 立即推 Hello
/// 2. 每 30s 推 Pong（不依赖客户端 ack）
/// 3. 持续从 hub 收新消息推给客户端
/// 4. 持续读客户端消息（忽略内容）
/// 5. 任一端关闭 → 清理 unsubscribe
async fn connection_task(mut socket: WebSocket, state: Arc<AppState>, user_id: Ulid) {
    let mut rx = state.services.ws_hub.subscribe(user_id).await;

    // 1. Hello
    let hello = serde_json::to_string(&WsMessage::Hello {
        user_id,
        server_time: chrono::Utc::now().timestamp(),
    })
    .unwrap_or_default();
    if socket.send(AxumMsg::Text(Utf8Bytes::from(hello))).await.is_err() {
        state.services.ws_hub.unsubscribe(user_id).await;
        return;
    }

    // 2. 心跳 + 双向事件
    let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
    heartbeat.tick().await; // 跳过立即 fire 的第一次 tick

    loop {
        tokio::select! {
            // 从 hub 收新消息 → 推给客户端
            ws_msg = rx.recv() => {
                match ws_msg {
                    Ok(msg) => {
                        let text = serde_json::to_string(&msg).unwrap_or_default();
                        if socket.send(AxumMsg::Text(Utf8Bytes::from(text))).await.is_err() {
                            break;
                        }
                    }
                    // 溢出：跳过旧消息继续；polling 兜底会拉全量未读
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
            // 心跳
            _ = heartbeat.tick() => {
                if socket.send(AxumMsg::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
            // 客户端消息（忽略内容即可）
            client_msg = socket.recv() => {
                match client_msg {
                    Some(Ok(_)) => continue,
                    _ => break,
                }
            }
        }
    }

    // 5. 清理
    state.services.ws_hub.unsubscribe(user_id).await;
}