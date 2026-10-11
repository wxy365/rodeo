pub mod agent;
pub mod ai;
pub mod attachment;
pub mod audit;
pub mod auth;
pub mod comment;
pub mod entry;
pub mod label;
pub mod mention;
pub mod message;
pub mod oauth;
pub mod oauth_bindings;
pub mod oauth_state;
pub mod relation;
pub mod rule;
pub mod search;
pub mod share;
pub mod view;
pub mod workspace;

use std::sync::Arc;

use crate::api::graphql::{build_schema, AppSchema};
use crate::config::Config;
use crate::error::AppError;
use crate::service::agent::runner::SessionTurns;
use crate::storage::{BlobStore, DocStore};

pub use agent::AgentService;
pub use ai::{AiService, RigClient};
pub use attachment::AttachmentService;
pub use audit::AuditService;
pub use auth::{AuthContext, AuthService};
pub use comment::CommentService;
pub use entry::EntryService;
pub use label::LabelService;
pub use message::MessageService;
pub use relation::RelationService;
pub use rule::{RuleEngine, RuleService};
pub use search::{MeilisearchBackend, SearchBackend, TantivyBackend};
pub use share::ShareService;
pub use view::ViewService;
pub use workspace::WorkspaceService;

/// 聚合所有服务，供 GraphQL 层共享。
pub struct Services {
    pub store: Arc<DocStore>,
    pub config: Arc<Config>,
    pub auth: AuthService,
    pub workspace: WorkspaceService,
    pub message: MessageService,
    pub entry: EntryService,
    pub comment: CommentService,
    pub attachment: AttachmentService,
    pub label: LabelService,
    pub audit: AuditService,
    pub search: Arc<dyn SearchBackend>,
    pub view: ViewService,
    pub ai: AiService,
    pub rig_client: Option<RigClient>,
    pub rule: RuleService,
    pub relation: RelationService,
    pub share: ShareService,
    pub agent: AgentService,
    pub schema: Arc<AppSchema>,
    pub agent_turns: Arc<SessionTurns>,
    pub oauth_registry: Arc<crate::service::oauth::OAuthRegistry>,
    pub oauth_state: Arc<crate::service::oauth_state::OAuthStateStore>,
    pub oauth_bindings: crate::service::oauth_bindings::OAuthBindingsService,
    /// 实时推送中枢。`/api/ws` 路由在 handshake 时 subscribe；`MessageService::send`
    /// 写库成功后 best-effort 推送 `NewMessage`。
    pub ws_hub: Arc<crate::api::ws::WsHub>,
}

impl Services {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Result<Self, AppError> {
        // 全文检索后端按 `[search] backend` 派发：Tantivy 本地嵌入式、
        // Meilisearch 外部服务。Meilisearch 走自己的多线程 tokio runtime
        // 与 rodeo current_thread 完全隔离；健康检查在启动期同步探活，
        // 端点/key/index 名错就直接带着原因失败，不留到第一次搜索。
        let search: Arc<dyn SearchBackend> = match config.search.backend {
            crate::config::SearchBackend::Tantivy => {
                let dir = config
                    .search
                    .tantivy
                    .dir
                    .clone()
                    .unwrap_or_else(|| config.data_dir());
                Arc::new(TantivyBackend::open(&format!("{dir}/search"))?)
            }
            crate::config::SearchBackend::Meilisearch => {
                let meili = MeilisearchBackend::new(&config.search.meilisearch)?;
                meili.health_check()?;
                tracing::info!(
                    "meilisearch 健康检查通过 (url={}, index={})",
                    config.search.meilisearch.url,
                    config.search.meilisearch.index_name
                );
                Arc::new(meili)
            }
        };
        // 未配置就保持 None，启动阶段不做任何网络操作。
        let rig_client = RigClient::from_config(&config.ai)?;
        let workspace = WorkspaceService::new(store.clone());
        // 实时推送通道（spec §2 per-user hub）。`Arc<WsHub>` 同时挂在 Services 上
        // 给 axum 路由握手时 subscribe 用；message 服务内部也持一份，写库成功后
        // best-effort 推 `NewMessage { message_id }`（spec §7.1 收敛点）。
        let ws_hub: Arc<crate::api::ws::WsHub> = crate::api::ws::WsHub::new();
        let message = MessageService::new(store.clone(), workspace.clone(), Some(ws_hub.clone()));
        // RelationService 在 EntryService 之前：条目软删除要把关联清理拼进自己的
        // write_batch，必须先有 instance 才能传引用。
        let relation = RelationService::new(store.clone());
        // messages 必须在 workspaces 之后构造，EntryService 的 mention 派发要走它的成员表。
        let entry = EntryService::with_search(
            store.clone(),
            search.clone(),
            message.clone(),
            relation.clone(),
        );
        // 评论服务要与 EntryService 共用同一份实例：评论变更后要触发它的重索引。
        let comment = CommentService::new(store.clone(), entry.clone(), message.clone());
        // 附件服务同样要与 EntryService 共用同一份实例：上传后要推进条目更新时间并重索引。
        // 附件后端在启动期就把根目录建好 / 客户端初始化好，配置写错在这里就暴露。
        let blobs = BlobStore::from_config(&config.storage)?;
        let attachment = AttachmentService::new(store.clone(), entry.clone(), blobs);
        let schema = Arc::new(build_schema());
        let agent_turns = Arc::new(SessionTurns::default());
        // OAuth：按 config 启用的 provider 灌进 registry；state/bindings 服务总是构造，
        // 这样 `[auth.oauth.wechat]` 整段缺失时启动依然通过（registry 为空、`enabled_names()` 返回空集）。
        let wechat_cfg_default = crate::config::WeChatOAuthConfig::default();
        let wechat_cfg = config
            .auth
            .oauth
            .wechat
            .as_ref()
            .unwrap_or(&wechat_cfg_default);
        let wechat = crate::service::oauth::wechat::WeChatProvider::from_config(wechat_cfg)?;
        let mut oauth_providers: Vec<Arc<dyn crate::service::oauth::OAuthProvider>> = Vec::new();
        if let Some(w) = wechat {
            oauth_providers.push(Arc::new(w));
        }
        if let Some(cfg) = &config.auth.oauth.google {
            if !cfg.client_id.trim().is_empty() {
                if let Some(p) =
                    crate::service::oauth::google::GoogleProvider::from_config(cfg)?
                {
                    oauth_providers.push(Arc::new(p));
                }
            }
        }
        if let Some(cfg) = &config.auth.oauth.github {
            if !cfg.client_id.trim().is_empty() {
                if let Some(p) =
                    crate::service::oauth::github::GithubProvider::from_config(cfg)?
                {
                    oauth_providers.push(Arc::new(p));
                }
            }
        }
        // 必须在 `let services = Self { store, ... }` 之前构造，否则 store 已 move，
        // 这里再调 `OAuthBindingsService::new(store.clone())` 会触发 E0382。
        let oauth_bindings =
            crate::service::oauth_bindings::OAuthBindingsService::new(store.clone());
        // Entry 分享：仅依赖 `DocStore`，在最后的 Services 拼装前构造。
        let share = ShareService::new(store.clone());
        let services = Self {
            auth: AuthService::new(store.clone(), config.clone()),
            workspace,
            message,
            entry,
            comment,
            attachment,
            label: LabelService::new(store.clone()),
            audit: AuditService::new(store.clone()),
            view: ViewService::new(store.clone(), workspace.clone(), message.clone()),
            ai: AiService::new(store.clone()),
            rig_client,
            rule: RuleService::new(store.clone()),
            relation,
            agent: AgentService::new(store.clone()),
            share,
            search,
            store,
            config,
            schema,
            agent_turns,
            oauth_registry: Arc::new(crate::service::oauth::OAuthRegistry::new(oauth_providers)),
            oauth_state: crate::service::oauth_state::OAuthStateStore::new(),
            oauth_bindings,
            ws_hub,
        };
        // 先修标签定义：搜索与打标回填都按当前结构读数据，让它们看到一致的 schema。
        services.label.repair_legacy_schemas()?;
        services.search.backfill(&services.store)?;
        services
            .entry
            .labelings_by_workspace_backfill(&services.store)?;
        // 标签值二级索引：仅在首次启动 / 新装时灌，幂等；已经存在就跳过。
        services
            .entry
            .labelings_by_label_backfill(&services.store)?;
        // 视图排序的转码修复必须排在 repair_default_view_name 之前：
        // 后者用 `self.get(id)?` 读视图，而 `DocStore::get` 在 bincode 解码失败时返回
        // `Err` 而非 `None`，旧编码的记录会让启动直接失败。
        services.view.repair_legacy_view_sort()?;
        services.view.repair_default_view_name()?;
        // 关联：早期版本 RelationSemantic 用 internally-tagged enum + data variant
        // 形态，bincode 不能解；切到 struct 形态后旧记录会让 entryRelations 整个挂掉。
        // 启动期把读不出来的删掉，幂等，再启动就清干净了。
        services.relation.repair_undecodable()?;
        // Agent 消息：buckaroo → rig 重写后旧 bincode 形态不可解码。
        // AgentService::repair_legacy_messages 只动 decode 失败的那批：OLD 形态
        // （Role + content + tool_calls）转写为 NEW，rig 中间形态删掉。
        // 不再整列清空——避免把用户历史对话内容一起抹掉。
        if let Err(e) = services.agent.repair_legacy_messages() {
            tracing::warn!("agent 消息启动修复失败（不阻塞启动）: {e}");
        }
        Ok(services)
    }
}
