use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// 审计操作类型（对齐 spec 11.1，本轮只发出 Entry/Label 相关变体）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditAction {
    AccountLogin,
    AccountLogout,
    AccountCreated,
    AccountDisabled,
    WorkspaceCreated,
    WorkspaceDeleted,
    MemberInvited,
    MemberRemoved,
    RoleChanged,
    EntryCreated,
    EntryUpdated,
    EntryDeleted,
    LabelSchemaCreated,
    LabelSchemaUpdated,
    LabelingSet,
    LabelingRemoved,
    ViewCreated,
    ViewUpdated,
    ViewDeleted,
    AttachmentUploaded,
    AttachmentDeleted,
    WorkspaceUpdated,
    WorkspaceRestored,
    // 追加在末尾：bincode 按变体序号编码，新变体只能往后加，否则存量审计日志会错位。
    EntryArchived,
    EntryUnarchived,
    MemberJoined,
    InviteDeclined,
    InviteRevoked,
}

/// 审计日志。before/after 为 JSON 字符串（序列化后的快照）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditLog {
    pub id: Ulid,
    pub action: AuditAction,
    pub actor_id: Ulid,
    pub resource_type: String,
    pub resource_id: String,
    pub workspace_id: Option<Ulid>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub at: DateTime<Utc>,
}

impl AuditLog {
    pub fn new(
        action: AuditAction,
        actor_id: Ulid,
        resource_type: &str,
        resource_id: &str,
        workspace_id: Option<Ulid>,
        before: Option<String>,
        after: Option<String>,
    ) -> Self {
        Self {
            id: Ulid::new(),
            action,
            actor_id,
            resource_type: resource_type.to_string(),
            resource_id: resource_id.to_string(),
            workspace_id,
            before,
            after,
            at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_log_new_populates_fields() {
        let actor = Ulid::new();
        let workspace = Ulid::new();
        let log = AuditLog::new(
            AuditAction::EntryUpdated,
            actor,
            "entry",
            "TESTCODE0001",
            Some(workspace),
            Some("{\"title\":\"old\"}".to_string()),
            Some("{\"title\":\"new\"}".to_string()),
        );

        assert!(!log.id.is_nil(), "id must be generated");
        assert_eq!(log.action, AuditAction::EntryUpdated);
        assert_eq!(log.actor_id, actor);
        assert_eq!(log.resource_type, "entry");
        assert_eq!(log.resource_id, "TESTCODE0001");
        assert_eq!(log.workspace_id, Some(workspace));
        assert_eq!(log.before.as_deref(), Some("{\"title\":\"old\"}"));
        assert_eq!(log.after.as_deref(), Some("{\"title\":\"new\"}"));
    }

    #[test]
    fn audit_log_new_allows_optional_fields_as_none() {
        let log = AuditLog::new(
            AuditAction::EntryCreated,
            Ulid::new(),
            "entry",
            "TESTCODE0002",
            None,
            None,
            None,
        );
        assert_eq!(log.workspace_id, None);
        assert_eq!(log.before, None);
        assert_eq!(log.after, None);
    }

    #[test]
    fn audit_log_roundtrips_through_bincode() {
        let log = AuditLog::new(
            AuditAction::EntryDeleted,
            Ulid::new(),
            "entry",
            "TESTCODE0003",
            Some(Ulid::new()),
            None,
            Some("{\"deleted\":true}".to_string()),
        );
        let bytes = bincode::serialize(&log).expect("serialize audit log");
        let decoded: AuditLog = bincode::deserialize(&bytes).expect("deserialize audit log");
        assert_eq!(decoded, log);
    }
}
