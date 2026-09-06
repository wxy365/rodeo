use leptos::prelude::*;
use leptos_meta::{provide_meta_context, MetaTags, Stylesheet, Title};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::{ParamSegment, StaticSegment};

use crate::frontend::pages::{Home, Login, WorkspaceList, WorkspaceMain};
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
    let _auth = provide_auth();

    view! {
        <Stylesheet id="leptos" href="/pkg/rodeo.css"/>
        <Title text="Rodeo"/>
        <Router>
            <main>
                <Routes fallback=|| view! { <p>"页面不存在"</p> }>
                    <Route path=StaticSegment("") view=Home/>
                    <Route path=StaticSegment("login") view=Login/>
                    <Route path=StaticSegment("workspaces") view=WorkspaceList/>
                    <Route path=ParamSegment("slug") view=WorkspaceMain/>
                </Routes>
            </main>
        </Router>
    }
}
