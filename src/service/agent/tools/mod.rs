//! LLM agent tool 集合。每个 tool 是 typed Rig Tool trait 实现（rig facade）；
//! 工厂返回 `AgentTools` struct，runner 通过 AgentBuilder::tool() 逐个传入。

use std::sync::Arc;

use rig::agent::AgentBuilder;
use ulid::Ulid;

use crate::service::{AuthContext, Services};

pub mod add_comment;
pub mod archive;
pub mod attachment_link;
pub mod create_entry;
pub mod delete_entry;
pub mod label_schemas;
pub mod query_entries;
pub mod remove_comment;
pub mod remove_label;
pub mod search_entries;
pub mod set_labels;
pub mod unarchive;
pub mod update_entry;
pub mod view_get;

/// 把 14 个 tool 装进 AgentBuilder。每个工具持有 Arc<Services> + AuthContext +
/// workspace_id（per spec §8）。runner 拿到 builder 后调 `.register_into(builder)`。
pub struct AgentTools {
    pub create_entry: create_entry::CreateEntryTool,
    pub update_entry: update_entry::UpdateEntryTool,
    pub delete_entry: delete_entry::DeleteEntryTool,
    pub query_entries: query_entries::QueryEntriesTool,
    pub search_entries: search_entries::SearchEntriesTool,
    pub label_schemas: label_schemas::LabelSchemasTool,
    pub set_labels: set_labels::SetLabelsTool,
    pub remove_label: remove_label::RemoveLabelTool,
    pub add_comment: add_comment::AddCommentTool,
    pub remove_comment: remove_comment::RemoveCommentTool,
    pub archive: archive::ArchiveTool,
    pub unarchive: unarchive::UnarchiveTool,
    pub view_get: view_get::ViewGetTool,
    pub attachment_link: attachment_link::AttachmentLinkTool,
}

impl AgentTools {
    /// 构造 14 个 tool。每个 tool 各自持有 Arc<Services> 克隆 + AuthContext 克隆 +
    /// workspace_id 副本——它们彼此独立，runner 可单独跨 turn 复用。
    pub fn build(services: Arc<Services>, auth: AuthContext, workspace_id: Ulid) -> Self {
        Self {
            create_entry: create_entry::CreateEntryTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            update_entry: update_entry::UpdateEntryTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            delete_entry: delete_entry::DeleteEntryTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            query_entries: query_entries::QueryEntriesTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            search_entries: search_entries::SearchEntriesTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            label_schemas: label_schemas::LabelSchemasTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            set_labels: set_labels::SetLabelsTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            remove_label: remove_label::RemoveLabelTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            add_comment: add_comment::AddCommentTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            remove_comment: remove_comment::RemoveCommentTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            archive: archive::ArchiveTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            unarchive: unarchive::UnarchiveTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            view_get: view_get::ViewGetTool {
                services: services.clone(),
                auth: auth.clone(),
                workspace_id,
            },
            attachment_link: attachment_link::AttachmentLinkTool {
                services,
                auth,
                workspace_id,
            },
        }
    }

    /// 把 14 个 tool 链式 `.tool(t)` 进 builder，runner 拿返回值接着配置。
    /// rig 0.42 `AgentBuilder` 已经擦除模型类型参数（见 rig 0.42 MIGRATING.md
    /// "Agents erase the model type at construction"），所以这里不需要模型泛型。
    /// 但 builder 的 tool state 是 typestate：第一次 `.tool(...)` 把
    /// `AgentBuilder<NoToolConfig>` 转成 `AgentBuilder<WithBuilderTools>`，再
    /// 链式追加仍是同一类型。所以返回类型必须显式写 `WithBuilderTools`。
    pub fn register_into(
        self,
        builder: AgentBuilder,
    ) -> rig::agent::AgentBuilder<rig::agent::WithBuilderTools> {
        builder
            .tool(self.create_entry)
            .tool(self.update_entry)
            .tool(self.delete_entry)
            .tool(self.query_entries)
            .tool(self.search_entries)
            .tool(self.label_schemas)
            .tool(self.set_labels)
            .tool(self.remove_label)
            .tool(self.add_comment)
            .tool(self.remove_comment)
            .tool(self.archive)
            .tool(self.unarchive)
            .tool(self.view_get)
            .tool(self.attachment_link)
    }
}