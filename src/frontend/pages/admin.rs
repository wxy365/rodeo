use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};

use crate::frontend::components::{copy_to_clipboard, logged_out, short_time};
use crate::frontend::graphql_client::{
    accounts, create_account, set_account_status, update_account, AdminAccount, CreatedAccount,
};
use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::{ic_add, ic_back, ic_copy};
use crate::frontend::{use_auth, User};

/// 当前站点的 origin（形如 `http://127.0.0.1:3099`），用于拼分享文案里的登录地址。
/// 非 wasm 目标返回空串：`created` 只在点击回调里被赋值，所以服务端渲染永远走不到这段文案；
/// 分成两个 cfg 版本是为了让 wasm 那个不在原生构建里编译。
#[cfg(target_arch = "wasm32")]
fn origin() -> String {
    leptos::web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn origin() -> String {
    String::new()
}

#[component]
pub fn Admin() -> impl IntoView {
    let auth = use_auth();
    let i18n: I18nController = use_i18n();
    let email = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let make_admin = RwSignal::new(false);
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    // 刚建出来的账号。初始密码在服务端只以 Argon2 哈希存在，明文只在响应里出现过
    // 这一次——所以必须当场展示并可复制，刷新页面它就永远拿不回来了。
    let created = RwSignal::new(None::<CreatedAccount>);
    let copied = RwSignal::new(false);

    // ---------- 账号列表（冻结 / 解冻 / 注销） ----------

    let account_list = RwSignal::new(None::<Vec<AdminAccount>>);
    let list_error = RwSignal::new(None::<String>);
    let list_refresh = RwSignal::new(0u32);
    // 正在等二次确认的那一行。注销不可逆，不做原生 `confirm`，就地变成「确认注销？」。
    let confirm_id = RwSignal::new(None::<String>);
    // 正在提交的那一行，用来禁用它的按钮、防连点。
    let row_busy = RwSignal::new(None::<String>);

    // ---------- 编辑姓名 / 角色 ----------

    // 正在编辑的那一行。留一份原始记录：回填姓名与角色要用它，判断「角色那一栏是否
    // 该禁用」也要用它（自己 / 内置管理员）。
    let edit_target = RwSignal::new(None::<AdminAccount>);
    let edit_name = RwSignal::new(String::new());
    let edit_admin = RwSignal::new(false);
    let edit_error = RwSignal::new(None::<String>);
    let edit_busy = RwSignal::new(false);

    // 保存编辑。成功后就地改那一行，与 `change_status` 同一个理由：整表重拉会把顺序和
    // 滚动位置一起动掉，而这里只改了两个字段。
    let save_edit = move |_| {
        let Some(target) = edit_target.get_untracked() else {
            return;
        };
        // 防连点：重复提交除了白跑一趟，还会把「改的是自己」那条分支也走两遍。
        if edit_busy.get_untracked() {
            return;
        }
        let name = edit_name.get_untracked().trim().to_string();
        if name.is_empty() {
            edit_error.set(Some(i18n.t("admin.name_required")));
            return;
        }
        let id = target.id.clone();
        let is_admin = edit_admin.get_untracked();
        edit_busy.set(true);
        spawn_local(async move {
            match update_account(&id, &name, is_admin).await {
                Ok(updated) => {
                    // 改的若是自己，右上角头像菜单里的名字也得跟着变——否则要刷新页面才更新。
                    if auth
                        .user
                        .get_untracked()
                        .is_some_and(|u| u.id == updated.id)
                    {
                        auth.user.set(Some(User {
                            id: updated.id.clone(),
                            email: updated.email.clone(),
                            name: updated.name.clone(),
                            is_admin: updated.is_admin,
                        }));
                    }
                    account_list.update(|l| {
                        if let Some(l) = l.as_mut() {
                            if let Some(row) = l.iter_mut().find(|a| a.id == updated.id) {
                                *row = updated;
                            }
                        }
                    });
                    edit_error.set(None);
                    edit_target.set(None);
                }
                Err(e) => edit_error.set(Some(e)),
            }
            edit_busy.set(false);
        });
    };

    // 列表只对系统管理员拉。`auth.user` 还没回来时也不拉——那时分不清
    // 「不是管理员」和「还在问」，拉了会白挨一个 Forbidden。
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        if !auth.user.get().map(|u| u.is_admin).unwrap_or(false) {
            return;
        }
        let _ = list_refresh.get();
        spawn_local(async move {
            match accounts().await {
                Ok(list) => {
                    account_list.set(Some(list));
                    list_error.set(None);
                }
                Err(e) => list_error.set(Some(e)),
            }
        });
    });

    // 冻结 / 解冻 / 注销。成功后就地改那一行，不整表重拉——重拉会把行顺序和滚动位置
    // 一起动掉，而这里只该变一个状态。
    // 写成 `Callback` 是为了让外层的多个 `on:click=move |_| change_status(...)` 各自
    // 拿到一份 clone；普通的 `move |...| { ... }` 闭包不是 Clone，多个内层闭包会争抢
    // 同一个实例，外层闭包被迫退化成 FnOnce。
    let change_status: Callback<(String, String)> = Callback::new(move |(id, status): (String, String)| {
        row_busy.set(Some(id.clone()));
        confirm_id.set(None);
        spawn_local(async move {
            match set_account_status(&id, &status).await {
                Ok(updated) => {
                    account_list.update(|l| {
                        if let Some(l) = l.as_mut() {
                            if let Some(row) = l.iter_mut().find(|a| a.id == updated.id) {
                                *row = updated;
                            }
                        }
                    });
                    list_error.set(None);
                }
                Err(e) => list_error.set(Some(e)),
            }
            row_busy.set(None);
        });
    });

    // 未登录、或本地令牌已被服务端判死，都回登录页。与 entry.rs 用同一条路径：
    // `logged_out()` 在 SSR 阶段恒为 true，所以只在 wasm 上跳转，否则服务端渲染会直接跳走。
    let navigate = use_navigate();
    // 返回按钮的文案与目的地看来源：来自工作空间（`?from=ws&slug=…`）就回那个工作
    // 空间并写「工作空间」；其它（包括工作空间列表 `→` 默认）走 `/workspaces` 并写
    // 「工作空间列表」。query 在 SSR 阶段可读，无需 wasm 分支。
    let query = use_query_map();
    let back_label = move || match query.get().get("from").as_deref() {
        Some("ws") => i18n.t("account.back_workspace"),
        _ => i18n.t("account.back_workspaces_list"),
    };
    let back_href = move || match (query.get().get("from").as_deref(), query.get().get("slug")) {
        (Some("ws"), Some(slug)) if !slug.is_empty() => format!("/{slug}"),
        _ => "/workspaces".to_string(),
    };
    let back = {
        let nav = navigate.clone();
        move |_| {
            let href = back_href();
            nav(&href, Default::default());
        }
    };
    Effect::new(move |_| {
        if cfg!(target_arch = "wasm32") && (logged_out() || auth.session_lost.get()) {
            navigate("/login", Default::default());
        }
    });

    let can_submit =
        move || !busy.get() && !email.get().trim().is_empty() && !name.get().trim().is_empty();

    let submit = move |_| {
        let e = email.get().trim().to_string();
        let n = name.get().trim().to_string();
        if e.is_empty() || n.is_empty() {
            error.set(Some(i18n.t("admin.email_name_required")));
            return;
        }
        // 防连点：初始密码是不可找回的，重复提交会白白作废一个密码。
        if busy.get_untracked() {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match create_account(&e, &n, make_admin.get_untracked(), None).await {
                Ok(c) => {
                    error.set(None);
                    copied.set(false);
                    created.set(Some(c));
                    email.set(String::new());
                    name.set(String::new());
                    make_admin.set(false);
                    // 新账号要出现在下面的列表里。
                    list_refresh.update(|n| *n += 1);
                }
                Err(msg) => error.set(Some(msg)),
            }
            busy.set(false);
        });
    };

    view! {
        <div class="page">
            <div class="crumb" style="display:flex;align-items:center;gap:8px">
                <button class="btn sm" on:click=back>{ic_back()}{back_label}</button>
                <span>{move || i18n.t("admin.crumb")}</span>
            </div>
            <div class="stats">
                <div class="panel stat">
                    <div class="v">
                        {move || account_list.get().map(|l| l.len().to_string()).unwrap_or_else(|| "—".to_string())}
                    </div>
                    <div class="l">{move || i18n.t("admin.stat_accounts")}</div>
                </div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">{move || i18n.t("admin.stat_workspaces")}</div></div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">{move || i18n.t("admin.stat_attachments")}</div></div>
            </div>

            {move || match auth.user.get() {
                // 令牌失效：说清楚为什么，否则这个面板会一直停在「加载中…」。
                None if auth.session_lost.get() => view! {
                    <div class="panel set-body"><div class="mut">{move || i18n.t("admin.session_lost")}</div></div>
                }.into_any(),
                // `user` 为 None 表示 me() 还没回来：此时既不能说「有权限」也不能说
                // 「无权限」，否则管理员会先看到一瞬「仅系统管理员可访问」。
                None => view! {
                    <div class="panel set-body"><div class="mut">{move || i18n.t("admin.loading")}</div></div>
                }.into_any(),
                Some(u) if !u.is_admin => view! {
                    <div class="panel set-body">
                        <h2>{move || i18n.t("admin.admin_only")}</h2>
                        <div class="mut">
                            {move || i18n.tf("admin.not_admin", &[("email", &u.email)])}
                            {move || i18n.t("admin.admin_email_help")}
                        </div>
                    </div>
                }.into_any(),
                Some(me) => {
                // 当前登录者：列表里自己那一行的操作要禁用。
                let me_id = me.id.clone();
                // 编辑弹窗另要一份 `me_id`：上面那份会被表格那层闭包 move 走，搬不回来。
                let me_id_for_edit = me.id.clone();
                view! {
                    <div class="panel set-body">
                        <h2>{move || i18n.t("admin.accounts_management")}</h2>
                        <div class="invite">
                            <input class="inp" placeholder={move || i18n.t("admin.create_email")} prop:value=email
                                on:input=move |ev| email.set(event_target_value(&ev)) />
                            <input class="inp" placeholder={move || i18n.t("admin.create_name")} prop:value=name
                                on:input=move |ev| name.set(event_target_value(&ev)) />
                            <label class="mut" style="display:flex;align-items:center;gap:6px;white-space:nowrap">
                                <input type="checkbox" prop:checked=make_admin
                                    on:change=move |ev| make_admin.set(event_target_checked(&ev)) />
                                {move || i18n.t("admin.set_admin")}
                            </label>
                            <button class="btn pri" disabled=move || !can_submit() on:click=submit>
                                {ic_add()}{move || i18n.t("admin.create_account")}
                            </button>
                        </div>
                        <div class="mut" style="margin-top:8px">
                            {move || i18n.t("admin.initial_password_hint")}
                        </div>
                        {move || error.get().map(|e| view! {
                            <div class="hint" style="margin-top:8px">{"⚠ "}{e}</div>
                        })}
                    </div>

                    {move || created.get().map(|c| {
                        let email_txt = c.account.email.clone();
                        let name_txt = c.account.name.clone();
                        let pw = c.initial_password.clone();
                        let origin_str = origin();
                        // 分发用的整段文案：它是要被原样转交给本人的，所以带上登录地址、
                        // 账号、初始密码和「登录后改密」的指引，而不是只丢一行数据。
                        // 这里直接 format! 一次：模板若包成闭包会把 `email_txt` / `pw` 一并
                        // move 走，导致下面的 `{email_txt}` / `{pw}` 再读时 use-after-move。
                        let share = format!(
                            "{intro}\n{login}\n{email_line}\n{pw_line}\n{change}\n{warn}",
                            intro = i18n.t("admin.share_intro"),
                            login = i18n.tf("admin.share_login_url", &[("url", &origin_str)]),
                            email_line = i18n.tf("admin.share_email", &[("email", &email_txt)]),
                            pw_line = i18n.tf("admin.share_password", &[("pw", &pw)]),
                            change = i18n.t("admin.share_change_pw"),
                            warn = i18n.t("admin.share_warning"),
                        );
                        view! {
                            <div class="panel set-body">
                                <h2>{move || i18n.t("admin.created_title")}</h2>
                                <div class="dmeta">
                                    <div><span class="mut">{move || i18n.t("admin.created_email")}</span>{email_txt.clone()}</div>
                                    <div><span class="mut">{move || i18n.t("admin.created_name")}</span>{name_txt}</div>
                                    <div><span class="mut">{move || i18n.t("admin.initial_password")}</span><code>{pw.clone()}</code></div>
                                </div>
                                <div style="display:flex;gap:8px;align-items:center;margin-top:10px">
                                    <button class="btn pri" on:click=move |_| {
                                        copy_to_clipboard(&share);
                                        copied.set(true);
                                    }>{ic_copy()}{move || i18n.t("admin.copy_share")}</button>
                                    {move || copied.get().then(|| view! {
                                        <span class="chip c-done">{move || i18n.t("admin.copied")}</span>
                                    })}
                                </div>
                                <div class="hint" style="margin-top:8px">
                                    {"⚠ "}
                                    {move || i18n.t("admin.password_warning")}
                                </div>
                            </div>
                        }
                    })}

                    <div class="panel set-body">
                        <h2>{move || i18n.t("admin.accounts")}</h2>
                        {move || list_error.get().map(|e| view! {
                            <div class="hint" style="margin-bottom:8px">{"⚠ "}{e}</div>
                        })}
                        {move || {
                        // 这层闭包会被反复调用，而里面那个 `For` 的 children 闭包要**拥有**一份
                        // `me_id`（String 不是 Copy）。所以在这里先克隆一份再交出去：外面的
                        // `me_id` 只被借用，这层闭包才是 `Fn` 而不是 `FnOnce`。
                        let me_id = me_id.clone();
                        match account_list.get() {
                            None => view! { <div class="mut">{move || i18n.t("admin.loading")}</div> }.into_any(),
                            Some(list) if list.is_empty() => {
                                view! { <div class="mut">{move || i18n.t("admin.no_accounts")}</div> }.into_any()
                            }
                            Some(_) => view! {
                                <table class="tbl">
                                    <thead>
                                        <tr>
                                            <th>{move || i18n.t("admin.col_email")}</th>
                                            <th>{move || i18n.t("admin.col_name")}</th>
                                            <th>{move || i18n.t("admin.col_role")}</th>
                                            <th>{move || i18n.t("admin.col_status")}</th>
                                            <th>{move || i18n.t("admin.col_created_at")}</th>
                                            <th style="width:240px"></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        <For
                                            each=move || account_list.get().unwrap_or_default()
                                            key=|a| a.id.clone()
                                            children=move |a: AdminAccount| {
                                                // 两道禁用的理由：不能操作自己（一点就把自己锁在外面），
                                                // 也不能动配置里指定的内置管理员（见服务端那两道守卫）。
                                                // 提前算成 `bool` 而不是闭包里的 `a.id == me_id` 比较，
                                                // 避免内层 `title=move || ...` 把 `a` / `me_id` 一并 move
                                                // 走，让外层闭包退化成 FnOnce。
                                                let is_self = a.id == me_id;
                                                let is_builtin = a.is_builtin;
                                                let locked = is_self || is_builtin;
                                                let status_cls = match a.status.as_str() {
                                                    "frozen" => "chip c-doing",
                                                    "deactivated" => "chip c-wont",
                                                    _ => "chip c-done",
                                                };
                                                // `status_txt` 闭包若直接 `move || a.status.as_str()` 会把
                                                // `a.status` 整个 move 走，导致下面 `frozen` / `deactivated`
                                                // 那两行还想读 `a.status` 时报「use after move」。先 clone
                                                // 一份出来让闭包只 move 这份 clone，`a` 仍可借用。
                                                let status_str = a.status.clone();
                                                let status_txt = move || match status_str.as_str() {
                                                    "frozen" => i18n.t("admin.status_frozen"),
                                                    "deactivated" => i18n.t("admin.status_deactivated"),
                                                    _ => i18n.t("admin.status_active"),
                                                };
                                                let frozen = a.status == "frozen";
                                                let deactivated = a.status == "deactivated";
                                                let next = if frozen { "active" } else { "frozen" };
                                                let id = a.id.clone();
                                                // 编辑弹窗要回填姓名与角色，所以它也得有一份。与 `id` 同理：
                                                // 外面只被借用，进内层 `on:click` 时每次现克隆一份。
                                                let edit_row = a.clone();
                                                view! {
                                                    <tr>
                                                        <td>{a.email.clone()}</td>
                                                        <td>{a.name.clone()}</td>
                                                        <td>{if a.is_admin { i18n.t("admin.role_admin") } else { i18n.t("admin.role_member") }}</td>
                                                        <td><span class=status_cls>{move || status_txt()}</span></td>
                                                        <td>{short_time(&a.created_at)}</td>
                                                        <td>
                                                            // 这一段必须待在 `move ||` 里：`confirm_id` / `row_busy`
                                                            // 变了要就地换掉这一格的内容，而把信号读在 `view!` 里
                                                            // 只会在建行时求值一次，之后再也没人重算。
                                                            // 列本身由 `each` 的键决定，改状态会换行，确认态却不会。
                                                            {move || {
                                                                // 外面的 `id` 只是被借用，交给内层 `on:click` 的每次克隆一份，
                                                                // 否则 move 出去一次就把这层闭包变成 `FnOnce`。
                                                                let id = id.clone();
                                                                // `change_status` 是 Callback，可以 clone；这样多个内层
                                                                // `on:click` 各自拿一份，外层闭包仍是 FnMut。
                                                                let change_status = change_status.clone();
                                                                if deactivated {
                                                                    // 注销是终态，没有可做的操作。
                                                                    view! { <span class="mut">"—"</span> }.into_any()
                                                                } else if confirm_id.get().as_deref() == Some(id.as_str()) {
                                                                    let id_ok = id.clone();
                                                                    view! {
                                                                        <span class="rowact">
                                                                            <span class="mut">{move || i18n.t("admin.confirm_deactivate")}</span>
                                                                            <button class="btn sm"
                                                                                disabled=move || row_busy.get().is_some()
                                                                                on:click=move |_| change_status.run((id_ok.clone(), "deactivated".to_string()))>
                                                                                {move || i18n.t("common.confirm")}
                                                                            </button>
                                                                            <button class="btn sm" on:click=move |_| confirm_id.set(None)>
                                                                                {move || i18n.t("common.cancel")}
                                                                            </button>
                                                                        </span>
                                                                    }.into_any()
                                                                } else {
                                                                    let (id_freeze, id_deactivate) = (id.clone(), id.clone());
                                                                    let edit_row = edit_row.clone();
                                                                    view! {
                                                                        <span class="rowact">
                                                                            // 编辑不随 `locked` 禁用：改自己的显示名无妨，真正不能做的是
                                                                            // 「撤销自己的管理员权限」，那件事在弹窗里单独禁掉。
                                                                            <button class="btn sm"
                                                                                disabled=move || row_busy.get().is_some()
                                                                                on:click=move |_| {
                                                                                    edit_name.set(edit_row.name.clone());
                                                                                    edit_admin.set(edit_row.is_admin);
                                                                                    edit_error.set(None);
                                                                                    edit_target.set(Some(edit_row.clone()));
                                                                                }>
                                                                                {move || i18n.t("common.edit")}
                                                                            </button>
                                                                            <button class="btn sm"
                                                                                disabled=move || locked || row_busy.get().is_some()
                                                                                title=move || if is_self { i18n.t("admin.lock_self") } else { i18n.t("admin.lock_builtin") }
                                                                                on:click=move |_| change_status.run((id_freeze.clone(), next.to_string()))>
                                                                                {move || if frozen { i18n.t("admin.unfreeze") } else { i18n.t("admin.freeze") }}
                                                                            </button>
                                                                            <button class="btn sm"
                                                                                disabled=move || locked || row_busy.get().is_some()
                                                                                title=move || if is_self { i18n.t("admin.lock_self") } else { i18n.t("admin.lock_builtin") }
                                                                                on:click=move |_| confirm_id.set(Some(id_deactivate.clone()))>
                                                                                {move || i18n.t("admin.deactivate")}
                                                                            </button>
                                                                        </span>
                                                                    }.into_any()
                                                                }
                                                            }}
                                                        </td>
                                                    </tr>
                                                }
                                            }
                                        />
                                    </tbody>
                                </table>
                            }.into_any(),
                        }
                        }}
                        <div class="mut" style="margin-top:8px">
                            {move || i18n.t("admin.freeze_deactivate_desc")}
                        </div>
                    </div>

                    {move || edit_target.get().map(|t| {
                        // 「角色」那栏能不能改：自己与内置管理员都不行，理由与服务端那两道守卫
                        // 一一对应（见 `update_account`）——错在这一栏点了必然失败，不如摆明原因。
                        let role_locked = t.id == me_id_for_edit || t.is_builtin;
                        let role_tip = if t.id == me_id_for_edit {
                            i18n.t("admin.cant_remove_self_admin")
                        } else {
                            i18n.t("admin.cant_remove_builtin_admin")
                        };
                        view! {
                            <div class="dmodal" on:click=move |_| edit_target.set(None)>
                                <div class="panel dmbox" on:click=|ev| ev.stop_propagation()>
                                    <h3>{move || i18n.t("admin.edit_account")}</h3>
                                    <div class="dfield">
                                        <span class="dlabel">{move || i18n.t("admin.col_email")}</span>
                                        <div class="mut">{t.email.clone()}</div>
                                    </div>
                                    <label class="dfield">
                                        <span class="dlabel">{move || i18n.t("admin.col_name")}</span>
                                        <input class="inp" prop:value=edit_name
                                            on:input=move |ev| edit_name.set(event_target_value(&ev)) />
                                    </label>
                                    <div class="dfield">
                                        <span class="dlabel">{move || i18n.t("admin.col_role")}</span>
                                        <label class="mut" style="display:flex;align-items:center;gap:6px">
                                            <input type="checkbox" prop:checked=edit_admin
                                                disabled=role_locked title=role_tip
                                                on:change=move |ev| edit_admin.set(event_target_checked(&ev)) />
                                            {move || i18n.t("admin.system_admin")}
                                        </label>
                                    </div>
                                    {move || edit_error.get().map(|e| view! {
                                        <p class="error" style="margin:0">{"⚠ "}{e}</p>
                                    })}
                                    <div style="display:flex;gap:8px;justify-content:flex-end">
                                        <button class="btn" on:click=move |_| edit_target.set(None)>{move || i18n.t("common.cancel")}</button>
                                        <button class="btn pri" disabled=move || edit_busy.get()
                                            on:click=save_edit>{move || i18n.t("common.save")}</button>
                                    </div>
                                </div>
                            </div>
                        }
                    })}
                }.into_any()
                }
            }}

            <div class="panel set-body">
                <h2>{move || i18n.t("admin.system_config")}</h2>
                <div class="cfg">
                    <div><b>{move || i18n.t("admin.open_registration")}</b><div class="mut">{move || i18n.t("admin.open_registration_help")}</div></div>
                    <button class="switch" disabled></button>
                </div>
                <div class="cfg">
                    <div><b>{move || i18n.t("admin.attachment_limit")}</b><div class="mut">{move || i18n.t("admin.attachment_limit_help")}</div></div>
                    <input class="inp" value="50 MB" disabled />
                </div>
                <div class="cfg">
                    <div><b>{move || i18n.t("admin.session_expiry")}</b><div class="mut">{move || i18n.t("admin.session_expiry_help")}</div></div>
                    <input class="inp" value="7 天" disabled />
                </div>
                <div class="cfg">
                    <div><b>{move || i18n.t("admin.trash_retention")}</b><div class="mut">{move || i18n.t("admin.trash_retention_help")}</div></div>
                    <input class="inp" value="30 天" disabled />
                </div>
                <div class="mut">{move || i18n.t("admin.audit_log_note")}</div>
            </div>
        </div>
    }
}
