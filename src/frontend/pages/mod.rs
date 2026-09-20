mod account;
mod admin;
mod entry;
mod login;
mod settings;
mod workspace_main;
mod workspaces;

pub use account::Account;
pub use admin::Admin;
pub use entry::EntryFullScreen;
pub use login::Login;
pub use settings::WorkspaceSettings;
pub use workspace_main::WorkspaceMain;
pub use workspaces::WorkspaceList;

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;

use crate::frontend::components::logged_out;

/// 首页：未登录跳登录，已登录跳工作空间列表。
#[component]
pub fn Home() -> impl IntoView {
    let navigate = use_navigate();
    Effect::new_sync(move |_| {
        if cfg!(target_arch = "wasm32") {
            if logged_out() {
                navigate("/login", Default::default());
            } else {
                navigate("/workspaces", Default::default());
            }
        }
    });
    view! { <div class="page-loading">"加载中…"</div> }
}
