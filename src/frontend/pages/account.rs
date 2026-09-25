use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};

use crate::frontend::components::logged_out;
use crate::frontend::graphql_client::change_password;
use crate::frontend::icons::ic_back;
use crate::frontend::use_auth;

/// 账号页：看自己的账号信息、改自己的密码。
///
/// 改密成功后服务端会吊销所有令牌并重签一张，客户端 helper 已把本地令牌换成新的，
/// 所以这里的文案才敢说「当前设备无需重新登录」——别的设备则必须重登。
#[component]
pub fn Account() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    // 返回按钮的文案与目的地看来源：来自工作空间（`?from=ws&slug=…`）就回那个工作
    // 空间并写「工作空间」；其它（包括工作空间列表 `→` 默认）走 `/workspaces` 并写
    // 「工作空间列表」。query 在 SSR 阶段可读，无需 wasm 分支。
    let query = use_query_map();
    let back_label = move || match query.get().get("from").as_deref() {
        Some("ws") => "工作空间",
        _ => "工作空间列表",
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

    let old = RwSignal::new(String::new());
    let new = RwSignal::new(String::new());
    let confirm = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let done = RwSignal::new(false);

    // 未登录、或本地令牌已被服务端判死，都回登录页。`logged_out()` 在 SSR 阶段恒为 true，
    // 所以只在 wasm 上跳，否则服务端渲染会直接跳走（同 admin.rs / entry.rs）。
    Effect::new(move |_| {
        if cfg!(target_arch = "wasm32") && (logged_out() || auth.session_lost.get()) {
            navigate("/login", Default::default());
        }
    });

    let can_submit = move || {
        !busy.get() && !old.get().is_empty() && !new.get().is_empty() && !confirm.get().is_empty()
    };

    let submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        // 防连点：重复提交没有意义，而且每次都会推进令牌版本。
        if busy.get_untracked() {
            return;
        }
        let o = old.get();
        let n = new.get();
        // 两次输入比对放在本地：它拦下的是手误，不该让服务端来判断。
        if n != confirm.get() {
            error.set(Some("两次输入的新密码不一致".to_string()));
            return;
        }
        busy.set(true);
        error.set(None);
        done.set(false);
        spawn_local(async move {
            match change_password(&o, &n).await {
                Ok(u) => {
                    auth.user.set(Some(u));
                    old.set(String::new());
                    new.set(String::new());
                    confirm.set(String::new());
                    done.set(true);
                }
                Err(e) => error.set(Some(e)),
            }
            busy.set(false);
        });
    };

    view! {
        <div class="page">
            <div class="crumb" style="display:flex;align-items:center;gap:8px">
                <button class="btn sm" on:click=back>{ic_back()}{back_label}</button>
                <span>"/account · 账号"</span>
            </div>

            <div class="panel set-body">
                <h2>"账号信息"</h2>
                {move || match auth.user.get() {
                    // 令牌失效：这里必须说清楚，否则面板就一直停在「加载中…」了。
                    None if auth.session_lost.get() => view! {
                        <div class="mut">"登录已失效，请重新登录。"</div>
                    }.into_any(),
                    None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                    Some(u) => view! {
                        <div class="dmeta">
                            <div><span class="mut">"邮箱"</span>{u.email}</div>
                            <div><span class="mut">"姓名"</span>{u.name}</div>
                        </div>
                    }.into_any(),
                }}
            </div>

            <div class="panel set-body">
                <h2>"修改密码"</h2>
                <form class="stack" on:submit=submit>
                    <label class="fld">
                        <span>"当前密码"</span>
                        <input class="inp" type="password" autocomplete="current-password" prop:value=old
                            on:input=move |ev| old.set(event_target_value(&ev)) />
                    </label>
                    <label class="fld">
                        <span>"新密码"</span>
                        <input class="inp" type="password" autocomplete="new-password" prop:value=new
                            on:input=move |ev| new.set(event_target_value(&ev)) />
                    </label>
                    <label class="fld">
                        <span>"确认新密码"</span>
                        <input class="inp" type="password" autocomplete="new-password" prop:value=confirm
                            on:input=move |ev| confirm.set(event_target_value(&ev)) />
                    </label>
                    <div class="mut">"最少 8 位，需包含大小写字母和数字。"</div>
                    <button class="btn pri" type="submit" style="align-self:flex-start"
                        disabled=move || !can_submit()>"修改密码"</button>
                </form>
                {move || error.get().map(|e| view! {
                    <div class="hint" style="margin-top:8px">{"⚠ "}{e}</div>
                })}
                {move || done.get().then(|| view! {
                    <div class="hint" style="margin-top:8px">
                        "密码已修改。当前设备无需重新登录，其他设备需要重新登录。"
                    </div>
                })}
            </div>
        </div>
    }
}
