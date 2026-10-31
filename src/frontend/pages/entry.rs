use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map, use_query_map};

use crate::frontend::attachment_list::AttachmentList;
use crate::frontend::comment_list::CommentList;
use crate::frontend::components::{fmt_datetime, logged_out, AuditTimeline, CodeCopy, TabBar};
use crate::frontend::graphql_client::{
    audit_logs, build_share_url, create_entry_share, delete_entry, entry, entry_next_in_view,
    entry_shares, label_schemas, members, revoke_entry_share, update_entry, workspace_by_slug,
    workspaces, AccountBrief, AuditLog, Entry, EntryShare, LabelSchema, Labeling, Member,
    Workspace,
};
use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::{ic_back, ic_check, ic_close, ic_share, ic_tag};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::relations_panel::RelationsPanel;
use crate::frontend::rich_editor::RichEditor;
use crate::frontend::share_clipboard::copy_share_url;
use crate::frontend::use_auth;
use crate::frontend::workspace_switcher::{WorkspaceOption, WorkspaceSwitcher};

// 全屏页侧栏的页签。标签编辑不在这里——它常驻在页签上方。
fn side_tab_labels(i18n: &I18nController) -> [(String, String); 3] {
    [
        ("relations".to_string(), i18n.t("entry.side_relations")),
        ("attachments".to_string(), i18n.t("entry.side_attachments")),
        ("history".to_string(), i18n.t("entry.side_history")),
    ]
}

#[component]
pub fn EntryFullScreen() -> impl IntoView {
    let params = use_params_map();
    // 用 `params.get()`（非 get_untracked）让 slug/code 跟着 :slug/:code 路由变化；
    // Effect::new_sync(load) 通过 load → slug/code 这条链建立依赖，
    // 路由参数变（比如点「下一个」跳到新条目）就会重新跑 load(true)，data/title/next_entry 才不会留在旧条目上。
    let slug = move || params.get().get("slug").unwrap_or_default();
    let code = move || params.get().get("code").unwrap_or_default();
    let navigate = use_navigate();
    let auth = use_auth();
    let i18n: I18nController = use_i18n();

    let data: RwSignal<
        Option<
            Result<
                (
                    Workspace,
                    Entry,
                    Vec<LabelSchema>,
                    Vec<AuditLog>,
                    Vec<Member>,
                ),
                String,
            >,
        >,
    > = RwSignal::new(None);
    let schemas = RwSignal::new(Vec::<LabelSchema>::new());
    let labels = RwSignal::new(Vec::<Labeling>::new());
    let ws_members = RwSignal::new(Vec::<Member>::new());
    let title = RwSignal::new(String::new());
    let detail = RwSignal::new(String::new());
    // 分享面板：默认收起，「Share」按钮第一次点开后落地。`share_loading` 用来
    // 在拉取 share 列表时灰掉触发按钮，避免重复请求。
    let show_share_panel = RwSignal::new(false);
    let share_loading = RwSignal::new(false);
    // 「当前编辑态 vs 已加载的条目」的差异。`data` 里的条目就是服务端最新版本
    // （保存成功后会被就地替换），所以无差异 ⇔ 没改动。
    // 不用一个 `saved` 布尔：它初值只能是 false，页面一加载就显示「未保存」，语义是错的。
    // 标题比较必须带 trim：服务端存/返回前会 trim（service/entry.rs），若不加，
    // 用户输入 "abc " 存回 "abc"，两者永不相等，dirty 卡在 true、按钮再也不置灰。
    // 别「简化」掉这个 trim，也别改成回头 set 缓冲——那会动到用户正在打的字。
    let dirty = Signal::derive(move || {
        data.get()
            .and_then(|r| r.ok())
            .is_some_and(|(_, e, _, _, _)| {
                e.title != title.get().trim() || e.detail != detail.get()
            })
    });
    let error = RwSignal::new(None::<String>);
    // 标题平时只读，点击才换成输入框；输入框挂载后由 Effect 补焦点。
    let editing_title = RwSignal::new(false);
    let title_ref: NodeRef<Input> = NodeRef::new();
    // 侧栏页签：attachments | history。
    let side_tab = RwSignal::new("attachments".to_string());
    // 审计日志另存一份供 `AuditTimeline` 消费；`data` 元组里的那份无人读取，仅因元组结构固定而保留。
    let logs = RwSignal::new(Vec::<AuditLog>::new());

    let load = move |overwrite: bool| {
        let s = slug();
        let c = code();
        if c.is_empty() {
            return;
        }
        if overwrite {
            title.set(String::new());
            detail.set(String::new());
            data.set(None);
            error.set(None);
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = async {
                    let ws = workspace_by_slug(&s)
                        .await?
                        .ok_or(i18n.t("entry.workspace_missing"))?;
                    let e = entry(&c).await?.ok_or(i18n.t("entry.entry_missing"))?;
                    let schema_list = label_schemas(&ws.id).await?;
                    let logs = audit_logs(&ws.id).await?;
                    // 成员表只为 Account 型标签的选择器服务，取不到就退化成空列表。
                    let member_list = members(&ws.id).await.unwrap_or_default();
                    Ok::<_, String>((ws, e, schema_list, logs, member_list))
                }
                .await;
                if let Ok((_, ref e, ref list, ref audit, ref member_list)) = result {
                    schemas.set(list.clone());
                    labels.set(e.labels.clone());
                    ws_members.set(member_list.clone());
                    logs.set(audit.clone());
                    if overwrite {
                        title.set(e.title.clone());
                        detail.set(e.detail.clone());
                        side_tab.set("attachments".to_string());
                    }
                }
                data.set(Some(result));
            });
        }
    };

    // 评论组件要按工作空间查当前用户角色；data 里的 workspace 是唯一来源。
    let ws_id = Signal::derive(move || {
        data.get()
            .and_then(|r| r.ok())
            .map(|(w, _, _, _, _)| w.id)
            .unwrap_or_default()
    });

    let nav_redirect = navigate.clone();
    Effect::new_sync(move |_| {
        if cfg!(target_arch = "wasm32") && (logged_out() || auth.session_lost.get()) {
            nav_redirect("/login", Default::default());
            return;
        }
        load(true);
    });

    // 「切换工作空间」按钮的候选列表：进页面拉一次即可——本页期间工作空间的
    // 成员关系不会变（新建 / 加入工作空间都会离开本页再进来）。
    let ws_options: RwSignal<Vec<WorkspaceOption>> = RwSignal::new(Vec::new());
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        spawn_local(async move {
            // 软删除的工作空间只出现在工作空间列表页的回收站里，不进候选。
            let list = workspaces().await.unwrap_or_default();
            ws_options.set(
                list.into_iter()
                    .filter(|w| w.workspace.deleted_at.is_none())
                    .map(|w| WorkspaceOption {
                        slug: w.workspace.slug,
                        name: w.workspace.name,
                    })
                    .collect(),
            );
        });
    });
    // 当前工作空间要等 `data` 加载完才知道（slug → ws 的映射在服务端）。
    let ws_current: RwSignal<WorkspaceOption> = RwSignal::new(WorkspaceOption {
        slug: String::new(),
        name: String::new(),
    });
    Effect::new_sync(move |_| {
        let Some(ws) = data.get().and_then(|r| r.ok()).map(|(w, _, _, _, _)| w) else {
            return;
        };
        ws_current.set(WorkspaceOption {
            slug: ws.slug.clone(),
            name: ws.name.clone(),
        });
    });

    let on_changed = Callback::new(move |_| load(false));
    let on_editor_change = Callback::new(move |d: String| {
        detail.set(d);
    });

    // 进入标题编辑态后把焦点交给输入框，否则用户还得再点一次。
    #[cfg(target_arch = "wasm32")]
    Effect::new(move |_| {
        if editing_title.get() {
            if let Some(el) = title_ref.get() {
                let _ = el.focus();
            }
        }
    });

    let do_save: Callback<()> = Callback::new(move |_| {
        // 无改动就不写：服务端即便 title/detail 未变也会 bump updated_at 并追加一条审计，
        // 而 updated_at 是乐观并发比的令牌。按钮那边由 disabled 挡住，快捷键这条路径得自己挡。
        if !dirty.get() {
            return;
        }
        let c = code();
        let Some(e) = data.get().and_then(|r| r.ok()).map(|(_, e, _, _, _)| e) else {
            return;
        };
        let expected = e.updated_at.clone();
        let t = title.get();
        let d = detail.get();
        spawn_local(async move {
            // 全屏页是常规编辑路径，标签由 LabelEditor 即时落库；保存时不再带 labelings，
            // 避免和 LabelEditor 的写入重复。草稿态路径（仅 EntryPanel）才传。
            match update_entry(&c, &expected, &t, &d, None).await {
                Ok(updated) => {
                    labels.set(updated.labels.clone());
                    data.update(|r| {
                        if let Some(Ok((_, ref mut e, _, _, _))) = r {
                            *e = updated;
                        }
                    });
                    error.set(None);
                }
                Err(e) => {
                    error.set(Some(e.clone()));
                    // 「已被他人修改」是 GraphQL 服务端硬编码的错误信息子串（始终是中文），
                    // 客户端靠它判定是否要重拉。展示给用户的文案另由
                    // `entry.concurrent_edit` 提供。
                    if e.contains("已被他人修改") {
                        load(true);
                    }
                }
            }
        });
    });

    // 删除后条目已不存在，留在详情页只会显示「条目不存在」，直接退回工作空间。
    let nav_del = navigate.clone();
    let del = move |_| {
        let c = code();
        let s = slug();
        let nav = nav_del.clone();
        spawn_local(async move {
            match delete_entry(&c).await {
                Ok(true) => nav(&format!("/{s}"), Default::default()),
                Ok(false) => error.set(Some(i18n.t("entry.delete_failed"))),
                Err(e) => error.set(Some(e)),
            }
        });
    };

    // 本页顶栏的「切换工作空间」：全屏页没有侧栏，用户没别的入口换工作空间，
    // 所以切换器就放在本页自己的顶栏里（全局顶栏不该替页面承载工作空间这种
    // 页面级状态）。
    //
    // 「切换」意味着离开当前条目——有未保存改动时先拦住（与 Esc 返回同款处理），
    // 否则改动会跟着导航一起丢掉。
    let nav_switch = navigate.clone();
    let on_switch = Callback::new(move |target: String| {
        if dirty.get_untracked() {
            error.set(Some(i18n.t("entry.unsaved_changes")));
            return;
        }
        // 切回当前工作空间 = 什么都不做。
        if target == slug() {
            return;
        }
        nav_switch(&format!("/{target}"), Default::default());
    });

    // 按钮与 Esc 监听两处都要用它；`Callback` 是 `Copy`，可以同时进两个 handler。
    // 详情页 URL 上的 `?view=<id>` 透传给返回地址，让用户回到工作空间时仍是
    // 之前选中的视图，而不是默认视图。
    let nav_back = navigate.clone();
    let view_on_back: Option<String> = use_query_map().get().get("view").filter(|s| !s.is_empty());
    // 「下一个」按钮也要用同一份 view id：clone 一次给 `go_back`（move 进自己的
    // 闭包后没法再借出来），原值留给我这里的 on_next / show_next 后续使用。
    let view_id_for_go_back = view_on_back.clone();
    // 「下一个」按钮的状态：外层 Option = 已尝试取数；内层 Option = 视图排序中是否还有下一条。
    // `None` 涵盖「来自默认视图（按钮根本不渲染）」与「正在加载」两种语义——前者由 view_on_back
    // 在 view! 里挡住，后者由 disabled 态承担。一律用 `is_some()` 但应 `Some(Some(_))` 才可用。
    let next_entry: RwSignal<Option<Option<Entry>>> = RwSignal::new(None);
    // 仅当来自非默认视图（URL 携带 `?view=<id>`）时才追下一条。视图 id 与当前条目 code 都进
    // 依赖：code 变更（保存后 data 被替换）触发重拉；view_on_back 在组件生命周期内不变。
    if let Some(view_id_for_next) = view_on_back.clone() {
        Effect::new(move |_| {
            let Some(current) = data.get().and_then(|r| r.ok()).map(|(_, e, _, _, _)| e) else {
                return;
            };
            if cfg!(target_arch = "wasm32") {
                let vid = view_id_for_next.clone();
                let cc = current.code.clone();
                spawn_local(async move {
                    let r = entry_next_in_view(&vid, &cc).await;
                    // 失败按「无下一项」处理——按钮回到禁用态，不会卡在加载。
                    next_entry.set(Some(r.unwrap_or(None)));
                });
            }
        });
    }
    let go_back: Callback<()> = Callback::new(move |_| {
        let href = match view_id_for_go_back.as_deref() {
            Some(id) => format!("/{}?view={}", slug(), id),
            None => format!("/{}", slug()),
        };
        nav_back(&href, Default::default());
    });

    // Esc 返回视图页；有未保存改动时拦住，并把原因写进现成的提示位。
    // 显式按钮不受影响：Esc 容易误触，按钮是明确动作。
    if cfg!(target_arch = "wasm32") {
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if ev.key() != "Escape" {
                return;
            }
            if dirty.get_untracked() {
                error.set(Some(i18n.t("entry.unsaved_changes")));
                return;
            }
            // 标题编辑态一并退出，避免监听触发后输入框还浮在页面上。
            editing_title.set(false);
            go_back.run(());
        });
        on_cleanup(move || handle.remove());
    }

    view! {
        <div class="page page-wide">
            <div class="panel entry-top">
                // 工作空间上下文：全屏页没有侧栏，换工作空间的入口住这里。
                <WorkspaceSwitcher current=ws_current options=ws_options on_switch=on_switch />
                <button class="btn" on:click=move |_| go_back.run(())>{ic_back()}{move || i18n.t("entry.back_to_view")}</button>
                // 「下一个」按钮：仅当 URL 携带 `?view=<id>`（来自非默认视图）时渲染。
                // 数据未就绪或视图已无下一条 → 禁用并切换 tooltip。点击后携带同样的
                // `?view=<id>` 跳到下一条的全屏页，让「下一个」链路不被打断。
                // 用 `Callback<()>` 包装 on_next（与同文件 go_back/do_save/del 同款），
                // 避免 on:click 闭包把 navigate/slug_fn move 进 view! 后只剩 FnOnce。
                {
                    let nav = navigate.clone();
                    let slug_fn = slug.clone();
                    // go_back 之前已经把 view_on_back 整个 move 走了，这里 clone
                    // 一份给 on_next 用；show_next 在 move 进 on_next 之前先把 bool
                    // 抓出来，避免后续借不到 `view_id_for_next`。
                    let view_id_for_next = view_on_back.clone();
                    let show_next = view_id_for_next.is_some();
                    let on_next: Callback<()> = Callback::new(move |_| {
                        let Some(Some(n)) = next_entry.get_untracked() else {
                            return;
                        };
                        let vid = view_id_for_next.clone().unwrap_or_default();
                        let url = format!("/{}/entry/{}?view={}", slug_fn(), n.code, vid);
                        nav(&url, Default::default());
                    });
                    let next_disabled = move || !matches!(next_entry.get(), Some(Some(_)));
                    let next_tip = move || match next_entry.get() {
                        Some(Some(e)) => {
                            i18n.tf("entry.next_hint", &[("title", &e.title)])
                        }
                        _ => i18n.t("entry.next_none"),
                    };
                    move || if show_next {
                        view! {
                            <button class="btn" disabled=next_disabled title=next_tip on:click=move |_| on_next.run(())>
                                {move || i18n.t("entry.next")}
                            </button>
                        }.into_any()
                    } else {
                        view! { <span></span> }.into_any()
                    }
                }
                <CodeCopy code=Signal::derive(code) />
                {move || if editing_title.get() {
                    view! {
                        <input class="inp entry-title-edit"
                            node_ref=title_ref
                            prop:value=title
                            on:input=move |ev| {
                                title.set(event_target_value(&ev));
                            }
                            on:blur=move |_| editing_title.set(false)
                            on:keydown=move |ev| {
                                if ev.key() == "Enter" {
                                    ev.prevent_default();
                                    editing_title.set(false);
                                } else if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("s") {
                                    ev.prevent_default();
                                    do_save.run(());
                                }
                            }
                        />
                    }.into_any()
                } else {
                    view! {
                        <h1 class="entry-title"
                            title=move || i18n.t("entry.edit_title")
                            on:click=move |_| editing_title.set(true)
                        >{move || title.get()}</h1>
                    }.into_any()
                }}
                <button class="btn pri" disabled=move || !dirty.get() on:click=move |_| do_save.run(())>{move || i18n.t("entry.save")}</button>
                {move || if dirty.get() {
                    view! { <span class="chip dim">{move || i18n.t("entry.unsaved")}</span> }.into_any()
                } else {
                    view! { <span class="chip c-done">{ic_check()}{move || i18n.t("entry.saved")}</span> }.into_any()
                }}
                // 「Share」按钮：切换面板可见性；拉 share 列表统一走下面的 effect。
                    <button class="btn" disabled=move || share_loading.get() on:click=move |_| {
                        show_share_panel.set(!show_share_panel.get_untracked());
                    }>{ic_share()}{move || i18n.t("entry.share")}</button>
                <button class="btn danger" on:click=del>{move || i18n.t("entry.delete")}</button>
            </div>

            {move || error.get().map(|e| view! { <div class="hint" style="margin-bottom:12px">{"⚠ "}{e}</div> })}

            {move || if show_share_panel.get() {
                let c = code();
                // ws_id 在 data 没就绪时是空串——下面的 load_shares 看到空 ws_id
                // 直接跳过，避免面板挂起等待。
                let ws_id = data
                    .get()
                    .and_then(|r| r.ok())
                    .map(|(w, _, _, _, _)| w.id)
                    .unwrap_or_default();
                view! { <SharePanel code=c ws_id=ws_id on_close=Callback::new(move |_| show_share_panel.set(false)) /> }.into_any()
            } else {
                view! { <div class="entry-share-panel hidden"></div> }.into_any()
            }}
            <div class="entry-layout">
                <div class="entry-main">
                    <div class="panel entry-editor" style="padding:0;overflow:hidden"
                        on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                            // keydown 从 Quill 的 contenteditable 冒泡到这里；Quill 自己不绑 Ctrl+S。
                            // 必须 prevent_default，否则浏览器会弹「保存网页」对话框。
                            if (ev.ctrl_key() || ev.meta_key()) && ev.key().eq_ignore_ascii_case("s") {
                                ev.prevent_default();
                                do_save.run(());
                            }
                        }>
                        {move || match data.get() {
                            Some(Ok((_ws, e, _, _, _))) => {
                                let initial = e.detail.clone();
                                // 成员名单：读 get_untracked 而不是 get()，
                                // 否则 ws_members 后改会让这个闭包重跑、编辑器
                                // 重新挂载、用户输入的内容丢光。data 装载的同
                                // 一帧里 ws_members.set() 已经走过，名单已就位。
                                let ms = ws_members.get_untracked();
                                view! {
                                    <RichEditor initial entry_code=Signal::derive(code)
                                        on_change=on_editor_change on_uploaded=on_changed
                                        members=ms />
                                }.into_any()
                            }
                            _ => view! {
                                <div class="ebody" style="min-height:340px;padding:16px">
                                    <span class="mut">{move || i18n.t("common.loading")}</span>
                                </div>
                            }.into_any(),
                        }}
                    </div>

                    <div class="panel entry-comments">
                        <CommentList
                            code=Signal::derive(code)
                            workspace_id=ws_id
                            on_changed=on_changed
                            on_uploaded=on_changed
                        />
                    </div>
                </div>

                <aside class="panel entry-side">
                    {move || data.get().and_then(|r| r.ok()).map(|(_, e, _, _, _)| {
                        let by = |a: &Option<AccountBrief>| a.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| "—".to_string());
                        view! {
                            // 元信息分两行：人（创建人 / 更新人）一行、时间（创建时间 /
                            // 更新时间）一行——两个时间值落在同一行，且整块位于标签区域
                            // 上方。`.dmeta-kv .v` 的 nowrap 保证时间戳本身不折行。
                            <div class="dmeta">
                                <div class="dmeta-line">
                                    <div class="dmeta-kv">
                                        <span class="mut">{move || i18n.t("entry.created_by")}</span>
                                        <span class="v">{by(&e.created_by_account)}</span>
                                    </div>
                                    <div class="dmeta-kv">
                                        <span class="mut">{move || i18n.t("entry.updated_by")}</span>
                                        <span class="v">{by(&e.updated_by_account)}</span>
                                    </div>
                                </div>
                                <div class="dmeta-line">
                                    <div class="dmeta-kv">
                                        <span class="mut">{move || i18n.t("entry.created_at")}</span>
                                        <span class="v">{fmt_datetime(&e.created_at)}</span>
                                    </div>
                                    <div class="dmeta-kv">
                                        <span class="mut">{move || i18n.t("entry.updated_at")}</span>
                                        <span class="v">{fmt_datetime(&e.updated_at)}</span>
                                    </div>
                                </div>
                                {e.archived_at.clone().map(|at| view! {
                                    <div class="dmeta-line">
                                        <div class="dmeta-kv">
                                            <span class="mut">{move || i18n.t("entry.archived_at")}</span>
                                            <span class="v">{fmt_datetime(&at)}</span>
                                        </div>
                                    </div>
                                })}
                            </div>
                        }
                    })}

                    <div>
                        <div class="grp-h">{ic_tag()}{move || i18n.t("entry.labels_hint")}</div>
                        <LabelEditor code=Signal::derive(code) schemas labels members=ws_members on_changed />
                    </div>

                    <div>
                        <TabBar tabs=side_tab_labels(&i18n).to_vec() active=side_tab />
                        {move || match side_tab.get().as_str() {
                            "relations" => view! {
                                <RelationsPanel
                                    code=Signal::derive(code)
                                    workspace_id=ws_id
                                    on_changed=on_changed
                                />
                            }.into_any(),
                            "history" => view! {
                                <AuditTimeline logs=logs code=Signal::derive(code) />
                            }.into_any(),
                            _ => view! {
                                <AttachmentList
                                    code=Signal::derive(code)
                                    workspace_id=ws_id
                                    on_changed=on_changed
                                />
                            }.into_any(),
                        }}
                    </div>
                </aside>
            </div>
        </div>
    }
}

/// Entry 详情下面的分享面板。`show_share_panel` 在父组件控制可见性；
/// 本组件挂载即拉一次 share 列表——避免每次点开都重发请求；同时提供
/// 「Create」「Revoke」「Copy URL」三件套。
#[component]
fn SharePanel(code: String, ws_id: String, on_close: Callback<()>) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let shares = RwSignal::new(Vec::<EntryShare>::new());
    let busy = RwSignal::new(false);
    let copied_id = RwSignal::new(None::<String>);
    // `code` / `ws_id` 是 String 不 Copy，多次闭包需要 clone 进去——直接把它们
    // 提到 RwSignal 一次性持有，闭包读副本，避免「value moved」连环报错。
    let code = RwSignal::new(code);
    let ws_id = RwSignal::new(ws_id);

    // 拉一次 share 列表。父组件点开按钮时 ws_id 已经走 data 装载完；这里用
    // `cfg!` 在 SSR 下跳过，hydrate 后再补一次。
    Effect::new(move |_| {
        if !cfg!(target_arch = "wasm32") || ws_id.get().is_empty() {
            return;
        }
        let c = code.get();
        let w = ws_id.get();
        spawn_local(async move {
            match entry_shares(&w, &c).await {
                Ok(list) => shares.set(list),
                Err(_) => {}
            }
        });
    });

    // 「创建」：落到服务端即可拿回带 id 的 share 行；插入前端列表头（最新在前）。
    let on_create = Callback::new(move |_: ()| {
        if !cfg!(target_arch = "wasm32") || ws_id.get().is_empty() {
            return;
        }
        if busy.get() {
            return;
        }
        busy.set(true);
        let c = code.get();
        let w = ws_id.get();
        spawn_local(async move {
            match create_entry_share(&w, &c).await {
                Ok(share) => shares.update(|list| list.insert(0, share)),
                Err(_) => {}
            }
            busy.set(false);
        });
    });

    // 「撤销」：调用 mutation，然后从前端列表就地更新——`revoked = true`，
    // 让原行变灰显示「已撤销」。不再触发新的 list 拉取。
    let on_revoke = Callback::new(move |sid: String| {
        if !cfg!(target_arch = "wasm32") || ws_id.get().is_empty() {
            return;
        }
        let w = ws_id.get();
        spawn_local(async move {
            if revoke_entry_share(&w, &sid).await.is_ok() {
                shares.update(|list| {
                    if let Some(s) = list.iter_mut().find(|x| x.id == sid) {
                        s.revoked = true;
                    }
                });
            }
        });
    });

    view! {
        <div class="panel entry-share-panel">
            <div class="entry-share-header">
                <span class="grp-h">{move || i18n.t("entry.share_title")}</span>
                <button class="btn sm" on:click=move |_| on_close.run(()) title={move || i18n.t("common.close")}>
                    {ic_close()}
                </button>
            </div>
            {move || {
                let list = shares.get();
                if list.is_empty() {
                    view! { <p class="mut">{move || i18n.t("entry.share_empty")}</p> }.into_any()
                } else {
                    view! {
                        <div class="entry-share-list">
                            {list.into_iter().map(|s| {
                                let sid = s.id.clone();
                                let sid_for_revoke = s.id.clone();
                                let sid_for_copy = s.id.clone();
                                let url = build_share_url(&s.id);
                                let revoked = s.revoked;
                                let copied_id = copied_id.clone();
                                view! {
                                    <div class="share-row">
                                        <code class="share-row-url url">{url}</code>
                                        <button class="btn sm" on:click={
                                            let sid = sid_for_copy.clone();
                                            let copied_id = copied_id.clone();
                                            move |_| {
                                                let url = build_share_url(&sid);
                                                copy_share_url(sid.clone(), url, copied_id);
                                            }
                                        } disabled=revoked>
                                            {move || if copied_id.get().as_deref() == Some(sid.as_str()) {
                                                i18n.t("entry.share_copied")
                                            } else {
                                                i18n.t("entry.share_copy")
                                            }}
                                        </button>
                                        <button class="btn sm danger" on:click={
                                            let sid = sid_for_revoke.clone();
                                            move |_| on_revoke.run(sid.clone())
                                        } disabled=revoked>
                                            {move || if revoked { i18n.t("entry.share_revoked") } else { i18n.t("entry.share_revoke") }}
                                        </button>
                                    </div>
                                }
                            }).collect::<Vec<_>>()}
                        </div>
                    }.into_any()
                }
            }}
            <div style="margin-top:8px">
                <button class="btn pri" disabled=move || busy.get() on:click=move |_| on_create.run(())>
                    {move || i18n.t("entry.share_create")}
                </button>
            </div>
        </div>
    }
}
