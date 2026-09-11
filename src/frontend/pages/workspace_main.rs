use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::frontend::components::{
    display_enum_value, label_chip_class, logged_out, short_time, value_to_string,
};
use crate::frontend::graphql_client::{
    create_entry, create_view, delete_entry, delete_view, entry, format_view_query, label_schemas,
    parse_view_query, query_entries, update_entry, update_view, views, workspace_by_slug, Entry,
    Labeling, LabelSchema, View, Workspace,
};
use crate::frontend::icons::{
    ic_add, ic_back, ic_close, ic_folder, ic_full, ic_search, ic_setting, ic_share,
};
use crate::frontend::label_editor::LabelEditor;
use crate::frontend::tiny_editor::TinyEditor;
use crate::frontend::view_filter::{build_query, chips, with_text, CondChip};

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

    // ---- 筛选查询状态 ----
    let query_ast = RwSignal::new(serde_json::json!({ "and": [] }));
    let expr_mode = RwSignal::new(false);
    let expr_text = RwSignal::new(String::new());
    let ad_hoc_text = RwSignal::new(String::new());
    let new_cond_label = RwSignal::new(String::new());
    let refresh_view = RwSignal::new(0u32);
    // ---- 分页状态 ----
    let page_signal = RwSignal::new(1i64);
    let page_size: i64 = 20;
    let total_signal = RwSignal::new(0i64);

    // ---- 视图侧栏状态 ----
    let view_list = RwSignal::new(Vec::<View>::new());
    let active_view = RwSignal::new(None::<View>);
    // 侧栏高亮只需 id；与 active_view 一并更新，保持二者同步。
    let active_id = RwSignal::new(None::<String>);
    // 选中视图变化时，播种 query_ast 并把 id/active_view 一并切换；id 未变则整体跳过。
    // Leptos 0.8 的 RwSignal::set 无相等短路，无条件写 active_view 会让 Effect（以
    // active_view / query_ast 为依赖）与 load_views 相互触发，形成无限刷新回环。
    let set_active = move |v: Option<View>| {
        let new_id = v.as_ref().map(|v| v.id.clone());
        if active_id.get_untracked() == new_id {
            return;
        }
        query_ast.set(
            v.as_ref()
                .map(|v| v.query.clone())
                .unwrap_or_else(|| serde_json::json!({ "and": [] })),
        );
        active_id.set(new_id);
        active_view.set(v);
        // 切换视图回到第 1 页，避免旧的页码超出新视图总页数导致空表。
        // 仅在实际需要时写，防止无谓地多触发一次 Effect。
        if page_signal.get_untracked() != 1 {
            page_signal.set(1);
        }
    };
    let load_views = move |ws_id: String| {
        spawn_local(async move {
            if let Ok(list) = views(&ws_id).await {
                // 仅当当前选中视图不在新列表里（切换工作空间、或列表为空）才重设，
                // 否则会把用户在侧栏上的选择覆盖回第一个视图。
                let current = active_id.get_untracked();
                let still_valid = current
                    .as_ref()
                    .is_some_and(|id| list.iter().any(|v| &v.id == id));
                if !still_valid {
                    set_active(list.first().cloned());
                }
                view_list.set(list);
            }
        });
    };
    let select_view = Callback::new(move |id: String| {
        if let Some(v) = view_list.get().into_iter().find(|v| v.id == id) {
            set_active(Some(v));
        }
    });
    let delete_view_cb = Callback::new(move |id: String| {
        spawn_local(async move {
            let _ = delete_view(&id).await;
            view_list.update(|l| l.retain(|v| v.id != id));
            if active_id.get().as_deref() == Some(id.as_str()) {
                set_active(view_list.get().first().cloned());
            }
        });
    });

    // ---- 新建视图弹窗 ----
    let show_view_dialog = RwSignal::new(false);
    let view_name_input = RwSignal::new(String::new());
    let view_shared_input = RwSignal::new(false);
    let view_columns_input = RwSignal::new(String::new()); // 逗号分隔的标签 name

    Effect::new_sync(move |_| {
        let s = slug().to_string();
        let _ = refresh.get();
        let _ = refresh_view.get();
        let _ = query_ast.get();
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        if logged_out() {
            navigate("/login", Default::default());
            return;
        }
        // 查询须在 spawn 前同步组装，signal 读取才会登记为 Effect 依赖。
        // 条件来自 query_ast（筛选芯片 / 表达式编辑它），再并入顶栏 ad-hoc 全文词。
        // R16：ad-hoc 词仅在回车时写入 query_ast，故这里用 get_untracked 读取，
        // 避免每次击键都触发重查；query_ast 才是真正的重查触发器。
        let ast = with_text(&query_ast.get(), &ad_hoc_text.get_untracked());
        let sort_field = active_view
            .get()
            .map(|v| v.sort.field)
            .unwrap_or_else(|| "updatedAt".to_string());
        let sort_desc = active_view.get().map(|v| v.sort.desc).unwrap_or(true);
        let page_now = page_signal.get();
        spawn_local(async move {
            let result = async {
                let ws = workspace_by_slug(&s).await?.ok_or("工作空间不存在".to_string())?;
                let ep = query_entries(&ws.id, &ast, &sort_field, sort_desc, page_now, page_size)
                    .await?;
                total_signal.set(ep.total);
                let schema_list = label_schemas(&ws.id).await?;
                Ok::<_, String>((ws, ep.items, schema_list))
            }
            .await;
            if let Ok((ref w, _, ref list)) = result {
                ws_name.set(w.name.clone());
                schemas.set(list.clone());
                load_views(w.id.clone());
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
                <WorkspaceSidebar
                    slug=slug().to_string()
                    name=ws_name
                    views=view_list
                    active=active_id
                    on_select=select_view
                    on_new=Callback::new(move |_| show_view_dialog.set(true))
                    on_delete=delete_view_cb
                />
                <div class="panel wmain">
                    <div class="vhead">
                        <h2>"全部任务"</h2>
                        <label class="inp">
                            {ic_search()}
                            <input placeholder="搜索本视图，可与过滤组合" prop:value=ad_hoc_text
                                on:input=move |ev| ad_hoc_text.set(event_target_value(&ev))
                                on:keydown=move |ev| {
                                    if ev.key() == "Enter" {
                                        let mut ast = query_ast.get();
                                        ast = with_text(&ast, &ad_hoc_text.get());
                                        query_ast.set(ast);
                                        refresh_view.update(|n| *n += 1);
                                    }
                                } />
                        </label>
                        <button class="btn" disabled>{ic_setting()}"视图配置"</button>
                        <button class="btn pri" on:click=move |_| show_new.set(!show_new.get())>
                            {ic_add()}
                            "新建 Entry"
                        </button>
                    </div>
                    <div class="filters">
                        {move || if expr_mode.get() {
                            view! {
                                <input class="inp" style="flex:1" placeholder="Task = \"Open\" AND present(Priority)"
                                    prop:value=expr_text
                                    on:input=move |ev| expr_text.set(event_target_value(&ev)) />
                                <button class="btn" on:click=move |_| {
                                    let Some(ws_id) = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone()) else { return };
                                    let expr = expr_text.get();
                                    spawn_local(async move {
                                        match parse_view_query(&ws_id, &expr).await {
                                            Ok(ast) => { query_ast.set(ast); expr_mode.set(false); error.set(None); }
                                            Err(e) => error.set(Some(e)),
                                        }
                                    });
                                }>"应用"</button>
                                <button class="btn" on:click=move |_| expr_mode.set(false)>"取消"</button>
                            }.into_any()
                        } else {
                            view! {
                                {move || chips(&query_ast.get()).into_iter().map(|c| chip_view(c, query_ast)).collect::<Vec<_>>()}
                                <select class="inp" style="width:130px" prop:value=new_cond_label
                                    on:change=move |ev| new_cond_label.set(event_target_value(&ev))>
                                    <option value="">"＋ 条件"</option>
                                    {move || schemas.get().into_iter().map(|s| view! {
                                        <option value=s.name.clone()>{s.title.clone()}</option>
                                    }).collect::<Vec<_>>()}
                                </select>
                                <button class="btn" on:click=move |_| {
                                    let name = new_cond_label.get();
                                    if name.is_empty() { return; }
                                    let mut cs = chips(&query_ast.get());
                                    cs.push(CondChip::Label { name, op: "present".into(), value: serde_json::Value::Null });
                                    query_ast.set(build_query(&cs));
                                    new_cond_label.set(String::new());
                                }>"添加"</button>
                                <button class="btn" on:click=move |_| {
                                    let ast = query_ast.get();
                                    // 表达式文本由服务端格式化，避免前端复刻语法
                                    let Some(ws_id) = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone()) else { return };
                                    spawn_local(async move {
                                        if let Ok(s) = format_view_query(&ws_id, &ast).await {
                                            expr_text.set(s);
                                        }
                                        expr_mode.set(true);
                                    });
                                }>"表达式"</button>
                            }.into_any()
                        }}
                        <button class="btn" style="margin-left:auto" on:click=move |_| {
                            let Some(v) = active_view.get() else { return };
                            let ast = query_ast.get();
                            let cols = v.columns.clone();
                            let shared = v.is_shared;
                            let id = v.id.clone();
                            let name = v.name.clone();
                            let field = v.sort.field.clone();
                            let desc = v.sort.desc;
                            spawn_local(async move {
                                if let Ok(saved) =
                                    update_view(&id, &name, &ast, &field, desc, &cols, shared).await
                                {
                                    view_list.update(|l| {
                                        if let Some(slot) = l.iter_mut().find(|x| x.id == saved.id) {
                                            *slot = saved.clone();
                                        }
                                    });
                                    active_view.set(Some(saved));
                                }
                            });
                        }>"保存视图"</button>
                        <span class="mut">{move || {
                            let (field, desc) = active_view
                                .get()
                                .map(|v| (v.sort.field, v.sort.desc))
                                .unwrap_or_else(|| ("updatedAt".to_string(), true));
                            format!("排序：{}", sort_label(&field, desc))
                        }}</span>
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
                            <EntryTable
                                data
                                schemas
                                selected
                                columns=Signal::derive(move || {
                                    active_view.get().map(|v| v.columns).unwrap_or_default()
                                })
                                sort_field=Signal::derive(move || {
                                    active_view
                                        .get()
                                        .map(|v| v.sort.field)
                                        .unwrap_or_else(|| "updatedAt".to_string())
                                })
                                sort_desc=Signal::derive(move || {
                                    active_view.get().map(|v| v.sort.desc).unwrap_or(true)
                                })
                                on_sort=Callback::new(move |field: String| {
                                    // 后端 SortField 仅支持 updatedAt/createdAt/title，
                                    // 未知字段会被拒绝；这里只接受受支持字段。
                                    if !matches!(field.as_str(), "updatedAt" | "createdAt" | "title") {
                                        return;
                                    }
                                    let (cur_field, cur_desc) = active_view
                                        .get()
                                        .map(|v| (v.sort.field, v.sort.desc))
                                        .unwrap_or_else(|| ("updatedAt".to_string(), true));
                                    let desc = if field == cur_field { !cur_desc } else { true };
                                    if let Some(mut v) = active_view.get() {
                                        v.sort.field = field;
                                        v.sort.desc = desc;
                                        active_view.set(Some(v));
                                    }
                                    page_signal.set(1);
                                })
                            />
                            <div class="pager">
                                <button on:click=move |_| page_signal.update(|p| *p = (*p - 1).max(1))>"‹"</button>
                                {move || {
                                    let pages = ((total_signal.get() + page_size - 1) / page_size).max(1);
                                    (1..=pages.min(9)).map(|p| {
                                        let cur = page_signal.get();
                                        view! {
                                            <button class=if p == cur { "on" } else { "" }
                                                on:click=move |_| page_signal.set(p)>{p}</button>
                                        }
                                    }).collect::<Vec<_>>()
                                }}
                                <button on:click=move |_| page_signal.update(|p| *p += 1)>"›"</button>
                                <span class="mut">{move || format!("共 {} 条", total_signal.get())}</span>
                            </div>
                        </div>
                        <EntryPanel code=selected slug=slug().to_string() schemas refresh />
                    </div>
                </div>
            </div>

            {move || if show_view_dialog.get() {
                let ws_id = data.get().and_then(|r| r.ok()).map(|(w, _, _)| w.id.clone());
                view! {
                    <div class="dmodal">
                        <div class="panel dmbox">
                            <h3>"新建视图"</h3>
                            <input class="inp" placeholder="视图名称" prop:value=view_name_input
                                on:input=move |ev| view_name_input.set(event_target_value(&ev)) />
                            <input class="inp" placeholder="展示为列的标签（逗号分隔，可空）" prop:value=view_columns_input
                                on:input=move |ev| view_columns_input.set(event_target_value(&ev)) />
                            <label style="display:flex;gap:6px;align-items:center">
                                <input type="checkbox" prop:checked=view_shared_input
                                    on:change=move |ev| view_shared_input.set(event_target_checked(&ev)) />
                                "共享给工作空间"
                            </label>
                            <div style="display:flex;gap:8px;justify-content:flex-end">
                                <button class="btn" on:click=move |_| show_view_dialog.set(false)>"取消"</button>
                                <button class="btn pri" on:click=move |_| {
                                    let Some(ws_id) = ws_id.clone() else { return };
                                    let name = view_name_input.get();
                                    let shared = view_shared_input.get();
                                    let cols: Vec<String> = view_columns_input.get().split(',')
                                        .map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                                    spawn_local(async move {
                                        if let Ok(v) = create_view(&ws_id, &name, &serde_json::json!({"and": []}),
                                            "updatedAt", true, &cols, shared).await {
                                            view_list.update(|l| l.push(v.clone()));
                                            set_active(Some(v));
                                            show_view_dialog.set(false);
                                        }
                                    });
                                }>"创建"</button>
                            </div>
                        </div>
                    </div>
                }.into_any()
            } else { view! { <div></div> }.into_any() }}
        </div>
    }
}

#[component]
fn WorkspaceSidebar(
    slug: String,
    name: RwSignal<String>,
    views: RwSignal<Vec<View>>,
    active: RwSignal<Option<String>>,
    on_select: Callback<String>,
    on_new: Callback<()>,
    on_delete: Callback<String>,
) -> impl IntoView {
    let list = move || views.get();
    let mine = move || list().into_iter().filter(|v| !v.is_shared).collect::<Vec<_>>();
    let shared = move || list().into_iter().filter(|v| v.is_shared).collect::<Vec<_>>();

    let row = move |v: View, shared_mark: bool| {
        let id = v.id.clone();
        let name = v.name.clone();
        let is_active = {
            let id = id.clone();
            move || active.get().as_deref() == Some(id.as_str())
        };
        let click_id = id.clone();
        let del_id = id.clone();
        view! {
            <div class=move || if is_active() { "it on" } else { "it" }
                 on:click=move |_| on_select.run(click_id.clone())>
                {if shared_mark { ic_share().into_any() } else { ic_folder().into_any() }}
                <span style="flex:1">{name}</span>
                <button class="ibtn" title="删除视图" on:click=move |ev| {
                    ev.stop_propagation();
                    on_delete.run(del_id.clone());
                }>"×"</button>
            </div>
        }
        .into_any()
    };

    view! {
        <aside class="panel wside">
            <div style="padding:8px 12px;display:flex;gap:8px;align-items:center">
                <b>{move || name.get()}</b>
            </div>
            <div class="grp">"我的视图"</div>
            {move || mine().into_iter().map(|v| row(v, false)).collect::<Vec<_>>()}
            <div class="grp">"共享视图"</div>
            {move || shared().into_iter().map(|v| row(v, true)).collect::<Vec<_>>()}
            <div class="it" style="color:var(--ink3)" on:click=move |_| on_new.run(())>
                {ic_add()}"新建视图"
            </div>
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

/// 动态列条目表：列来自当前视图（`columns` 里的标签 name），标题/更新时间可点击排序。
#[component]
fn EntryTable(
    data: RwSignal<Option<Result<(Workspace, Vec<Entry>, Vec<LabelSchema>), String>>>,
    schemas: RwSignal<Vec<LabelSchema>>,
    selected: RwSignal<String>,
    columns: Signal<Vec<String>>,
    sort_field: Signal<String>,
    sort_desc: Signal<bool>,
    on_sort: Callback<String>,
) -> impl IntoView {
    let cols = move || columns.get();
    // 当前排序列的升降序箭头；未排序列为空。
    let arrow = move |field: &str| -> &'static str {
        if sort_field.get() == field {
            if sort_desc.get() {
                " ↓"
            } else {
                " ↑"
            }
        } else {
            ""
        }
    };
    // 可排序表头。后端 SortField 仅支持 title/updatedAt/createdAt，
    // 标签列因此不接排序，避免提交未知字段被服务端拒绝。
    let sortable_th = move |field: &'static str, label: &'static str| {
        view! {
            <th class="sortable" on:click=move |_| on_sort.run(field.to_string())>
                {move || format!("{label}{}", arrow(field))}
            </th>
        }
    };

    view! {
        <table class="tbl">
            <thead>
                <tr>
                    <th>"Code"</th>
                    {sortable_th("title", "标题")}
                    {move || cols().iter().map(|name| {
                        let title = schemas.get().into_iter()
                            .find(|s| &s.name == name)
                            .map(|s| s.title)
                            .unwrap_or_else(|| name.clone());
                        view! { <th>{title}</th> }
                    }).collect::<Vec<_>>()}
                    {sortable_th("updatedAt", "更新时间")}
                </tr>
            </thead>
            <tbody>
                {move || match data.get() {
                    None => view! {
                        <tr><td colspan="20" class="empty">"加载中…"</td></tr>
                    }.into_any(),
                    Some(Err(e)) => view! {
                        <tr><td colspan="20" class="empty error">{e.clone()}</td></tr>
                    }.into_any(),
                    Some(Ok((_ws, items, _))) if items.is_empty() => view! {
                        <tr><td colspan="20" class="empty">"暂无条目，点击「新建 Entry」创建"</td></tr>
                    }.into_any(),
                    Some(Ok((_ws, items, _))) => {
                        let sc = schemas.get();
                        items.iter().map(|e| {
                            let code = e.code.clone();
                            let code_for_class = e.code.clone();
                            let code_for_click = e.code.clone();
                            let labels = e.labels.clone();
                            let names = cols();
                            view! {
                                <tr
                                    class=move || if selected.get() == code_for_class { "sel".to_string() } else { String::new() }
                                    on:click=move |_| selected.set(code_for_click.clone())
                                >
                                    <td class="code">{code.clone()}</td>
                                    <td>{e.title.clone()}</td>
                                    {names.iter().map(|name| {
                                        let lv = labels.iter()
                                            .find(|l| &l.label_name == name)
                                            .map(|l| l.value.clone());
                                        match lv {
                                            None => view! { <td class="mut">"—"</td> }.into_any(),
                                            Some(v) => {
                                                let s = value_to_string(&v);
                                                let is_enum = sc.iter().any(|sch| {
                                                    &sch.name == name && sch.value_type == "enum"
                                                });
                                                if is_enum {
                                                    let cls = label_chip_class(name, &s);
                                                    view! {
                                                        <td><span class=format!("chip {cls}")>{display_enum_value(&s)}</span></td>
                                                    }.into_any()
                                                } else {
                                                    view! { <td>{s}</td> }.into_any()
                                                }
                                            }
                                        }
                                    }).collect::<Vec<_>>()}
                                    <td class="mut">{short_time(&e.updated_at)}</td>
                                </tr>
                            }
                        }).collect::<Vec<_>>().into_any()
                    }
                }}
            </tbody>
        </table>
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

/// 一枚可移除的条件芯片：点击 × 后从 AST 中剔除并回写。
fn chip_view(chip: CondChip, ast: RwSignal<serde_json::Value>) -> impl IntoView {
    let label = match &chip {
        CondChip::Label { name, op, value } => format!("{name} {op} {}", value_to_string(value)),
        CondChip::Time { field, op, value } => format!("{field} {op} {}", value_to_string(value)),
        CondChip::Text { keyword } => format!("全文：「{keyword}」"),
    };
    let target = chip.clone();
    view! {
        <span class="chip sel">
            {label}
            <button class="ibtn" title="移除" on:click=move |_| {
                let mut cs = chips(&ast.get());
                cs.retain(|c| c != &target);
                ast.set(build_query(&cs));
            }>"×"</button>
        </span>
    }
}

fn sort_label(field: &str, desc: bool) -> String {
    let name = match field {
        "title" => "标题",
        "createdAt" => "创建时间",
        "updatedAt" | "" => "更新时间",
        other => other,
    };
    format!("{name} {}", if desc { "↓" } else { "↑" })
}
