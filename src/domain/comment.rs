use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// Entry 评论。正文是 Quill Delta JSON，与 `Entry.detail` 同构。
///
/// 没有 `deleted_at`：评论走物理删除（对齐 `remove_labeling`），被删掉的正文
/// 留在审计的 before 快照里，不需要在列表里留空洞。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comment {
    pub id: Ulid,
    pub entry_code: String,
    /// 冗余存一份：权限校验与「这条评论属于哪个空间」不必每次回查 Entry。
    /// 不参与键布局——Entry Code 全局唯一，`entry_code` 单独作前缀已足够。
    pub workspace_id: Ulid,
    pub body: String,
    pub created_by: Ulid,
    pub updated_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Comment {
    pub fn new(entry_code: String, workspace_id: Ulid, body: String, actor: Ulid) -> Self {
        let now = Utc::now();
        Self {
            id: Ulid::new(),
            entry_code,
            workspace_id,
            body,
            created_by: actor,
            updated_by: actor,
            created_at: now,
            updated_at: now,
        }
    }
}
