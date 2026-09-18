#![recursion_limit = "1024"]

#[cfg(feature = "ssr")]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use std::sync::Arc;

    use axum::routing::{get, post};
    use axum::{Extension, Router};
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};
    use tower_http::limit::RequestBodyLimitLayer;

    use rodeo::api::{build_schema, download_attachment, graphql_handler, AppState};
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

    // 附件上限 50MB，服务层会返回「附件超过 50MB」的友好错误；这里放到 64MB，
    // 给 multipart 边界与其余表单字段留出余量，别让 55MB 的验收请求先被 413 掉。
    // 仍然只是一条路由上的硬上限，未鉴权客户端刷不出无限大的临时文件。
    const GRAPHQL_BODY_LIMIT: usize = 64 * 1024 * 1024;

    let app = Router::new()
        .route(
            "/api/graphql",
            post(graphql_handler).layer(RequestBodyLimitLayer::new(GRAPHQL_BODY_LIMIT)),
        )
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/attachments/{id}", get(download_attachment))
        .leptos_routes(&leptos_options, routes, {
            let leptos_options = leptos_options.clone();
            move || shell(leptos_options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        .layer(Extension(app_state))
        .with_state(leptos_options);

    match config.tls() {
        Some(tls) => {
            let rustls_config =
                axum_server::tls_rustls::RustlsConfig::from_pem_file(&tls.cert_path, &tls.key_path)
                    .await
                    .unwrap_or_else(|e| {
                        panic!(
                            "加载 TLS 证书失败 (cert={}, key={}): {e}",
                            tls.cert_path, tls.key_path
                        )
                    });
            tracing::info!("listening on https://{addr}");
            axum_server::bind_rustls(addr, rustls_config)
                .serve(app.into_make_service())
                .await
                .unwrap();
        }
        None => {
            tracing::info!("listening on http://{addr}");
            let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
            axum::serve(listener, app.into_make_service()).await.unwrap();
        }
    }
}

#[cfg(not(feature = "ssr"))]
pub fn main() {}
