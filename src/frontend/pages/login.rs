use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::graphql_client::{login, set_token};
use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::ic_logo;
use crate::frontend::pages::wechat_bind::WeChatBindPanel;
use crate::frontend::use_auth;

fn url_param(search: &str, key: &str) -> Option<String> {
    search.trim_start_matches('?').split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| urlencoding_decode(v))
    })
}

fn urlencoding_decode(s: &str) -> String {
    urlencoding::decode(s)
        .map(|c| c.into_owned())
        .unwrap_or_else(|_| s.to_string())
}

fn return_to_default() -> String {
    // 简单起见总是 /workspaces；如果以后想从 session 取上次访问点可改这里。
    "/workspaces".to_string()
}

/// `web_sys::window().location().search()` 的 wasm/ssr 兼容壳。SSR 没有
/// DOM，`web_sys` 也不在 ssr 依赖里——直接调用链接都过不去。SSR 侧返回
/// 空串，下面的 Effect 因 `cfg!` 早 return 本来也不会用。
#[cfg(target_arch = "wasm32")]
fn window_search() -> String {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn window_search() -> String {
    String::new()
}

/// `window.location().set_href(...)` 的 wasm/ssr 兼容壳。SSR 端是 no-op。
#[cfg(target_arch = "wasm32")]
fn window_set_href(url: &str) {
    if let Some(w) = web_sys::window() {
        w.location().set_href(url).ok();
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn window_set_href(_url: &str) {}

#[component]
pub fn Login() -> impl IntoView {
    let auth = use_auth();
    let _navigate = use_navigate();
    let i18n: I18nController = use_i18n();
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // 客户端是否已接管这个表单。SSR 直出的只是个普通 <form>，`on:submit` 要等 wasm 下载并
    // hydrate 之后才存在；在那之前点「登录」走的是浏览器原生表单提交——整页刷新回登录页、
    // 输入被丢弃，而且会重新开始下载 wasm，慢网下一次点击就能让用户一直卡在登录页。
    // 所以客户端接管前禁用按钮：按钮禁用时浏览器也不会做隐式提交，回车同样安全。
    let ready = RwSignal::new(false);
    Effect::new(move |_| {
        if cfg!(target_arch = "wasm32") {
            ready.set(true);
        }
    });

    let oauth_providers = RwSignal::new(Vec::<String>::new());
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        spawn_local(async move {
            if let Ok(p) = crate::frontend::graphql_client::oauth_providers().await {
                oauth_providers.set(p);
            }
        });
    });

    // SSR 直出时 window.location 不存在；只 wasm 跑
    let bind_info = RwSignal::new(None::<(String, String, String)>);
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        let search = window_search();
        if let Some(bind_token) = url_param(&search, "bind") {
            let provider = url_param(&search, "provider").unwrap_or_default();
            let return_to =
                url_param(&search, "return_to").unwrap_or_else(|| "/workspaces".to_string());
            bind_info.set(Some((bind_token, provider, return_to)));
        }
    });

    // OAuthCallback 失败跳转过来时带的提示（如 `#token` 缺失），显示在表单上方。
    let oauth_error = RwSignal::new(None::<String>);
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        let search = window_search();
        if let Some(code) = url_param(&search, "oauth_error") {
            oauth_error.set(Some(match code.as_str() {
                "missing_token" => i18n.t("login.oauth_missing_token"),
                _ => i18n.tf("login.oauth_failed", &[("code", &code)]),
            }));
        }
    });

    let submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let email = email.get();
        let password = password.get();
        busy.set(true);
        error.set(None);
        spawn_local(async move {
            let result = login(&email, &password).await;
            match result {
                Ok((token, user)) => {
                    set_token(&token);
                    auth.user.set(Some(user));
                    // 登录成功，失效提示就此翻篇；否则这个位会一直留着，用户下次
                    // 主动来登录页还会看到一句莫名其妙的「登录已失效」。
                    auth.session_lost.set(false);
                    // 用整页跳转而不是 `navigate("/workspaces", …)`：邮箱密码登录
                    // 没有 OAuthCallback 那种独立路由做中转，hydrate 之后直接从 async
                    // 闭包里调 navigate 在 leptos 0.8.x 有边角问题（URL pushState 了但
                    // Router 没换页——曾让用户卡在 Login 看到 /workspaces URL）。
                    // OAuth 按钮走 `/api/auth/{}/start` 整页跳走，没有这个问题。
                    // `replace` 让浏览器历史里没有「/login」这一站，回退按钮不会把用户
                    // 拽回已登录态的登录页。
                    window_set_href("/workspaces");
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
            {move || bind_info.get().map(|(t, p, r)| view! {
                <WeChatBindPanel bind_token=t provider=p return_to=r />
            }.into_any()).unwrap_or_else(|| {
                // `submit` 是 `move`-capture 的非 `Copy` 闭包。`unwrap_or_else`
                // 自身是 FnOnce，里面 `view!` 又要把 `submit` move 到 `<form on:submit>`
                // 里——外层 `move ||` 一并变 FnOnce，leptos 重渲就坏了。提前克隆
                // 一份给 form，让 view! 拿的是独立副本。
                let submit_for_form = submit.clone();
                view! {
                    <form class="panel login-card" on:submit=submit_for_form>
                        <div class="logo">
                            {ic_logo()}
                            <b>{move || i18n.t("appbar.brand")}</b>
                            <span class="chip dim">{move || i18n.t("login.tagline")}</span>
                        </div>
                        <input class="inp" type="text" placeholder={move || i18n.t("login.identifier")} prop:value=email on:input=move |ev| email.set(event_target_value(&ev)) />
                        <input class="inp" type="password" placeholder={move || i18n.t("login.password")} prop:value=password on:input=move |ev| password.set(event_target_value(&ev)) />
                        {move || auth.session_lost.get().then(|| view! {
                            <p class="error">{move || i18n.t("login.session_lost")}</p>
                        })}
                        {move || oauth_error.get().map(|m| view! {
                            <p class="error">{m}</p>
                        })}
                        {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
                        {move || (!ready.get()).then(|| view! {
                            <p class="mut" style="text-align:center">{move || i18n.t("login.loading")}</p>
                        })}
                        <button class="btn pri" type="submit" style="justify-content:center;padding:9px" disabled=move || busy.get() || !ready.get()>
                            {move || i18n.t("login.submit_login")}
                        </button>
                        <div class="mut" style="text-align:center">
                            {move || i18n.t("login.no_account_hint")}
                        </div>
                        {move || {
                            let ps = oauth_providers.get();
                            if ps.is_empty() {
                                None
                            } else {
                                // 先把每个 provider 的 label + start_url 算成 owned 字符串，
                                // 收集成 `Vec<(String, String)>` 再交给 view!——直接 `ps.iter()`
                                // 会让 view! 借 `ps`，而 `ps` 是这个闭包的栈帧局部，view! 的
                                // 生命周期跟闭包返回点对不上，会爆 E0515。
                                let buttons: Vec<(String, String)> = ps.iter().map(|name| {
                                    let label = match name.as_str() {
                                        "wechat" => i18n.t("login.oauth_wechat"),
                                        "google" => i18n.t("login.oauth_google"),
                                        "github" => i18n.t("login.oauth_github"),
                                        _ => name.clone(),
                                    };
                                    let start_url = format!(
                                        "/api/auth/{}/start?return_to={}",
                                        name,
                                        urlencoding::encode(&return_to_default()),
                                    );
                                    (label.to_string(), start_url)
                                }).collect();
                                Some(view! {
                                    <div class="divider">{move || i18n.t("login.or_continue")}</div>
                                    {buttons.into_iter().map(|(label, start_url)| view! {
                                        <button class="btn" type="button"
                                            style="justify-content:center"
                                            on:click=move |_| window_set_href(&start_url)>
                                            <b>{label}</b>
                                        </button>
                                    }).collect::<Vec<_>>()}
                                })
                            }
                        }}
                        <div class="mut" style="text-align:center">{move || i18n.t("login.footer")}</div>
                    </form>
                }.into_any()
            })}
        </div>
    }
}
