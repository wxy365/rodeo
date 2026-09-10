use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::frontend::components::{action_label, logged_out, short_time};
use crate::frontend::graphql_client::{
    audit_logs, entry, label_schemas, update_entry, workspace_by_slug, AuditLog, Entry,
    Labeling, LabelSchema, Workspace,
};
use crate::frontend::icons::{
    ic_back, ic_check, ic_history, ic_share, ic_tag, ic_upload,
};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::tiny_editor::TinyEditor;

#[component]
pub fn EntryFullScreen() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let code = move || params.get().get("code").unwrap_or_default();
    let navigate = use_navigate();

    let data: RwSignal<Option<Result<(Workspace, Entry, Vec<LabelSchema>, Vec<AuditLog>), String>>> =
        RwSignal::new(None);
    let schemas = RwSignal::new(Vec::<LabelSchema>::new());
    let labels = RwSignal::new(Vec::<Labeling>::new());
    let title = RwSignal::new(String::new());
    let detail = RwSignal::new(String::new());
    let saved = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);

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
                    Ok::<_, String>((ws, e, schema_list, logs))
                }
                .await;
                if let Ok((_, ref e, ref list, _)) = result {
                    schemas.set(list.clone());
                    labels.set(e.labels.clone());
                    if overwrite {
                        title.set(e.title.clone());
                        detail.set(e.detail.clone());
                    }
                }
                data.set(Some(result));
            });
        }
    };

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

    let save = move |_| {
        let c = code();
        let Some(e) = data.get().and_then(|r| r.ok()).map(|(_, e, _, _)| e) else {
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
                        if let Some(Ok((_, ref mut e, _, _))) = r {
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

    let back = move |_| navigate(&format!("/{}", slug()), Default::default());

    view! {
        <div class="page">
            <div class="panel entry-top">
                <button class="btn" on:click=back>{ic_back()}"返回视图"</button>
                <span class="code">{code}</span>
                <input class="inp" prop:value=title on:input=move |ev| {
                    title.set(event_target_value(&ev));
                    saved.set(false);
                } />
                <button class="btn pri" on:click=save>"保存"</button>
                {move || if saved.get() {
                    view! { <span class="chip c-done">{ic_check()}"已保存"</span> }.into_any()
                } else {
                    view! { <span class="chip dim">"未保存"</span> }.into_any()
                }}
                <button class="btn" disabled>{ic_share()}"分享链接（即将上线）"</button>
            </div>

            {move || error.get().map(|e| view! { <div class="hint" style="margin-bottom:12px">{"⚠ "}{e}</div> })}

            <div class="entry-layout">
                <div class="panel" style="padding:0;overflow:hidden">
                    {move || match data.get() {
                        Some(Ok((_ws, e, _, _))) => {
                            let initial = e.detail.clone();
                            view! {
                                <TinyEditor initial on_change=on_editor_change />
                            }.into_any()
                        }
                        _ => view! {
                            <div class="ebody" style="min-height:340px;padding:16px">
                                <span class="mut">"加载中…"</span>
                            </div>
                        }.into_any(),
                    }}
                </div>

                <aside class="panel entry-side">
                    <div>
                        <div class="grp-h">{ic_tag()}"标签（变更即保存）"</div>
                        <LabelEditor code=code() schemas labels on_changed />
                    </div>

                    <div>
                        <div class="grp-h">{ic_upload()}"附件（≤ 50MB）"<span class="mut">"即将上线"</span></div>
                        <div class="mut">"暂无附件"</div>
                    </div>

                    <div>
                        <div class="grp-h">{ic_history()}"审计历史"</div>
                        {move || {
                            let c = code();
                            match data.get() {
                                Some(Ok((_, _, _, logs))) => {
                                    let mine: Vec<AuditLog> = logs.into_iter().filter(|l| l.resource_id == c).collect();
                                    if mine.is_empty() {
                                        view! { <div class="mut">"暂无记录"</div> }.into_any()
                                    } else {
                                        view! {
                                            {mine.iter().map(|l| view! {
                                                <div class="tl">
                                                    <span class="t">{short_time(&l.at)}</span>
                                                    <span>{action_label(&l.action)}</span>
                                                </div>
                                            }).collect::<Vec<_>>()}
                                        }.into_any()
                                    }
                                }
                                _ => view! { <div class="mut">"加载中…"</div> }.into_any(),
                            }
                        }}
                    </div>
                </aside>
            </div>
        </div>
    }
}
