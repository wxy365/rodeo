//! AppBar 左侧的「切换工作空间」按钮：文件夹图标 + 当前工作空间名，
//! hover 弹下拉列出候选工作空间。
//!
//! 交互沿用 `.lang-menu` 那套：`:hover` + `:focus-within` 双触发，无需 JS
//! 切显隐。菜单项点击后做什么由页面通过槽里的 `on_switch` 决定——本组件
//! 只负责把目标 slug 递出去。

use leptos::prelude::*;

use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::{ic_check, ic_folder};
use crate::frontend::WorkspaceSwitcherSlot;

#[component]
pub fn WorkspaceSwitcherButton() -> impl IntoView {
    let slot =
        use_context::<WorkspaceSwitcherSlot>().expect("WorkspaceSwitcherSlot 未在 App 层 provide");
    let i18n: I18nController = use_i18n();

    view! {
        // 槽为空 = 当前页面不需要切换器（只有 Entry 全屏页写这个槽）。
        {move || slot.current.get().map(|sw| {
            let current = sw.current;
            let options = sw.options;
            let on_switch = sw.on_switch;
            view! {
                <div class="ws-switch">
                    <button class="ws-switch-btn"
                        title={move || i18n.t("appbar.switch_workspace")}
                        aria-label={move || i18n.t("appbar.workspace_menu")}>
                        {ic_folder()}
                        <span class="ws-switch-name">{move || current.get().name}</span>
                    </button>
                    <div class="ws-switch-drop" role="menu">
                        {move || options.get().into_iter().map(|opt| {
                            let slug = opt.slug.clone();
                            let name = opt.name.clone();
                            let is_current = current.get().slug == opt.slug;
                            let on_switch = on_switch;
                            view! {
                                <button class="ws-switch-mi"
                                    class:ws-switch-mi-active=move || is_current
                                    role="menuitem"
                                    on:click=move |_| on_switch.run(slug.clone())>
                                    <span class="ws-switch-mi-name">{name}</span>
                                    {if is_current { ic_check().into_any() } else { view! { <span></span> }.into_any() }}
                                </button>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                </div>
            }
        })}
    }
}
