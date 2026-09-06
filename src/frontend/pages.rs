use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use super::graphql_client::{
    clear_token, create_entry, create_workspace, entries, label_schemas, login, set_labeling,
    set_token, workspace_by_slug, workspaces, Entry, LabelSchema, Workspace, WorkspaceItem,
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
                    let schemas = label_schemas(&ws.id).await?;
                    Ok::<_, String>((ws, items, schemas))
                }
                .await;
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
                            items.iter().map(|e| entry_row(e, &task_opts, refresh)).collect::<Vec<_>>().into_any()
                        }
                    }}
                </tbody>
            </table>
        </div>
    }
}

fn entry_row(entry: &Entry, task_opts: &[String], refresh: RwSignal<u32>) -> impl IntoView {
    let current = entry
        .labels
        .iter()
        .find(|l| l.label_name == "Task")
        .and_then(|l| l.value.as_str())
        .unwrap_or("")
        .to_string();
    let code = entry.code.clone();
    let opts = task_opts.to_vec();
    let title = entry.title.clone();
    let updated_at = entry.updated_at.clone();

    view! {
        <tr>
            <td>{title}</td>
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
