#![recursion_limit = "1024"]

#[cfg(feature = "ssr")]
#[tokio::main(flavor = "current_thread")]
async fn main() {
    use std::sync::Arc;

    use axum::routing::{get, post};
    use axum::{Extension, Router};
    use leptos::prelude::*;
    use leptos_axum::{generate_route_list, LeptosRoutes};
    use tower_http::compression::CompressionLayer;
    use tower_http::limit::RequestBodyLimitLayer;
    use tower_http::services::ServeFile;

    use rodeo::api::{
        agent_stream_handler, build_schema, download_attachment, github_callback, github_start,
        graphql_handler, google_callback, google_start, wechat_callback, wechat_start, ws_handler,
        AppState,
    };
    use rodeo::app::*;
    use rodeo::config::Config;
    use rodeo::service::Services;
    use rodeo::storage::DocStore;

    // 必须先于任何 TLS 工作：rig-core 0.42 的 `rustls` 特性会拉进 reqwest 0.13 的
    // rustls 路径，而 reqwest 0.13 的 `rustls` 特性 = `["__rustls-aws-lc-rs", ...]`,
    // 即强制打开 rustls 的 `aws_lc_rs` 特性。仓库顶层 rustls 又显式开了 `ring`——
    // 两个 provider 同时编译进二进制。rustls 的
    // `CryptoProvider::from_crate_features()` 在这种情况下返回 None，
    // `get_default_or_install_from_crate_features()` 紧接着 panic（消息原话
    // "make sure exactly one of the 'aws-lc-rs' and 'ring' features is enabled"）。
    // axum-server 的 `ServerConfig::builder()`、hyper-rustls 都先看
    // `CryptoProvider::get_default()`：装好 ring 之后它们就拿到 ring，不会再走
    // `from_crate_features`，panic 路径被切断。aws-lc-rs 仍编译进二进制但永不被使用。
    install_ring_crypto_provider();

    // config 必须先于 tracing init：开文件 appender 需要 logging.dir。
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.toml".to_string());
    let config = Arc::new(Config::load(&config_path).expect("加载配置失败"));

    let log_dir = std::path::Path::new(&config.logging.dir);
    std::fs::create_dir_all(log_dir).expect("创建日志目录失败");

    // 双写：stdout 保留 dev / 容器 / journald 输出；文件按日滚动用于事后排查。
    init_tracing(&config.logging);
    // panic hook 在 init 之后才能用 tracing。
    install_panic_hook();
    // 启动期清理早于 keep_days 的日志；0 表示不清理。
    cleanup_old_logs(log_dir, &config.logging.prefix, config.logging.keep_days);

    let store = Arc::new(DocStore::from_config(&config.storage).expect("打开存储失败"));
    let services = Arc::new(Services::new(store.clone(), config.clone()).expect("初始化服务失败"));
    services.auth.bootstrap_admin().expect("初始化管理员失败");

    tracing::info!("全文检索后端: {}", services.search.name());

    // 附件后端探活：rustfs 的端点/桶/密钥配错，应当在启动期就带着原因失败，
    // 而不是等用户第一次上传时给一条含糊的错误。
    if let Err(e) = services.attachment.check_blob_store().await {
        panic!("附件存储不可用: {e}");
    }

    let app_state = Arc::new(AppState {
        services: services.clone(),
        schema: build_schema(),
    });

    let conf = get_configuration(None).unwrap();
    let leptos_options = conf.leptos_options;

    // 绑定地址：LEPTOS_SITE_ADDR 优先——cargo leptos watch 靠它注入 dev 地址，也留给运维
    // 临时覆盖；没设时（手工跑生产二进制）才轮到 config.toml 的 [server]。这是配置第一次
    // 真正生效的地方，所以把来源一并打进日志，别再让它静默。
    let (addr, addr_src) = match std::env::var("LEPTOS_SITE_ADDR") {
        Ok(v) => (
            v.parse()
                .unwrap_or_else(|e| panic!("LEPTOS_SITE_ADDR 非法（{v}）: {e}")),
            "LEPTOS_SITE_ADDR",
        ),
        Err(_) => (
            config.bind_addr().unwrap_or_else(|e| panic!("{e}")),
            "config.toml [server]",
        ),
    };

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
        .route("/api/auth/wechat/start", get(wechat_start))
        .route("/api/auth/wechat/callback", get(wechat_callback))
        .route("/api/auth/google/start", get(google_start))
        .route("/api/auth/google/callback", get(google_callback))
        .route("/api/auth/github/start", get(github_start))
        .route("/api/auth/github/callback", get(github_callback))
        .route("/api/health", get(|| async { "ok" }))
        .route("/api/attachments/{id}", get(download_attachment))
        .route("/api/agent/turns/{turn_id}/stream", get(agent_stream_handler))
        // WebSocket 实时推送通道。token 走 query param（axum WebSocketUpgrade 不能
        // 收自定义 header）。鉴权失败在 upgrade 之前返回 401，handler 内部完成。
        .route("/api/ws", get(ws_handler))
        // favicon 必须放在 leptos_routes 之前：`/favicon.svg` 是单段路径，
        // 会被 Leptos 的 ParamSegment("slug") workspace 路由抢走渲染成工作空间
        // 页面，SPA fallback 的 ServeDir 因此永远收不到它。
        .route_service(
            "/favicon.svg",
            ServeFile::new(format!("{}/favicon.svg", leptos_options.site_root)),
        )
        .leptos_routes(&leptos_options, routes, {
            let leptos_options = leptos_options.clone();
            move || shell(leptos_options.clone())
        })
        .fallback(leptos_axum::file_and_error_handler(shell))
        // 全局 gzip：CompressionLayer 默认 predicate 自动跳过已是 image/* /
        // video/* / font/* / application-*-compressed 等「自带压缩」的内容，
        // 并尊重请求 Accept-Encoding。GraphQL JSON / SSR HTML 都受益，
        // 静态资源原样吐回。放在 Extension 之前——后者只是注入 AppState，
        // 顺序无关。
        .layer(CompressionLayer::new())
        .layer(Extension(app_state))
        .with_state(leptos_options);

    // 把两种监听模式包成同型 future 喂给 select。任一异常退出都将被记录。
    let server: std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<()>>>,
    > = match config.tls() {
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
            tracing::info!("listening on https://{addr}（来源 {addr_src}）");
            Box::pin(async move {
                axum_server::bind_rustls(addr, rustls_config)
                    .serve(app.into_make_service())
                    .await
            })
        }
        None => {
            tracing::info!("listening on http://{addr}（来源 {addr_src}）");
            let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
            Box::pin(async move {
                axum::serve(listener, app.into_make_service()).await
            })
        }
    };

    // 任一分支先完成即退出：服务端异常 → 记 error 退出；收到退出信号 → 记 warn 退出。
    tokio::select! {
        res = server => {
            if let Err(e) = res {
                tracing::error!("axum serve 异常退出: {e}");
            } else {
                tracing::info!("axum serve 正常退出");
            }
        }
        sig = shutdown_signal() => {
            tracing::warn!("收到 {sig}，开始退出");
        }
    }
}

#[cfg(not(feature = "ssr"))]
pub fn main() {}

/// 在 tokio runtime 起来之前同步跑一次：把 ring 钉死为全局默认 crypto provider。
/// 见 `main()` 顶部注释；`install_default` 重复占用已经注册过的 provider 时会
/// 返回 `Err`，我们忽略——这里只要「第一个调用者赢」就够了。
#[cfg(feature = "ssr")]
fn install_ring_crypto_provider() {
    use rustls::crypto::CryptoProvider;
    let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
}

/// stdout + 文件双写。`tracing_appender::rolling::daily` 内部走同步 IO（不开
/// `non_blocking` 缓冲），panic / 突然断电场景下日志直接落盘。
#[cfg(feature = "ssr")]
fn init_tracing(logging: &rodeo::config::LoggingConfig) {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "rodeo=info,tower_http=info".into());

    let stdout_layer = fmt::layer().with_writer(std::io::stdout);
    let file_appender = tracing_appender::rolling::daily(&logging.dir, &logging.prefix);
    let file_layer = fmt::layer().with_writer(file_appender).with_ansi(false);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();
}

/// 把 panic 信息也写进文件，方便事后排查。默认 hook 在 `RUST_BACKTRACE=1` 时
/// 打出 backtrace；systemd unit / docker 部署建议带上。
#[cfg(feature = "ssr")]
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_string());
        // 用 tracing 写——文件 layer 的 writer 是同步 IO，panic unwind 之前
        // 这条日志已落盘。
        tracing::error!("PANIC at {loc}: {msg}");
        default_hook(info);
    }));
}

/// 启动期清理早于 `keep_days` 的 `{prefix}.YYYY-MM-DD.log`。`keep_days = 0` 跳过。
/// 只匹配同前缀同后缀的文件，不会误删同目录下无关文件。
#[cfg(feature = "ssr")]
fn cleanup_old_logs(dir: &std::path::Path, prefix: &str, keep_days: u32) {
    if keep_days == 0 {
        return;
    }
    let Some(cutoff) = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(u64::from(keep_days) * 86_400))
    else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with(&format!("{prefix}.")) || !name.ends_with(".log") {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        if modified < cutoff {
            match std::fs::remove_file(&path) {
                Ok(_) => tracing::info!("已清理过期日志 {:?}", path),
                Err(e) => tracing::warn!("清理过期日志 {:?} 失败: {e}", path),
            }
        }
    }
}

/// 监听 SIGTERM / SIGINT / SIGHUP，第一个到达的即返回。Linux 上 systemkd stop、
/// Ctrl+C、`nginx -s reload` 风格的 HUP 都能触发干净退出。
#[cfg(feature = "ssr")]
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("注册 SIGTERM 监听失败");
        let mut int = signal(SignalKind::interrupt()).expect("注册 SIGINT 监听失败");
        let mut hup = signal(SignalKind::hangup()).expect("注册 SIGHUP 监听失败");
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv()  => "SIGINT",
            _ = hup.recv()  => "SIGHUP",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl-C"
    }
}
