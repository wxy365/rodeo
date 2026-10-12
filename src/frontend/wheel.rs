//! Shift + 鼠标滚轮 → 横向滚动。
//!
//! `EventTarget::dyn_into::<HtmlElement>()` 的 `JsCast` impl 只在 wasm 目标下存在，
//! 所以这里整段按 wasm32 编译；native 下整个模块为空、调用点也按 cfg 切，闭包
//! 体退化为「仅按 Shift 时 `prevent_default`」。
//!
//! 调用约定：调用方先判断 `ev.shift_key() && ev.delta_y() != 0.0`，命中时
//! `prevent_default` 并调用本函数。本函数只负责把垂直 delta 转到水平
//! `scrollLeft`——非命中分支（默认竖直滚动）由浏览器自己处理。

#[cfg(target_arch = "wasm32")]
pub fn shift_wheel_horizontal(ev: &leptos::ev::WheelEvent) {
    use wasm_bindgen::JsCast;
    if let Some(el) = ev
        .current_target()
        .and_then(|t| t.dyn_into::<leptos::web_sys::HtmlElement>().ok())
    {
        el.set_scroll_left(el.scroll_left() + ev.delta_y() as i32);
    }
}
