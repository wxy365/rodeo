//! 二级索引键编码：固定长度的 Ulid 字节用于前缀扫描，字符串键用于唯一索引。

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
