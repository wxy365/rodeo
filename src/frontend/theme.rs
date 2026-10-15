//! 主题（亮 / 暗）状态。
//!
//! 三档：`Light` / `Dark` / `System`。`System` 是默认——颜色交给 CSS 里
//! `@media (prefers-color-scheme: dark)`；`Light` / `Dark` 通过在 `<html>`
//! 上挂 `data-theme="light" | "dark"` 覆盖系统偏好。
//!
//! 持久化：存到 `localStorage.rodeo_theme`（值 "light" / "dark" / "system"）。
//! SSR 期间不读 localStorage——首屏靠 CSS @media 系统色，已能给出正确色；
//! hydrate 后第一帧才把用户偏好同步到 DOM。
//!
//! FOUC 防御：`<html data-theme>` 由 `shell()` 里的内联脚本在首屏 CSS 解析
//! 之前就打好位——这套 CSS 故意把 `:root[data-theme="..."]` 写在
//! `@media (prefers-color-scheme: dark)` 之后，所以一旦挂了属性，系统 @media
//! 就被压住。inline 脚本必须早于 `HashedStylesheet`，否则会有几毫秒的色闪。

use leptos::prelude::*;

use super::i18n::{use_i18n, I18nController};
use super::icons::{ic_moon, ic_sun};

/// 主题模式。
///
/// `System` 跟随操作系统；`Light` / `Dark` 由用户硬选，CSS 端的 `data-theme`
/// 优先级永远最高。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeMode {
    System,
    Light,
    Dark,
}

impl ThemeMode {
    /// 从字符串解析（localStorage 反序列化）。解析失败一律当 System。
    pub fn parse(s: &str) -> Self {
        match s {
            "light" => ThemeMode::Light,
            "dark" => ThemeMode::Dark,
            _ => ThemeMode::System,
        }
    }

    /// 序列化为字符串。System 是默认值——存 / 不存都行；这里存是为了排查时
    /// 一眼看出「用户没改过」，而不是从 key 缺失推断。
    pub fn as_str(&self) -> &'static str {
        match self {
            ThemeMode::System => "system",
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }
}

/// 当前主题 + 切换函数。
///
/// `provide_theme` 在 App 层 provide；任何组件通过 `use_theme()` 拿到。
/// 切换函数直接读写 `data-theme` 属性与 localStorage，并触发重渲。
#[derive(Clone, Copy)]
pub struct ThemeController {
    pub mode: RwSignal<ThemeMode>,
}

impl ThemeController {
    /// 切换到下一档：System → Light → Dark → System。
    /// 循环顺序按使用频次排——大多数用户的「默认 → 想要」路径只需点一次。
    pub fn cycle(&self) {
        let next = match self.mode.get_untracked() {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        };
        self.set(next);
    }

    /// 显式设定：写信号 + 写 `<html data-theme>` + 写 localStorage。
    /// 三个副作用在同一个 set() 里完成，保证观察者从外部看是原子的——
    /// 否则读到「新 mode + 旧 DOM 属性」的状态会出现几毫秒的错色。
    pub fn set(&self, mode: ThemeMode) {
        apply_to_dom(mode);
        write_storage(mode);
        self.mode.set(mode);
    }
}

pub fn provide_theme() -> ThemeController {
    let ctrl = ThemeController {
        mode: RwSignal::new(read_storage()),
    };
    provide_context(ctrl);
    ctrl
}

pub fn use_theme() -> ThemeController {
    use_context::<ThemeController>().expect("ThemeController 未提供")
}

/// 把当前 mode 推到 `<html data-theme>`：System 时清掉属性（让 @media 起作用），
/// 显式两档则挂上对应值。
fn apply_to_dom(mode: ThemeMode) {
    let value: Option<&'static str> = match mode {
        ThemeMode::System => None,
        ThemeMode::Light => Some("light"),
        ThemeMode::Dark => Some("dark"),
    };
    apply_data_theme(value);
}

#[cfg(target_arch = "wasm32")]
fn apply_data_theme(value: Option<&str>) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(doc) = window.document() else { return };
    let Some(html) = doc.document_element() else {
        return;
    };
    match value {
        Some(v) => {
            html.set_attribute("data-theme", v).ok();
        }
        None => {
            html.remove_attribute("data-theme").ok();
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn apply_data_theme(_value: Option<&str>) {
    // SSR 阶段没 DOM 可写；CSS 在 hydrate 时一次性接管即可。
}

#[cfg(target_arch = "wasm32")]
fn write_storage(mode: ThemeMode) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return;
    };
    let _ = storage.set_item("rodeo_theme", mode.as_str());
}

#[cfg(not(target_arch = "wasm32"))]
fn write_storage(_mode: ThemeMode) {}

/// 读 localStorage 返回 mode。仅在 hydrate 时调一次（启动默认值）。
#[cfg(target_arch = "wasm32")]
pub fn read_storage() -> ThemeMode {
    let Some(window) = web_sys::window() else {
        return ThemeMode::System;
    };
    let Ok(Some(storage)) = window.local_storage() else {
        return ThemeMode::System;
    };
    let Ok(Some(s)) = storage.get_item("rodeo_theme") else {
        return ThemeMode::System;
    };
    ThemeMode::parse(&s)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_storage() -> ThemeMode {
    ThemeMode::System
}

/// 主题按钮：在标题栏显示太阳 / 月亮 / 自动三态循环。
///
/// 按一下走一档；`System` 档下显示「左半暗 + 右半亮」组合图标，明示
/// 「这是跟系统走的，不是我定的」——避免用户疑惑「我点过了怎么还是亮色」。
#[component]
pub fn ThemeButton() -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let ctrl = use_theme();
    let mode = ctrl.mode;

    view! {
        <button class="theme-btn"
            title=move || match mode.get() {
                ThemeMode::System => i18n.t("theme.title_system"),
                ThemeMode::Light => i18n.t("theme.title_light"),
                ThemeMode::Dark => i18n.t("theme.title_dark"),
            }
            aria-label=move || i18n.t("theme.aria")
            on:click=move |_| ctrl.cycle()>
            {move || match mode.get() {
                ThemeMode::System => view! {
                    // 「左半暗 + 右半亮」组合图标：实心半圆 + 描边半圆并排。
                    // 实心在左表示「系统现在偏暗」，描边在左则是亮。
                    // 用单一 path 同时表达两半即可——fill-rule 拆开。
                    <svg class="ic" viewBox="0 0 24 24" aria-hidden="true">
                        <path d="M12 4C7.58172 4 4 7.58172 4 12C4 16.4183 7.58172 20 12 20V4Z"/>
                        <path d="M12 4V20C16.4183 20 20 16.4183 20 12C20 7.58172 16.4183 4 12 4Z"
                            fill="none" stroke="currentColor" stroke-width="1.2"/>
                        <circle cx="12" cy="3" r="1" fill="currentColor"/>
                        <circle cx="12" cy="21" r="1" fill="currentColor"/>
                        <circle cx="3" cy="12" r="1" fill="currentColor"/>
                        <circle cx="21" cy="12" r="1" fill="currentColor"/>
                    </svg>
                }.into_any(),
                ThemeMode::Light => ic_sun().into_any(),
                ThemeMode::Dark => ic_moon().into_any(),
            }}
        </button>
    }
}
