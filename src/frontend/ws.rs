//! 浏览器侧 WebSocket 客户端。
//!
//! `WsContext` 暴露给组件订阅，组件通过 `use_context::<WsContext>()` 拿到。
//! 启动函数 `spawn_ws_connected_root` 在 `app.rs::App` 入口挂一次，
//! 后续断线 / 重连由内部循环负责（spec §6.2）。
//!
//! 浏览器拿不到 auth 头（`new WebSocket(url)` 不能加自定义 header），
//! 所以 token 走 `?token=<jwt>` query param——与服务端 `/api/ws` handler
//! 约定一致。

use leptos::prelude::*;
use serde::Deserialize;
#[cfg(target_arch = "wasm32")]
use std::cell::{Cell, RefCell};
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;
use ulid::Ulid;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use web_sys::{CloseEvent, ErrorEvent, MessageEvent, WebSocket};

/// 连接状态。`status` 是 `RwSignal`——组件可以订阅。
#[derive(Clone, Copy, Debug)]
pub enum WsStatus {
    Connecting,
    Connected,
    Disconnected {
        reason: &'static str,
        next_retry_ms: u64,
    },
}

#[derive(Clone, Copy)]
pub struct WsContext {
    pub status: RwSignal<WsStatus>,
    /// 最近一次推送的 message_id。订阅这个 signal 的组件可以 trigger refetch。
    pub last_message: RwSignal<Option<Ulid>>,
}

/// 服务端推送消息。serde 协议与 `api::ws::message::WsMessage` 完全一致。
#[derive(Deserialize, Debug)]
#[serde(tag = "type", content = "data")]
pub enum WsClientMessage {
    #[serde(rename = "hello")]
    Hello { user_id: Ulid, server_time: i64 },
    #[serde(rename = "new_message")]
    NewMessage { message_id: Ulid },
    #[serde(rename = "pong")]
    Pong,
}

/// 在 App 根启动 ws 连接循环。`localStorage.rodeo_jwt` 取 token；断线指数退避。
/// 仅 wasm 实际建连；SSR / native 端函数体为空（被 `#[cfg]` 屏蔽）。
pub fn spawn_ws_connected_root() {
    #[cfg(target_arch = "wasm32")]
    {
        let status: RwSignal<WsStatus> = RwSignal::new(WsStatus::Connecting);
        let last_message: RwSignal<Option<Ulid>> = RwSignal::new(None);
        provide_context(WsContext {
            status,
            last_message,
        });

        leptos::task::spawn_local(async move {
            const BACKOFF_MS: [u64; 7] = [500, 1_000, 2_000, 4_000, 8_000, 16_000, 30_000];
            let mut idx = 0usize;
            loop {
                status.set(WsStatus::Connecting);
                let token = match get_local_token() {
                    Some(t) => t,
                    None => {
                        // 没 token 时（例如未登录）：等 2s 后重试，让登录后有机会生效
                        gloo_timers::future::TimeoutFuture::new(2_000).await;
                        continue;
                    }
                };
                if let Err(_e) = connect_once(&token, status, last_message).await {
                    let wait = BACKOFF_MS[idx.min(BACKOFF_MS.len() - 1)];
                    status.set(WsStatus::Disconnected {
                        reason: "ws closed",
                        next_retry_ms: wait,
                    });
                    idx += 1;
                    gloo_timers::future::TimeoutFuture::new(wait.try_into().unwrap()).await;
                } else {
                    idx = 0;
                }
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
fn get_local_token() -> Option<String> {
    web_sys::window()?
        .local_storage()
        .ok()
        .flatten()?
        .get_item("rodeo_jwt")
        .ok()
        .flatten()
}

#[cfg(target_arch = "wasm32")]
async fn connect_once(
    token: &str,
    status: RwSignal<WsStatus>,
    last_message: RwSignal<Option<Ulid>>,
) -> Result<(), String> {
    let loc = web_sys::window().unwrap().location();
    // web-sys 的 protocol() / host() 都返 Result，要解包才能用。
    let proto = match loc.protocol() {
        Ok(p) if p == "https:" => "wss",
        _ => "ws",
    };
    let host = loc.host().unwrap_or_default();
    let url = format!("{}://{}/api/ws?token={}", proto, host, token);

    let ws = WebSocket::new(&url).map_err(|e| format!("ws new: {:?}", e))?;

    // 把 close / error 事件转成 await 信号。`futures-channel::mpsc` 在 wasm32
    // 需要 `target-feature=+atomics` 才能编译（其 mpsc 模块受
    // `#[cfg(target_has_atomic = "ptr")]` 门控），我们不想强加 RUSTFLAGS，
    // 所以改用一个共享的 `Cell<bool>` 关闭标记 + 短间隔轮询：一旦置位就退出。
    //
    // `outcome` 携带 close code：1000 Normal Closure → Ok(())（优雅结束，
    // 重置 backoff）；其它 code（1001 GoingAway / 1006 Abnormal Closure 等）
    // → Err 让外层 backoff 启动。
    let outcome: Rc<RefCell<Option<Result<(), String>>>> = Rc::new(RefCell::new(None));
    let closed: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    let last_message_for_msg = last_message;
    let status_for_msg = status;
    let onmessage: Closure<dyn FnMut(MessageEvent)> = Closure::new(move |ev: MessageEvent| {
        if let Some(text) = ev.data().as_string() {
            match serde_json::from_str::<WsClientMessage>(&text) {
                Ok(WsClientMessage::Hello { .. }) => {
                    status_for_msg.set(WsStatus::Connected);
                }
                Ok(WsClientMessage::NewMessage { message_id }) => {
                    last_message_for_msg.set(Some(message_id));
                }
                Ok(WsClientMessage::Pong) => { /* ignore */ }
                Err(_) => { /* 忽略非 JSON / 异常 */ }
            }
        }
    });
    ws.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
    onmessage.forget();

    let outcome_for_close = outcome.clone();
    let closed_for_close = closed.clone();
    let onclose: Closure<dyn FnMut(CloseEvent)> = Closure::new(move |ev: CloseEvent| {
        let code = ev.code();
        *outcome_for_close.borrow_mut() = Some(if code == 1000 {
            Ok(())
        } else {
            Err("ws closed abnormally".to_string())
        });
        closed_for_close.set(true);
    });
    ws.set_onclose(Some(onclose.as_ref().unchecked_ref()));
    onclose.forget();

    let outcome_for_error = outcome.clone();
    let closed_for_error = closed.clone();
    let onerror: Closure<dyn FnMut(ErrorEvent)> = Closure::new(move |_ev: ErrorEvent| {
        if outcome_for_error.borrow().is_none() {
            *outcome_for_error.borrow_mut() = Some(Err("ws error event".to_string()));
        }
        closed_for_error.set(true);
    });
    ws.set_onerror(Some(onerror.as_ref().unchecked_ref()));
    onerror.forget();

    // 等待 close / error → 重连。50ms 轮询，肉眼看不出延迟；
    // 对比 mpsc channel，CPU 代价可忽略（连接生命周期以秒计）。
    while !closed.get() {
        gloo_timers::future::TimeoutFuture::new(50).await;
    }
    let _ = ws.close();
    // 提早结束 borrow 临时量，避免函数返回表达式与本地变量析构顺序冲突
    // （编译器会要求先 drop Ref 再 drop outcome 这个 Rc）。
    let result = outcome
        .borrow()
        .clone()
        .unwrap_or(Err("ws closed".to_string()));
    result
}
