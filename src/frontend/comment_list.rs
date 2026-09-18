use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::components::{logged_out, role_at_least, short_time, Avatar};
use crate::frontend::graphql_client::{
    comments, create_comment, delete_comment, me, my_role, update_comment, Comment,
};
use crate::frontend::icons::ic_comment;
use crate::frontend::tiny_editor::{delta_to_html, TinyEditor};

/// 列表项：正文的 HTML 在取数时一次算好。
/// 不在 `view!` 里现算是因为 SSR 阶段没有 JS 桥，渲染会得到空串。
#[derive(Clone)]
struct CommentView {
    comment: Comment,
    html: String,
}

/// 让出一个宏任务。Leptos 只在这次让渡之后才会把信号变更真正落到 DOM 上，
/// 因此「卸载再挂回编辑器」中间必须有它。
/// 双 `cfg` 两个实现，照 `graphql()` 的写法办：`gloo-timers` 只有 wasm 实现，
/// native 目标下这个函数体只能是空的（SSR 阶段也不会走到发表评论）。
#[cfg(target_arch = "wasm32")]
async fn next_tick() {
    gloo_timers::future::TimeoutFuture::new(0).await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn next_tick() {}

#[component]
pub fn CommentList(
    /// 条目编码；空串时不取数也不渲染（详情面板未选中条目时）。
    code: Signal<String>,
    /// 所属工作空间，用来查当前用户角色。
    workspace_id: Signal<String>,
    /// 评论增删改后通知外层：表格的「更新时间」列与评论徽标要跟着刷新。
    on_changed: Callback<()>,
) -> impl IntoView {
    let items = RwSignal::new(None::<Result<Vec<CommentView>, String>>);
    let role = RwSignal::new(String::new());
    let my_id = RwSignal::new(String::new());
    let draft = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<String>);
    let edit_body = RwSignal::new(String::new());
    let confirm_del = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);
    // 发表后要清空输入框，而 TinyEditor 只在挂载时读一次 initial，改 prop 不会清内容。
    // 用一个「卸载 → 让出一帧 → 重新挂载」的开关把编辑器整个换掉。
    let composer_ready = RwSignal::new(true);

    let load = move || {
        // 组件可能已被卸载：双击行会先由第一次单击挂出侧栏，随即被全屏浮层替换，
        // 而侧栏那次请求多半还没回来；此后读 `code` / `workspace_id` 会 panic。
        // 卸载后结果也无处可放，直接丢弃。
        if items.is_disposed() {
            return;
        }
        let c = code.get();
        let ws = workspace_id.get();
        if c.is_empty() || ws.is_empty() {
            return;
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let r = async {
                    let list = comments(&c).await?;
                    let user = me().await?;
                    let r = my_role(&ws).await?;
                    Ok::<_, String>((list, user, r))
                }
                .await;
                // 请求返回前组件也可能已经被卸载（同上），此时 `code` 已释放，读取会 panic。
                if items.is_disposed() || code.get_untracked() != c {
                    return;
                }
                match r {
                    Ok((list, user, r)) => {
                        my_id.set(user.map(|u| u.id).unwrap_or_default());
                        role.set(r);
                        items.set(Some(Ok(list
                            .into_iter()
                            .map(|comment| CommentView {
                                html: delta_to_html(&comment.body),
                                comment,
                            })
                            .collect())));
                    }
                    Err(e) => items.set(Some(Err(e))),
                }
            });
        }
    };

    Effect::new(move |_| {
        code.get();
        workspace_id.get();
        if logged_out() {
            return;
        }
        load();
    });

    let on_composer_change = Callback::new(move |d: String| draft.set(d));
    let on_edit_change = Callback::new(move |d: String| edit_body.set(d));

    let submit = move |_| {
        let c = code.get();
        let body = draft.get();
        spawn_local(async move {
            match create_comment(&c, &body).await {
                Ok(_) => {
                    error.set(None);
                    // 先卸载编辑器，等一个宏任务让这次卸载真正落到 DOM 上，再挂回来
                    // ——内容自然是空的。中间不能省这一步：两次 set 同一批次完成的话
                    // 观察者只看到最终值，编辑器根本不会卸载。
                    composer_ready.set(false);
                    next_tick().await;
                    composer_ready.set(true);
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let save_edit = move |id: String| {
        let c = code.get();
        let body = edit_body.get();
        spawn_local(async move {
            match update_comment(&c, &id, &body).await {
                Ok(_) => {
                    editing.set(None);
                    error.set(None);
                    load();
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let do_delete = move |id: String| {
        let c = code.get();
        spawn_local(async move {
            match delete_comment(&c, &id).await {
                Ok(_) => {
                    confirm_del.set(None);
                    error.set(None);
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    view! {
        <div class="comments">
            <div class="grp-h">
                {ic_comment()}
                {move || format!(
                    "评论（{}）",
                    items.get().and_then(|r| r.ok()).map(|v| v.len()).unwrap_or(0),
                )}
            </div>

            {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}

            {move || {
                let can_write = role_at_least(&role.get(), "worker");
                let can_moderate = role_at_least(&role.get(), "maintainer");
                let editing_now = editing.get();
                match items.get() {
                    None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            view! { <div class="mut">"暂无评论"</div> }.into_any()
                        } else {
                            let mine = my_id.get();
                            view! {
                                <div class="c-list">
                                    {list.into_iter().map(|cv| {
                                        let c = cv.comment.clone();
                                        let id = c.id.clone();
                                        let id_for_edit = id.clone();
                                        let id_for_save = id.clone();
                                        let id_for_del = id.clone();
                                        let id_for_confirm = id.clone();
                                        let is_mine = c.created_by == mine;
                                        let can_edit = can_write && is_mine;
                                        let can_delete = is_mine || can_moderate;
                                        let is_editing = editing_now.as_deref() == Some(id.as_str());
                                        let author = c
                                            .created_by_account
                                            .as_ref()
                                            .map(|a| a.name.clone())
                                            .unwrap_or_else(|| "已注销账号".to_string());
                                        let body_html = cv.html.clone();
                                        // 两份克隆：「编辑」按钮的闭包和编辑器组件各要一份，
                                        // 一份会被闭包 move 走，另一份留给 `initial=`。
                                        let body_for_btn = c.body.clone();
                                        let body_for_editor = c.body.clone();
                                        let edited = c.updated_at != c.created_at;
                                        view! {
                                            <div class="c-item">
                                                <Avatar text=author.clone() />
                                                <div class="c-main">
                                                    <div class="c-head">
                                                        <span class="c-name">{author}</span>
                                                        <span class="t">{short_time(&c.created_at)}</span>
                                                        {edited.then(|| view! {
                                                            <span class="mut">"已编辑"</span>
                                                        })}
                                                        <span style="margin-left:auto" class="c-acts">
                                                            {can_edit.then(|| view! {
                                                                <button
                                                                    class="btn sm"
                                                                    disabled=is_editing
                                                                    on:click=move |_| {
                                                                        editing.set(Some(id_for_edit.clone()));
                                                                        edit_body.set(body_for_btn.clone());
                                                                    }
                                                                >"编辑"</button>
                                                            })}
                                                            {can_delete.then(|| view! {
                                                                <button
                                                                    class="btn sm danger"
                                                                    on:click=move |_| confirm_del.set(Some(id_for_del.clone()))
                                                                >"删除"</button>
                                                            })}
                                                        </span>
                                                    </div>
                                                    {if is_editing {
                                                        view! {
                                                            <div class="c-edit">
                                                                <TinyEditor initial=body_for_editor entry_code=code on_change=on_edit_change />
                                                                <div class="c-edit-acts">
                                                                    <button class="btn pri sm" on:click={
                                                                        let sid = id_for_save.clone();
                                                                        move |_| save_edit(sid.clone())
                                                                    }>"保存"</button>
                                                                    <button class="btn sm" on:click=move |_| editing.set(None)>"取消"</button>
                                                                </div>
                                                            </div>
                                                        }.into_any()
                                                    } else {
                                                        view! {
                                                            <div class="c-body" inner_html=body_html></div>
                                                        }.into_any()
                                                    }}
                                                    {(confirm_del.get().as_deref() == Some(id_for_confirm.as_str())).then(|| view! {
                                                        <div class="c-confirm">
                                                            <span class="mut">"删除后不可恢复，确定？"</span>
                                                            <button class="btn danger sm" on:click={
                                                                let sid = id_for_confirm.clone();
                                                                move |_| do_delete(sid.clone())
                                                            }>"确认删除"</button>
                                                            <button class="btn sm" on:click=move |_| confirm_del.set(None)>"取消"</button>
                                                        </div>
                                                    })}
                                                </div>
                                            </div>
                                        }
                                    }).collect::<Vec<_>>()}
                                </div>
                            }.into_any()
                        }
                    }
                }
            }}

            {move || {
                if !role_at_least(&role.get(), "worker") {
                    return view! { <div></div> }.into_any();
                }
                // 已有评论进入编辑态时收起新增框：页面上同时只能有一个带工具栏的编辑器，
                // glue.js 的工具栏清理作用域依赖这条不变式。
                if editing.get().is_some() {
                    return view! {
                        <div class="mut">"正在编辑评论，保存或取消后可继续发表"</div>
                    }.into_any();
                }
                view! {
                    <div class="c-composer">
                        {move || if composer_ready.get() {
                            view! {
                                <TinyEditor initial=String::new() entry_code=code on_change=on_composer_change />
                            }.into_any()
                        } else {
                            view! { <div class="mut">"…"</div> }.into_any()
                        }}
                        <div class="c-edit-acts">
                            <button class="btn pri sm" on:click=submit>"发表评论"</button>
                        </div>
                    </div>
                }.into_any()
            }}
        </div>
    }
}
