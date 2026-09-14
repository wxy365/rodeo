use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// 权限排序: Owner > Maintainer > Worker > Reader（枚举声明顺序即权限升序）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum WorkspaceRole {
    Reader,
    Worker,
    Maintainer,
    Owner,
}

impl WorkspaceRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            WorkspaceRole::Reader => "reader",
            WorkspaceRole::Worker => "worker",
            WorkspaceRole::Maintainer => "maintainer",
            WorkspaceRole::Owner => "owner",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "reader" => Some(WorkspaceRole::Reader),
            "worker" => Some(WorkspaceRole::Worker),
            "maintainer" => Some(WorkspaceRole::Maintainer),
            "owner" => Some(WorkspaceRole::Owner),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Workspace {
    pub id: Ulid,
    pub name: String,
    pub slug: String,
    pub description: String,
    pub owner_id: Ulid,
    pub created_at: DateTime<Utc>,
}

impl Workspace {
    pub fn new(name: String, slug: String, description: String, owner_id: Ulid) -> Self {
        Self {
            id: Ulid::new(),
            name,
            slug,
            description,
            owner_id,
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceMember {
    pub workspace_id: Ulid,
    pub account_id: Ulid,
    pub role: WorkspaceRole,
    pub joined_at: DateTime<Utc>,
}

impl WorkspaceMember {
    pub fn new(workspace_id: Ulid, account_id: Ulid, role: WorkspaceRole) -> Self {
        Self {
            workspace_id,
            account_id,
            role,
            joined_at: Utc::now(),
        }
    }
}

/// 待接受的邀请。存在即「待接受」——接受 / 拒绝 / 撤销都是删掉这条记录，
/// 状态变迁只留在审计日志里，因此这里没有 status 字段，也不会出现「已接受的邀请」。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Invite {
    pub workspace_id: Ulid,
    pub account_id: Ulid,
    pub role: WorkspaceRole,
    pub invited_by: Ulid,
    pub created_at: DateTime<Utc>,
}

impl Invite {
    pub fn new(
        workspace_id: Ulid,
        account_id: Ulid,
        role: WorkspaceRole,
        invited_by: Ulid,
    ) -> Self {
        Self {
            workspace_id,
            account_id,
            role,
            invited_by,
            created_at: Utc::now(),
        }
    }
}
