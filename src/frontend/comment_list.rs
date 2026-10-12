use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::agent_side_effects::AgentSideEffects;
use crate::frontend::components::{logged_out, role_at_least, short_time, Avatar};
use crate::frontend::graphql_client::{
    comments, create_comment, delete_comment, me, members as fetch_members, my_role, update_comment,
    Comment, Member,
};
use crate::frontend::i18n::{use_i18n, I18nController};
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
    /// 评论输入框里粘贴图片上传完成后通知外层：只需要面板自己重取条目
    /// （`updated_at` 推前了，乐观并发版本号失效），不刷表——表格里看不到附件，
    /// 走 `on_changed` 会顺带把整张表重查、父布局重建、把评论 TinyEditor 整个
    /// 换掉，用户刚粘贴还在上传的图就丢了。与 `entry.rs` 全屏页 `on_changed`
    /// 是一致的分离。`on_changed` 仍然负责评论本身的增删改。
    on_uploaded: Callback<()>,
    /// 只读模式（匿名分享页）：列表照常取数 / 展示，但输入表单、每条评论的
    /// 编辑 / 删除按钮全部不渲染——读者无法写评论、更无法操作他人的评论。
    /// 取数逻辑仍保留，正文 HTML 仍渲染（评论是 Entry 的一部分）。
    #[prop(default = false)]
    read_only: bool,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let items = RwSignal::new(None::<Result<Vec<CommentView>, String>>);
    // 评论列表的「签名」，用来在 `load()` 末尾判断是否真的需要回写 items。
    // 见下方 `load()` 内 `my_id` / `role` 处的注释——同一类问题，items.set 也得守护。
    // 用 (id, updated_at_str) 而不是只比 id，是因为编辑评论时 id 不变但 body 改了、
    // 服务端的 updated_at 也会被推前，那种刷新必须透传到 UI。
    let items_sig: RwSignal<Vec<(String, String)>> = RwSignal::new(Vec::new());
    let role = RwSignal::new(String::new());
    let my_id = RwSignal::new(String::new());
    // @ 提及候选：评论编辑器挂在工作空间里，候选就是工作空间成员。
    // 单独信号而不是跟 role 一起拿，是因为 role 经常变（用户角色变更）
    // 但成员名单不会每个 tick 都变——让 TinyEditor 在挂载时一次性快照。
    let ws_members = RwSignal::new(Vec::<Member>::new());
    // 首次取到成员名单（成功或失败）的标记。编辑器必须等这个信号亮了才挂：
    // mount_when_ready 只跑一次（节点上的 __rodeo_editor 守卫会跳过重挂），
    // 因此挂载时 ws_members 还是空 Vec 的话，下拉永远 enabled=false。
    // 这里与 entry.rs EntryPanel 的「data 装载后编辑器才出现」是同一个不变式
    // ——EntryPanel 靠 data → editor 的天然顺序保证；这里父组件一上来就渲染编辑器，
    // 必须手动加一层闸。
    let members_loaded = RwSignal::new(false);
    let draft = RwSignal::new(String::new());
    let editing = RwSignal::new(None::<String>);
    let edit_body = RwSignal::new(String::new());
    let confirm_del = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);
    // 发表后要清空输入框，而 TinyEditor 只在挂载时读一次 initial，改 prop 不会清内容。
    // 用一个「卸载 → 让出一帧 → 重新挂载」的开关把编辑器整个换掉。
    let composer_ready = RwSignal::new(true);

    let load = move || {
        // 组件可能已被卸载：双击行会先由第一次单击挂出侧栏，随即跳转条目全屏页
        // 卸下整个工作空间页，而侧栏那次请求多半还没回来；此后读 `code` /
        // `workspace_id` 会 panic。卸载后结果也无处可放，直接丢弃。
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
                    // 取成员名单：给评论编辑器做 @ 候选。失败就空列表。
                    let ml = fetch_members(&ws).await.unwrap_or_default();
                    Ok::<_, String>((list, user, r, ml))
                }
                .await;
                // 请求返回前组件也可能已经被卸载（同上），此时 `code` 已释放，读取会 panic。
                if items.is_disposed() || code.get_untracked() != c {
                    return;
                }
                match r {
                    Ok((list, user, r, ml)) => {
                        // 只在真正变化时写入。这两项被「发表评论」子树读取，而本次刷新常由
                        // 外部原因触发（粘贴图片上传成功后会推进 entry.updated_at，宿主随即
                        // 重取条目，连带重跑本组件的 load）。无条件 set 会重建那个子树，把
                        // 编辑器和刚插入的图片一起换掉——图片于是「粘贴后一闪就没」。
                        let uid = user.map(|u| u.id).unwrap_or_default();
                        if my_id.get_untracked() != uid {
                            my_id.set(uid);
                        }
                        if role.get_untracked() != r {
                            role.set(r);
                        }
                        // 成员表也守护：list 翻页或评论增删不会动成员，无脑 set
                        // 会顺带让评论编辑器重挂——同 items 的守护一个理由。
                        if ws_members.get_untracked() != ml {
                            ws_members.set(ml);
                        }
                        // 首次取到成员就把闸门打开，编辑器得以挂上正确的名单。
                        // 放在「值变了才写」的 guard 之后，避免同名单 no-op 时漏亮信号。
                        members_loaded.set(true);
                        // 同理守护 items：用 (id, updated_at) 当签名，list 里有任何
                        // 新评论、删除评论、编辑评论（id 不变但 updated_at 推前了）才算
                        // 真变化；其他时候跳过 set，评论子树和编辑器整体保留。
                        let new_sig: Vec<(String, String)> = list
                            .iter()
                            .map(|c| (c.id.clone(), c.updated_at.clone()))
                            .collect();
                        if items_sig.get_untracked() != new_sig {
                            items_sig.set(new_sig);
                            items.set(Some(Ok(list
                                .into_iter()
                                .map(|comment| CommentView {
                                    html: delta_to_html(&comment.body),
                                    comment,
                                })
                                .collect())));
                        }
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

    // AI 写了新评论（comment 域）→ 当前组件的列表要拉一次。`load` 自身有
    // `code`/`workspace_id` 防御，未挂载（`code` 为空）时早返。
    if let Some(eff) = use_context::<AgentSideEffects>() {
        Effect::new(move |_| {
            eff.tick.track();
            let Some(hint) = eff.last.get_untracked() else {
                return;
            };
            if hint.domain == "comment" {
                load();
            }
        });
    }

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
                    "{}（{}）",
                    i18n.t("comment.section_title"),
                    items.get().and_then(|r| r.ok()).map(|v| v.len()).unwrap_or(0),
                )}
            </div>

            {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}

            {move || {
                let can_write = role_at_least(&role.get(), "worker");
                let can_moderate = role_at_least(&role.get(), "maintainer");
                let editing_now = editing.get();
                match items.get() {
                    None => view! { <div class="mut">{move || i18n.t("common.loading")}</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            view! { <div class="mut">{move || i18n.t("comment.empty")}</div> }.into_any()
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
                                        // 只读模式：即便当前用户满足角色条件，也
                                        // 强制关闭一切编辑 / 删除操作入口——分享
                                        // 公开页共用这个面板，避免忘记改这里。
                                        let can_edit = !read_only && can_write && is_mine;
                                        let can_delete = !read_only && (is_mine || can_moderate);
                                        let is_editing = editing_now.as_deref() == Some(id.as_str());
                                        let author = c
                                            .created_by_account
                                            .as_ref()
                                            .map(|a| a.name.clone())
                                            .unwrap_or_else(|| i18n.t("comment.deactivated_author"));
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
                                                            <span class="mut">{move || i18n.t("comment.edited")}</span>
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
                                                                >{move || i18n.t("common.edit")}</button>
                                                            })}
                                                            {can_delete.then(|| view! {
                                                                <button
                                                                    class="btn sm danger"
                                                                    on:click=move |_| confirm_del.set(Some(id_for_del.clone()))
                                                                >{move || i18n.t("common.delete")}</button>
                                                            })}
                                                        </span>
                                                    </div>
                                                    {if is_editing {
                                                        // 闸门：ws_members 第一次没就位就显示加载占位，
                                                        // 让 TinyEditor 在挂载时已经持有非空成员名单。
                                                        if !members_loaded.get() {
                                                            view! { <div class="c-edit"><div class="mut">{move || i18n.t("common.loading")}</div></div> }.into_any()
                                                        } else {
                                                            let ms = ws_members.get_untracked();
                                                            view! {
                                                                <div class="c-edit">
                                                                    <TinyEditor initial=body_for_editor entry_code=code
                                                                        on_change=on_edit_change on_uploaded=on_uploaded
                                                                        members=ms />
                                                                    <div class="c-edit-acts">
                                                                        <button class="btn pri sm" on:click={
                                                                            let sid = id_for_save.clone();
                                                                            move |_| save_edit(sid.clone())
                                                                        }>{move || i18n.t("common.save")}</button>
                                                                        <button class="btn sm" on:click=move |_| editing.set(None)>{move || i18n.t("common.cancel")}</button>
                                                                    </div>
                                                                </div>
                                                            }.into_any()
                                                        }
                                                    } else {
                                                        view! {
                                                            <div class="c-body" inner_html=body_html></div>
                                                        }.into_any()
                                                    }}
                                                    {(confirm_del.get().as_deref() == Some(id_for_confirm.as_str())).then(|| view! {
                                                        <div class="c-confirm">
                                                            <span class="mut">{move || i18n.t("comment.delete_confirm")}</span>
                                                            <button class="btn danger sm" on:click={
                                                                let sid = id_for_confirm.clone();
                                                                move |_| do_delete(sid.clone())
                                                            }>{move || i18n.t("comment.delete_confirm_yes")}</button>
                                                            <button class="btn sm" on:click=move |_| confirm_del.set(None)>{move || i18n.t("common.cancel")}</button>
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
                // 只读模式：连发表评论的入口都不渲染——匿名读者来这是看，不是写。
                if read_only {
                    return view! { <div></div> }.into_any();
                }
                if !role_at_least(&role.get(), "worker") {
                    return view! { <div></div> }.into_any();
                }
                // 已有评论进入编辑态时收起新增框：页面上同时只能有一个带工具栏的编辑器，
                // glue.js 的工具栏清理作用域依赖这条不变式。
                if editing.get().is_some() {
                    return view! {
                        <div class="mut">{move || i18n.t("comment.editing_hint")}</div>
                    }.into_any();
                }
                view! {
                    <div class="c-composer">
                        {move || if composer_ready.get() {
                            // 闸门：首次成员名单还没回就不挂编辑器，否则 mention 模块
                            // 会在 enabled=false 上定型，再来多少成员都救不回来。
                            if !members_loaded.get() {
                                view! { <div class="mut">{move || i18n.t("common.loading")}</div> }.into_any()
                            } else {
                                let ms = ws_members.get_untracked();
                                view! {
                                    <TinyEditor initial=String::new() entry_code=code
                                        on_change=on_composer_change on_uploaded=on_uploaded
                                        members=ms />
                                }.into_any()
                            }
                        } else {
                            view! { <div class="mut">"…"</div> }.into_any()
                        }}
                        <div class="c-edit-acts">
                            <button class="btn pri sm" on:click=submit>{move || i18n.t("comment.submit")}</button>
                        </div>
                    </div>
                }.into_any()
            }}
        </div>
    }
}
