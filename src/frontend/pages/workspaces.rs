use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::components::{role_chip_class, role_label, short_time};
use crate::frontend::graphql_client::{
    accept_invite, create_workspace, decline_invite, my_invites, restore_workspace, workspaces,
    Invite, WorkspaceItem,
};
use crate::frontend::icons::{ic_add, ic_folder, ic_history};
use crate::frontend::use_auth;

use super::super::components::logged_out;

#[component]
pub fn WorkspaceList() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let data: RwSignal<Option<Result<Vec<WorkspaceItem>, String>>> = RwSignal::new(None);
    // 收到但未接受的邀请。接受后才会出现在下面的工作空间卡片里。
    let inbox: RwSignal<Vec<Invite>> = RwSignal::new(Vec::new());
    let show_create = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let desc = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let refresh = RwSignal::new(0u32);

    let nav_effect = navigate.clone();
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        // 令牌已被服务端判死时同样回登录页：`logged_out()` 只看本地有没有令牌，
        // 死令牌在启动期被清掉之前它是看不出来的。
        if logged_out() || auth.session_lost.get() {
            nav_effect("/login", Default::default());
            return;
        }
        let _ = refresh.get();
        spawn_local(async move {
            data.set(Some(workspaces().await));
            inbox.set(my_invites().await.unwrap_or_default());
        });
    });

    let restore = Callback::new(move |id: String| {
        spawn_local(async move {
            match restore_workspace(&id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    let nav_accept = navigate.clone();
    let accept = Callback::new(move |(ws_id, slug): (String, String)| {
        let nav = nav_accept.clone();
        spawn_local(async move {
            match accept_invite(&ws_id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                    // 加入成功直接进工作空间，省得用户在列表里再找一遍。
                    nav(&format!("/{}", slug), Default::default());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    let decline = Callback::new(move |ws_id: String| {
        spawn_local(async move {
            match decline_invite(&ws_id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
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

    let nav_grid = navigate.clone();
    let nav_trash = navigate.clone();

    view! {
        <div class="page">
            // 顶部 appbar 取消：logo 现在挂在 SideNav 顶部，搜索框未上线也跟着砍掉。
            // 留给未来真有全文检索能力时再做——避免现在挂一个永远 disabled 的输入框招点。
            <div style="display:flex;align-items:center;margin-bottom:16px">
                <h2 style="font-size:20px;font-weight:500">"我的工作空间"</h2>
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

            // 「我的工作空间」+ 「邀请收件箱」合在同一栅格里。
            // 邀请卡片用虚线 + 「邀请」角标与正式卡片区分；接受 / 拒绝仍随卡片就地操作。
            <div class="ws-grid">
                {move || match data.get() {
                    None => view! { <div class="empty">"加载中…"</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="empty error">{e.clone()}</div> }.into_any(),
                    Some(Ok(list)) => {
                        let live: Vec<_> = list
                            .into_iter()
                            .filter(|w| w.workspace.deleted_at.is_none())
                            .collect();
                        view! {
                            // 邀请卡片：与正式卡片同栅格、视觉用虚线 / 「邀请」角标区分。
                            // 接受 / 拒绝按钮就地放在卡片上，避免在页面顶部再来一条横条收件箱。
                            {inbox.get().into_iter().map(|inv| {
                                let accept_id = inv.workspace_id.clone();
                                let slug = inv.workspace_slug.clone();
                                let decline_id = inv.workspace_id.clone();
                                view! {
                                    <div class="panel ws-card ws-invite">
                                        <h3>{ic_folder()}<span class="invite-tag">"邀请"</span>{inv.workspace_name.clone()}</h3>
                                        <span class="code">{inv.workspace_slug.clone()}</span>
                                        <div class="mut">
                                            {format!("{} 邀请你以 {} 身份加入",
                                                inv.invited_by.trim().is_empty().then(|| inv.email.clone()).unwrap_or_else(|| inv.invited_by.clone()),
                                                role_label(&inv.role)
                                            )}
                                        </div>
                                        <div style="display:flex;gap:8px;align-items:center;margin-top:auto">
                                            <span class=format!("chip {}", role_chip_class(&inv.role))>{role_label(&inv.role)}</span>
                                            <button class="btn pri sm" style="margin-left:auto"
                                                on:click=move |_| accept.run((accept_id.clone(), slug.clone()))>"接受"</button>
                                            <button class="btn sm"
                                                on:click=move |_| decline.run(decline_id.clone())>"拒绝"</button>
                                        </div>
                                    </div>
                                }
                            }).collect::<Vec<_>>()}
                            {live.into_iter().map(|w| {
                                let slug = w.workspace.slug.clone();
                                let nav = nav_grid.clone();
                                let card = w.workspace;
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
                        }.into_any()
                    }
                }}
            </div>

            // 回收站：只列自己有 Owner 权限的已删除工作空间（软删除的数据仍在，可恢复）。
            {move || {
                let trashed: Vec<_> = data
                    .get()
                    .and_then(|r| r.ok())
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|w| w.workspace.deleted_at.is_some() && w.role == "owner")
                    .collect();
                if trashed.is_empty() {
                    return ().into_any();
                }
                let nav = nav_trash.clone();
                view! {
                    <div class="ws-trash-head">
                        {ic_history()}<b>"回收站"</b>
                        <span class="mut">"已删除的工作空间，数据保留，可恢复。"</span>
                    </div>
                    <div class="ws-grid">
                        {trashed.into_iter().map(|w| {
                            let slug = w.workspace.slug.clone();
                            let nav = nav.clone();
                            let card = w.workspace;
                            let id = card.id.clone();
                            let deleted = card.deleted_at.clone().unwrap_or_default();
                            view! {
                                <div class="panel ws-card ws-deleted">
                                    <h3 style="cursor:pointer" on:click=move |_| nav(&format!("/{}", slug), Default::default())>
                                        {ic_folder()}{card.name.clone()}
                                    </h3>
                                    <span class="code">{card.slug.clone()}</span>
                                    <div class="mut">{format!("删除于 {}", short_time(&deleted))}</div>
                                    <div style="display:flex;gap:8px;align-items:center">
                                        <span class=format!("chip {}", role_chip_class(&w.role))>{role_label(&w.role)}</span>
                                        <button class="btn sm" on:click=move |_| restore.run(id.clone())>"恢复"</button>
                                    </div>
                                </div>
                            }
                        }).collect::<Vec<_>>()}
                    </div>
                }.into_any()
            }}
        </div>
    }
}
