use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub code: String,
    pub workspace_id: Ulid,
    pub title: String,
    pub detail: String,
    pub created_by: Ulid,
    pub updated_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Entry {
    pub fn new(workspace_id: Ulid, title: String, actor: Ulid) -> Self {
        let now = Utc::now();
        Self {
            code: generate_entry_code(),
            workspace_id,
            title,
            detail: String::new(),
            created_by: actor,
            updated_by: actor,
            created_at: now,
            updated_at: now,
        }
    }
}

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// 生成 16 字符全局唯一编码：Ulid(128bit) → base62 → 左补 0 到 16 位，超长取最高位。
pub fn generate_entry_code() -> String {
    let id = Ulid::new();
    base62(id.to_bytes())
}

fn base62(mut bytes: [u8; 16]) -> String {
    let mut digits: Vec<u8> = Vec::with_capacity(22);
    while bytes.iter().any(|&b| b != 0) {
        let mut rem: u32 = 0;
        for byte in bytes.iter_mut() {
            let cur = rem * 256 + *byte as u32;
            *byte = (cur / 62) as u8;
            rem = cur % 62;
        }
        digits.push(BASE62[rem as usize]);
    }
    digits.reverse();
    if digits.is_empty() {
        digits.push(b'0');
    }
    // 左补 0 到 16 位，超长截取最高 16 位（保留 Ulid 时间有序的高位）。
    while digits.len() < 16 {
        digits.insert(0, b'0');
    }
    digits.truncate(16);
    String::from_utf8(digits).expect("base62 output is always ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn code_is_16_chars_from_base62_alphabet() {
        for _ in 0..1000 {
            let code = generate_entry_code();
            assert_eq!(code.len(), 16, "code length must be 16: {code}");
            assert!(
                code.bytes().all(|b| b.is_ascii_alphanumeric()),
                "code must be alphanumeric: {code}"
            );
        }
    }

    #[test]
    fn codes_are_unique() {
        let mut set = HashSet::new();
        for _ in 0..10_000 {
            assert!(set.insert(generate_entry_code()), "duplicate code generated");
        }
    }
}
