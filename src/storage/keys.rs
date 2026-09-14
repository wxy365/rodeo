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
pub fn audit_by_resource_key(resource_type: &str, resource_id: &str, at: DateTime<Utc>, id: Ulid) -> Vec<u8> {
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

/// 默认视图指针：每个 workspace 一条，键即 workspace_id（16 字节），值为 view_id 的 16 字节。
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

/// (workspace_id, entry_code, label_name) 复合键，前缀扫描取整个 workspace 的打标。
pub fn labeling_by_workspace_key(workspace_id: Ulid, code: &str, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(16 + code.len() + name.len());
    key.extend_from_slice(&workspace_id.to_bytes());
    key.extend_from_slice(code.as_bytes());
    key.extend_from_slice(name.as_bytes());
    key
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
}
