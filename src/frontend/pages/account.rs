use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_query_map};

use crate::frontend::components::logged_out;
use crate::frontend::graphql_client::{
    change_password, my_oauth_bindings, set_password, set_token, unbind_oauth_binding,
    OAuthBinding,
};
use crate::frontend::icons::ic_back;
use crate::frontend::{use_auth, AuthState};

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

    // 第三方绑定列表 + 解绑错误。空列表时不渲染 section，所以即使服务器对无绑定账号
    // 也回 `[]` 也不影响布局。
    let bindings = RwSignal::new(Vec::<OAuthBinding>::new());
    let bind_error = RwSignal::new(None::<String>);

    // 后补密码表单：仅 OAuth-only 账号（`!has_password`）展示，已设密码的账号直接走
    // 上面的「修改密码」即可，不需要再让用户重设一遍。
    let new_pw = RwSignal::new(String::new());
    let pw_error = RwSignal::new(None::<String>);
    let pw_busy = RwSignal::new(false);

    // 未登录、或本地令牌已被服务端判死，都回登录页。`logged_out()` 在 SSR 阶段恒为 true，
    // 所以只在 wasm 上跳，否则服务端渲染会直接跳走（同 admin.rs / entry.rs）。
    Effect::new(move |_| {
        if cfg!(target_arch = "wasm32") && (logged_out() || auth.session_lost.get()) {
            navigate("/login", Default::default());
        }
    });

    // 拉到 `auth.user` 之后再拉绑定列表——`me()` 还在进行时调 `myOAuthBindings` 会
    // 因没带令牌被服务端拒。`auth.user.get()` 既是触发器也是守卫。
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        if auth.user.get().is_none() {
            return;
        }
        spawn_local(async move {
            if let Ok(b) = my_oauth_bindings().await {
                bindings.set(b);
            }
        });
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

            {move || (!bindings.get().is_empty()).then(|| view! {
                <div class="panel set-body">
                    <h2>"第三方账号"</h2>
                    <div class="mut">"通过下列方式登录后即视为已绑定；解除绑定后该登录方式将无法再用于此账号。"</div>
                    {binding_rows(&bindings, &bind_error, &auth)}
                    {move || bind_error.get().map(|e| view! {
                        <div class="hint" style="margin-top:8px">{"⚠ "}{e}</div>
                    })}
                </div>
            })}

            {move || {
                let show = auth.user.get().as_ref().map(|u| !u.has_password).unwrap_or(false);
                show.then(|| set_password_section(auth, &new_pw, &pw_busy, &pw_error))
            }}

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

/// 把内部 provider 标识翻成中文/英文标签。新 provider 在这里加一行就行——别去
/// fallback 到原字符串，避免「wechat」这种内部值直接漏到用户眼前。
fn provider_label(p: &str) -> &'static str {
    match p {
        "wechat" => "微信",
        "google" => "Google",
        "github" => "GitHub",
        _ => "其他",
    }
}

/// 把 RFC3339 时间戳截到 `YYYY-MM-DD`。GraphQL 的 `DateTime<Utc>` 序列化成
/// RFC3339（带 `T` 与 `+00:00`），前 10 位就是日期切片。
fn format_dt(bound_at: &str) -> &str {
    if bound_at.len() >= 10 {
        &bound_at[..10]
    } else {
        bound_at
    }
}

/// 渲染所有第三方绑定行。把迭代从 `view!` 里挪出来——`view!` 解析器对「多层 closure
/// 套 collect」的写法不稳定，会把外层 `}` 误判成分隔符。每行 on:click 直接拿当前快照，
/// 解绑成功后通过 `bindings.update` 把这一行摘掉，再清掉之前的错误。
fn binding_rows(
    bindings: &RwSignal<Vec<OAuthBinding>>,
    bind_error: &RwSignal<Option<String>>,
    user: &AuthState,
) -> Vec<AnyView> {
    let snapshot: Vec<OAuthBinding> = bindings.get_untracked();
    snapshot
        .into_iter()
        .map(|b| {
            let provider = b.provider.clone();
            let external_id = b.external_id.clone();
            let account_id = user
                .user
                .get_untracked()
                .as_ref()
                .map(|u| u.id.clone())
                .unwrap_or_default();
            let p_click = provider.clone();
            let e_click = external_id.clone();
            let b_sig = *bindings;
            let e_sig = *bind_error;
            let email = b.email.clone();
            let display_name = b.display_name.clone();
            let bound_at = b.bound_at.clone();
            let external_for_fallback = external_id.clone();
            view! {
                <div
                    class="binding-row"
                    style="display:flex;justify-content:space-between;align-items:center;padding:8px 0;border-bottom:1px solid var(--bd)"
                >
                    <div>
                        <b>{provider_label(&provider)}</b>
                        <span class="mut" style="margin-left:8px">
                            {email
                                .or(display_name)
                                .unwrap_or_else(|| external_for_fallback.clone())}
                        </span>
                        <span class="mut" style="margin-left:8px;font-size:12px">
                            {"绑定于 "}{format_dt(&bound_at)}
                        </span>
                    </div>
                    <button
                        class="btn sm"
                        on:click=move |_| {
                            let account_id = account_id.clone();
                            let p = p_click.clone();
                            let e = e_click.clone();
                            spawn_local(async move {
                                match unbind_oauth_binding(&account_id, &p, &e).await {
                                    Ok(()) => {
                                        b_sig.update(|v| {
                                            v.retain(|x| !(x.provider == p && x.external_id == e))
                                        });
                                        e_sig.set(None);
                                    }
                                    Err(err) => e_sig.set(Some(err)),
                                }
                            });
                        }
                    >"解除绑定"</button>
                </div>
            }
            .into_any()
        })
        .collect()
}

/// 渲染「设置密码」section。提出来的原因同 `binding_rows`：`view!` 解析器吃不消
/// 多层 closure 套娃。提出来后整段都是直白的 `view!` 块。
fn set_password_section(
    user: AuthState,
    new_pw: &RwSignal<String>,
    pw_busy: &RwSignal<bool>,
    pw_error: &RwSignal<Option<String>>,
) -> AnyView {
    let user_sig = user.user;
    let pw = *new_pw;
    let busy = *pw_busy;
    let err = *pw_error;
    let submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        if busy.get_untracked() {
            return;
        }
        let p = pw.get();
        busy.set(true);
        err.set(None);
        spawn_local(async move {
            match set_password(&p).await {
                Ok((token, user)) => {
                    set_token(&token);
                    user_sig.set(Some(user));
                    pw.set(String::new());
                    busy.set(false);
                }
                Err(e) => {
                    err.set(Some(e));
                    busy.set(false);
                }
            }
        });
    };
    view! {
        <div class="panel set-body">
            <h2>"设置密码"</h2>
            <div class="mut">"此账号尚未设置本地密码。设置后即可用邮箱 + 密码登录。"</div>
            <form class="stack" on:submit=submit>
                <label class="fld">
                    <span>"新密码"</span>
                    <input
                        class="inp"
                        type="password"
                        autocomplete="new-password"
                        prop:value=pw
                        on:input=move |ev| pw.set(event_target_value(&ev))
                    />
                </label>
                <div class="mut">"最少 8 位，需包含大小写字母和数字。"</div>
                {move || err.get().map(|e| view! {
                    <div class="hint" style="margin-top:8px">{"⚠ "}{e}</div>
                })}
                <button
                    class="btn pri"
                    type="submit"
                    style="align-self:flex-start"
                    disabled=move || busy.get() || pw.get().is_empty()
                >"保存密码"</button>
            </form>
        </div>
    }
    .into_any()
}
