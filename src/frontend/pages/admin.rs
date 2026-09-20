use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::components::{copy_to_clipboard, logged_out};
use crate::frontend::graphql_client::{create_account, CreatedAccount};
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

    // 未登录时回到登录页。与 entry.rs 用同一条路径：`logged_out()` 在 SSR 阶段恒为
    // true，所以只在 wasm 上跳转，否则服务端渲染会直接跳走。
    let navigate = use_navigate();
    Effect::new(move |_| {
        if cfg!(target_arch = "wasm32") && logged_out() {
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
                <div class="panel stat"><div class="v">"—"</div><div class="l">"账号总数"</div></div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">"工作空间"</div></div>
                <div class="panel stat"><div class="v">"—"</div><div class="l">"附件存储占用"</div></div>
            </div>

            {move || match auth.user.get() {
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
                Some(_) => view! {
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
                             首次登录后请尽快修改初始密码。这串密码只显示这一次，请妥善保管。",
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
                }.into_any(),
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
