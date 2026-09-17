pub mod ai;
pub mod audit;
pub mod auth;
pub mod comment;
pub mod entry;
pub mod label;
pub mod rule;
pub mod search;
pub mod view;
pub mod workspace;

use std::sync::Arc;

use crate::config::Config;
use crate::error::AppError;
use crate::storage::DocStore;

pub use ai::{AiClient, AiService};
pub use audit::AuditService;
pub use auth::{AuthContext, AuthService};
pub use comment::CommentService;
pub use entry::EntryService;
pub use label::LabelService;
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
    pub entry: EntryService,
    pub comment: CommentService,
    pub label: LabelService,
    pub audit: AuditService,
    pub search: Arc<SearchIndex>,
    pub view: ViewService,
    pub ai: AiService,
    pub ai_client: Option<AiClient>,
    pub rule: RuleService,
}

impl Services {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Result<Self, AppError> {
        let search = Arc::new(SearchIndex::open(&format!("{}/search", config.data_dir()))?);
        // 未配置就保持 None，启动阶段不做任何网络操作。
        let ai_client = AiClient::from_config(&config.ai)?;
        let entry = EntryService::with_search(store.clone(), search.clone());
        // 评论服务要与 EntryService 共用同一份实例：评论变更后要触发它的重索引。
        let comment = CommentService::new(store.clone(), entry.clone());
        let services = Self {
            auth: AuthService::new(store.clone(), config.clone()),
            workspace: WorkspaceService::new(store.clone()),
            entry,
            comment,
            label: LabelService::new(store.clone()),
            audit: AuditService::new(store.clone()),
            view: ViewService::new(store.clone()),
            ai: AiService::new(store.clone()),
            ai_client,
            rule: RuleService::new(store.clone()),
            search,
            store,
            config,
        };
        services.search.backfill(&services.store)?;
        services.entry.labelings_by_workspace_backfill(&services.store)?;
        Ok(services)
    }
}
