//! Opaque pagination cursor. Encoded as base64(JSON) — clients MUST NOT parse it.
//!
//! Carries workspace id (replay protection) and a hash of the SortSpec used
//! when the cursor was minted. If the current request's sort hash differs,
//! the cursor is invalid: spec changed, reset pagination.

use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// One sort-key value at the cursor position. Encoding chosen to match the
/// index key bytes so `next_position` can build the resume key directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "k", content = "v")]
pub enum CursorValue {
    /// i64 毫秒（updated_at / created_at / 数值类标签排序）
    TsMs(i64),
    /// 标签值经 encode_label_value_for_index 编码后的字节
    Bytes(Vec<u8>),
    /// 字符串（title 等）
    Str(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// workspace id —— resolver 必须校验 cursor.ws == 入参 ws。
    pub ws: Ulid,
    /// SortSpec 的规范化哈希。
    pub sort_hash: u64,
    /// 各级排序键的最后看到值，长度等于 SortSpec.keys.len()。
    /// Phase 1 仅首键（多键排序的二级键留 Phase 2）。
    pub last_values: Vec<CursorValue>,
    /// tiebreaker：在 (sort_values) 相同时按 code 升序推进。
    pub last_code: String,
    /// 上一轮 emit 的最后一条 entry 的完整索引 key（in 主索引 CF 的原样字节）。
    /// 用于 cursor 续扫时的精确 resume：desc 排序下 end=last_key；asc 下
    /// start=last_key 之后下一字节（实际不需要：本轮只支持 desc 默认排序）。
    #[serde(default)]
    pub last_key: Vec<u8>,
    /// 第一版只走 forward（next 方向）；反向预留但 GraphQL 不暴露。
    #[serde(default)]
    pub backward: bool,
}
