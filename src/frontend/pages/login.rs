use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::graphql_client::{login, register, set_token};
use crate::frontend::use_auth;

#[component]
pub fn Login() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let is_register = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);

    let submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let navigate = navigate.clone();
        let email = email.get();
        let password = password.get();
        let name = name.get();
        let registering = is_register.get();
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let result = if registering {
                register(&email, &name, &password).await
            } else {
                login(&email, &password).await
            };
            match result {
                Ok((token, user)) => {
                    set_token(&token);
                    auth.user.set(Some(user));
                    navigate("/workspaces", Default::default());
                }
                Err(e) => {
                    error.set(Some(e));
                    busy.set(false);
                }
            }
        });
    };

    view! {
        <div class="login-wrap">
            <form class="panel login-card" on:submit=submit>
                <div class="logo">
                    <b>"Rodeo"</b>
                    <span class="chip dim">"任务 / 问题跟踪"</span>
                </div>
                {move || if is_register.get() {
                    view! {
                        <input class="inp" placeholder="姓名" prop:value=name on:input=move |ev| name.set(event_target_value(&ev)) />
                    }.into_any()
                } else {
                    view! { <div></div> }.into_any()
                }}
                <input class="inp" type="email" placeholder="邮箱" prop:value=email on:input=move |ev| email.set(event_target_value(&ev)) />
                <input class="inp" type="password" placeholder="密码（至少 8 位，含大小写和数字）" prop:value=password on:input=move |ev| password.set(event_target_value(&ev)) />
                {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
                <button class="btn pri" type="submit" style="justify-content:center;padding:9px" disabled=move || busy.get()>
                    {move || if is_register.get() { "注册并登录" } else { "登录" }}
                </button>
                <div style="display:flex;justify-content:space-between" class="mut">
                    <span class="link" on:click=move |_| is_register.update(|v| *v = !*v)>
                        {move || if is_register.get() { "已有账号，去登录" } else { "开放注册已开启，立即注册" }}
                    </span>
                    <u class="mut">"忘记密码"</u>
                </div>
                <div class="divider">"或使用以下方式继续"</div>
                <button class="btn" type="button" style="justify-content:center" disabled>
                    <b>"GitHub"</b>&nbsp;"OAuth 登录（即将上线）"
                </button>
                <button class="btn" type="button" style="justify-content:center" disabled>
                    <b>"企业 OIDC"</b>&nbsp;"单点登录（即将上线）"
                </button>
                <div class="mut" style="text-align:center">"私有部署 · 数据不出内网 · 会话经 HttpOnly Cookie"</div>
            </form>
        </div>
    }
}
