use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::frontend::components::{action_label, logged_out, short_time, value_type_label};
use crate::frontend::graphql_client::{
    audit_logs, create_label_schema, label_schemas, my_role, update_label_schema,
    workspace_by_slug, AuditLog, LabelSchema, Workspace,
};
use crate::frontend::icons::{ic_add, ic_history, ic_profile, ic_share, ic_tag};

fn is_builtin(schema: &LabelSchema) -> bool {
    schema.name == "Task" || schema.name == "Bug"
}

#[component]
pub fn WorkspaceSettings() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let navigate = use_navigate();

    let data: RwSignal<Option<Result<(Workspace, String, Vec<LabelSchema>, Vec<AuditLog>), String>>> =
        RwSignal::new(None);
    let refresh = RwSignal::new(0u32);
    let tab = RwSignal::new(String::from("labels"));
    let error = RwSignal::new(None::<String>);

    // 新建标签表单
    let new_name = RwSignal::new(String::new());
    let new_title = RwSignal::new(String::new());
    let new_type = RwSignal::new(String::from("enum"));
    let new_enum = RwSignal::new(String::new());

    Effect::new_sync(move |_| {
        let s = slug();
        let _ = refresh.get();
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        if logged_out() {
            navigate("/login", Default::default());
            return;
        }
        spawn_local(async move {
            let result = async {
                let ws = workspace_by_slug(&s).await?.ok_or("工作空间不存在".to_string())?;
                let role = my_role(&ws.id).await?;
                let schemas = label_schemas(&ws.id).await?;
                let logs = audit_logs(&ws.id).await?;
                Ok::<_, String>((ws, role, schemas, logs))
            }
            .await;
            data.set(Some(result));
        });
    });

    let create = move |ev: SubmitEvent| {
        ev.prevent_default();
        let Some(ws_id) = data
            .get()
            .and_then(|r| r.ok())
            .map(|(w, _, _, _)| w.id.clone())
        else {
            return;
        };
        let n = new_name.get();
        let t = new_title.get();
        let vt = new_type.get();
        let evals: Vec<String> = new_enum
            .get()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        spawn_local(async move {
            match create_label_schema(&ws_id, &n, &t, &vt, &evals, None, &serde_json::json!([])).await {
                Ok(_) => {
                    new_name.set(String::new());
                    new_title.set(String::new());
                    new_enum.set(String::new());
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    view! {
        <div class="page">
            <div class="crumb">
                {move || format!("/{} · 设置（Maintainer 及以上）", slug())}
            </div>
            <div class="set-layout">
                <aside class="panel set-nav">
                    <div class="grp" style="padding:8px 12px 4px;font-size:12px;color:var(--ink3)">
                        {move || data.get().and_then(|r| r.ok()).map(|(w, _, _, _)| w.name.clone()).unwrap_or_default()}
                        " · 设置"
                    </div>
                    <div class="it" class:on=move || tab.get() == "members" on:click=move |_| tab.set("members".into())>
                        {ic_profile()}"成员（即将上线）"
                    </div>
                    <div class="it" class:on=move || tab.get() == "labels" on:click=move |_| tab.set("labels".into())>
                        {ic_tag()}"标签定义"
                    </div>
                    <div class="it" class:on=move || tab.get() == "views" on:click=move |_| tab.set("views".into())>
                        {ic_share()}"视图共享（即将上线）"
                    </div>
                    <div class="it" class:on=move || tab.get() == "audit" on:click=move |_| tab.set("audit".into())>
                        {ic_history()}"审计日志"
                    </div>
                    <div class="it dgr" class:on=move || tab.get() == "danger" on:click=move |_| tab.set("danger".into())>
                        "危险操作（即将上线）"
                    </div>
                </aside>

                <div class="panel set-body">
                    {move || error.get().map(|e| view! { <p class="error">{e}</p> })}

                    {move || match data.get() {
                        None => view! { <div class="empty">"加载中…"</div> }.into_any(),
                        Some(Err(e)) => view! { <div class="empty error">{e.clone()}</div> }.into_any(),
                        Some(Ok((_ws, role, schemas, logs))) => {
                            let can_manage = role == "owner" || role == "maintainer";
                            let cur_tab = tab.get();
                            if cur_tab == "labels" {
                                view! {
                                    <div style="display:flex;align-items:center">
                                        <h2 style="margin-right:auto">"标签定义（LabelSchema）"</h2>
                                    </div>
                                    {if can_manage {
                                        view! {
                                            <form class="invite" on:submit=create>
                                                <input class="inp" placeholder="名称（不可改，如 Priority）" prop:value=new_name on:input=move |ev| new_name.set(event_target_value(&ev)) />
                                                <input class="inp" placeholder="显示名称" prop:value=new_title on:input=move |ev| new_title.set(event_target_value(&ev)) />
                                                <select class="inp" style="width:120px" prop:value=new_type on:change=move |ev| new_type.set(event_target_value(&ev))>
                                                    <option value="enum">"Enum"</option>
                                                    <option value="string">"String"</option>
                                                    <option value="boolean">"Boolean"</option>
                                                    <option value="integer">"Integer"</option>
                                                    <option value="float">"Float"</option>
                                                </select>
                                                <input class="inp" placeholder="枚举值（逗号分隔）" prop:value=new_enum on:input=move |ev| new_enum.set(event_target_value(&ev)) />
                                                <button class="btn pri" type="submit">{ic_add()}"新建标签"</button>
                                            </form>
                                        }.into_any()
                                    } else {
                                        view! { <p class="mut">"仅 Maintainer 及以上可管理标签"</p> }.into_any()
                                    }}
                                    <table class="tbl">
                                        <thead><tr><th>"name（不可改）"</th><th>"title"</th><th>"值类型"</th><th>"可选值 / 说明"</th><th>"来源"</th><th style="width:80px"></th></tr></thead>
                                        <tbody>
                                            {schemas.iter().map(|s| schema_row(s, can_manage, _ws.id.clone(), refresh, error)).collect::<Vec<_>>()}
                                        </tbody>
                                    </table>
                                    <div class="mut">"同一 Entry 对同一 Schema 仅一条 Labeling，更新即 upsert；变更自动记录操作人与时间。"</div>
                                }.into_any()
                            } else if cur_tab == "audit" {
                                view! {
                                    <h2>"审计日志"</h2>
                                    <table class="tbl">
                                        <thead><tr><th>"时间"</th><th>"操作"</th><th>"资源类型"</th><th>"资源"</th></tr></thead>
                                        <tbody>
                                            {if logs.is_empty() {
                                                view! { <tr><td colspan="4" class="empty">"暂无记录"</td></tr> }.into_any()
                                            } else {
                                                logs.iter().map(|l| view! {
                                                    <tr class="static">
                                                        <td class="mut">{short_time(&l.at)}</td>
                                                        <td>{action_label(&l.action)}</td>
                                                        <td class="mut">{l.resource_type.clone()}</td>
                                                        <td class="code">{l.resource_id.clone()}</td>
                                                    </tr>
                                                }).collect::<Vec<_>>().into_any()
                                            }}
                                        </tbody>
                                    </table>
                                }.into_any()
                            } else {
                                view! {
                                    <h2>"即将上线"</h2>
                                    <p class="mut">"成员管理、视图共享、危险操作等能力暂未开放，敬请期待。"</p>
                                }.into_any()
                            }
                        }
                    }}
                </div>
            </div>
        </div>
    }
}

fn schema_row(
    s: &LabelSchema,
    can_manage: bool,
    ws_id: String,
    refresh: RwSignal<u32>,
    error: RwSignal<Option<String>>,
) -> impl IntoView {
    let builtin = is_builtin(s);
    let name = s.name.clone();
    let value_type = s.value_type.clone();
    let enum_str = s.enum_values.join(",");

    let title_input = RwSignal::new(s.title.clone());
    let enum_input = RwSignal::new(enum_str.clone());
    let color_input = s.color.clone();
    let value_colors_input = s.value_colors.clone();

    view! {
        <tr class="static">
            <td class="code">{name.clone()}</td>
            <td>
                {if builtin {
                    view! { <span>{s.title.clone()}</span> }.into_any()
                } else if can_manage {
                    view! {
                        <input class="inp" style="width:100%" prop:value=title_input on:input=move |ev| title_input.set(event_target_value(&ev)) />
                    }.into_any()
                } else {
                    view! { <span>{s.title.clone()}</span> }.into_any()
                }}
            </td>
            <td class="mut">{value_type_label(&value_type)}</td>
            <td>
                {if value_type == "enum" {
                    if builtin {
                        view! { <span class="mut">{enum_str.clone()}</span> }.into_any()
                    } else if can_manage {
                        view! {
                            <input class="inp" style="width:100%" placeholder="逗号分隔" prop:value=enum_input on:input=move |ev| enum_input.set(event_target_value(&ev)) />
                        }.into_any()
                    } else {
                        view! { <span class="mut">{enum_str.clone()}</span> }.into_any()
                    }
                } else {
                    view! { <span class="mut">"—"</span> }.into_any()
                }}
            </td>
            <td>
                {if builtin {
                    view! { <span class="chip dim">"内置"</span> }.into_any()
                } else {
                    view! { <span class="chip c-open">"自定义"</span> }.into_any()
                }}
            </td>
            <td>
                {if !builtin && can_manage {
                    let ws2 = ws_id.clone();
                    let nm = name.clone();
                    view! {
                        <button class="btn rowact" on:click=move |_| {
                            let evals: Vec<String> = enum_input.get().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                            let t = title_input.get();
                            let ws = ws2.clone();
                            let n = nm.clone();
                            let clr = color_input.clone();
                            let vcs = value_colors_input.clone();
                            spawn_local(async move {
                                if let Err(e) = update_label_schema(&ws, &n, &t, &evals, clr.as_deref(), &vcs).await {
                                    error.set(Some(e));
                                }
                                refresh.update(|x| *x += 1);
                            });
                        }>"保存"</button>
                    }.into_any()
                } else {
                    view! { <span></span> }.into_any()
                }}
            </td>
        </tr>
    }
}
