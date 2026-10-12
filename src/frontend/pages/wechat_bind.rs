use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::graphql_client::{bind_oauth_to_existing, set_token};
use crate::frontend::use_auth;

#[component]
pub fn WeChatBindPanel(bind_token: String, provider: String, return_to: String) -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let navigate_existing = navigate.clone();
    let bind_token_existing = bind_token.clone();
    let return_to_existing = return_to.clone();

    // 现有账号
    let existing_email = RwSignal::new(String::new());
    let existing_password = RwSignal::new(String::new());
    let existing_error = RwSignal::new(None::<String>);
    let existing_busy = RwSignal::new(false);

    let submit_existing = move |ev: SubmitEvent| {
        ev.prevent_default();
        let navigate = navigate_existing.clone();
        let bind_token = bind_token_existing.clone();
        let return_to = return_to_existing.clone();
        let email = existing_email.get();
        let password = existing_password.get();
        existing_busy.set(true);
        existing_error.set(None);
        spawn_local(async move {
            match bind_oauth_to_existing(&bind_token, &email, &password).await {
                Ok((token, user)) => {
                    set_token(&token);
                    auth.user.set(Some(user));
                    auth.session_lost.set(false);
                    navigate(&return_to, Default::default());
                }
                Err(e) => {
                    existing_error.set(Some(e));
                    existing_busy.set(false);
                }
            }
        });
    };

    view! {
        <div class="wechat-bind">
            <p class="mut" style="text-align:center">
                {format!("首次使用 {provider} 登录，请关联已有的 Rodeo 账号。如尚未创建账号，请联系系统管理员创建账号后再回到此扫码绑定。")}
            </p>

            // —— 关联现有 ——
            <form class="panel" on:submit=submit_existing>
                <h4>"关联现有 Rodeo 账号"</h4>
                <input class="inp" type="text" placeholder="账号或邮箱" prop:value=existing_email
                    on:input=move |ev| existing_email.set(event_target_value(&ev)) />
                <input class="inp" type="password" placeholder="密码" prop:value=existing_password
                    on:input=move |ev| existing_password.set(event_target_value(&ev)) />
                {move || existing_error.get().map(|e| view! { <p class="error">{e}</p> })}
                <button class="btn pri" type="submit" disabled=move || existing_busy.get()>
                    "关联并登录"
                </button>
            </form>
        </div>
    }
}
