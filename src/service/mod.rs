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
pub mod view;
pub mod workspace;

use std::sync::Arc;

use crate::api::graphql::{build_schema, AppSchema};
use crate::config::Config;
use crate::error::AppError;
use crate::service::agent::runner::SessionTurns;
use crate::service::agent::templates::{validate_templates, ToolSchema};
use crate::service::agent::tools::build_tools;
use crate::storage::{BlobStore, DocStore};

pub use agent::AgentService;
pub use ai::{AiClient, AiService};
pub use attachment::AttachmentService;
pub use audit::AuditService;
pub use auth::{AuthContext, AuthService};
pub use comment::CommentService;
pub use entry::EntryService;
pub use label::LabelService;
pub use message::MessageService;
pub use relation::RelationService;
pub use rule::{RuleEngine, RuleService};
pub use search::SearchIndex;
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
    pub search: Arc<SearchIndex>,
    pub view: ViewService,
    pub ai: AiService,
    pub ai_client: Option<AiClient>,
    pub rule: RuleService,
    pub relation: RelationService,
    pub agent: AgentService,
    pub schema: Arc<AppSchema>,
    pub agent_tools: Arc<Vec<ToolSchema>>,
    pub agent_turns: Arc<SessionTurns>,
    pub oauth_registry: Arc<crate::service::oauth::OAuthRegistry>,
    pub oauth_state: Arc<crate::service::oauth_state::OAuthStateStore>,
    pub oauth_bindings: crate::service::oauth_bindings::OAuthBindingsService,
}

impl Services {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Result<Self, AppError> {
        let search = Arc::new(SearchIndex::open(&format!("{}/search", config.data_dir()))?);
        // 未配置就保持 None，启动阶段不做任何网络操作。
        let ai_client = AiClient::from_config(&config.ai)?;
        let workspace = WorkspaceService::new(store.clone());
        let message = MessageService::new(store.clone(), workspace.clone());
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
        let tools =
            build_tools(&schema).map_err(|e| AppError::Ai(format!("build_tools 失败: {e}")))?;
        validate_templates(&schema)
            .map_err(|e| AppError::Ai(format!("validate_templates 失败: {e}")))?;
        let agent_turns = Arc::new(SessionTurns::default());
        // OAuth：按 config 启用的 provider 灌进 registry；state/bindings 服务总是构造，
        // 这样 `[auth.oauth.wechat]` 整段缺失时启动依然通过（registry 为空、`enabled_names()` 返回空集）。
        let wechat_cfg = config
            .auth
            .oauth
            .wechat
            .as_ref()
            .unwrap_or(&crate::config::WeChatOAuthConfig::default());
        let wechat = crate::service::oauth::wechat::WeChatProvider::from_config(wechat_cfg)?;
        let mut oauth_providers: Vec<Arc<dyn crate::service::oauth::OAuthProvider>> = Vec::new();
        if let Some(w) = wechat {
            oauth_providers.push(Arc::new(w));
        }
        // 必须在 `let services = Self { store, ... }` 之前构造，否则 store 已 move，
        // 这里再调 `OAuthBindingsService::new(store.clone())` 会触发 E0382。
        let oauth_bindings =
            crate::service::oauth_bindings::OAuthBindingsService::new(store.clone());
        let services = Self {
            auth: AuthService::new(store.clone(), config.clone()),
            workspace,
            message,
            entry,
            comment,
            attachment,
            label: LabelService::new(store.clone()),
            audit: AuditService::new(store.clone()),
            view: ViewService::new(store.clone()),
            ai: AiService::new(store.clone()),
            ai_client,
            rule: RuleService::new(store.clone()),
            relation,
            agent: AgentService::new(store.clone(), config.clone()),
            search,
            store,
            config,
            schema,
            agent_tools: tools,
            agent_turns,
            oauth_registry: Arc::new(crate::service::oauth::OAuthRegistry::new(oauth_providers)),
            oauth_state: crate::service::oauth_state::OAuthStateStore::new(),
            oauth_bindings,
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
        Ok(services)
    }
}
