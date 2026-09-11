#![recursion_limit = "1024"]

#[cfg(feature = "ssr")]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use std::sync::Arc;

    use axum::routing::{get, post};
    use axum::{Extension, Router};
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};

    use rodeo::api::{build_schema, graphql_handler, AppState};
    use rodeo::app::*;
    use rodeo::config::Config;
    use rodeo::service::Services;
    use rodeo::storage::DocStore;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rodeo=info,tower_http=info".into()),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let config = Arc::new(Config::load(&config_path).expect("加载配置失败"));
    let store = Arc::new(DocStore::open(&config.data_dir()).expect("打开存储失败"));
    let services = Arc::new(Services::new(store.clone(), config.clone()).expect("初始化服务失败"));
    services.auth.bootstrap_admin().expect("初始化管理员失败");

    let app_state = Arc::new(AppState {
        services: services.clone(),
        schema: build_schema(),
    });

    let conf = get_configuration(None).unwrap();
    let addr = conf.leptos_options.site_addr;
    let leptos_options = conf.leptos_options;
    let routes = generate_route_list(App);

    let app = Router::new()
        .route("/api/graphql", post(graphql_handler))
        .route("/api/health", get(|| async { "ok" }))
        .leptos_routes(&leptos_options, routes, {
            let leptos_options = leptos_options.clone();
            move || shell(leptos_options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .layer(Extension(app_state))
        .with_state(leptos_options);

    tracing::info!("listening on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app.into_make_service()).await.unwrap();
}

#[cfg(not(feature = "ssr"))]
pub fn main() {}
