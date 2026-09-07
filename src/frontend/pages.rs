use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use super::graphql_client::{
    clear_token, create_entry, create_workspace, delete_entry, entries, entry, label_schemas,
    login, remove_labeling, set_labeling, set_token, update_entry, workspace_by_slug, workspaces,
    Entry, LabelSchema, Workspace, WorkspaceItem,
};
use super::use_auth;

#[component]
pub fn Home() -> impl IntoView {
    view! {
        <div class="landing">
            <h1>"Rodeo"</h1>
            <p>"任务与问题跟踪"</p>
            <div class="landing-links">
                <A href="/login">"登录"</A>
                <A href="/workspaces">"工作空间"</A>
            </div>
        </div>
    }
}

#[component]
pub fn Login() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    let on_submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let navigate = navigate.clone();
        let email = email.get();
        let password = password.get();
        spawn_local(async move {
            match login(&email, &password).await {
                Ok((token, user)) => {
                    set_token(&token);
                    auth.user.set(Some(user));
                    navigate("/workspaces", Default::default());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    view! {
        <div class="auth-page">
            <form class="auth-form" on:submit=on_submit>
                <h1>"Rodeo"</h1>
                <input type="email" required placeholder="邮箱" prop:value=email on:input=move |ev| email.set(event_target_value(&ev)) />
                <input type="password" required placeholder="密码" prop:value=password on:input=move |ev| password.set(event_target_value(&ev)) />
                {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
                <button type="submit">"登录"</button>
                <p class="hint">"默认管理员：admin@local / Admin12345"</p>
            </form>
        </div>
    }
}

#[component]
pub fn WorkspaceList() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let name = RwSignal::new(String::new());
    let desc = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    let ws: RwSignal<Option<Result<Vec<WorkspaceItem>, String>>> = RwSignal::new(None);

    Effect::new_sync(move |_| {
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = workspaces().await;
                ws.set(Some(result));
            });
        }
    });

    let create_ws = {
        let navigate = navigate.clone();
        move |ev: SubmitEvent| {
            ev.prevent_default();
            let navigate = navigate.clone();
            let name = name.get();
            let desc = desc.get();
            spawn_local(async move {
                match create_workspace(&name, &desc).await {
                    Ok(ws) => navigate(&format!("/{}", ws.slug), Default::default()),
                    Err(e) => error.set(Some(e)),
                }
            });
        }
    };

    let logout = {
        let navigate = navigate.clone();
        move |_| {
            clear_token();
            auth.user.set(None);
            navigate("/login", Default::default());
        }
    };

    view! {
        <div class="ws-page">
            <header class="topbar">
                <h1>"工作空间"</h1>
                <span class="user">{move || auth.user.get().map(|u| u.email).unwrap_or_default()}</span>
                <button on:click=logout>"退出"</button>
            </header>

            <form class="new-ws" on:submit=create_ws>
                <input placeholder="名称" prop:value=name on:input=move |ev| name.set(event_target_value(&ev)) />
                <input placeholder="描述" prop:value=desc on:input=move |ev| desc.set(event_target_value(&ev)) />
                <button type="submit">"新建工作空间"</button>
            </form>

            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}

            <ul class="ws-list">
                {move || match ws.get() {
                    None => view! { <li>"加载中…"</li> }.into_any(),
                    Some(Ok(list)) => view! {
                        {list.iter().map(|w| {
                            let name = w.workspace.name.clone();
                            let slug = w.workspace.slug.clone();
                            let role = w.role.clone();
                            view! {
                                <li>
                                    <A href=format!("/{}", slug)>{name}</A>
                                    <span class="role">{role}</span>
                                </li>
                            }
                        }).collect::<Vec<_>>()}
                    }.into_any(),
                    Some(Err(e)) => view! { <li class="error">{e.clone()}</li> }.into_any(),
                }}
            </ul>
        </div>
    }
}

#[component]
pub fn WorkspaceMain() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let refresh = RwSignal::new(0u32);
    let data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>> =
        RwSignal::new(None);
    // 详情面板当前选中的 entry code；空串表示未选中。
    let selected = RwSignal::new(String::new());
    // 标签 schema 的响应式投影：列表加载完成后填充，右侧 EntryPanel/LabelEditor 读取它，
    // 使面板实例不随列表刷新而重建（从而保留未保存的编辑与乐观并发 updatedAt）。
    let schemas = RwSignal::new(Vec::<LabelSchema>::new());

    Effect::new_sync(move |_| {
        let s = slug();
        let _ = refresh.get();
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = async {
                    let ws = workspace_by_slug(&s)
                        .await?
                        .ok_or("工作空间不存在".to_string())?;
                    let items = entries(&ws.id).await?;
                    let schema_list = label_schemas(&ws.id).await?;
                    Ok::<_, String>((ws, items, schema_list))
                }
                .await;
                if let Ok((_, _, ref list)) = result {
                    schemas.set(list.clone());
                }
                data.set(Some(result));
            });
        }
    });

    let title = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    let create = move |ev: SubmitEvent| {
        ev.prevent_default();
        let t = title.get();
        let Some(id) = data
            .get()
            .and_then(|r| r.ok())
            .map(|(ws, _, _)| ws.id.clone())
        else {
            return;
        };
        spawn_local(async move {
            if let Err(e) = create_entry(&id, &t).await {
                error.set(Some(e));
            } else {
                title.set(String::new());
                refresh.update(|n| *n += 1);
            }
        });
    };

    view! {
        <div class="ws-main">
            <header class="topbar">
                <A href="/workspaces">"← 工作空间"</A>
                <h1>{move || data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.name.clone()).unwrap_or_default()}</h1>
            </header>

            <form class="new-entry" on:submit=create>
                <input placeholder="新条目标题" prop:value=title on:input=move |ev| title.set(event_target_value(&ev)) />
                <button type="submit">"新建"</button>
            </form>

            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}

            <div class="ws-body">
                <div class="entry-list">
                    <table class="entry-table">
                        <thead><tr><th>"标题"</th><th>"Task"</th><th>"更新时间"</th></tr></thead>
                        <tbody>
                            {move || match data.get() {
                                None => view! { <tr><td colspan="3">"加载中…"</td></tr> }.into_any(),
                                Some(Err(e)) => view! { <tr><td colspan="3" class="error">{e.clone()}</td></tr> }.into_any(),
                                Some(Ok((_ws, items, schemas))) => {
                                    let task_opts: Vec<String> = schemas
                                        .iter()
                                        .find(|s| s.name == "Task")
                                        .map(|s| s.enum_values.clone())
                                        .unwrap_or_default();
                                    items.iter()
                                        .map(|e| entry_row(e, &task_opts, refresh, selected))
                                        .collect::<Vec<_>>()
                                        .into_any()
                                }
                            }}
                        </tbody>
                    </table>
                </div>
                <EntryPanel code=selected schemas=schemas refresh=refresh />
            </div>
        </div>
    }
}

fn entry_row(
    entry: &Entry,
    task_opts: &[String],
    refresh: RwSignal<u32>,
    selected: RwSignal<String>,
) -> impl IntoView {
    let current = entry
        .labels
        .iter()
        .find(|l| l.label_name == "Task")
        .and_then(|l| l.value.as_str())
        .unwrap_or("")
        .to_string();
    let code = entry.code.clone();
    let open_code = entry.code.clone();
    let opts = task_opts.to_vec();
    let title = entry.title.clone();
    let updated_at = entry.updated_at.clone();

    view! {
        <tr>
            <td>
                <a
                    href="#"
                    on:click=move |ev| {
                        ev.prevent_default();
                        selected.set(open_code.clone());
                    }
                >{title}</a>
            </td>
            <td>
                <select on:change=move |ev| {
                    let v = event_target_value(&ev);
                    let code = code.clone();
                    spawn_local(async move {
                        let _ = set_labeling(&code, "Task", &Value::String(v)).await;
                        refresh.update(|n| *n += 1);
                    });
                }>
                    <option value="" selected=current.is_empty()>"（未设置）"</option>
                    {opts.iter().cloned().map(|o| view! {
                        <option value=o.clone() selected=current == o>{o.clone()}</option>
                    }).collect::<Vec<_>>()}
                </select>
            </td>
            <td>{updated_at}</td>
        </tr>
    }
}

/// 右侧条目详情面板：标题 + 详情 textarea 编辑 + 保存（乐观并发）+ 标签 + 删除。
/// code 为当前选中的 entry code（空表示关闭）；schemas 为工作空间的标签 schema。
/// 组件在 WorkspaceMain 中始终挂载，code 为空时渲染为空，避免随列表刷新被重建而丢失编辑。
#[component]
fn EntryPanel(
    code: RwSignal<String>,
    schemas: RwSignal<Vec<LabelSchema>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let data: RwSignal<Option<Result<Entry, String>>> = RwSignal::new(None);
    let title = RwSignal::new(String::new());
    let detail = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    // 拉取并填充当前 code 对应的条目。
    // clear == true 时先清空编辑缓冲/错误（切换选中条目）；false 保留（乐观并发冲突后重载）。
    let load = move |clear: bool| {
        let c = code.get();
        if c.is_empty() {
            return;
        }
        if clear {
            title.set(String::new());
            detail.set(String::new());
            data.set(None);
            error.set(None);
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = entry(&c).await;
                // 拉取期间用户可能已切换/关闭条目：丢弃过期响应。
                if code.get() != c {
                    return;
                }
                match result {
                    Ok(Some(e)) => {
                        title.set(e.title.clone());
                        detail.set(e.detail.clone());
                        data.set(Some(Ok(e)));
                    }
                    Ok(None) => data.set(Some(Err("条目不存在".to_string()))),
                    Err(err) => data.set(Some(Err(err))),
                }
            });
        }
    };

    // 选中条目变化时（重新）加载；挂载时 code 为空则直接返回。
    Effect::new_sync(move |_| load(true));

    let save = move |ev: SubmitEvent| {
        ev.prevent_default();
        let c = code.get();
        // 以最近一次已知 updatedAt 作为 expectedUpdatedAt（乐观并发）。
        let Some(expected) = data.get().and_then(|r| r.ok()).map(|e| e.updated_at) else {
            return; // 尚未加载完成，忽略提交
        };
        let t = title.get();
        let d = detail.get();
        spawn_local(async move {
            match update_entry(&c, &expected, &t, &d).await {
                Ok(updated) => {
                    // 记录服务器返回的最新 updatedAt，作为下一次 expectedUpdatedAt。
                    data.set(Some(Ok(updated)));
                    error.set(None);
                    refresh.update(|n| *n += 1);
                }
                Err(e) => {
                    if e.contains("内容已被他人修改") {
                        // 并发冲突：保留提示并重新加载服务端最新内容。
                        load(false);
                    }
                    error.set(Some(e));
                }
            }
        });
    };

    let del = move |_| {
        let c = code.get();
        spawn_local(async move {
            match delete_entry(&c).await {
                Ok(true) => {
                    code.set(String::new());
                    refresh.update(|n| *n += 1);
                }
                Ok(false) => error.set(Some("删除失败".to_string())),
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let close = move |_| code.set(String::new());

    view! {
        {move || {
            if code.get().is_empty() {
                view! { <div></div> }.into_any()
            } else {
                view! {
                    <div class="entry-panel">
                        <div class="panel-head">
                            <strong>"条目详情"</strong>
                            <button on:click=close>"关闭"</button>
                        </div>
                        {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <form class="entry-form" on:submit=save>
                            <label>"标题"</label>
                            <input prop:value=title on:input=move |ev| title.set(event_target_value(&ev)) />
                            <label>"详情"</label>
                            <textarea rows="6" prop:value=detail on:input=move |ev| detail.set(event_target_value(&ev)) />
                            <button type="submit">"保存"</button>
                        </form>
                        <div class="panel-labels">
                            <LabelEditor code=code schemas=schemas refresh=refresh />
                        </div>
                        <button class="danger" on:click=del>"删除"</button>
                    </div>
                }
                .into_any()
            }
        }}
    }
}

/// 标签增删：每个 label schema 一行。enum 类型用下拉选择（空值=清除），
/// 其它类型用输入框 + 「设置」，并均提供「移除」按钮。操作后刷新列表。
#[component]
fn LabelEditor(
    code: RwSignal<String>,
    schemas: RwSignal<Vec<LabelSchema>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    view! {
        <div>
            {move || schemas.get().into_iter().map(move |s| {
                let name = s.name.clone();
                let title_text = s.title.clone();
                let is_enum = s.value_type == "enum";

                view! {
                    <div class="label-row">
                        <span class="label-title">{title_text}</span>
                        {if is_enum {
                            let nm = name.clone();
                            let opts = s.enum_values.clone();
                            view! {
                                <select on:change=move |ev| {
                                    let v = event_target_value(&ev);
                                    let c = code.get();
                                    let n = nm.clone();
                                    spawn_local(async move {
                                        if v.is_empty() {
                                            let _ = remove_labeling(&c, &n).await;
                                        } else {
                                            let _ = set_labeling(&c, &n, &Value::String(v)).await;
                                        }
                                        refresh.update(|x| *x += 1);
                                    });
                                }>
                                    <option value="">"（清除）"</option>
                                    {opts.into_iter().map(|o| view! {
                                        <option value=o.clone()>{o.clone()}</option>
                                    }).collect::<Vec<_>>()}
                                </select>
                            }.into_any()
                        } else {
                            let nm = name.clone();
                            let input = RwSignal::new(String::new());
                            view! {
                                <input placeholder="值" prop:value=input on:input=move |ev| input.set(event_target_value(&ev)) />
                                <button on:click=move |_| {
                                    let v = input.get();
                                    let c = code.get();
                                    let n = nm.clone();
                                    spawn_local(async move {
                                        let _ = set_labeling(&c, &n, &Value::String(v)).await;
                                        refresh.update(|x| *x += 1);
                                    });
                                }>"设置"</button>
                            }.into_any()
                        }}
                        <button class="danger" on:click=move |_| {
                            let c = code.get();
                            let n = name.clone();
                            spawn_local(async move {
                                let _ = remove_labeling(&c, &n).await;
                                refresh.update(|x| *x += 1);
                            });
                        }>"移除"</button>
                    </div>
                }
            }).collect::<Vec<_>>().into_any()}
        </div>
    }
}
