//! 分享面板的 clipboard 助手：wasm 才有 window / clipboard 概念。
//!
//! `native`（SSR）侧：复制按钮永远不挂——它整段被 cfg 切走；保留一个空的
//! stub 函数签名，避免父组件分支。这样 `cargo check --features ssr` 与
//! `cargo check --target wasm32-unknown-unknown` 都能直接编译。

use leptos::prelude::RwSignal;

#[cfg(target_arch = "wasm32")]
use leptos::prelude::{Get, Set};

/// 把分享 URL 写入剪贴板，并把 `copied_id` 切到当前 share_id。
/// 2 秒后切回 `None`，按钮恢复成「Copy link」。
///
/// `Navigator::clipboard()` 与 `Clipboard::write_text()` 在 web-sys 0.3 里
/// 都是 `unsafe`-free 的同步 extern，返回值是 `Clipboard` 与 `::js_sys::Promise`，
/// 所以不用 `.ok()` / `if let Some`——只在外部用 `if let Some(win)` 兜底
/// 「不在浏览器里」的唯一失败模式（SSR 头一次 hydrate 之前的早期调用）。
#[cfg(target_arch = "wasm32")]
pub fn copy_share_url(sid: String, url: String, copied_id: RwSignal<Option<String>>) {
    if let Some(win) = web_sys::window() {
        let navigator = win.navigator();
        let clipboard = navigator.clipboard();
        // write_text 返回的 Promise 在主线程上自行驱动；用户点击手势
        // 不会跨 await，所以这里 fire-and-forget。
        let _ = clipboard.write_text(&url);
        copied_id.set(Some(sid.clone()));
        let copied = copied_id.clone();
        let sid_for_reset = sid;
        leptos::task::spawn_local(async move {
            gloo_timers::future::TimeoutFuture::new(2000).await;
            if copied.get().as_deref() == Some(sid_for_reset.as_str()) {
                copied.set(None);
            }
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn copy_share_url(_sid: String, _url: String, _copied_id: RwSignal<Option<String>>) {}