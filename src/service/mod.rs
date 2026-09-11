pub mod audit;
pub mod auth;
pub mod entry;
pub mod label;
pub mod search;
pub mod workspace;

use std::sync::Arc;

use crate::config::Config;
use crate::storage::DocStore;

pub use audit::AuditService;
pub use auth::{AuthContext, AuthService};
pub use entry::EntryService;
pub use label::LabelService;
pub use search::SearchIndex;
pub use workspace::WorkspaceService;

/// 聚合所有服务，供 GraphQL 层共享。
pub struct Services {
    pub store: Arc<DocStore>,
    pub config: Arc<Config>,
    pub auth: AuthService,
    pub workspace: WorkspaceService,
    pub entry: EntryService,
    pub label: LabelService,
    pub audit: AuditService,
}

impl Services {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Self {
        let auth = AuthService::new(store.clone(), config.clone());
        let workspace = WorkspaceService::new(store.clone());
        let entry = EntryService::new(store.clone());
        let label = LabelService::new(store.clone());
        let audit = AuditService::new(store.clone());
        Self {
            store,
            config,
            auth,
            workspace,
            entry,
            label,
            audit,
        }
    }
}
