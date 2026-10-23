use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::{provide_meta_context, HashedStylesheet, MetaTags, Script, Stylesheet, Title};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_navigate;
use leptos_router::{ParamSegment, StaticSegment};

use crate::frontend::agent_panel::{provide_agent_panel_open, AgentPanel};
use crate::frontend::agent_side_effects::provide_agent_side_effects;
use crate::frontend::components::AppBar;
use crate::frontend::graphql_client::{clear_token, get_token, me, set_token};
use crate::frontend::i18n::{provide_i18n, Locale};
use crate::frontend::pages::{
    Account, Admin, EntryFullScreen, Home, Login, SharedEntry, WorkspaceList, WorkspaceMain,
    WorkspaceSettings,
};
use crate::frontend::provide_auth;
use crate::frontend::use_auth;
use crate::frontend::ws::spawn_ws_connected_root;

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
            // 拉一下 me() 拿到 isAdmin / 完整 user，再 navigate。
            // me() 同样走 App 启动 Effect 的 retry 思路：失败一次不写 `auth.user`，
            // 直接 navigate 会把人送进一个 AppBar 整棵子树不挂载的页面——尤其
            // return_to=工作空间时拿不到「新建 Entry」入口。
            spawn_local(async move {
                let mut user_opt: Option<crate::frontend::User> = None;
                for attempt in 0..2 {
                    match me().await {
                        Ok(Some(u)) => {
                            user_opt = Some(u);
                            break;
                        }
                        Ok(None) => break, // token 已被服务端判死，不再写 user
                        Err(_) if attempt == 0 => {
                            #[cfg(target_arch = "wasm32")]
                            {
                                gloo_timers::future::TimeoutFuture::new(500).await;
                            }
                            if get_token().is_none() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                if let Some(user) = user_opt {
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
/// 路径下没有 Parts——读 `<html lang>`（head 内联脚本已经把 `localStorage.rodeo_lang`
/// 同步过来），与 SSR 决议一致；再退回 En。
fn resolve_initial_locale() -> Locale {
    #[cfg(feature = "ssr")]
    {
        use axum::http::request::Parts;
        if let Some(parts) = use_context::<Parts>() {
            return resolve_from_parts(&parts);
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(s) = read_html_lang() {
            if let Some(loc) = Locale::from_browser(&s) {
                return loc;
            }
        }
    }
    Locale::En
}

/// 读 `<html lang>`。head 内联脚本里 `localStorage.rodeo_lang` 优先：用户最近一次
/// 在 AppBar 显式选过的 locale 会被同步成 `<html lang>`，hydrate 直接拿同一个值。
/// 失败 / 缺失一律返回 None，让调用方走默认 En。
#[cfg(target_arch = "wasm32")]
fn read_html_lang() -> Option<String> {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
        .and_then(|el| el.get_attribute("lang"))
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
            if let Some(loc) = Locale::from_browser(lang) {
                return loc;
            }
        }
    }
    if let Some(cookie) = parts.headers.get(COOKIE).and_then(|v| v.to_str().ok()) {
        for pair in cookie.split(';') {
            let mut kv = pair.trim().splitn(2, '=');
            if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
                if k.trim() == "rodeo_lang" {
                    if let Some(loc) = Locale::from_browser(v.trim()) {
                        return loc;
                    }
                }
            }
        }
    }
    if let Some(al) = parts
        .headers
        .get(ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(loc) = Locale::from_accept_language(al) {
            return loc;
        }
    }
    Locale::En
}

/// 极简 `?lang=xx` 解析。不引 `url` 依赖，避免给 ssr 二进制再增一条 cargo 路径。
#[cfg(feature = "ssr")]
fn parse_query_lang(qs: &str) -> Option<&str> {
    for pair in qs.split('&') {
        let mut kv = pair.splitn(2, '=');
        if let (Some(k), Some(v)) = (kv.next(), kv.next()) {
            if k == "lang" {
                return Some(v);
            }
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
    // 主题上下文：ThemeButton / Theme 相关的所有 use_theme() 调用都从这里拿。
    // 之前没人 provide（只在 theme.rs 里写了一个 never-called 的 provide_theme），
    // 之所以没炸是因为 `<Show when=auth.user.get().is_some()>` 间接挡住了 AppBar
    // 里的 ThemeButton 挂载。现在 AppBar 改成永远挂载，这里必须补上。
    let _theme = crate::frontend::theme::provide_theme();
    // i18n：App 是 hydrate 入口，`ShellInner` 仅 SSR 走。`provide_i18n` 必须
    // 在这里挂——否则 hydrate 时 `use_i18n` 拿到的是 `fallback_controller`
    // 的 EN 默认值，body 永远停在 SSR 的语言。SSR 路径下 ShellInner 还会再
    // provide 一次（同类型覆盖同类型），最终 locale 与之前一致——不会改变
    // SSR 输出。
    let _i18n = provide_i18n(resolve_initial_locale());
    // LanguageButton 切换时由 `I18nController::set` 把信号 + localStorage + cookie +
    // `<html lang/dir>` 四件事同步完成——下次 SSR 从 cookie 回来，下次 hydrate 从
    // `<html lang>` 回来，hydrate 不再退回硬编码的英文。

    // 实时推送通道：在 App 入口 provide `WsContext`（仅 wasm 端实际建连）。
    // 30s polling 仍保留为兜底，AppBar 监听 `WsContext` 收到 `NewMessage` 时
    // 立即 refetch unread_message_count。
    spawn_ws_connected_root();

    // 应用挂载时若已登录（有 token）则拉取当前账号，供头像/昵称展示。
    //
    // `me` 返回 null 有两种来路：请求没带令牌，或令牌没通过校验（过期、已吊销）。这里的前提
    // 正是本地有令牌，所以拿到 null 只可能是后者——那张令牌已经死了，必须就地清掉并标记会话
    // 失效。否则 `/account`、`/admin` 会永远停在「加载中…」：它们只看得到 `user` 是 None，
    // 分不清「还在问」和「没有会话」。
    //
    // `Err` 不能直接吞：`<Show when=auth.user.get().is_some()>` 守护着 AppBar 整棵子树，
    // `auth.user` 停在 `None` 时用户在 WorkspaceMain 里就拿不到「新建 Entry」入口。
    // 失败通常是瞬时网络抖动，等 500ms 再试一次；两次都失败就放弃，把这条线索留给
    // 后续受保护页 Effect 重跑或用户主动操作时再触发。
    Effect::new_sync(move |_| {
        if !cfg!(target_arch = "wasm32") || get_token().is_none() || auth.user.get().is_some() {
            return;
        }
        spawn_local(async move {
            for attempt in 0..2 {
                match me().await {
                    Ok(Some(u)) => {
                        auth.user.set(Some(u));
                        return;
                    }
                    Ok(None) => {
                        clear_token();
                        auth.session_lost.set(true);
                        return;
                    }
                    Err(_) if attempt == 0 => {
                        // `gloo_timers` 是 wasm-only，SSR 侧这个分支根本走不到
                        // （外层 `cfg!` 早就 return 了），但 Rust 的类型检查仍然
                        // 要解析这段闭包——用 `#[cfg]` 守住，native build 就不
                        // 引用 `gloo_timers` 也就不会编错。
                        #[cfg(target_arch = "wasm32")]
                        {
                            gloo_timers::future::TimeoutFuture::new(500).await;
                        }
                        // 等待期间 OAuthCallback 或别处已经填上了 user / token 被清掉了，
                        // 都直接收手，别覆盖别人的结论。
                        if auth.user.get().is_some() || get_token().is_none() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
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
        //
        // **不再用 `<Show when=auth.user.get().is_some()>` 守卫 AppBar**：「新建 Entry」
        // 按钮由 `WorkspaceNewMenuSlot` 驱动（WorkspaceMain 挂载时写入槽），跟 auth.user
        // 没有直接耦合。一旦让 auth.user 决定 AppBar 出不出来，中间任何一环出错——
        // `me()` 在 hydrate 后返回 Err / Ok(None)、account 被 reset-data 清掉、token 的
        // `ver` 对不上当前版本——用户都会卡在「/login 进得去，/workspaces 出不来，进任何
        // /<slug> 都没有 AppBar」的状态。AppBar 改成始终挂载，AvatarMenu 内部根据
        // auth.user 决定显示用户头像还是登录入口；WorkspaceMain 的 slot 决定「新建」按钮。
        //
        // **必须放在 `<Router>` 内部**：AppBar 里 `use_navigate()`（点品牌回工作空间列表）
        // 需要 Router context。Slot 由 App 层 provide，挪进 Router 不影响读写；挂载顺序
        // 也不变（AppBar 是 Router 第一个子节点，WorkspaceMain 在 Routes 里随后挂载）。
        <Router>
            <AppBar />
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
                    // 公开的匿名只读分享页：与 workspace shell 同样挂 AppBar，但仍
                    // 通过 Routes 走标准渲染——`SharedEntry` 自己的 view 不显示任何
                    // 仅登录态可见的控件。`ParamSegment("token")` 必须是句末段，
                    // 否则会被 `:slug` 优先匹配。
                    <Route path=(StaticSegment("share"), ParamSegment("token")) view=SharedEntry/>
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
