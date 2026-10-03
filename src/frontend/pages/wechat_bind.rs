use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::graphql_client::{bind_oauth_to_existing, bind_oauth_to_new, set_token};
use crate::frontend::use_auth;

#[component]
pub fn WeChatBindPanel(bind_token: String, provider: String, return_to: String) -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    // 两个 submit 闭包都要 move-capture `navigate` / `bind_token` / `return_to`，
    // 但每个值只能被 move 一次——把克隆提到外层作用域，两个闭包各自 move 一个副本。
    let navigate_existing = navigate.clone();
    let bind_token_existing = bind_token.clone();
    let return_to_existing = return_to.clone();
    let navigate_new = navigate;
    let bind_token_new = bind_token;
    let return_to_new = return_to;

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

    // 新账号
    let new_email = RwSignal::new(String::new());
    let new_name = RwSignal::new(String::new());
    let new_password = RwSignal::new(String::new());
    let new_password_set = RwSignal::new(false); // 用户是否想设密码
    let new_error = RwSignal::new(None::<String>);
    let new_busy = RwSignal::new(false);

    let submit_new = move |ev: SubmitEvent| {
        ev.prevent_default();
        let navigate = navigate_new.clone();
        let bind_token = bind_token_new.clone();
        let return_to = return_to_new.clone();
        let email = new_email.get();
        let name = new_name.get();
        // 把 password 收成 owned `Option<String>` 而不是 `Option<&str>`：
        // `spawn_local` 的 future 要求 'static，`&str` 借的是这个 submit 闭包的栈帧局部，
        // 跨 await 活不到 helper 调用点。先把 String move 进 future，再在 future 内部
        // `.as_deref()` 转回 `&str` 给 `bind_oauth_to_new` 那个签名。
        let password_owned = new_password.get();
        let password_owned: Option<String> =
            if new_password_set.get() && !password_owned.is_empty() {
                Some(password_owned)
            } else {
                None
            };
        new_busy.set(true);
        new_error.set(None);
        spawn_local(async move {
            let password_opt = password_owned.as_deref();
            match bind_oauth_to_new(&bind_token, &email, &name, password_opt).await {
                Ok((token, user)) => {
                    set_token(&token);
                    auth.user.set(Some(user));
                    auth.session_lost.set(false);
                    navigate(&return_to, Default::default());
                }
                Err(e) => {
                    new_error.set(Some(e));
                    new_busy.set(false);
                }
            }
        });
    };

    view! {
        <div class="wechat-bind">
            <p class="mut" style="text-align:center">
                {format!("首次使用 {provider} 登录，请关联一个 Rodeo 账号或创建一个新账号。")}
            </p>

            // —— 关联现有 ——
            <form class="panel" on:submit=submit_existing>
                <h4>"关联现有 Rodeo 账号"</h4>
                <input class="inp" type="email" placeholder="邮箱" prop:value=existing_email
                    on:input=move |ev| existing_email.set(event_target_value(&ev)) />
                <input class="inp" type="password" placeholder="密码" prop:value=existing_password
                    on:input=move |ev| existing_password.set(event_target_value(&ev)) />
                {move || existing_error.get().map(|e| view! { <p class="error">{e}</p> })}
                <button class="btn pri" type="submit" disabled=move || existing_busy.get()>
                    "关联并登录"
                </button>
            </form>

            <div class="divider">"或"</div>

            // —— 创建新 ——
            <form class="panel" on:submit=submit_new>
                <h4>"创建新 Rodeo 账号"</h4>
                <input class="inp" type="email" placeholder="邮箱" prop:value=new_email
                    on:input=move |ev| new_email.set(event_target_value(&ev)) />
                <input class="inp" placeholder="姓名" prop:value=new_name
                    on:input=move |ev| new_name.set(event_target_value(&ev)) />
                <label style="display:flex;gap:8px;align-items:center">
                    <input type="checkbox" prop:checked=new_password_set
                        on:change=move |ev| new_password_set.set(event_target_checked(&ev)) />
                    <span class="mut">"同时设置密码（不勾选则只能用第三方登录）"</span>
                </label>
                {move || new_password_set.get().then(|| view! {
                    <input class="inp" type="password" placeholder="密码（至少 8 位，含大小写和数字）"
                        prop:value=new_password
                        on:input=move |ev| new_password.set(event_target_value(&ev)) />
                })}
                {move || new_error.get().map(|e| view! { <p class="error">{e}</p> })}
                <button class="btn pri" type="submit" disabled=move || new_busy.get()>
                    "创建并登录"
                </button>
            </form>
        </div>
    }
}
