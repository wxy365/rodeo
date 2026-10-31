//! 工作空间切换器：文件夹图标 + 当前工作空间名，hover 弹下拉列出候选工作空间。
//!
//! 交互沿用 `.lang-menu` 那套：`:hover` + `:focus-within` 双触发，无需 JS
//! 切显隐。菜单项点击后做什么由调用方决定——本组件只负责把目标 slug 递出去。
//!
//! 组件是「纯展示 + 一个回调」：谁需要切换工作空间，谁就自己渲染它、并决定
//! 「切换」的语义（直接导航？先拦住未保存的改动？）。它原先挂在 AppBar 上、
//! 靠 App 层的一次性共享槽把数据递过去；现在按钮住工作空间页自己渲染，
//! 父子关系天然成立，那个槽也就没必要了。

use leptos::prelude::*;

use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::{ic_check, ic_folder};

/// 切换器里的一个工作空间条目。
#[derive(Clone, PartialEq, Eq)]
pub struct WorkspaceOption {
    pub slug: String,
    pub name: String,
}

#[component]
pub fn WorkspaceSwitcher(
    /// 当前所在的工作空间——按钮上显示的就是它。
    current: RwSignal<WorkspaceOption>,
    /// 候选工作空间（已剔除软删除的）。
    options: RwSignal<Vec<WorkspaceOption>>,
    /// 点某个候选项时回调，参数是目标 slug。
    ///
    /// 「切换」具体意味着什么由页面自己决定（跨工作空间导航要离开当前条目，
    /// 还得先拦住未保存的改动），本组件只负责把 slug 递出去。
    on_switch: Callback<String>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();

    view! {
        <div class="ws-switch">
            <button class="ws-switch-btn"
                title=move || i18n.t("workspaces.switch_workspace")
                aria-label=move || i18n.t("workspaces.workspace_menu")>
                {ic_folder()}
                <span class="ws-switch-name">{move || current.get().name}</span>
            </button>
            <div class="ws-switch-drop" role="menu">
                {move || options.get().into_iter().map(|opt| {
                    let slug = opt.slug.clone();
                    let name = opt.name.clone();
                    // 「当前项」跟着 `current` 走：切换工作空间后勾选要挪到新的一项，
                    // 不能只在候选列表变化时才重算一次。
                    let is_current = {
                        let current = current;
                        let slug = slug.clone();
                        move || current.get().slug == slug
                    };
                    // 闭包不是 Copy：class 指令与勾号两处各要一份。
                    let is_current_icon = is_current.clone();
                    let on_switch = on_switch;
                    view! {
                        <button class="ws-switch-mi"
                            class:ws-switch-mi-active=move || is_current()
                            role="menuitem"
                            on:click=move |_| on_switch.run(slug.clone())>
                            <span class="ws-switch-mi-name">{name}</span>
                            {move || if is_current_icon() {
                                ic_check().into_any()
                            } else {
                                view! { <span></span> }.into_any()
                            }}
                        </button>
                    }
                }).collect::<Vec<_>>()}
            </div>
        </div>
    }
}
