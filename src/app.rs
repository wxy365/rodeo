use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::{provide_meta_context, MetaTags, Script, Stylesheet, Title};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::{ParamSegment, StaticSegment};

use crate::frontend::graphql_client::{get_token, me};
use crate::frontend::pages::{
    Admin, EntryFullScreen, Home, Login, WorkspaceList, WorkspaceMain, WorkspaceSettings,
};
use crate::frontend::provide_auth;

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="zh">
            <head>
                <meta charset="utf-8"/>
                <meta name="viewport" content="width=device-width, initial-scale=1"/>
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

    // 应用挂载时若已登录（有 token）则拉取当前账号，供头像/昵称展示。
    Effect::new_sync(move |_| {
        if cfg!(target_arch = "wasm32") && get_token().is_some() && auth.user.get().is_none() {
            spawn_local(async move {
                if let Ok(Some(u)) = me().await {
                    auth.user.set(Some(u));
                }
            });
        }
    });

    view! {
        <Stylesheet id="leptos" href="/pkg/rodeo.css"/>
        <Stylesheet id="tiny-editor" href="/tiny-editor/style.css"/>
        <Script type_="module" src="/tiny-editor/glue.js"/>
        <Title text="Rodeo"/>
        <Router>
            <main>
                <Routes fallback=|| view! { <p>"页面不存在"</p> }>
                    <Route path=StaticSegment("") view=Home/>
                    <Route path=StaticSegment("login") view=Login/>
                    <Route path=StaticSegment("workspaces") view=WorkspaceList/>
                    <Route path=StaticSegment("admin") view=Admin/>
                    <Route path=(ParamSegment("slug"), StaticSegment("settings")) view=WorkspaceSettings/>
                    <Route path=(ParamSegment("slug"), StaticSegment("entry"), ParamSegment("code")) view=EntryFullScreen/>
                    <Route path=ParamSegment("slug") view=WorkspaceMain/>
                </Routes>
            </main>
        </Router>
    }
}
