//! Cursor 编解码 + sort_spec_hash 规范化。
//!
//! `sort_spec_hash` 用 fnv1a 64 —— 简单稳定、不引入新依赖。键序**不**重新排序
//! （用户的键序是有意义的），所以多键排序换顺序会被识别为不同的 sort、
//! cursor 失效。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json;

use crate::domain::cursor::{Cursor, CursorValue};
use crate::domain::view::{SortField, SortSpec};
use crate::error::AppError;

/// 把 SortSpec 规范化后 fnv1a 64。规范化：keys 按 field 名字典序排序后再序列化，
/// 这样 `{a, b}` 与 `{b, a}` 命中同一 hash —— 避免 UI 拖动列顺序就重置 cursor。
/// keys 的相对顺序不保留（与 spec §3.4 "SortKey 按 field 名字典序排序" 一致）。
pub fn sort_spec_hash(spec: &SortSpec) -> u64 {
    let mut keys = spec.keys.clone();
    keys.sort_by(|a, b| a.field.as_str().cmp(b.field.as_str()));
    let canon_spec = SortSpec { keys };
    let canon = serde_json::to_string(&canon_spec).unwrap_or_default();
    let mut h: u64 = 0xcbf29ce484222325;
    for b in canon.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

pub fn encode_cursor(c: &Cursor) -> String {
    let json = serde_json::to_vec(c).expect("Cursor JSON serialize");
    URL_SAFE_NO_PAD.encode(json)
}

pub fn decode_cursor(s: &str) -> Result<Cursor, AppError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s)
        .map_err(|_| AppError::InvalidQuery("cursor 解码失败".to_string()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| AppError::InvalidQuery("cursor 格式错误".to_string()))
}

/// 从 SortField + 当前 entry 取对应 CursorValue；用作 next_cursor 的 last_values 字段。
pub fn cursor_value_for_entry(
    field: &SortField,
    entry: &crate::domain::Entry,
    labels: &[crate::domain::Labeling],
) -> CursorValue {
    use crate::domain::label::LabelValue;
    match field {
        SortField::UpdatedAt | SortField::CreatedAt => {
            let ms = if matches!(field, SortField::UpdatedAt) {
                entry.updated_at.timestamp_millis()
            } else {
                entry.created_at.timestamp_millis()
            };
            CursorValue::TsMs(ms)
        }
        SortField::Title => CursorValue::Str(entry.title.clone()),
        SortField::Label(name) => labels
            .iter()
            .find(|l| &l.label_name == name)
            .map(|l| match &l.value {
                LabelValue::Int(i) => CursorValue::TsMs(*i),
                LabelValue::Float(f) => CursorValue::TsMs(*f as i64),
                LabelValue::String(s) => CursorValue::Bytes(s.as_bytes().to_vec()),
                _ => CursorValue::Bytes(serde_json::to_vec(&l.value.to_json()).unwrap_or_default()),
            })
            .unwrap_or(CursorValue::Bytes(Vec::new())),
        SortField::CreatedBy | SortField::UpdatedBy => {
            // 本轮 #2 不索引 CreatedBy / UpdatedBy。理论上不应被调用。
            CursorValue::Bytes(Vec::new())
        }
    }
}
