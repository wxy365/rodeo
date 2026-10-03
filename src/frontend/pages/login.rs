use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::graphql_client::{
    allow_registration as fetch_allow_registration, login, register, set_token,
};
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
    let navigate = use_navigate();
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let is_register = RwSignal::new(false);
    let error = RwSignal::new(None::<String>);
    let busy = RwSignal::new(false);
    // 服务端是否开放注册；None = 还没问到。未知时先不渲染注册入口，避免闪一下又消失。
    let allow_reg = RwSignal::new(None::<bool>);
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

    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        spawn_local(async move {
            allow_reg.set(Some(fetch_allow_registration().await));
        });
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
                    // 登录成功，失效提示就此翻篇；否则这个位会一直留着，用户下次
                    // 主动来登录页还会看到一句莫名其妙的「登录已失效」。
                    auth.session_lost.set(false);
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
                        {move || auth.session_lost.get().then(|| view! {
                            <p class="error">"登录已失效，请重新登录"</p>
                        })}
                        {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
                        {move || (!ready.get()).then(|| view! {
                            <p class="mut" style="text-align:center">"正在加载客户端资源，加载完成后即可登录…"</p>
                        })}
                        <button class="btn pri" type="submit" style="justify-content:center;padding:9px" disabled=move || busy.get() || !ready.get()>
                            {move || if is_register.get() { "注册并登录" } else { "登录" }}
                        </button>
                        <div style="display:flex;justify-content:space-between" class="mut">
                            {move || match allow_reg.get() {
                                Some(true) => view! {
                                    <span class="link" on:click=move |_| is_register.update(|v| *v = !*v)>
                                        {if is_register.get() { "已有账号，去登录" } else { "立即注册" }}
                                    </span>
                                }.into_any(),
                                Some(false) => view! {
                                    <span class="mut">"已关闭开放注册，请联系系统管理员"</span>
                                }.into_any(),
                                None => ().into_any(),
                            }}
                            <u class="mut">"忘记密码"</u>
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
                                        "wechat" => "微信扫码登录",
                                        "google" => "Google 账号登录",
                                        "github" => "GitHub 账号登录",
                                        _ => name.as_str(),
                                    };
                                    let start_url = format!(
                                        "/api/auth/{}/start?return_to={}",
                                        name,
                                        urlencoding::encode(&return_to_default()),
                                    );
                                    (label.to_string(), start_url)
                                }).collect();
                                Some(view! {
                                    <div class="divider">"或使用以下方式继续"</div>
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
                        <div class="mut" style="text-align:center">"私有部署 · 数据不出内网 · 会话经 HttpOnly Cookie"</div>
                    </form>
                }.into_any()
            })}
        </div>
    }
}
