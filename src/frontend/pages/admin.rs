use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::components::{copy_to_clipboard, logged_out, short_time};
use crate::frontend::graphql_client::{
    accounts, create_account, set_account_status, AdminAccount, CreatedAccount,
};
use crate::frontend::icons::{ic_add, ic_copy};
use crate::frontend::use_auth;

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
    let change_status = move |(id, status): (String, String)| {
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
    };

    // 未登录、或本地令牌已被服务端判死，都回登录页。与 entry.rs 用同一条路径：
    // `logged_out()` 在 SSR 阶段恒为 true，所以只在 wasm 上跳转，否则服务端渲染会直接跳走。
    let navigate = use_navigate();
    Effect::new(move |_| {
        if cfg!(target_arch = "wasm32") && (logged_out() || auth.session_lost.get()) {
            navigate("/login", Default::default());
        }
    });

    let can_submit = move || {
        !busy.get() && !email.get().trim().is_empty() && !name.get().trim().is_empty()
    };

    let submit = move |_| {
        let e = email.get().trim().to_string();
        let n = name.get().trim().to_string();
        if e.is_empty() || n.is_empty() {
            error.set(Some("邮箱与姓名都要填".to_string()));
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
            <div class="crumb">"/admin · 仅系统管理员"</div>
            <div class="stats">
                <div class="panel stat">
                    <div class="v">
                        {move || account_list.get().map(|l| l.len().to_string()).unwrap_or_else(|| "—".to_string())}
                    </div>
                    <div class="l">"账号总数"</div>
                </div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">"工作空间"</div></div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">"附件存储占用"</div></div>
            </div>

            {move || match auth.user.get() {
                // 令牌失效：说清楚为什么，否则这个面板会一直停在「加载中…」。
                None if auth.session_lost.get() => view! {
                    <div class="panel set-body"><div class="mut">"登录已失效，请重新登录。"</div></div>
                }.into_any(),
                // `user` 为 None 表示 me() 还没回来：此时既不能说「有权限」也不能说
                // 「无权限」，否则管理员会先看到一瞬「仅系统管理员可访问」。
                None => view! {
                    <div class="panel set-body"><div class="mut">"加载中…"</div></div>
                }.into_any(),
                Some(u) if !u.is_admin => view! {
                    <div class="panel set-body">
                        <h2>"仅系统管理员可访问"</h2>
                        <div class="mut">
                            "当前账号（"{u.email}"）不是系统管理员。"
                            "管理员账号由部署配置的 auth.builtin.admin_email 指定。"
                        </div>
                    </div>
                }.into_any(),
                Some(me) => {
                // 当前登录者：列表里自己那一行的操作要禁用。
                let me_id = me.id.clone();
                view! {
                    <div class="panel set-body">
                        <h2>"账号管理"</h2>
                        <div class="invite">
                            <input class="inp" placeholder="邮箱" prop:value=email
                                on:input=move |ev| email.set(event_target_value(&ev)) />
                            <input class="inp" placeholder="姓名" prop:value=name
                                on:input=move |ev| name.set(event_target_value(&ev)) />
                            <label class="mut" style="display:flex;align-items:center;gap:6px;white-space:nowrap">
                                <input type="checkbox" prop:checked=make_admin
                                    on:change=move |ev| make_admin.set(event_target_checked(&ev)) />
                                "设为管理员"
                            </label>
                            <button class="btn pri" disabled=move || !can_submit() on:click=submit>
                                {ic_add()}"创建账号"
                            </button>
                        </div>
                        <div class="mut" style="margin-top:8px">
                            "初始密码由服务端随机生成，创建后只显示这一次——请立刻复制并转交本人。"
                        </div>
                        {move || error.get().map(|e| view! {
                            <div class="hint" style="margin-top:8px">{"⚠ "}{e}</div>
                        })}
                    </div>

                    {move || created.get().map(|c| {
                        let email_txt = c.account.email.clone();
                        let name_txt = c.account.name.clone();
                        let pw = c.initial_password.clone();
                        // 分发用的整段文案：它是要被原样转交给本人的，所以带上登录地址、
                        // 账号、初始密码和「登录后改密」的指引，而不是只丢一行数据。
                        let share = format!(
                            "你的 Rodeo 账号已创建\n\
                             登录地址：{}/login\n\
                             邮箱：{email_txt}\n\
                             初始密码：{pw}\n\
                             首次登录后请尽快修改初始密码：登录后点右上角头像 →「个人信息」，在「修改密码」里填入当前密码与新密码。\n\
                             这串密码只显示这一次，请妥善保管。",
                            origin(),
                        );
                        view! {
                            <div class="panel set-body">
                                <h2>"账号已创建，请立即分发"</h2>
                                <div class="dmeta">
                                    <div><span class="mut">"邮箱"</span>{email_txt}</div>
                                    <div><span class="mut">"姓名"</span>{name_txt}</div>
                                    <div><span class="mut">"初始密码"</span><code>{pw}</code></div>
                                </div>
                                <div style="display:flex;gap:8px;align-items:center;margin-top:10px">
                                    <button class="btn pri" on:click=move |_| {
                                        copy_to_clipboard(&share);
                                        copied.set(true);
                                    }>{ic_copy()}"复制分享文案"</button>
                                    {move || copied.get().then(|| view! {
                                        <span class="chip c-done">"已复制"</span>
                                    })}
                                </div>
                                <div class="hint" style="margin-top:8px">
                                    {"⚠ "}
                                    "这串密码不会再显示：服务端只保存 Argon2 哈希。现在就把它交给本人。"
                                </div>
                            </div>
                        }
                    })}

                    <div class="panel set-body">
                        <h2>"账号"</h2>
                        {move || list_error.get().map(|e| view! {
                            <div class="hint" style="margin-bottom:8px">{"⚠ "}{e}</div>
                        })}
                        {move || {
                        // 这层闭包会被反复调用，而里面那个 `For` 的 children 闭包要**拥有**一份
                        // `me_id`（String 不是 Copy）。所以在这里先克隆一份再交出去：外面的
                        // `me_id` 只被借用，这层闭包才是 `Fn` 而不是 `FnOnce`。
                        let me_id = me_id.clone();
                        match account_list.get() {
                            None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                            Some(list) if list.is_empty() => {
                                view! { <div class="mut">"还没有账号。"</div> }.into_any()
                            }
                            Some(_) => view! {
                                <table class="tbl">
                                    <thead>
                                        <tr>
                                            <th>"邮箱"</th>
                                            <th>"姓名"</th>
                                            <th>"角色"</th>
                                            <th>"状态"</th>
                                            <th>"创建时间"</th>
                                            <th style="width:180px"></th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        <For
                                            each=move || account_list.get().unwrap_or_default()
                                            key=|a| a.id.clone()
                                            children=move |a: AdminAccount| {
                                                // 两道禁用的理由：不能操作自己（一点就把自己锁在外面），
                                                // 也不能动配置里指定的内置管理员（见服务端那两道守卫）。
                                                let locked = a.id == me_id || a.is_builtin;
                                                let lock_tip = if a.id == me_id {
                                                    "不能对自己的账号执行该操作"
                                                } else {
                                                    "内置管理员账号不能冻结或注销"
                                                };
                                                let status_cls = match a.status.as_str() {
                                                    "frozen" => "chip c-doing",
                                                    "deactivated" => "chip c-wont",
                                                    _ => "chip c-done",
                                                };
                                                let status_txt = match a.status.as_str() {
                                                    "frozen" => "已冻结",
                                                    "deactivated" => "已注销",
                                                    _ => "正常",
                                                };
                                                let frozen = a.status == "frozen";
                                                let deactivated = a.status == "deactivated";
                                                let next = if frozen { "active" } else { "frozen" };
                                                let next_txt = if frozen { "解冻" } else { "冻结" };
                                                let id = a.id.clone();
                                                view! {
                                                    <tr>
                                                        <td>{a.email.clone()}</td>
                                                        <td>{a.name.clone()}</td>
                                                        <td>{if a.is_admin { "管理员" } else { "成员" }}</td>
                                                        <td><span class=status_cls>{status_txt}</span></td>
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
                                                                if deactivated {
                                                                    // 注销是终态，没有可做的操作。
                                                                    view! { <span class="mut">"—"</span> }.into_any()
                                                                } else if confirm_id.get().as_deref() == Some(id.as_str()) {
                                                                    let id_ok = id.clone();
                                                                    view! {
                                                                        <span class="rowact">
                                                                            <span class="mut">"确认注销？"</span>
                                                                            <button class="btn sm"
                                                                                disabled=move || row_busy.get().is_some()
                                                                                on:click=move |_| change_status((id_ok.clone(), "deactivated".to_string()))>
                                                                                "确认"
                                                                            </button>
                                                                            <button class="btn sm" on:click=move |_| confirm_id.set(None)>
                                                                                "取消"
                                                                            </button>
                                                                        </span>
                                                                    }.into_any()
                                                                } else {
                                                                    let (id_freeze, id_deactivate) = (id.clone(), id.clone());
                                                                    view! {
                                                                        <span class="rowact">
                                                                            <button class="btn sm"
                                                                                disabled=move || locked || row_busy.get().is_some()
                                                                                title=lock_tip
                                                                                on:click=move |_| change_status((id_freeze.clone(), next.to_string()))>
                                                                                {next_txt}
                                                                            </button>
                                                                            <button class="btn sm"
                                                                                disabled=move || locked || row_busy.get().is_some()
                                                                                title=lock_tip
                                                                                on:click=move |_| confirm_id.set(Some(id_deactivate.clone()))>
                                                                                "注销"
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
                            "冻结会立刻踢该账号下线，且需重新登录才能恢复；注销不可逆，但会释放邮箱地址供重新注册。"
                        </div>
                    </div>
                }.into_any()
                }
            }}

            <div class="panel set-body">
                <h2>"系统配置"</h2>
                <div class="cfg">
                    <div><b>"开放注册"</b><div class="mut">"关闭后仅系统管理员可创建账号"</div></div>
                    <button class="switch" disabled></button>
                </div>
                <div class="cfg">
                    <div><b>"附件大小限制"</b><div class="mut">"Entry 附件单文件上限，默认 50MB"</div></div>
                    <input class="inp" value="50 MB" disabled />
                </div>
                <div class="cfg">
                    <div><b>"会话过期时间"</b><div class="mut">"JWT 过期后需重新认证"</div></div>
                    <input class="inp" value="7 天" disabled />
                </div>
                <div class="cfg">
                    <div><b>"Workspace 回收站保留期"</b><div class="mut">"软删除后可恢复窗口"</div></div>
                    <input class="inp" value="30 天" disabled />
                </div>
                <div class="mut">"审计日志保留 180 天 · 全局操作流水见「审计日志」标签页"</div>
            </div>
        </div>
    }
}
