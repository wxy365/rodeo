use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use crate::frontend::components::{display_enum_value, label_chip_class, logged_out, value_to_string};
use crate::frontend::graphql_client::{
    create_entry, delete_entry, entries, entry, label_schemas, update_entry, workspace_by_slug,
    Entry, Labeling, LabelSchema, Workspace,
};
use crate::frontend::icons::{
    ic_add, ic_back, ic_close, ic_folder, ic_full, ic_search, ic_setting, ic_share,
};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::tiny_editor::TinyEditor;

#[component]
pub fn WorkspaceMain() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let navigate = use_navigate();
    let refresh = RwSignal::new(0u32);
    let data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>> =
        RwSignal::new(None);
    let schemas = RwSignal::new(Vec::<LabelSchema>::new());
    let ws_name = RwSignal::new(String::new());
    let selected = RwSignal::new(String::new());
    let show_new = RwSignal::new(false);
    let new_title = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    Effect::new_sync(move |_| {
        let s = slug().to_string();
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
                let items = entries(&ws.id).await?;
                let schema_list = label_schemas(&ws.id).await?;
                Ok::<_, String>((ws, items, schema_list))
            }
            .await;
            if let Ok((ref w, _, ref list)) = result {
                ws_name.set(w.name.clone());
                schemas.set(list.clone());
            }
            data.set(Some(result));
        });
    });

    let create_submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let t = new_title.get();
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
                new_title.set(String::new());
                show_new.set(false);
                refresh.update(|n| *n += 1);
            }
        });
    };

    view! {
        <div class="page">
            <div class="crumb">
                {move || format!("/{} · 默认视图「全部任务」", slug())}
            </div>
            <div class="ws-layout">
                <WorkspaceSidebar slug=slug().to_string() name=ws_name />
                <div class="panel wmain">
                    <div class="vhead">
                        <h2>"全部任务"</h2>
                        <label class="inp">
                            {ic_search()}
                            <input placeholder="搜索本视图（即将上线）" disabled />
                        </label>
                        <button class="btn" disabled>{ic_setting()}"视图配置"</button>
                        <button class="btn pri" on:click=move |_| show_new.set(!show_new.get())>
                            {ic_add()}
                            "新建 Entry"
                        </button>
                    </div>
                    <div class="filters">
                        <span class="chip sel">"全部条目"</span>
                        <span class="chip dim">"筛选 / 全文检索（即将上线）"</span>
                        <span style="margin-left:auto" class="mut">"排序：更新时间 ↓"</span>
                    </div>

                    {move || if show_new.get() {
                        view! {
                            <form class="filters" on:submit=create_submit>
                                <input class="inp" style="flex:1" placeholder="新条目标题" prop:value=new_title on:input=move |ev| new_title.set(event_target_value(&ev)) />
                                <button class="btn pri" type="submit">"创建"</button>
                                <button class="btn" type="button" on:click=move |_| show_new.set(false)>"取消"</button>
                            </form>
                        }.into_any()
                    } else {
                        view! { <div></div> }.into_any()
                    }}

                    {move || error.get().map(|e| view! { <p class="error" style="padding:8px 16px">{e}</p> })}

                    <div class=move || if selected.get().is_empty() { "view-body full".to_string() } else { "view-body".to_string() }>
                        <div>
                            <EntryTable data schemas selected />
                        </div>
                        <EntryPanel code=selected slug=slug().to_string() schemas refresh />
                    </div>
                </div>
            </div>
        </div>
    }
}

#[component]
fn WorkspaceSidebar(slug: String, name: RwSignal<String>) -> impl IntoView {
    view! {
        <aside class="panel wside">
            <div style="padding:8px 12px;display:flex;gap:8px;align-items:center">
                <b>{move || name.get()}</b>
            </div>
            <div class="grp">"我的视图"</div>
            <div class="it on">{ic_folder()}"全部任务"</div>
            <div class="it" title="即将上线">{ic_folder()}"我指派的"</div>
            <div class="it" title="即将上线">{ic_folder()}"待修复 Bug"</div>
            <div class="it" title="即将上线">{ic_folder()}"本周归档"</div>
            <div class="grp">"共享视图"</div>
            <div class="it" title="即将上线">{ic_share()}"P0 缺陷看板"</div>
            <div class="it" title="即将上线">{ic_share()}"迭代计划"</div>
            <div class="it" style="color:var(--ink3)">{ic_add()}"新建视图（即将上线）"</div>
            <div style="border-top:1px solid var(--line);margin-top:8px;padding-top:8px">
                <A href=format!("/{slug}/settings")>
                    <div class="it">{ic_setting()}"工作空间设置"</div>
                </A>
                <A href="/workspaces">
                    <div class="it">{ic_back()}"工作空间列表"</div>
                </A>
            </div>
        </aside>
    }
}

#[component]
fn EntryTable(
    data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>>,
    schemas: RwSignal<Vec<LabelSchema>>,
    selected: RwSignal<String>,
) -> impl IntoView {
    view! {
        <table class="tbl">
            <thead>
                <tr>
                    <th>"Code"</th>
                    <th>"标题"</th>
                    {move || schemas.get().iter().map(|s| view! {
                        <th>{s.title.clone()}</th>
                    }).collect::<Vec<_>>()}
                    <th>"更新时间"</th>
                </tr>
            </thead>
            <tbody>
                {move || match data.get() {
                    None => view! { <tr><td colspan="20" class="empty">"加载中…"</td></tr> }.into_any(),
                    Some(Err(e)) => view! { <tr><td colspan="20" class="empty error">{e.clone()}</td></tr> }.into_any(),
                    Some(Ok((_ws, items, _schemas))) => {
                        let sc = schemas.get();
                        if items.is_empty() {
                            view! { <tr><td colspan="20" class="empty">"暂无条目，点击「新建 Entry」创建"</td></tr> }.into_any()
                        } else {
                            items.iter().map(|e| entry_row(e, &sc, selected)).collect::<Vec<_>>().into_any()
                        }
                    }
                }}
            </tbody>
        </table>
    }
}

fn find_label<'a>(entry: &'a Entry, name: &str) -> Option<&'a Value> {
    entry
        .labels
        .iter()
        .find(|l| l.label_name == name)
        .map(|l| &l.value)
}

fn label_cell(schema: &LabelSchema, entry: &Entry) -> impl IntoView {
    let v = find_label(entry, &schema.name);
    let is_enum = schema.value_type == "enum";
    match v {
        None => view! { <td class="mut">"—"</td> }.into_any(),
        Some(val) => {
            let s = value_to_string(val);
            if is_enum {
                let disp = display_enum_value(&s);
                let cls = label_chip_class(&schema.name, &s);
                view! { <td><span class=format!("chip {cls}")>{disp}</span></td> }.into_any()
            } else {
                view! { <td>{s}</td> }.into_any()
            }
        }
    }
}

fn entry_row(entry: &Entry, schemas: &[LabelSchema], selected: RwSignal<String>) -> impl IntoView {
    let code = entry.code.clone();
    let title = entry.title.clone();
    let updated_at = entry.updated_at.clone();
    let code_for_class = entry.code.clone();
    let code_for_click = entry.code.clone();
    view! {
        <tr
            class=move || if selected.get() == code_for_class { "sel".to_string() } else { String::new() }
            on:click=move |_| selected.set(code_for_click.clone())
        >
            <td class="code">{code.clone()}</td>
            <td>{title.clone()}</td>
            {schemas.iter().map(|s| label_cell(s, entry)).collect::<Vec<_>>()}
            <td class="mut">{updated_at.clone()}</td>
        </tr>
    }
}

/// 右侧详情面板：只编辑详情与标签（标题在 Entry 全屏编辑），保存走乐观并发。
#[component]
fn EntryPanel(
    code: RwSignal<String>,
    slug: String,
    schemas: RwSignal<Vec<LabelSchema>>,
    refresh: RwSignal<u32>,
) -> impl IntoView {
    let navigate = use_navigate();
    let data: RwSignal<Option<Result<Entry, String>>> = RwSignal::new(None);
    let labels = RwSignal::new(Vec::<Labeling>::new());
    let detail = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    // overwrite=true：清空编辑缓冲后全量填充（选中变化 / 并发冲突重载）。
    // overwrite=false：软重载，仅更新 data/labels，保留未保存的详情编辑。
    let load = move |overwrite: bool| {
        let c = code.get();
        if c.is_empty() {
            return;
        }
        if overwrite {
            detail.set(String::new());
            labels.set(Vec::new());
            data.set(None);
            error.set(None);
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = entry(&c).await;
                if code.get() != c {
                    return;
                }
                match result {
                    Ok(Some(e)) => {
                        labels.set(e.labels.clone());
                        if overwrite {
                            detail.set(e.detail.clone());
                        }
                        data.set(Some(Ok(e)));
                    }
                    Ok(None) => data.set(Some(Err("条目不存在".to_string()))),
                    Err(err) => data.set(Some(Err(err))),
                }
            });
        }
    };

    Effect::new_sync(move |_| load(true));

    let on_changed = Callback::new(move |_| load(false));
    let on_editor_change = Callback::new(move |d: String| detail.set(d));

    let save = move |_| {
        let c = code.get();
        let Some(entry_now) = data.get().and_then(|r| r.ok()) else {
            return;
        };
        let expected = entry_now.updated_at.clone();
        let t = entry_now.title.clone();
        let d = detail.get();
        spawn_local(async move {
            match update_entry(&c, &expected, &t, &d).await {
                Ok(updated) => {
                    labels.set(updated.labels.clone());
                    data.set(Some(Ok(updated)));
                    error.set(None);
                    refresh.update(|n| *n += 1);
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

    let open_full = move |_| {
        let c = code.get();
        if !c.is_empty() {
            navigate(&format!("/{}/entry/{}", slug, c), Default::default());
        }
    };

    view! {
        {move || {
            if code.get().is_empty() {
                view! { <div></div> }.into_any()
            } else {
                view! {
                    <aside class="detail">
                        {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}
                        <div class="dhead">
                            <span class="code">{move || code.get()}</span>
                            <button class="ibtn" title="复制编码">{ic_share()}</button>
                            <h3>{move || data.get().and_then(|r| r.ok()).map(|e| e.title.clone()).unwrap_or_default()}</h3>
                            <button class="ibtn" title="关闭面板" on:click=close>{ic_close()}</button>
                        </div>
                        <div class="editing"><span class="dot"></span>"乐观并发 · 保存时检测冲突"</div>
                        <div class="dtabs">
                            <button class="on">"详情"</button>
                            <button disabled>"附件"</button>
                            <button disabled>"历史"</button>
                        </div>
                        <div class="editor">
                            {move || match data.get() {
                                Some(Ok(e)) => {
                                    let initial = e.detail.clone();
                                    view! {
                                        <TinyEditor initial on_change=on_editor_change />
                                    }.into_any()
                                }
                                _ => view! {
                                    <div class="ebody"><span class="mut">"加载中…"</span></div>
                                }.into_any(),
                            }}
                        </div>
                        <LabelEditor code=code.get() schemas labels on_changed />
                        <div style="display:flex;gap:8px;margin-top:auto">
                            <button class="btn" style="flex:1;justify-content:center" on:click=open_full.clone()>{ic_full()}"全屏打开"</button>
                            <button class="btn pri" style="flex:1;justify-content:center" on:click=save>"保存"</button>
                        </div>
                        <button class="btn danger" style="justify-content:center" on:click=del>"删除"</button>
                    </aside>
                }
                .into_any()
            }
        }}
    }
}
