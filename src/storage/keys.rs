//! 二级索引键编码：固定长度的 Ulid 字节用于前缀扫描，字符串键用于唯一索引。

use chrono::{DateTime, Utc};
use ulid::Ulid;

pub fn ulid_bytes(id: Ulid) -> [u8; 16] {
    id.to_bytes()
}

/// (workspace_id, account_id) 复合键，32 字节。
pub fn member_key(workspace_id: Ulid, account_id: Ulid) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&workspace_id.to_bytes());
    key[16..].copy_from_slice(&account_id.to_bytes());
    key
}

/// (account_id, workspace_id) 反向索引，32 字节。
pub fn member_by_account_key(account_id: Ulid, workspace_id: Ulid) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&account_id.to_bytes());
    key[16..].copy_from_slice(&workspace_id.to_bytes());
    key
}

/// (workspace_id, account_id) 复合键，32 字节。邀请与成员关系同构、键布局一致，
/// 但落在不同列族，故另起名字，免得读者以为两者可以互查。
pub fn invite_key(workspace_id: Ulid, account_id: Ulid) -> [u8; 32] {
    member_key(workspace_id, account_id)
}

/// (account_id, workspace_id) 反向索引，32 字节。
pub fn invite_by_account_key(account_id: Ulid, workspace_id: Ulid) -> [u8; 32] {
    member_by_account_key(account_id, workspace_id)
}

/// (workspace_id, entry_code) 复合键，16 + 16 字节。
pub fn entry_by_workspace_key(workspace_id: Ulid, code: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(32);
    key.extend_from_slice(&workspace_id.to_bytes());
    key.extend_from_slice(code.as_bytes());
    key
}

/// (workspace_id, label_name) 复合键。
pub fn label_schema_key(workspace_id: Ulid, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + name.len());
    key.extend_from_slice(&workspace_id.to_bytes());
    key.extend_from_slice(name.as_bytes());
    key
}

/// (entry_code, label_name) 复合键。
pub fn labeling_key(code: &str, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(code.len() + name.len());
    key.extend_from_slice(code.as_bytes());
    key.extend_from_slice(name.as_bytes());
    key
}

/// (entry_code, comment_id) 复合键，16 + 16 字节。
/// `comment_id` 是 ULID，字节序即时间序，因此按 entry_code 前缀扫描
/// 天然得到按发表时间升序的评论列表，不需要额外的排序字段。
pub fn comment_key(entry_code: &str, comment_id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(entry_code.len() + 16);
    key.extend_from_slice(entry_code.as_bytes());
    key.extend_from_slice(&comment_id.to_bytes());
    key
}

/// (recipient_id, 时间倒序, id) 复合键。recipient_id 前缀扫描做分区，
/// 时间倒序让最新消息在前。键总长 = 16 + 8 + 16 = 40 字节。
pub fn message_key(recipient_id: Ulid, at: chrono::DateTime<chrono::Utc>, id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(40);
    key.extend_from_slice(&recipient_id.to_bytes());
    let desc = i64::MAX - at.timestamp_millis();
    key.extend_from_slice(&desc.to_be_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}

/// (entry_code, attachment_id) 复合键。`attachment_id` 是 ULID，字节序即时间序，
/// 因此按 entry_code 前缀扫描天然得到按上传时间升序的附件列表——与 `comment_key` 同构。
pub fn attachment_by_entry_key(entry_code: &str, attachment_id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(entry_code.len() + 16);
    key.extend_from_slice(entry_code.as_bytes());
    key.extend_from_slice(&attachment_id.to_bytes());
    key
}

/// 审计主键：(时间倒序, id)。i64::MAX - millis 实现降序，前缀扫描最新在前。
pub fn audit_log_key(at: DateTime<Utc>, id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(24);
    let desc = i64::MAX - at.timestamp_millis();
    key.extend_from_slice(&desc.to_be_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}

/// (workspace_id, 时间倒序, id)：按 workspace 前缀扫描，最新在前。
pub fn audit_by_workspace_key(workspace_id: Ulid, at: DateTime<Utc>, id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(40);
    key.extend_from_slice(&workspace_id.to_bytes());
    let desc = i64::MAX - at.timestamp_millis();
    key.extend_from_slice(&desc.to_be_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}

/// (resource_type \0 resource_id \0 时间倒序, id)：按资源前缀扫描。
pub fn audit_by_resource_key(
    resource_type: &str,
    resource_id: &str,
    at: DateTime<Utc>,
    id: Ulid,
) -> Vec<u8> {
    let mut key = Vec::new();
    key.extend_from_slice(resource_type.as_bytes());
    key.push(0);
    key.extend_from_slice(resource_id.as_bytes());
    key.push(0);
    let desc = i64::MAX - at.timestamp_millis();
    key.extend_from_slice(&desc.to_be_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}

/// 视图主键：16 字节 ulid。
pub fn view_key(id: Ulid) -> [u8; 16] {
    id.to_bytes()
}

/// 基础视图指针：每个 workspace 一条，键即 workspace_id（16 字节），值为 view_id 的 16 字节。
pub fn default_view_key(workspace_id: Ulid) -> [u8; 16] {
    workspace_id.to_bytes()
}

/// (workspace_id, view_id) 复合键，32 字节。
pub fn view_by_workspace_key(workspace_id: Ulid, id: Ulid) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&workspace_id.to_bytes());
    key[16..].copy_from_slice(&id.to_bytes());
    key
}

/// 规则主键：16 字节 ulid。
pub fn rule_key(id: Ulid) -> [u8; 16] {
    id.to_bytes()
}

/// (workspace_id, rule_id) 复合键，32 字节。
pub fn rule_by_workspace_key(workspace_id: Ulid, id: Ulid) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[..16].copy_from_slice(&workspace_id.to_bytes());
    key[16..].copy_from_slice(&id.to_bytes());
    key
}

/// (workspace_id, entry_code, label_name) 复合键，前缀扫描取整个 workspace 的打标。
pub fn labeling_by_workspace_key(workspace_id: Ulid, code: &str, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + code.len() + name.len());
    key.extend_from_slice(&workspace_id.to_bytes());
    key.extend_from_slice(code.as_bytes());
    key.extend_from_slice(name.as_bytes());
    key
}

/// 标签值二级索引键：`workspace_id ‖ label_name ‖ 0x00 ‖ encoded_value ‖ entry_code`。
/// `label_name` 是不定长字符串，中间用 `\x00` 作分隔——它不会出现在合法名字里，
/// 这样 `(ws, name)` 前缀下所有 value 按字典序连成一段，可直接 scan_prefix 取值集合。
/// `encoded_value` 来自 `crate::domain::label::encode_label_value_for_index`，
/// 字节序与领域序一致以支持范围扫描；列表类型每个元素各占一条记录。
pub fn labeling_by_label_value_key(
    workspace_id: Ulid,
    label_name: &str,
    encoded_value: &[u8],
    code: &str,
) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + label_name.len() + 1 + encoded_value.len() + code.len());
    key.extend_from_slice(&workspace_id.to_bytes());
    key.extend_from_slice(label_name.as_bytes());
    key.push(0);
    key.extend_from_slice(encoded_value);
    key.extend_from_slice(code.as_bytes());
    key
}

/// 给定 `(ws, label_name)` 前缀扫描所需的字节：workspace_id + 名字 + 分隔符。
/// 真正的 value 边界在调用方按 `encode_label_value_for_index` 拼上。
pub fn labeling_by_label_prefix(workspace_id: Ulid, label_name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + label_name.len() + 1);
    key.extend_from_slice(&workspace_id.to_bytes());
    key.extend_from_slice(label_name.as_bytes());
    key.push(0);
    key
}

/// Agent 会话。user 键 < ws 键 < session 键：所有读都强制走 user 前缀，
/// 跨用户读取不到任何 session（不存在性也不暴露）。
pub fn agent_session_key(user_id: Ulid, workspace_id: Ulid, id: Ulid) -> Vec<u8> {
    let mut out = Vec::with_capacity(48);
    out.extend_from_slice(&user_id.to_bytes());
    out.extend_from_slice(&workspace_id.to_bytes());
    out.extend_from_slice(&id.to_bytes());
    out
}

pub fn agent_session_prefix(user_id: Ulid) -> [u8; 16] {
    user_id.to_bytes()
}

pub fn agent_message_key(session_id: Ulid, message_id: Ulid) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[..16].copy_from_slice(&session_id.to_bytes());
    out[16..].copy_from_slice(&message_id.to_bytes());
    out
}

pub fn agent_message_prefix(session_id: Ulid) -> [u8; 16] {
    session_id.to_bytes()
}

/// 关联主键：(workspace_id, relation_id) → Relation。前 16 字节即工作空间
/// 前缀，扫「某 workspace 全部关联」用 `relation_prefix(workspace_id)`。
pub fn relation_key(workspace_id: Ulid, relation_id: Ulid) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    out.extend_from_slice(&workspace_id.to_bytes());
    out.extend_from_slice(&relation_id.to_bytes());
    out
}

/// 工作空间前缀：扫整 workspace 的全部关联。
pub fn relation_prefix(workspace_id: Ulid) -> [u8; 16] {
    workspace_id.to_bytes()
}

/// FROM 索引键：(workspace_id, from_code, relation_id)。from_code 是不定长
/// 字符串，前缀扫描按 entry_code 收窄到单个条目。
pub fn relation_by_from_key(workspace_id: Ulid, from_code: &str, relation_id: Ulid) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + from_code.len() + 16);
    out.extend_from_slice(&workspace_id.to_bytes());
    out.extend_from_slice(from_code.as_bytes());
    out.extend_from_slice(&relation_id.to_bytes());
    out
}

/// FROM 索引的前缀：`(workspace_id, from_code)`，扫「来自此条目的关联」用。
pub fn relation_by_from_prefix(workspace_id: Ulid, from_code: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + from_code.len());
    out.extend_from_slice(&workspace_id.to_bytes());
    out.extend_from_slice(from_code.as_bytes());
    out
}

/// TO 索引键：(workspace_id, to_code, relation_id)。
pub fn relation_by_to_key(workspace_id: Ulid, to_code: &str, relation_id: Ulid) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + to_code.len() + 16);
    out.extend_from_slice(&workspace_id.to_bytes());
    out.extend_from_slice(to_code.as_bytes());
    out.extend_from_slice(&relation_id.to_bytes());
    out
}

/// TO 索引的前缀：`(workspace_id, to_code)`，扫「指向此条目的关联」用。
pub fn relation_by_to_prefix(workspace_id: Ulid, to_code: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + to_code.len());
    out.extend_from_slice(&workspace_id.to_bytes());
    out.extend_from_slice(to_code.as_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_keys_encode_prefixes() {
        let ws = ulid::Ulid::new();
        let id = ulid::Ulid::new();
        assert_eq!(view_key(id).len(), 16);
        let k = view_by_workspace_key(ws, id);
        assert_eq!(k.len(), 32);
        assert!(k.starts_with(&ws.to_bytes()));

        let lk = labeling_by_workspace_key(ws, "CODE0001", "Task");
        assert!(lk.starts_with(&ws.to_bytes()));
        assert_eq!(&lk[16..], b"CODE0001Task");
    }

    #[test]
    fn label_value_index_key_layout() {
        let ws = ulid::Ulid::new();
        let key = labeling_by_label_value_key(ws, "Task", b"\x05Open", "CODE0000000000001");
        let prefix = labeling_by_label_prefix(ws, "Task");
        assert!(key.starts_with(&prefix));
        let after = &key[prefix.len()..];
        assert_eq!(&after[..5], b"\x05Open");
        assert_eq!(&after[5..], b"CODE0000000000001");
    }
}
