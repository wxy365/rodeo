//! AppBar 语言切换按钮：地球图标 + 当前 locale 短码，点开下拉六选项。
//!
//! 复用 `.theme-btn` 的视觉重量；菜单用 `:hover` + `:focus-within` 双触发，
//! 与 `AvatarMenu` 同款（无需 JS 切换显隐）。
//!
//! `i18n.set(loc)` 四件事同步：更新控制器信号 + 写 `localStorage.rodeo_lang` +
//! 写 `rodeo_lang` cookie + 改 `<html lang/dir>`。下次 SSR 从 cookie 回来同一个
//! locale，下次 hydrate 从 `<html lang>` 拿到同一个 locale——hydrate 不再退回
//! 硬编码的英文。

use leptos::prelude::*;

use crate::frontend::i18n::{use_i18n, Locale};
use crate::frontend::icons::ic_globe;

#[component]
pub fn LanguageButton() -> impl IntoView {
    let i18n = use_i18n();
    // 复制一份 locale 信号，避免后面闭包 move 走 i18n。
    let locale_sig = i18n.locale;

    view! {
        <div class="lang-menu">
            <button class="lang-btn"
                title={move || i18n.t("appbar.language")}
                aria-label={move || i18n.t("appbar.language_menu")}>
                {ic_globe()}
                <span class="lang-btn-cur">{move || locale_sig.get().short_label()}</span>
            </button>
            <div class="lang-drop" role="menu">
                {Locale::ALL.iter().map(|&loc| {
                    let i18n = i18n;
                    let cur = locale_sig;
                    let label = loc.label();
                    view! {
                        <button class="lang-mi"
                            class:lang-mi-active={move || cur.get() == loc}
                            role="menuitem"
                            on:click=move |_| i18n.set(loc)>
                            {label}
                        </button>
                    }
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}
