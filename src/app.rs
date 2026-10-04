use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::{provide_meta_context, HashedStylesheet, MetaTags, Script, Stylesheet, Title};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_navigate;
use leptos_router::{ParamSegment, StaticSegment};

use crate::frontend::agent_panel::{provide_agent_panel_open, AgentPanel};
use crate::frontend::agent_side_effects::provide_agent_side_effects;
use crate::frontend::components::AppBar;
use crate::frontend::graphql_client::{clear_token, get_token, me};
use crate::frontend::i18n::{provide_i18n, Locale};
use crate::frontend::pages::{
    Account, Admin, EntryFullScreen, Home, Login, WorkspaceList, WorkspaceMain, WorkspaceSettings,
};
use crate::frontend::provide_auth;
use crate::frontend::use_auth;

/// `web_sys::window().location().hash()` 的 wasm/ssr 兼容壳。SSR 没有
/// DOM，`web_sys` 也不在 ssr 依赖里——直接调用链接都过不去。SSR 侧返回
/// 空串，下面的 Effect 因 `cfg!` 早 return 本来也不会用。
#[cfg(target_arch = "wasm32")]
fn read_window_hash() -> String {
    web_sys::window()
        .and_then(|w| w.location().hash().ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_window_hash() -> String {
    String::new()
}

#[component]
pub fn OAuthCallback() -> impl IntoView {
    let auth = use_auth();
    let navigate = use_navigate();
    let _ = Effect::new(move |_| {
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        let hash = read_window_hash();
        // 形如 "#token=xxx&return_to=/workspaces"
        let mut token: Option<String> = None;
        let mut return_to = "/workspaces".to_string();
        for kv in hash.trim_start_matches('#').split('&') {
            if let Some((k, v)) = kv.split_once('=') {
                match k {
                    "token" => {
                        token = Some(
                            urlencoding::decode(v)
                                .map(|c| c.into_owned())
                                .unwrap_or_else(|_| v.to_string()),
                        )
                    }
                    "return_to" => {
                        return_to = urlencoding::decode(v)
                            .map(|c| c.into_owned())
                            .unwrap_or_else(|_| v.to_string())
                    }
                    _ => {}
                }
            }
        }
        // `navigate` 是 Fn，不是 Copy；Effect 闭包是 FnMut，调用 `navigate(...)`
        // 必须通过克隆走，否则闭包被推断成 FnOnce。克隆 `impl Fn(...) + Clone`
        // 廉价，每帧一次能接受。
        let navigate = navigate.clone();
        if let Some(t) = token {
            set_token(&t);
            // 拉一下 me() 拿到 isAdmin / 完整 user，再 navigate
            spawn_local(async move {
                if let Ok(Some(user)) = me().await {
                    auth.user.set(Some(user));
                    auth.session_lost.set(false);
                }
                navigate(&return_to, Default::default());
            });
        } else {
            navigate("/login?oauth_error=missing_token", Default::default());
        }
    });

    view! {
        <div class="oauth-callback">
            <p class="mut" style="text-align:center">"正在完成登录…"</p>
        </div>
    }
}

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <ShellInner options=options/>
    }
}

/// SSR / hydrate 都能复用的真壳：解析 locale（SSR 从 Parts 读，hydrate 从
/// localStorage 读），挂上 I18nController，再把 `<html lang/dir>` 与防闪屏
/// 内联脚本交给 `view!`。
#[component]
fn ShellInner(options: LeptosOptions) -> impl IntoView {
    let locale = resolve_initial_locale();
    let dir = if locale.is_rtl() { "rtl" } else { "ltr" };
    // 提供给 App 与所有子组件；首次渲染就拿对了 locale，避免 hydrate 后闪一下。
    let _ctrl = provide_i18n(locale);
    view! {
        <!DOCTYPE html>
        <html lang={locale.as_str()} dir=dir>
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
                // 浏览器页签图标：SVG 现代浏览器都吃，静态资源走 `assets-dir = "public"`。
                <link rel="icon" type="image/svg+xml" href="/favicon.svg"/>
                // 主题早应用脚本：必须在 HashedStylesheet 之前 inline，否则
                // CSS 第一次解析时还没有 `data-theme` 属性，系统 @media
                // 会先生效几毫秒——肉眼可见的色闪。这段脚本读 localStorage，
                // 把 data-theme 提前挂到 <html> 上；同步执行，跑在 DOM 构建前。
                <script>
                    "(function(){try{var s=localStorage.getItem('rodeo_theme');if(s==='light'||s==='dark'){document.documentElement.setAttribute('data-theme',s);}}catch(e){}})();"
                </script>
                // 语言防闪屏：localStorage 优先于 SSR 选择——前者是用户最近一次
                // 在 AppBar 显式选过的（带 cookie 写回），后者只是 Accept-Language
                // 推断；用户偏好 > 浏览器推断。脚本在 HashedStylesheet 之前跑。
                <script>
                    "(function(){try{var l=localStorage.getItem('rodeo_lang');if(l==='zh'||l==='en'||l==='ja'||l==='pt'||l==='es'||l==='ar'){var h=document.documentElement;h.setAttribute('lang',l);h.setAttribute('dir',l==='ar'?'rtl':'ltr');}else{var h2=document.documentElement.getAttribute('lang');if(h2){try{localStorage.setItem('rodeo_lang',h2);document.cookie='rodeo_lang='+h2+'; path=/; max-age=31536000; samesite=lax';}catch(e){}}}}catch(e){}})();"
                </script>
                // 主样式表走 cargo-leptos 的内容哈希；`Stylesheet` 不解析哈希，必须用这个。
                <HashedStylesheet id="leptos" options=options.clone() />
                <AutoReload options=options.clone() />
                <HydrationScripts options />
                <MetaTags/>
            </head>
            <body>
                <App/>
            </body>
        </html>
    }
}

/// SSR 路径下 leptos_axum 已经把 axum 的 `Parts` 作为 context 注入；hydrate
/// 路径下没有 Parts——退回 En（head 内联脚本会让 `document.cookie` 与
/// `localStorage` 写回用户最近一次的选择，下一轮 SSR 自然收敛）。
fn resolve_initial_locale() -> Locale {
    #[cfg(feature = "ssr")]
    {
        use axum::http::request::Parts;
        if let Some(parts) = use_context::<Parts>() {
            return resolve_from_parts(&parts);
        }
    }
    Locale::En
}

/// 把 `Parts`（请求头 + URL）走一遍：
///   1. `?lang=` query 覆盖（手动测试 / 链接分享用）
///   2. `rodeo_lang` cookie（用户在 AppBar 显式选过的）
///   3. `Accept-Language` 头（浏览器默认语言）
///   4. 兜底英文
#[cfg(feature = "ssr")]
fn resolve_from_parts(parts: &axum::http::request::Parts) -> Locale {
    use axum::http::header::{ACCEPT_LANGUAGE, COOKIE};
    if let Some(q) = parts.uri.query() {
        if let Some(lang) = parse_query_lang(q) {
            if let Some(loc) = Locale::from_browser(lang) { return loc; }
        }
    }
    if let Some(cookie) = parts.headers.get(COOKIE).and_then(|v| v.to_str().ok()) {
        for pair in cookie.split(';') {
            let mut kv = pair.trim().splitn(2, '=');
            if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
                if k.trim() == "rodeo_lang" {
                    if let Some(loc) = Locale::from_browser(v.trim()) { return loc; }
                }
            }
        }
    }
    if let Some(al) = parts.headers.get(ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()) {
        if let Some(loc) = Locale::from_accept_language(al) { return loc; }
    }
    Locale::En
}

/// 极简 `?lang=xx` 解析。不引 `url` 依赖，避免给 ssr 二进制再增一条 cargo 路径。
#[cfg(feature = "ssr")]
fn parse_query_lang(qs: &str) -> Option<&str> {
    for pair in qs.split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
            if k == "lang" { return Some(v); }
        }
    }
    None
}

#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    let auth = provide_auth();
    // 工作空间「新建」按钮的共享槽：在 App 层 provide，使 AppBar（Router 的
    // 兄弟）与 WorkspaceMain（Router 的后代）都能看到同一份信号。
    let _new_menu_slot = crate::frontend::provide_workspace_new_menu_slot();
    // 工作空间「时间轴切换」按钮的共享槽，与「新建」同源问题——AppBar 兄弟
    // 与 Router 后代共享一份读写。
    let _timeline_slot = crate::frontend::provide_workspace_timeline_slot();
    // Agent 面板开关：AppBar 里的 AI 按钮与 `<AgentPanel/>` 各自取用同一份信号。
    let _agent_panel_open = provide_agent_panel_open();
    // Agent 副作用 store：服务端 SSE 写入（Task 7/8）后由面板订阅刷新。
    let _agent_side_effects = provide_agent_side_effects();
    // i18n：locale 由 `ShellInner` 在解析 `Parts` 后提供；这里不再重复 provide。
    // LanguageButton 在用户切换时只更新既有 controller 的信号 + 写 localStorage + 写 cookie，
    // 三件事同步完成，下次 SSR 自动从 cookie 回来同一个 locale。

    // 应用挂载时若已登录（有 token）则拉取当前账号，供头像/昵称展示。
    //
    // `me` 返回 null 有两种来路：请求没带令牌，或令牌没通过校验（过期、已吊销）。这里的前提
    // 正是本地有令牌，所以拿到 null 只可能是后者——那张令牌已经死了，必须就地清掉并标记会话
    // 失效。否则 `/account`、`/admin` 会永远停在「加载中…」：它们只看得到 `user` 是 None，
    // 分不清「还在问」和「没有会话」。
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") || get_token().is_none() || auth.user.get().is_some() {
            return;
        }
        spawn_local(async move {
            match me().await {
                Ok(Some(u)) => auth.user.set(Some(u)),
                Ok(None) => {
                    clear_token();
                    auth.session_lost.set(true);
                }
                // 网络或服务端故障：身份其实未知，继续显示「加载中…」好过谎报「登录已失效」。
                Err(_) => {}
            }
        });
    });

    view! {
        <Stylesheet id="tiny-editor" href="/tiny-editor/style.css"/>
        <Script type_="module" src="/tiny-editor/glue.js"/>
        <Title text="Rodeo"/>
        // 顶栏抽到独立组件里：父组件把 `<Show>` 当兄弟元素用时，需要它的内部闭包是
        // `Fn`-compatible。把 navigate 调用挪到 AppBar 自己的 setup 阶段能避开这个
        // 边界——直接照搬 AvatarMenu 已经验证过的写法。
        <Show when=move || auth.user.get().is_some()>
            <AppBar />
        </Show>
        <Router>
            <main>
                <Routes fallback=|| view! { <p>"页面不存在"</p> }>
                    <Route path=StaticSegment("") view=Home/>
                    <Route path=StaticSegment("login") view=Login/>
                    <Route path=StaticSegment("oauth/callback") view=OAuthCallback/>
                    <Route path=StaticSegment("workspaces") view=WorkspaceList/>
                    <Route path=StaticSegment("admin") view=Admin/>
                    <Route path=StaticSegment("account") view=Account/>
                    <Route path=(ParamSegment("slug"), StaticSegment("settings")) view=WorkspaceSettings/>
                    <Route path=(ParamSegment("slug"), StaticSegment("entry"), ParamSegment("code")) view=EntryFullScreen/>
                    <Route path=ParamSegment("slug") view=WorkspaceMain/>
                </Routes>
            </main>
            // Agent 面板挂在 Router 内、main 外：use_params_map() 需要 Router context
            // （不能放在 </Router> 之外，否则启动期 panic）。可见性由 `AgentPanelOpen`
            // 内部 `<Show>` 控制；不再额外包一层 auth 判定，免得 user 信号闪动时
            // 面板也跟着消失。
            <AgentPanel/>
        </Router>
    }
}
