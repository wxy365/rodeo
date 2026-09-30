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
use crate::service::agent::templates::{validate_query_templates, validate_templates, ToolSchema};
use crate::service::agent::tools::{build_query_tools, build_tools};
use crate::storage::{cf, BatchOp, BlobStore, DocStore};

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
    pub rig_client: Option<RigClient>,
    pub rule: RuleService,
    pub relation: RelationService,
    pub agent: AgentService,
    pub schema: Arc<AppSchema>,
    pub agent_tools: Arc<Vec<ToolSchema>>,
    pub agent_turns: Arc<SessionTurns>,
}

impl Services {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Result<Self, AppError> {
        let search = Arc::new(SearchIndex::open(&format!("{}/search", config.data_dir()))?);
        // 未配置就保持 None，启动阶段不做任何网络操作。
        let rig_client = RigClient::from_config(&config.ai)?;
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
        let mutation_tools =
            build_tools(&schema).map_err(|e| AppError::Ai(format!("build_tools 失败: {e}")))?;
        let query_tools = build_query_tools(&schema)
            .map_err(|e| AppError::Ai(format!("build_query_tools 失败: {e}")))?;
        validate_templates(&schema)
            .map_err(|e| AppError::Ai(format!("validate_templates 失败: {e}")))?;
        validate_query_templates(&schema)
            .map_err(|e| AppError::Ai(format!("validate_query_templates 失败: {e}")))?;
        let tools: Arc<Vec<ToolSchema>> = Arc::new(
            mutation_tools
                .iter()
                .chain(query_tools.iter())
                .cloned()
                .collect(),
        );
        let agent_turns = Arc::new(SessionTurns::default());
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
            rig_client,
            rule: RuleService::new(store.clone()),
            relation,
            agent: AgentService::new(store.clone()),
            search,
            store,
            config,
            schema,
            agent_tools: tools,
            agent_turns,
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
        // Agent 消息：buckaroo → rig 重写后旧 bincode 形态不可解码；
        // 对话历史是会话级缓存、丢了不影响业务，直接整列族清空。失败不阻塞启动。
        if let Err(e) = clear_legacy_agent_messages(&services.store) {
            tracing::warn!("清空旧 agent 消息失败（不阻塞启动）: {e}");
        }
        Ok(services)
    }
}

/// 启动期 idempotent 清空 `cf::AGENT_MESSAGES`：旧 buckaroo 形态的 bincode 与
/// rig `Message` 形态不兼容，逐条扫出全部键后一次性删除。空 CF 是 no-op。
fn clear_legacy_agent_messages(store: &DocStore) -> Result<usize, AppError> {
    let mut count = 0;
    let mut ops = Vec::new();
    for (key, _) in store.scan_prefix(cf::AGENT_MESSAGES, b"")? {
        ops.push(BatchOp::delete(cf::AGENT_MESSAGES, key));
        count += 1;
    }
    if !ops.is_empty() {
        store.write_batch(ops)?;
    }
    if count > 0 {
        tracing::info!("已清空 {count} 条旧 agent 消息（rig 重写，bincode 不兼容）");
    }
    Ok(count)
}
