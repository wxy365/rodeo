use leptos::html::Input;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::frontend::attachment_list::AttachmentList;
use crate::frontend::comment_list::CommentList;
use crate::frontend::components::{fmt_datetime, logged_out, AuditTimeline, CodeCopy, TabBar};
use crate::frontend::graphql_client::{
    audit_logs, delete_entry, entry, label_schemas, members, update_entry, workspace_by_slug,
    AccountBrief, AuditLog, Entry, Labeling, LabelSchema, Member, Workspace,
};
use crate::frontend::icons::{
    ic_back, ic_check, ic_share, ic_tag,
};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::tiny_editor::TinyEditor;

// 全屏页侧栏的两个页签。标签编辑不在这里——它常驻在页签上方。
const SIDE_TABS: &[(&str, &str)] = &[("attachments", "附件"), ("history", "历史")];

#[component]
pub fn EntryFullScreen() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let code = move || params.get().get("code").unwrap_or_default();
    let navigate = use_navigate();

    let data: RwSignal<
        Option<Result<(Workspace, Entry, Vec<LabelSchema>, Vec<AuditLog>, Vec<Member>), String>>,
    > = RwSignal::new(None);
    let schemas = RwSignal::new(Vec::<LabelSchema>::new());
    let labels = RwSignal::new(Vec::<Labeling>::new());
    let ws_members = RwSignal::new(Vec::<Member>::new());
    let title = RwSignal::new(String::new());
    let detail = RwSignal::new(String::new());
    let saved = RwSignal::new(false);
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
                    let ws = workspace_by_slug(&s).await?.ok_or("工作空间不存在".to_string())?;
                    let e = entry(&c).await?.ok_or("条目不存在".to_string())?;
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
        if cfg!(target_arch = "wasm32") && logged_out() {
            nav_redirect("/login", Default::default());
            return;
        }
        load(true);
    });

    let on_changed = Callback::new(move |_| load(false));
    let on_editor_change = Callback::new(move |d: String| {
        detail.set(d);
        saved.set(false);
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

    let save = move |_| {
        let c = code();
        let Some(e) = data.get().and_then(|r| r.ok()).map(|(_, e, _, _, _)| e) else {
            return;
        };
        let expected = e.updated_at.clone();
        let t = title.get();
        let d = detail.get();
        spawn_local(async move {
            match update_entry(&c, &expected, &t, &d).await {
                Ok(updated) => {
                    labels.set(updated.labels.clone());
                    data.update(|r| {
                        if let Some(Ok((_, ref mut e, _, _, _))) = r {
                            *e = updated;
                        }
                    });
                    saved.set(true);
                    error.set(None);
                }
                Err(e) => {
                    error.set(Some(e.clone()));
                    if e.contains("内容已被他人修改") {
                        load(true);
                    }
                }
            }
        });
    };

    // 删除后条目已不存在，留在详情页只会显示「条目不存在」，直接退回工作空间。
    let nav_del = navigate.clone();
    let del = move |_| {
        let c = code();
        let s = slug();
        let nav = nav_del.clone();
        spawn_local(async move {
            match delete_entry(&c).await {
                Ok(true) => nav(&format!("/{s}"), Default::default()),
                Ok(false) => error.set(Some("删除失败".to_string())),
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let back = move |_| navigate(&format!("/{}", slug()), Default::default());

    view! {
        <div class="page">
            <div class="panel entry-top">
                <button class="btn" on:click=back>{ic_back()}"返回视图"</button>
                <CodeCopy code=Signal::derive(code) />
                {move || if editing_title.get() {
                    view! {
                        <input class="inp entry-title-edit"
                            node_ref=title_ref
                            prop:value=title
                            on:input=move |ev| {
                                title.set(event_target_value(&ev));
                                saved.set(false);
                            }
                            on:blur=move |_| editing_title.set(false)
                            on:keydown=move |ev| {
                                if ev.key() == "Enter" {
                                    ev.prevent_default();
                                    editing_title.set(false);
                                }
                            }
                        />
                    }.into_any()
                } else {
                    view! {
                        <h1 class="entry-title"
                            title="点击编辑标题"
                            on:click=move |_| editing_title.set(true)
                        >{move || title.get()}</h1>
                    }.into_any()
                }}
                <button class="btn pri" on:click=save>"保存"</button>
                {move || if saved.get() {
                    view! { <span class="chip c-done">{ic_check()}"已保存"</span> }.into_any()
                } else {
                    view! { <span class="chip dim">"未保存"</span> }.into_any()
                }}
                <button class="btn" disabled>{ic_share()}"分享链接（即将上线）"</button>
                <button class="btn danger" on:click=del>"删除"</button>
            </div>

            {move || error.get().map(|e| view! { <div class="hint" style="margin-bottom:12px">{"⚠ "}{e}</div> })}

            <div class="entry-layout">
                <div class="entry-main">
                    <div class="panel entry-editor" style="padding:0;overflow:hidden">
                        {move || match data.get() {
                            Some(Ok((_ws, e, _, _, _))) => {
                                let initial = e.detail.clone();
                                view! {
                                    <TinyEditor initial entry_code=Signal::derive(code) on_change=on_editor_change on_uploaded=on_changed />
                                }.into_any()
                            }
                            _ => view! {
                                <div class="ebody" style="min-height:340px;padding:16px">
                                    <span class="mut">"加载中…"</span>
                                </div>
                            }.into_any(),
                        }}
                    </div>

                    <div class="panel entry-comments">
                        <CommentList
                            code=Signal::derive(code)
                            workspace_id=ws_id
                            on_changed=on_changed
                        />
                    </div>
                </div>

                <aside class="panel entry-side">
                    {move || data.get().and_then(|r| r.ok()).map(|(_, e, _, _, _)| {
                        let by = |a: &Option<AccountBrief>| a.as_ref().map(|x| x.name.clone()).unwrap_or_else(|| "—".to_string());
                        view! {
                            <div class="dmeta">
                                <div><span class="mut">"创建人"</span>{by(&e.created_by_account)}</div>
                                <div><span class="mut">"创建时间"</span>{fmt_datetime(&e.created_at)}</div>
                                <div><span class="mut">"更新人"</span>{by(&e.updated_by_account)}</div>
                                <div><span class="mut">"更新时间"</span>{fmt_datetime(&e.updated_at)}</div>
                                {e.archived_at.clone().map(|at| view! {
                                    <div><span class="mut">"归档时间"</span>{fmt_datetime(&at)}</div>
                                })}
                            </div>
                        }
                    })}

                    <div>
                        <div class="grp-h">{ic_tag()}"标签（变更即保存）"</div>
                        <LabelEditor code=Signal::derive(code) schemas labels members=ws_members on_changed />
                    </div>

                    <div>
                        <TabBar tabs=SIDE_TABS active=side_tab />
                        {move || if side_tab.get() == "history" {
                            view! {
                                <AuditTimeline logs=logs code=Signal::derive(code) />
                            }.into_any()
                        } else {
                            view! {
                                <AttachmentList
                                    code=Signal::derive(code)
                                    workspace_id=ws_id
                                    on_changed=on_changed
                                />
                            }.into_any()
                        }}
                    </div>
                </aside>
            </div>
        </div>
    }
}
