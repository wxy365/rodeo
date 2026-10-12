use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::components::{role_chip_class, role_label, short_time};
use crate::frontend::graphql_client::{
    accept_invite, create_workspace, decline_invite, my_invites, restore_workspace, workspaces,
    Invite, WorkspaceItem,
};
use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::{ic_add, ic_folder, ic_history, ic_search};
use crate::frontend::use_auth;

use super::super::components::logged_out;

#[component]
pub fn WorkspaceList() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let i18n: I18nController = use_i18n();
    let data: RwSignal<Option<Result<Vec<WorkspaceItem>, String>>> = RwSignal::new(None);
    // 收到但未接受的邀请。接受后才会出现在下面的工作空间卡片里。
    let inbox: RwSignal<Vec<Invite>> = RwSignal::new(Vec::new());
    let show_create = RwSignal::new(false);
    let name = RwSignal::new(String::new());
    let desc = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let refresh = RwSignal::new(0u32);
    // 顶部搜索框当前输入。按 name / slug / description 做客户端子串过滤——
    // 工作空间列表通常几十条，客户端过滤不需要搜索后端，也避免每键一次 roundtrip。
    let filter_text = RwSignal::new(String::new());

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
            // 顶栏（logo + 全局操作）现在由 `<AppBar/>` 统一渲染，不在本页面画一遍。
            // 这里的「我的工作空间」标题行只承担页面级 H1 与右侧搜索框——后者在 spec §14
            // 里被定成「全文检索本视图」的占位，未来服务端接通后用同一个 query bar 即可。
            <div style="display:flex;align-items:center;margin-bottom:16px;gap:12px">
                <h2 style="font-size:20px;font-weight:500;margin:0">{move || i18n.t("workspaces.title")}</h2>
                <label class="ws-search">
                    {ic_search()}
                    <input type="search" aria-label={move || i18n.t("workspaces.search")}
                        prop:value=filter_text
                        on:input=move |ev| filter_text.set(event_target_value(&ev)) />
                </label>
            </div>

            {move || if show_create.get() {
                view! {
                    <form class="panel ws-card" style="margin-bottom:16px;gap:12px" on:submit=create.clone()>
                        <input class="inp" placeholder={move || i18n.t("workspaces.name")} prop:value=name on:input=move |ev| name.set(event_target_value(&ev)) />
                        <input class="inp" placeholder={move || i18n.t("workspaces.desc")} prop:value=desc on:input=move |ev| desc.set(event_target_value(&ev)) />
                        <div style="display:flex;gap:8px">
                            <button class="btn pri" type="submit">{move || i18n.t("common.create")}</button>
                            <button class="btn" type="button" on:click=move |_| show_create.set(false)>{move || i18n.t("common.cancel")}</button>
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
                    None => view! { <div class="empty">{move || i18n.t("workspaces.loading")}</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="empty error">{e.clone()}</div> }.into_any(),
                    Some(Ok(list)) => {
                        let live_raw: Vec<_> = list
                            .into_iter()
                            .filter(|w| w.workspace.deleted_at.is_none())
                            .collect();
                        // 客户端子串过滤：trim + lowercase 后对 name / slug / description 做 contains。
                        // CJK 不需要 ngram——列表通常远小于索引条目，contains 已经足够。
                        let query = filter_text.get().trim().to_lowercase();
                        let live_filtered: Vec<_> = if query.is_empty() {
                            live_raw.clone()
                        } else {
                            live_raw
                                .iter()
                                .filter(|w| {
                                    w.workspace.name.to_lowercase().contains(&query)
                                        || w.workspace.slug.to_lowercase().contains(&query)
                                        || w.workspace.description.to_lowercase().contains(&query)
                                })
                                .cloned()
                                .collect()
                        };
                        let inbox_now = inbox.get();
                        // 真·空状态：没有任何工作空间 + 没有任何邀请。
                        // 普通状态下网格外已经有那张小 dashed CTA，但用户初次进入时小卡夹
                        // 在栅格里和正式卡片并列不显眼——hero 变体在这里给它一个独立的"邀请位"。
                        let truly_empty = live_raw.is_empty() && inbox_now.is_empty();
                        if truly_empty {
                            return view! {
                                <div class="panel ws-card ws-hero">
                                    {ic_folder()}
                                    <h3>{move || i18n.t("workspaces.hero_title")}</h3>
                                    <div class="mut">
                                        {move || i18n.t("workspaces.hero_desc")}
                                    </div>
                                    <button class="btn pri" style="align-self:flex-start;margin-top:auto"
                                        on:click=move |_| show_create.set(true)>
                                        {ic_add()}<span>{move || i18n.t("workspaces.new_workspace")}</span>
                                    </button>
                                </div>
                            }.into_any();
                        }
                        // 过滤后无命中：用户输了字但没匹配到任何工作空间——
                        // 在邀请卡之后、`+` CTA 之前插一行提示，CTA 与邀请仍保留。
                        let no_match = !query.is_empty() && live_filtered.is_empty();
                        view! {
                            // 邀请卡片：与正式卡片同栅格、视觉用虚线 / 「邀请」角标区分。
                            // 接受 / 拒绝按钮就地放在卡片上，避免在页面顶部再来一条横条收件箱。
                            {inbox_now.into_iter().map(|inv| {
                                let accept_id = inv.workspace_id.clone();
                                let slug = inv.workspace_slug.clone();
                                let decline_id = inv.workspace_id.clone();
                                let role_str = role_label(&inv.role, &i18n);
                                let inviter = inv.invited_by.trim().is_empty().then(|| inv.email.clone()).unwrap_or_else(|| inv.invited_by.clone());
                                view! {
                                    <div class="panel ws-card ws-invite">
                                        <h3>{ic_folder()}<span class="invite-tag">{move || i18n.t("workspaces.invite_tag")}</span>{inv.workspace_name.clone()}</h3>
                                        <span class="code">{inv.workspace_slug.clone()}</span>
                                        <div class="mut">
                                            {let role_for_invite = role_str.clone(); move || i18n.tf("workspaces.invited_by", &[("inviter", &inviter), ("role", &role_for_invite)])}
                                        </div>
                                        <div style="display:flex;gap:8px;align-items:center;margin-top:auto">
                                            <span class=format!("chip {}", role_chip_class(&inv.role))>{role_str.clone()}</span>
                                            <button class="btn pri sm" style="margin-left:auto"
                                                on:click=move |_| accept.run((accept_id.clone(), slug.clone()))>{move || i18n.t("workspaces.accept")}</button>
                                            <button class="btn sm"
                                                on:click=move |_| decline.run(decline_id.clone())>{move || i18n.t("workspaces.decline")}</button>
                                        </div>
                                    </div>
                                }
                            }).collect::<Vec<_>>()}
                            {move || if no_match {
                                Some(view! { <div class="empty">{move || i18n.t("workspaces.no_match")}</div> }.into_any())
                            } else {
                                None
                            }}
                            {live_filtered.into_iter().map(|w| {
                                let slug = w.workspace.slug.clone();
                                let nav = nav_grid.clone();
                                let card = w.workspace;
                                view! {
                                    <div class="panel ws-card" style="cursor:pointer" on:click=move |_| nav(&format!("/{}", slug), Default::default())>
                                        <h3>{ic_folder()}{card.name.clone()}</h3>
                                        <span class="code">{card.slug.clone()}</span>
                                        <div class="mut">{card.description.clone()}</div>
                                        <div>
                                            <span class=format!("chip {}", role_chip_class(&w.role))>{role_label(&w.role, &i18n)}</span>
                                        </div>
                                    </div>
                                }
                            }).collect::<Vec<_>>()}
                            <div class="panel ws-card" style="border-style:dashed;cursor:pointer;min-height:132px;align-items:center;justify-content:center" on:click=move |_| show_create.set(true)>
                                <span class="mut" style="display:flex;gap:8px;align-items:center">{ic_add()}{move || i18n.t("workspaces.new_workspace_owner")}</span>
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
                        {ic_history()}<b>{move || i18n.t("workspaces.trash")}</b>
                        <span class="mut">{move || i18n.t("workspaces.trash_desc")}</span>
                    </div>
                    <div class="ws-grid">
                        {trashed.into_iter().map(|w| {
                            let slug = w.workspace.slug.clone();
                            let nav = nav.clone();
                            let card = w.workspace;
                            let id = card.id.clone();
                            let deleted = card.deleted_at.clone().unwrap_or_default();
                            let time_str = short_time(&deleted);
                            view! {
                                <div class="panel ws-card ws-deleted">
                                    <h3 style="cursor:pointer" on:click=move |_| nav(&format!("/{}", slug), Default::default())>
                                        {ic_folder()}{card.name.clone()}
                                    </h3>
                                    <span class="code">{card.slug.clone()}</span>
                                    <div class="mut">{move || i18n.tf("workspaces.deleted_at", &[("time", &time_str)])}</div>
                                    <div style="display:flex;gap:8px;align-items:center">
                                        <span class=format!("chip {}", role_chip_class(&w.role))>{role_label(&w.role, &i18n)}</span>
                                        <button class="btn sm" on:click=move |_| restore.run(id.clone())>{move || i18n.t("workspaces.restore")}</button>
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
