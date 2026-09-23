use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// 站内消息。一条只发给一个收件人；同一次 @ 多人 = 多条独立消息。
///
/// 没有「消息体」，只存轻量元数据 + preview。点击消息跳到来源 Entry，
/// 详情正文以最新版本为准——这是有意的：评论 / Entry 被编辑后，原文
/// 预览会过时；保留原文还得另存一份快照，得不偿失。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: Ulid,
    /// 谁接收这条消息。前缀扫描 `cf::MESSAGES_BY_RECIPIENT` 时作为分区键。
    pub recipient_id: Ulid,
    /// 冗余存一份：列表 UI 直接显示空间名，不必回查 Entry。
    pub workspace_id: Ulid,
    pub entry_code: String,
    /// "entry" / "comment"。
    pub source_type: String,
    /// 评论 id；entry 来源时为 None。
    pub source_id: Option<Ulid>,
    /// 谁触发（@ 别人的人）。
    pub actor_id: Ulid,
    /// 冗余作者姓名：账号被注销 / 改名后消息列表仍能展示历史文案。
    pub actor_name: String,
    /// 从触发动作截取的预览（评论正文前若干字符 / Entry 标题），用于消息列表一行展示。
    pub preview: String,
    pub read: bool,
    pub created_at: DateTime<Utc>,
}

impl Message {
    pub fn new(
        recipient_id: Ulid,
        workspace_id: Ulid,
        entry_code: String,
        source_type: &str,
        source_id: Option<Ulid>,
        actor_id: Ulid,
        actor_name: String,
        preview: String,
    ) -> Self {
        Self {
            id: Ulid::new(),
            recipient_id,
            workspace_id,
            entry_code,
            source_type: source_type.to_string(),
            source_id,
            actor_id,
            actor_name,
            preview,
            read: false,
            created_at: Utc::now(),
        }
    }
}