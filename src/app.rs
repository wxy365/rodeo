use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::{
    provide_meta_context, HashedStylesheet, MetaTags, Script, Stylesheet, Title,
};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::{ParamSegment, StaticSegment};

use crate::frontend::components::AppBar;
use crate::frontend::graphql_client::{clear_token, get_token, me};
use crate::frontend::pages::{
    Account, Admin, EntryFullScreen, Home, Login, WorkspaceList, WorkspaceMain, WorkspaceSettings,
};
use crate::frontend::provide_auth;

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="zh">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
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
                    <Route path=StaticSegment("workspaces") view=WorkspaceList/>
                    <Route path=StaticSegment("admin") view=Admin/>
                    <Route path=StaticSegment("account") view=Account/>
                    <Route path=(ParamSegment("slug"), StaticSegment("settings")) view=WorkspaceSettings/>
                    <Route path=(ParamSegment("slug"), StaticSegment("entry"), ParamSegment("code")) view=EntryFullScreen/>
                    <Route path=ParamSegment("slug") view=WorkspaceMain/>
                </Routes>
            </main>
        </Router>
    }
}
