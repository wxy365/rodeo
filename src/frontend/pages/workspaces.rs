use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::components::{role_chip_class, role_label, Avatar};
use crate::frontend::graphql_client::{clear_token, create_workspace, workspaces, WorkspaceItem};
use crate::frontend::icons::{ic_add, ic_folder, ic_search};
use crate::frontend::use_auth;

use super::super::components::logged_out;

#[component]
pub fn WorkspaceList() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let data: RwSignal<Option<Result<Vec<WorkspaceItem>, String>>> = RwSignal::new(None);
    let show_create = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let desc = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    let nav_effect = navigate.clone();
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        if logged_out() {
            nav_effect("/login", Default::default());
            return;
        }
        spawn_local(async move {
            data.set(Some(workspaces().await));
        });
    });

    let nav_create = navigate.clone();
    let create = move |ev: SubmitEvent| {
        ev.prevent_default();
        let nav = nav_create.clone();
        let n = name.get();
        let d = desc.get();
        spawn_local(async move {
            match create_workspace(&n, &d).await {
                Ok(ws) => nav(&format!("/{}", ws.slug), Default::default()),
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let nav_logout = navigate.clone();
    let logout = move |_| {
        clear_token();
        auth.user.set(None);
        nav_logout("/login", Default::default());
    };

    view! {
        <div class="page">
            <div class="appbar">
                <b style="font-size:17px">"Rodeo"</b>
                <label class="inp">
                    {ic_search()}
                    <input placeholder="全文检索：标题 / 详情 / 标签值（即将上线）" disabled />
                </label>
                <span style="margin-left:auto" class="online">
                    {move || {
                        let display = auth.user.get().map(|u| {
                            if u.name.is_empty() { u.email.clone() } else { u.name.clone() }
                        }).unwrap_or_default();
                        view! { <Avatar text=display /> }
                    }}
                </span>
                <button class="btn" on:click=logout>"退出"</button>
            </div>

            <div style="display:flex;align-items:center;margin-bottom:16px">
                <h2 style="font-size:20px;font-weight:500">"我的工作空间"</h2>
                <button class="btn pri" style="margin-left:auto" on:click=move |_| show_create.set(!show_create.get())>
                    {ic_add()}
                    "新建工作空间"
                </button>
            </div>

            {move || if show_create.get() {
                view! {
                    <form class="panel ws-card" style="margin-bottom:16px;gap:12px" on:submit=create.clone()>
                        <input class="inp" placeholder="名称" prop:value=name on:input=move |ev| name.set(event_target_value(&ev)) />
                        <input class="inp" placeholder="描述（可选）" prop:value=desc on:input=move |ev| desc.set(event_target_value(&ev)) />
                        <div style="display:flex;gap:8px">
                            <button class="btn pri" type="submit">"创建"</button>
                            <button class="btn" type="button" on:click=move |_| show_create.set(false)>"取消"</button>
                        </div>
                    </form>
                }.into_any()
            } else {
                view! { <div></div> }.into_any()
            }}

            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}

            <div class="ws-grid">
                {move || match data.get() {
                    None => view! { <div class="empty">"加载中…"</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="empty error">{e.clone()}</div> }.into_any(),
                    Some(Ok(list)) => view! {
                        {list.iter().map(|w| {
                            let slug = w.workspace.slug.clone();
                            let nav = navigate.clone();
                            let card = w.workspace.clone();
                            view! {
                                <div class="panel ws-card" style="cursor:pointer" on:click=move |_| nav(&format!("/{}", slug), Default::default())>
                                    <h3>{ic_folder()}{card.name.clone()}</h3>
                                    <span class="code">{card.slug.clone()}</span>
                                    <div class="mut">{card.description.clone()}</div>
                                    <div>
                                        <span class=format!("chip {}", role_chip_class(&w.role))>{role_label(&w.role)}</span>
                                    </div>
                                </div>
                            }
                        }).collect::<Vec<_>>()}
                        <div class="panel ws-card" style="border-style:dashed;cursor:pointer;min-height:132px;align-items:center;justify-content:center" on:click=move |_| show_create.set(true)>
                            <span class="mut" style="display:flex;gap:8px;align-items:center">{ic_add()}"新建工作空间（自动成为 Owner）"</span>
                        </div>
                    }.into_any(),
                }}
            </div>
        </div>
    }
}
