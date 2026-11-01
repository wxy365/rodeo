//! 开发期热重载中继。只在 `cargo leptos watch` 下有意义（路由由 main.rs 按
//! LEPTOS_WATCH 注册）。
//!
//! 为什么需要中继：cargo-leptos 的 reload server（默认 :3001）是纯明文
//! `axum::serve`，没有 TLS；而 Leptos 注入的 AutoReload 脚本在 https 页面下会
//! 强制用 `wss://`（leptos/src/hydration/reload_script.js 第一行），Chrome 又
//! 禁止 https 页面发起明文 ws://（混合内容）。三者叠在一起，「HTTPS 开发页 +
//! 明文 reload 端口」这个组合下热重载必然连不上——浏览器控制台恒定报
//! `WebSocket connection to 'wss://127.0.0.1:3001/live_reload' failed`。
//!
//! 做法：应用在自己的监听端口上开一条 /live_reload（与页面同源、同 TLS），
//! 服务端再连回 cargo-leptos 的明文 reload 端口，把 reload 消息转发给浏览器。
//! 浏览器侧指向本端口，由 main.rs 设置 `options.reload_external_port` 完成。

use axum::extract::ws::{Message as AxumMsg, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use futures_util::StreamExt;
use leptos::prelude::LeptosOptions;
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message as WsMsg;

/// `GET /live_reload` —— 浏览器侧的热重载通道。
///
/// 上游是 cargo-leptos 的 reload server：`ws://127.0.0.1:<reload_port>/live_reload`。
pub async fn live_reload(
    ws: WebSocketUpgrade,
    State(options): State<LeptosOptions>,
) -> Response {
    let upstream = format!("ws://127.0.0.1:{}/live_reload", options.reload_port);
    ws.on_upgrade(move |browser| relay(browser, upstream))
}

/// 上游 → 浏览器 的单向转发。
///
/// 上游只推 reload 消息（文本帧），浏览器不发东西，所以只需要「读上游 → 写浏览器」
/// 这一个方向；写失败即视为浏览器已经断开。
async fn relay(mut browser: WebSocket, upstream: String) {
    let Ok((mut source, _)) = tokio_tungstenite::connect_async(&upstream).await else {
        tracing::warn!("热重载中继：连不上 cargo-leptos 的 reload server {upstream}");
        return;
    };
    tracing::info!("热重载中继已连上 {upstream}");

    while let Some(Ok(msg)) = source.next().await {
        let payload = match msg {
            WsMsg::Text(text) => translate(&text),
            WsMsg::Close(_) => break,
            // Ping / Pong / Binary：cargo-leptos 只发文本帧，其余忽略。
            _ => continue,
        };
        if browser.send(AxumMsg::Text(Utf8Bytes::from(payload))).await.is_err() {
            break;
        }
    }
}

/// 把 cargo-leptos 的 reload 消息翻译成浏览器脚本能用的形状。
///
/// 它有三种：
/// - `{"all":true}` —— 整页刷新，原样转发；
/// - `{"css":"pkg/rodeo.css"}` —— 换样式表。**原样转发是无效的**：脚本按
///   `link.href.includes(msg.css)` 找那张表，而本仓库开了 hash-files，页面上的
///   地址是 `/pkg/rodeo.<hash>.css`，永远匹配不上「pkg/rodeo.css」，脚本只会在
///   控制台留一句 "Could not find a <link ...>" 然后什么也不做。所以这里把它升级
///   成整页刷新——重新 SSR 出来的页面会带上当前的样式表地址。
/// - `{"view":"..."}` —— view 宏补丁，要脚本里的 patch() 参与，原样转发。
fn translate(text: &str) -> String {
    let Ok(msg) = serde_json::from_str::<Value>(text) else {
        return text.to_string();
    };
    if msg.get("all").and_then(Value::as_bool).unwrap_or(false) {
        return text.to_string();
    }
    if msg.get("css").and_then(Value::as_str).is_some() {
        return r#"{"all":true}"#.to_string();
    }
    text.to_string()
}
