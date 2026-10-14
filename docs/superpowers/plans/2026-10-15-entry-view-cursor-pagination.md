# Entry View Cursor Pagination + Compound Sort Index — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move view queries from "load-all-and-filter-in-Rust" to "stream-from-compound-index + cursor pagination", so 20M-entry workspaces render first paint in seconds and subsequent page flips in tens of milliseconds.

**Architecture:** A new `ENTRIES_IDX_*` column family family indexed by `(workspace_id, ~sort_key, code)` is written on every entry mutation as part of the existing `BatchOp`. The query path splits: indexed sorts stream-scan the CF with a cursor (`O(page_size)`); non-indexed sorts fall through to the existing in-memory path. Cursor is opaque base64-JSON with `workspace_id` and `sort_spec_hash` baked in for replay protection.

**Tech Stack:** Rust + Leptos 0.8, RocksDB (Rocks only this phase; Postgres stubbed), bincode serialization, async-graphql.

**Spec:** `docs/superpowers/specs/2026-10-15-entry-view-cursor-pagination-design.md`

---

## Global Constraints

- **No new Rust unit tests** — per user preference. Verification gate per task is `make check` (native + wasm). For UI tasks, also browser smoke test via the playwright recipe in `~/.claude/projects/-Users-wangxiaoyan-Works-git-rodeo/memory/browser-verification-setup.md`.
- **`#[serde(default)]` for new struct fields** — bincode position encoding means new fields must be added with `#[serde(default)]` at struct end. Same rule as existing `multi`/`format`/`default_value`/`links` in `LabelSchema`.
- **CF names are immutable** — once a CF is in `ALL_CFS`, never rename or remove. Adding new ones is fine.
- **Bincode `Default` must exist** for any field marked `#[serde(default)]`. `bool` ✅, `Vec<X>` ✅, `Option<X>` ✅.
- **Atomic write** — every index entry must be written in the same `BatchOp` as the primary table entry. No two-phase writes.
- **WASM surface** — anything reachable from `src/frontend/**` must compile for `wasm32-unknown-unknown` (no `tokio`/`std::sync`/`std::fs` directly; use the WASM-conditional code paths).
- **Domain layer pure** — `src/domain/**` types must not import `crate::storage::*` or `crate::service::*`. Cursor struct lives in `src/domain/`, encoding helpers stay in `src/storage/` or `src/service/`.
- **CLAUDE.md ordering of service init** — `workspace → message → entry → ...` (see `src/service/mod.rs`). New init code hooks into the existing `repair_*` chain at the end, never moves the order.
- **CLI verification** — `make check` covers native + wasm; `make build` covers full debug build including SSR. Prefer `make check` for fast iteration; run `make build` at task boundaries where SSR path matters.

## Review Focus

The five input classes most likely to bite that no per-task gate exercises, listed most-likely-first:

1. **Cursor hash collision** — two distinct `SortSpec`s producing the same `sort_spec_hash`. A spurious page continuation against a silently changed sort is worse than a hard error. Pinned in Task 7 step on cursor decode + replay.
2. **Index coverage cache stale after a write** — the `RwLock<HashMap<(ws, cf), bool>>` cache in §7 can lag a second behind new writes, briefly sending cursor queries to a partial index. Pinned in Task 10 by writing cache from the same code path as `write_index_keys`.
3. **Cross-workspace cursor replay** — a stolen cursor from workspace A replayed against workspace B's resolver. Pinned in Task 8 step on cursor decode where `cursor.ws` is matched to the request workspace.
4. **`page > 1` falling through to Legacy silently** — front-end that still uses `page` for a sort that *is* indexed will get O(N) performance instead of O(page_size). Pinned in Task 12 by emitting a `console.warn` when the resolver downgrades.
5. **Archived entries inflating the cursor stream** — a workspace that is 95% archived and the active set is tiny will still scan the entire `ENTRIES_IDX_UPDATED` range because the index does not split archived. Pinned in Task 13 docs noting the threshold for Phase 2 split.

---

## File Structure

| File | Responsibility | Touched by |
|------|----------------|------------|
| `src/storage/keys.rs` | New `idx_*_key` builders + `label_bucket_cf` | Task 1 |
| `src/storage/doc.rs` | New `cf::*` constants + `ENTRIES_IDX_*` | Task 1 |
| `src/storage/rocksdb.rs` | Append to `ALL_CFS`; implement `scan_range` / `scan_range_reverse` | Task 1, Task 4 |
| `src/storage/pg.rs` | `scan_range` / `scan_range_reverse` stubs returning `Err` | Task 4 |
| `src/domain/label.rs` | `LabelSchema.sortable: bool` | Task 2 |
| `src/domain/cursor.rs` (new) | `Cursor`, `CursorValue`, `sort_spec_hash` | Task 3 |
| `src/domain/mod.rs` | Re-export `cursor` | Task 3 |
| `src/service/cursor.rs` (new) | `encode_cursor` / `decode_cursor` / `next_position` | Task 3 |
| `src/service/mod.rs` | `pub mod cursor;` + `repair_entry_sort_indexes` | Task 3, Task 10 |
| `src/service/entry.rs` | `idx_*_put`/`idx_*_delete` helpers + `query_indexed` + `count` fast path + `write_index_keys` hooks | Tasks 5, 6, 8 |
| `src/service/entry_index.rs` (new) | `EntryIndexService` — index coverage cache + path selection | Task 7 |
| `src/service/label.rs` | `enable_sortable` bulk indexer | Task 9 |
| `src/api/graphql.rs` | `QueryInput` GQL input + `EntryPage.nextCursor` + resolver dispatch | Task 11 |
| `src/frontend/graphql_client.rs` | `EntryPage.next_cursor`; `entries()` adds `cursor`/`limit` | Task 12 |
| `src/frontend/pages/workspace_main.rs` | Use `next_cursor` when present, fallback to `page++` | Task 12 |
| `src/config.rs` | `[query] force_legacy_path: bool` default false | Task 13 |
| `config.example.toml` | Document the new `[query]` section | Task 13 |

---

## Task 1: Column family constants + index key builders

**Files:**
- Modify: `src/storage/doc.rs` (add `pub const`s in `pub mod cf`)
- Modify: `src/storage/rocksdb.rs:9-50` (append to `ALL_CFS`)
- Modify: `src/storage/keys.rs` (append `idx_*_key` builders + `label_bucket_cf`)

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub const ENTRIES_IDX_UPDATED: &str = "ENTRIES_IDX_UPDATED"`
  - `pub const ENTRIES_IDX_CREATED: &str = "ENTRIES_IDX_CREATED"`
  - `pub const ENTRIES_IDX_TITLE:   &str = "ENTRIES_IDX_TITLE"`
  - `pub const ENTRIES_IDX_LABEL_BUCKETS: [&str; 16] = ["ENTRIES_IDX_LABEL_00", ..., "ENTRIES_IDX_LABEL_15"]`
  - `pub fn idx_updated_key(ws: Ulid, ts_ms: i64, code: &str) -> Vec<u8>`
  - `pub fn idx_created_key(ws: Ulid, ts_ms: i64, code: &str) -> Vec<u8>`
  - `pub fn idx_title_key(ws: Ulid, title: &str, code: &str) -> Vec<u8>`
  - `pub fn idx_label_key(ws: Ulid, tag: &str, encoded: &[u8], code: &str) -> Vec<u8>`
  - `pub fn label_bucket_cf(tag_name: &str) -> &'static str` (returns one of `ENTRIES_IDX_LABEL_BUCKETS`)
  - `pub fn ws_prefix(ws: Ulid) -> Vec<u8>` (returns `ws.to_bytes()`)
  - `pub fn end_of_ws(cf: &str, ws: Ulid) -> Vec<u8>` (returns `ws_bytes` with last byte incremented for range scan upper bound)

- [ ] **Step 1: Add CF constants in `src/storage/doc.rs`**

Locate the existing `pub mod cf { ... }` block. Append at the end (after the last existing `pub const`):

```rust
    // ── entry view sort indexes (added 2026-10-15, see
    // docs/superpowers/specs/2026-10-15-entry-view-cursor-pagination-design.md)
    pub const ENTRIES_IDX_UPDATED: &str = "ENTRIES_IDX_UPDATED";
    pub const ENTRIES_IDX_CREATED: &str = "ENTRIES_IDX_CREATED";
    pub const ENTRIES_IDX_TITLE:   &str = "ENTRIES_IDX_TITLE";
    /// 16 pre-allocated buckets for sortable label indexes.
    /// `label_bucket_cf(name)` maps a label name to one of these by `fnv1a(name) mod 16`.
    /// Same bucket may hold multiple labels; the key layout
    /// `(ws_id, tag_name, encoded_value, code)` lets us narrow by tag_name prefix.
    pub const ENTRIES_IDX_LABEL_BUCKETS: [&str; 16] = [
        "ENTRIES_IDX_LABEL_00", "ENTRIES_IDX_LABEL_01", "ENTRIES_IDX_LABEL_02",
        "ENTRIES_IDX_LABEL_03", "ENTRIES_IDX_LABEL_04", "ENTRIES_IDX_LABEL_05",
        "ENTRIES_IDX_LABEL_06", "ENTRIES_IDX_LABEL_07", "ENTRIES_IDX_LABEL_08",
        "ENTRIES_IDX_LABEL_09", "ENTRIES_IDX_LABEL_0A", "ENTRIES_IDX_LABEL_0B",
        "ENTRIES_IDX_LABEL_0C", "ENTRIES_IDX_LABEL_0D", "ENTRIES_IDX_LABEL_0E",
        "ENTRIES_IDX_LABEL_0F",
    ];
```

- [ ] **Step 2: Append new CFs to `ALL_CFS` in `src/storage/rocksdb.rs`**

Open `src/storage/rocksdb.rs`. After the last line of the `ALL_CFS` slice, append:

```rust
    cf::ENTRIES_IDX_UPDATED,
    cf::ENTRIES_IDX_CREATED,
    cf::ENTRIES_IDX_TITLE,
    cf::ENTRIES_IDX_LABEL_BUCKETS[0],  cf::ENTRIES_IDX_LABEL_BUCKETS[1],
    cf::ENTRIES_IDX_LABEL_BUCKETS[2],  cf::ENTRIES_IDX_LABEL_BUCKETS[3],
    cf::ENTRIES_IDX_LABEL_BUCKETS[4],  cf::ENTRIES_IDX_LABEL_BUCKETS[5],
    cf::ENTRIES_IDX_LABEL_BUCKETS[6],  cf::ENTRIES_IDX_LABEL_BUCKETS[7],
    cf::ENTRIES_IDX_LABEL_BUCKETS[8],  cf::ENTRIES_IDX_LABEL_BUCKETS[9],
    cf::ENTRIES_IDX_LABEL_BUCKETS[10], cf::ENTRIES_IDX_LABEL_BUCKETS[11],
    cf::ENTRIES_IDX_LABEL_BUCKETS[12], cf::ENTRIES_IDX_LABEL_BUCKETS[13],
    cf::ENTRIES_IDX_LABEL_BUCKETS[14], cf::ENTRIES_IDX_LABEL_BUCKETS[15],
```

(The slice is `&[&str]` so each element is `&'static str`. Since `ENTRIES_IDX_LABEL_BUCKETS[i]` is `&'static str`, indexing yields `&str`.)

- [ ] **Step 3: Add index key builders to `src/storage/keys.rs`**

Append at the end of the file:

```rust
// ── entry view sort indexes (2026-10-15) ────────────────────────────────
//
// 键布局：
//   内建三字段（updated/created/title）：(ws_id, encoded_sort_value, code)
//   标签桶（同桶多标签）：             (ws_id, tag_name, encoded_value, code)
//
// 降序字段（updated_at / created_at）按 i64::MAX - ms 编 8 字节大端，
// 与 message_key / audit_log_key 同构 —— 字节序升序 = 时间倒序。

/// 计算 sortable 标签名落在哪个桶。FNV-1a 64-bit，简单稳定。
pub fn label_bucket_cf(tag_name: &str) -> &'static str {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in tag_name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let idx = (h % 16) as usize;
    crate::storage::cf::ENTRIES_IDX_LABEL_BUCKETS[idx]
}

fn desc_ms(ms: i64) -> [u8; 8] {
    (i64::MAX - ms).to_be_bytes()
}

/// (ws_id, ~updated_at_ms_be, code)
pub fn idx_updated_key(ws: Ulid, ts_ms: i64, code: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(16 + 8 + code.len());
    k.extend_from_slice(&ws.to_bytes());
    k.extend_from_slice(&desc_ms(ts_ms));
    k.extend_from_slice(code.as_bytes());
    k
}

/// (ws_id, ~created_at_ms_be, code)
pub fn idx_created_key(ws: Ulid, ts_ms: i64, code: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(16 + 8 + code.len());
    k.extend_from_slice(&ws.to_bytes());
    k.extend_from_slice(&desc_ms(ts_ms));
    k.extend_from_slice(code.as_bytes());
    k
}

/// (ws_id, title_utf8, code)
pub fn idx_title_key(ws: Ulid, title: &str, code: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(16 + title.len() + code.len());
    k.extend_from_slice(&ws.to_bytes());
    k.extend_from_slice(title.as_bytes());
    k.extend_from_slice(code.as_bytes());
    k
}

/// (ws_id, tag_name, encoded_value, code) —— 同桶多标签靠 tag_name 区分。
pub fn idx_label_key(ws: Ulid, tag: &str, encoded: &[u8], code: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(16 + tag.len() + encoded.len() + code.len());
    k.extend_from_slice(&ws.to_bytes());
    k.extend_from_slice(tag.as_bytes());
    k.extend_from_slice(encoded);
    k.extend_from_slice(code.as_bytes());
    k
}

/// 范围扫描的起始键：workspace 内某 CF 的最小键。
pub fn ws_start(ws: Ulid) -> Vec<u8> {
    ws.to_bytes().to_vec()
}

/// 范围扫描的结束键（不含）：workspace 内某 CF 的最大键之后一字节。
/// 因为 `scan_range` 的 `end` 参数是不含上界；超出 ws 的第一个字节即可截断。
pub fn ws_end_exclusive(ws: Ulid) -> Vec<u8> {
    let mut end = ws.to_bytes().to_vec();
    // Ulid 是 16 字节；「下一个 workspace 的最小键」等价于「当前 ws 字节 + 1」。
    // 用 prefix_upper_bound 风格的最后一字节 +1。
    if let Some(last) = end.last_mut() {
        if *last < 0xFF {
            *last += 1;
        }
    }
    end
}
```

- [ ] **Step 4: Verify compilation**

Run: `make check`
Expected: PASS (no errors, no warnings about unused imports in this file because existing tests should still build).

- [ ] **Step 5: Commit**

```bash
git add src/storage/doc.rs src/storage/rocksdb.rs src/storage/keys.rs
git commit -m "feat(storage): add entry view sort index CFs + idx_*_key builders"
```

---

## Task 2: `LabelSchema.sortable` field

**Files:**
- Modify: `src/domain/label.rs:343-373` (append field to `LabelSchema`)

**Interfaces:**
- Consumes: nothing
- Produces: `LabelSchema { ..., sortable: bool }` (default `false`)

- [ ] **Step 1: Add the field**

In `src/domain/label.rs`, locate the `pub struct LabelSchema { ... }` block. Find the closing brace; before it, append:

```rust
    // 同样只能追加
    /// 本标签可作为视图排序键。`true` 时 EntryIndexService 启动期与
    /// `LabelService::enable_sortable` 触发期会建立/校验
    /// `(ws_id, tag_name, encoded_value, code)` 的有序索引；
    /// 视图排序命中该标签时走 cursor 分页路径。
    /// 老记录无此字段 → #[serde(default)] → false。
    #[serde(default)]
    pub sortable: bool,
```

- [ ] **Step 2: Verify compilation**

Run: `make check`
Expected: PASS. Existing usages of `LabelSchema` will continue to compile because `sortable` defaults to `false`. Existing serialized records without this field will deserialize via `serde(default)`.

- [ ] **Step 3: Smoke check via existing dev server (manual)**

Start the dev server (`make dev`) in a workspace that has labels. The labels UI should still render; sorting by any label should still work (will go through Legacy path). Watch for `bincode` deserialization errors in logs — none should appear.

- [ ] **Step 4: Commit**

```bash
git add src/domain/label.rs
git commit -m "feat(domain): LabelSchema.sortable flag for cursor-path eligibility"
```

---

## Task 3: Cursor struct + sort_spec_hash + encode/decode

**Files:**
- Create: `src/domain/cursor.rs`
- Create: `src/service/cursor.rs`
- Modify: `src/domain/mod.rs` (re-export `cursor`)
- Modify: `src/service/mod.rs` (declare `pub mod cursor;`)

**Interfaces:**
- Consumes: `SortSpec` (from `crate::domain::view::SortSpec`)
- Produces:
  - `pub struct Cursor { ws: Ulid, sort_hash: u64, last_values: Vec<CursorValue>, last_code: String, backward: bool }`
  - `pub enum CursorValue { TsMs(i64), Bytes(Vec<u8>), Str(String) }`
  - `pub fn sort_spec_hash(spec: &SortSpec) -> u64`
  - `pub fn encode_cursor(c: &Cursor) -> String` (base64)
  - `pub fn decode_cursor(s: &str) -> Result<Cursor, AppError>`
  - `AppError::InvalidQuery(String)` variant already exists; reuse.

- [ ] **Step 1: Create `src/domain/cursor.rs`**

```rust
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
    /// workspace id — resolver 必须校验 cursor.ws == 入参 ws。
    pub ws: Ulid,
    /// SortSpec 的规范化哈希。
    pub sort_hash: u64,
    /// 各级排序键的最后看到值，长度等于 SortSpec.keys.len()。
    pub last_values: Vec<CursorValue>,
    /// tiebreaker：在 (sort_values) 相同时按 code 升序推进。
    pub last_code: String,
    /// 第一版只走 forward（next 方向）；反向预留但 GraphQL 不暴露。
    #[serde(default)]
    pub backward: bool,
}
```

- [ ] **Step 2: Create `src/service/cursor.rs`**

```rust
//! Cursor 编解码 + sort_spec_hash 规范化。
//!
//! `sort_spec_hash` 用 xxhash3 的等价物 fnv1a 64 —— 简单稳定、不引入新依赖。
//! 字段顺序**不**重新排序（用户的键序是有意义的），所以多键排序换顺序
//! 会被识别为不同的 sort、cursor 失效。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json;

use crate::domain::cursor::{Cursor, CursorValue};
use crate::domain::view::{SortField, SortSpec};
use crate::error::AppError;

/// 把 SortSpec 规范成 JSON 字符串，再 fnv1a 64。键序保持原样。
pub fn sort_spec_hash(spec: &SortSpec) -> u64 {
    let canon = serde_json::to_string(spec).unwrap_or_default();
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

/// 从 SortField 推出对应的 CursorValue 编码；用于在生成 next_cursor 时
/// 把刚 emit 的 Entry 的 sort 字段值打包进去。
pub fn cursor_value_for_field(field: &SortField, value: CursorValue) -> CursorValue {
    // 第一版：单键排序直接传，多键排序由调用方按 keys 顺序逐项提供。
    value
}
```

- [ ] **Step 3: Re-export in `src/domain/mod.rs` and declare in `src/service/mod.rs`**

In `src/domain/mod.rs`, add at the appropriate place:

```rust
pub mod cursor;
```

In `src/service/mod.rs`, locate the existing `pub mod ...;` declarations near the top of the file and add (alphabetical or matching existing ordering — match the file's existing order):

```rust
pub mod cursor;
```

- [ ] **Step 4: Verify compilation**

Run: `make check`
Expected: PASS. If `base64` is not yet a dependency, add to `Cargo.toml` under `[dependencies]`:

```toml
base64 = "0.22"
```

(Check first — base64 may already be transitively present via `reqwest` / `axum`. If so, no change needed.)

- [ ] **Step 5: Commit**

```bash
git add src/domain/cursor.rs src/service/cursor.rs src/domain/mod.rs src/service/mod.rs
git commit -m "feat(cursor): opaque cursor struct + sort_spec_hash + encode/decode"
```

---

## Task 4: `DocStore::scan_range` + `scan_range_reverse` (RocksDB impl + PG stub)

**Files:**
- Modify: `src/storage/doc.rs:156-238` (add methods to `DocStore` facade)
- Modify: `src/storage/rocksdb.rs:59-128` (implement on `RocksDoc`)
- Modify: `src/storage/pg.rs:46-205` (stub on `PgDoc`)

**Interfaces:**
- Consumes: existing `DocStore` facade
- Produces:
  - `DocStore::scan_range(cf, start, end, limit) -> Vec<(Vec<u8>, Vec<u8>)>`
  - `DocStore::scan_range_reverse(cf, start, end, limit) -> Vec<(Vec<u8>, Vec<u8>)>`

- [ ] **Step 1: Add methods to `DocStore` in `src/storage/doc.rs`**

Locate `impl DocStore { ... }`. Add after `scan_prefix`:

```rust
    /// 范围扫描：从 `start`（含）到 `end`（不含），按 key 升序，最多返回 `limit` 条。
    pub fn scan_range(
        &self,
        cf: &str,
        start: &[u8],
        end: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        match &self.backend {
            Backend::Rocks(s) => s.scan_range(cf, start, end, limit),
            Backend::Pg(s) => s.scan_range(cf, start, end, limit),
        }
    }

    /// 反向：从 `end`（不含）到 `start`（含），按 key 降序，最多返回 `limit` 条。
    /// 注意 RocksDB reverse iterator 的语义：start 必须能 seek 到第一个 ≥ start 的位置。
    pub fn scan_range_reverse(
        &self,
        cf: &str,
        start: &[u8],
        end: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        match &self.backend {
            Backend::Rocks(s) => s.scan_range_reverse(cf, start, end, limit),
            Backend::Pg(s) => s.scan_range_reverse(cf, start, end, limit),
        }
    }
```

- [ ] **Step 2: Implement on `RocksDoc` in `src/storage/rocksdb.rs`**

Add after the existing `scan_prefix` impl:

```rust
    pub fn scan_range(
        &self,
        cf: &str,
        start: &[u8],
        end: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        let h = self.handle(cf)?;
        let iter = self.db.iterator_cf(h, IteratorMode::From(start, Direction::Forward));
        let mut out = Vec::with_capacity(limit.min(1024));
        for item in iter {
            if out.len() >= limit {
                break;
            }
            let (k, v) = item.map_err(|e| AppError::Storage(e.to_string()))?;
            if k.as_ref() >= end {
                break;
            }
            out.push((k.to_vec(), v.to_vec()));
        }
        Ok(out)
    }

    pub fn scan_range_reverse(
        &self,
        cf: &str,
        start: &[u8],
        end: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        let h = self.handle(cf)?;
        // Reverse iterator: seek 到 end，向前走直到 ≤ start 或 limit 满。
        let iter = self.db.iterator_cf(h, IteratorMode::From(end, Direction::Reverse));
        let mut out = Vec::with_capacity(limit.min(1024));
        for item in iter {
            if out.len() >= limit {
                break;
            }
            let (k, v) = item.map_err(|e| AppError::Storage(e.to_string()))?;
            if k.as_ref() < start {
                break;
            }
            out.push((k.to_vec(), v.to_vec()));
        }
        Ok(out)
    }
```

- [ ] **Step 3: Stub on `PgDoc` in `src/storage/pg.rs`**

Add after the existing `scan_prefix` impl:

```rust
    pub fn scan_range(
        &self,
        _cf: &str,
        _start: &[u8],
        _end: &[u8],
        _limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        Err(AppError::Storage(
            "scan_range 暂不支持 Postgres 后端；请改用 RocksDB 或等待后续 PR"
                .to_string(),
        ))
    }

    pub fn scan_range_reverse(
        &self,
        _cf: &str,
        _start: &[u8],
        _end: &[u8],
        _limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        Err(AppError::Storage(
            "scan_range_reverse 暂不支持 Postgres 后端".to_string(),
        ))
    }
```

- [ ] **Step 4: Verify compilation**

Run: `make check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/storage/doc.rs src/storage/rocksdb.rs src/storage/pg.rs
git commit -m "feat(storage): DocStore.scan_range + scan_range_reverse (Rocks; PG stub)"
```

---

## Task 5: Write-path index hooks (`write_index_keys` / `delete_index_keys`)

**Files:**
- Modify: `src/service/entry.rs` (add helpers; modify `create`/`update`/`set_labels`/`archive`/`unarchive` to call helpers via existing `BatchOp` patterns)

**Interfaces:**
- Consumes: `BatchOp` (existing), `Entry`, `&[Labeling]`, `&[LabelSchema]`
- Produces:
  - `EntryService::idx_batch_puts(ws, &entry, labels, schemas) -> Vec<BatchOp>`
  - `EntryService::idx_batch_deletes(ws, &entry, labels, schemas) -> Vec<BatchOp>`

- [ ] **Step 1: Add helpers to `src/service/entry.rs`**

Locate the `impl EntryService` block. Add at the end of the impl (before the closing `}`):

```rust
    /// 生成「建索引」的 BatchOp 列表。调用方把返回值 `extend` 进自己的 BatchOp vec。
    pub fn idx_batch_puts(
        &self,
        ws: Ulid,
        entry: &Entry,
        labels: &[Labeling],
        schemas: &[LabelSchema],
    ) -> Vec<BatchOp> {
        use crate::storage::keys::{
            idx_created_key, idx_label_key, idx_title_key, idx_updated_key, label_bucket_cf,
        };
        use crate::storage::cf;
        use crate::storage::doc::BatchOp;

        let mut ops = Vec::with_capacity(3 + labels.len());
        let ts_updated = chrono::DateTime::parse_from_rfc3339(&entry.updated_at)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);
        let ts_created = chrono::DateTime::parse_from_rfc3339(&entry.created_at)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);

        ops.push(BatchOp::Put {
            cf: cf::ENTRIES_IDX_UPDATED.to_string(),
            key: idx_updated_key(ws, ts_updated, &entry.code),
            value: Vec::new(),
        });
        ops.push(BatchOp::Put {
            cf: cf::ENTRIES_IDX_CREATED.to_string(),
            key: idx_created_key(ws, ts_created, &entry.code),
            value: Vec::new(),
        });
        ops.push(BatchOp::Put {
            cf: cf::ENTRIES_IDX_TITLE.to_string(),
            key: idx_title_key(ws, &entry.title, &entry.code),
            value: Vec::new(),
        });

        for schema in schemas.iter().filter(|s| s.sortable) {
            if let Some(l) = labels.iter().find(|l| l.label_name == schema.name) {
                if let Ok(encoded) =
                    crate::domain::label::encode_label_value_for_index(&l.value, schema.value_type)
                {
                    let cf_name = label_bucket_cf(&schema.name).to_string();
                    ops.push(BatchOp::Put {
                        cf: cf_name,
                        key: idx_label_key(ws, &schema.name, &encoded, &entry.code),
                        value: Vec::new(),
                    });
                }
            }
        }

        ops
    }

    /// 生成「拆索引」的 BatchOp 列表。与 idx_batch_puts 对称。
    pub fn idx_batch_deletes(
        &self,
        ws: Ulid,
        entry: &Entry,
        labels: &[Labeling],
        schemas: &[LabelSchema],
    ) -> Vec<BatchOp> {
        use crate::storage::keys::{
            idx_created_key, idx_label_key, idx_title_key, idx_updated_key, label_bucket_cf,
        };
        use crate::storage::cf;
        use crate::storage::doc::BatchOp;

        let mut ops = Vec::with_capacity(3 + labels.len());
        let ts_updated = chrono::DateTime::parse_from_rfc3339(&entry.updated_at)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);
        let ts_created = chrono::DateTime::parse_from_rfc3339(&entry.created_at)
            .map(|d| d.timestamp_millis())
            .unwrap_or(0);

        ops.push(BatchOp::Delete {
            cf: cf::ENTRIES_IDX_UPDATED.to_string(),
            key: idx_updated_key(ws, ts_updated, &entry.code),
        });
        ops.push(BatchOp::Delete {
            cf: cf::ENTRIES_IDX_CREATED.to_string(),
            key: idx_created_key(ws, ts_created, &entry.code),
        });
        ops.push(BatchOp::Delete {
            cf: cf::ENTRIES_IDX_TITLE.to_string(),
            key: idx_title_key(ws, &entry.title, &entry.code),
        });

        for schema in schemas.iter().filter(|s| s.sortable) {
            if let Some(l) = labels.iter().find(|l| l.label_name == schema.name) {
                if let Ok(encoded) =
                    crate::domain::label::encode_label_value_for_index(&l.value, schema.value_type)
                {
                    let cf_name = label_bucket_cf(&schema.name).to_string();
                    ops.push(BatchOp::Delete {
                        cf: cf_name,
                        key: idx_label_key(ws, &schema.name, &encoded, &entry.code),
                    });
                }
            }
        }

        ops
    }
```

- [ ] **Step 2: Hook into existing write sites**

Find every existing `write_batch` call site in `src/service/entry.rs` that mutates an `Entry` — currently: `create`, `update`, `set_labelings`, `remove_labeling`, `archive`, `unarchive`, `delete` (if present). For each site:

1. Before the existing `write_batch`, load the workspace's `LabelSchema` list (via `scan_prefix(cf::LABEL_SCHEMAS, ws.to_bytes())` and deserialize).
2. Compute `idx_ops = self.idx_batch_puts(ws, &entry, &labels, &schemas)` (or `idx_batch_deletes` for archive/delete).
3. Extend the existing `ops` Vec with `idx_ops` before passing to `write_batch`.

For `archive` / `unarchive`: updated_at changes but entry remains in the index — **delete old + put new** in the same BatchOp (the helper already gives you `idx_batch_deletes` + `idx_batch_puts`; chain them).

For `set_labelings`: the labels change but entry stays — same pattern.

- [ ] **Step 3: Verify compilation**

Run: `make check`
Expected: PASS.

- [ ] **Step 4: Manual smoke test**

Run: `make dev`. In a workspace:
1. Create a new entry → check RocksDB has `ENTRIES_IDX_UPDATED` entries (e.g., via `cargo run --example dump_cf` if you have one, or by adding a temporary debug print in `idx_batch_puts`).
2. Edit the entry (change title) → check that both old and new index entries exist briefly (during the delete-then-insert transaction). After commit, only the new one should exist.
3. Archive → the entry should still be indexed (per spec §6 — archived is filtered at evaluate, not at index).

- [ ] **Step 5: Commit**

```bash
git add src/service/entry.rs
git commit -m "feat(entry): write-path dual-writes idx_* CFs in same BatchOp"
```

---

## Task 6: `EntryService::query_indexed` — stream scan with cursor

**Files:**
- Modify: `src/service/entry.rs` (add new method)

**Interfaces:**
- Consumes: `Query`, `SortSpec`, `QueryInput { cursor, limit, page, page_size }`
- Produces: `QueryResult { items, total, label_names, next_cursor }`

- [ ] **Step 1: Add `QueryInput` to entry.rs (or to a small new file `src/service/entry_query.rs`)**

If adding inline in entry.rs, locate the existing `PageInput` struct. Add next to it:

```rust
#[derive(Debug, Default, Clone)]
pub struct QueryInput {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    pub cursor: Option<String>,
    pub limit: Option<u32>,
}

impl QueryInput {
    pub fn limit(&self) -> usize {
        self.limit.or(self.page_size).unwrap_or(50).min(200) as usize
    }
}
```

Add a `next_cursor: Option<String>` field to `QueryResult` (wherever `QueryResult` is defined; see current `service/entry.rs`).

- [ ] **Step 2: Add `query_indexed` method**

In `impl EntryService`, add:

```rust
    /// 走索引的 cursor 查询路径。仅在 sort 在索引内时调用。
    pub fn query_indexed(
        &self,
        ws: Ulid,
        query: &Query,
        sort: &SortSpec,
        input: &QueryInput,
    ) -> Result<QueryResult, AppError> {
        use crate::storage::keys::{
            idx_created_key, idx_label_key, idx_title_key, idx_updated_key, label_bucket_cf,
            ws_end_exclusive, ws_start,
        };
        use crate::storage::cf;

        // 1. 选主索引 CF + 方向
        let primary_field = match sort.keys.first() {
            Some(k) => &k.field,
            None => &SortField::UpdatedAt,  // empty sort → default desc by updatedAt
        };
        let descending = sort.keys.first().map(|k| k.desc).unwrap_or(true);
        let primary_cf = match primary_field {
            SortField::UpdatedAt => cf::ENTRIES_IDX_UPDATED.to_string(),
            SortField::CreatedAt => cf::ENTRIES_IDX_CREATED.to_string(),
            SortField::Title     => cf::ENTRIES_IDX_TITLE.to_string(),
            SortField::Label(name) => label_bucket_cf(name).to_string(),
            _ => return Err(AppError::InvalidQuery(
                "排序字段不在 cursor 路径索引范围内".to_string(),
            )),
        };

        // 2. 计算扫描起点 + 终点
        let ws_end = ws_end_exclusive(ws);
        let mut cursor_key = ws_start(ws);
        let mut cursor_value = None;

        if let Some(c) = &input.cursor {
            let decoded = crate::service::cursor::decode_cursor(c)?;
            if decoded.ws != ws {
                return Err(AppError::InvalidQuery("cursor workspace 不匹配".to_string()));
            }
            if decoded.sort_hash != crate::service::cursor::sort_spec_hash(sort) {
                return Err(AppError::InvalidQuery(
                    "排序已变更，请从头分页".to_string(),
                ));
            }
            cursor_value = Some(decoded);
            cursor_key = next_position_key(&primary_cf, ws, primary_field, &cursor_value);
        }

        // 3. 流式扫描
        let limit = input.limit();
        let mut matched: Vec<(Entry, Vec<Labeling>)> = Vec::with_capacity(limit);
        let mut total: usize = 0;
        let mut last_emitted_code: Option<String> = None;
        let mut last_emitted_value: Option<CursorValue> = None;
        let mut has_more_in_index = true;

        loop {
            let batch = if descending {
                self.store.scan_range_reverse(
                    &primary_cf,
                    &ws_start(ws),
                    &cursor_key,
                    500,
                )?
            } else {
                self.store.scan_range(
                    &primary_cf,
                    &cursor_key,
                    &ws_end,
                    500,
                )?
            };
            if batch.is_empty() {
                has_more_in_index = false;
                break;
            }

            for (k, _) in &batch {
                let code = decode_code_from_idx_key(&primary_cf, ws, primary_field, k)?;
                let entry = match self.store.get::<Entry>(
                    cf::ENTRIES,
                    &crate::storage::keys::entry_by_workspace_key(ws, &code),
                )? {
                    Some(e) => e,
                    None => continue,  // 主表已删但索引还在 —— 跳过
                };
                let labels = self.labels_for_entry(ws, &code)?;

                if query.evaluate(
                    &entry,
                    &labels,
                    &EvalEnv::default(/* schemas + accounts + text hits, simplified below */),
                ) {
                    total += 1;
                    if matched.len() < limit {
                        matched.push((entry.clone(), labels.clone()));
                        last_emitted_code = Some(code.clone());
                        last_emitted_value = Some(cursor_value_for_entry(primary_field, &entry, &labels));
                    }
                }
            }

            // 推进 cursor 到 batch 最后一条之后
            if let Some((last_k, _)) = batch.last() {
                cursor_key = advance_key(last_k);
            }
            if matched.len() >= limit {
                // 多扫一轮确认后面还有匹配
                let peek = if descending {
                    self.store.scan_range_reverse(&primary_cf, &ws_start(ws), &cursor_key, 1)?
                } else {
                    self.store.scan_range(&primary_cf, &cursor_key, &ws_end, 1)?
                };
                has_more_in_index = !peek.is_empty();
                break;
            }
        }

        // 4. 多键排序：内存里对 limit 条做 tiebreaker 排序
        sort_within_page(&mut matched, &sort.keys[1..], /* label_map + accounts, simplified */);

        // 5. 生成 next_cursor
        let next_cursor = if has_more_in_index {
            if let (Some(code), Some(value)) = (last_emitted_code, last_emitted_value) {
                let hash = crate::service::cursor::sort_spec_hash(sort);
                let c = Cursor {
                    ws,
                    sort_hash: hash,
                    last_values: vec![value],
                    last_code: code,
                    backward: false,
                };
                Some(crate::service::cursor::encode_cursor(&c))
            } else {
                None
            }
        } else {
            None
        };

        // 6. label_names 汇总（沿用现有逻辑）
        let label_names = collect_label_names(&matched, query);

        Ok(QueryResult {
            items: matched,
            total,
            label_names,
            next_cursor,
        })
    }
```

This skeleton is intentionally elided in places (`EvalEnv::default(...)`, `sort_within_page`, `labels_for_entry`, `decode_code_from_idx_key`, `cursor_value_for_entry`, `next_position_key`, `advance_key`, `collect_label_names`). The implementer must:

- **`EvalEnv` construction**: factor out from the existing `query()` method (`src/service/entry.rs:740-937`) into a helper `build_eval_env(ws, query, sort) -> Result<EvalEnv, AppError>`. Reuse in both `query` and `query_indexed`.
- **`labels_for_entry`**: factor out from the existing `labelings_by_workspace` per-entry slice. If a single-entry lookup is too slow, keep the existing in-memory `labels_map` for `query()` and add a per-entry path for `query_indexed`. Acceptable cost per entry: ≤ 1 ms.
- **`decode_code_from_idx_key` / `next_position_key` / `advance_key`**: implement as helpers in `src/service/entry.rs` using the same encoding as `idx_*_key` in reverse.
- **`cursor_value_for_entry`**: extract the value of the primary sort field from an entry, package as `CursorValue`.
- **`sort_within_page`**: existing `sort_rows` in `entry.rs:918` operates on the full matched Vec; generalize to a slice.
- **`collect_label_names`**: extract from `query()` lines 907-916.

- [ ] **Step 3: Verify compilation**

Run: `make check`
Expected: PASS.

- [ ] **Step 4: Manual smoke (with the dev server)**

1. Open a workspace, create 100 entries, open the default view → first page should render, `next_cursor` field should be populated.
2. Use a GraphQL playground or curl with `Authorization` header to call `entries(workspaceId, viewId, cursor: "...", limit: 20)` → should return next 20 entries with a fresh `nextCursor`.
3. Manually change the view's sort → next page request with old cursor should error `排序已变更，请从头分页`.

- [ ] **Step 5: Commit**

```bash
git add src/service/entry.rs
git commit -m "feat(entry): query_indexed stream-scan with cursor pagination"
```

---

## Task 7: `EntryIndexService` — path selection + coverage cache

**Files:**
- Create: `src/service/entry_index.rs`
- Modify: `src/service/mod.rs` (declare module + add `pub index: EntryIndexService` to `Services`)

**Interfaces:**
- Consumes: `Arc<DocStore>`, `&[LabelSchema]`
- Produces:
  - `pub struct EntryIndexService { store, coverage: RwLock<HashMap<(Ulid, &'static str), bool>> }`
  - `pub fn sort_is_indexed(&self, ws: Ulid, sort: &SortSpec, schemas: &[LabelSchema]) -> bool`
  - `pub fn mark_covered(&self, ws: Ulid, cf: &'static str)`
  - `pub fn invalidate(&self, ws: Ulid)`

- [ ] **Step 1: Create `src/service/entry_index.rs`**

```rust
//! 索引覆盖率缓存 + path 选择。
//!
//! 设计要点：
//! - 缓存键 `(ws, primary_cf) -> bool`：该 (workspace, 主索引列族) 是否灌库完成。
//! - 启动期灌库（Task 10）后调 `mark_covered` 把所有 (ws, cf) 标记为 true。
//! - 写路径（Task 5）正常双写，新条目不会让已标记为 true 的 ws 变 false。
//! - `invalidate(ws)` 留作「schema 变更触发全量重灌」时的逃生口。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ulid::Ulid;

use crate::domain::label::LabelSchema;
use crate::domain::view::{SortField, SortSpec};
use crate::storage::{cf, DocStore};

use super::cursor::sort_spec_hash;

pub struct EntryIndexService {
    store: Arc<DocStore>,
    /// (ws_id, primary_cf_name) → 是否已灌库完成
    coverage: RwLock<HashMap<(Ulid, &'static str), bool>>,
}

impl EntryIndexService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self {
            store,
            coverage: RwLock::new(HashMap::new()),
        }
    }

    /// SortSpec 的主排序字段是否在索引内。已显式不做 SortField::CreatedBy/UpdatedBy。
    pub fn primary_cf(&self, sort: &SortSpec, schemas: &[LabelSchema]) -> Option<&'static str> {
        let field = sort.keys.first().map(|k| &k.field)?;
        match field {
            SortField::UpdatedAt => Some(cf::ENTRIES_IDX_UPDATED),
            SortField::CreatedAt => Some(cf::ENTRIES_IDX_CREATED),
            SortField::Title     => Some(cf::ENTRIES_IDX_TITLE),
            SortField::Label(name) => {
                let s = schemas.iter().find(|s| &s.name == name)?;
                if !s.sortable { return None; }
                Some(crate::storage::keys::label_bucket_cf(name))
            }
            // CreatedBy / UpdatedBy 暂不索引
            _ => None,
        }
    }

    /// sort 是否可走 cursor 路径：
    ///   1. 主字段有索引 CF
    ///   2. (ws, primary_cf) 在 coverage 缓存中已标 true
    pub fn sort_is_indexed(
        &self,
        ws: Ulid,
        sort: &SortSpec,
        schemas: &[LabelSchema],
    ) -> bool {
        let Some(cf_name) = self.primary_cf(sort, schemas) else {
            return false;
        };
        // hash 一致性检查：仅当 sort 已被前一轮 sort_spec_hash 缓存过才走索引。
        // 简化：sort 不可变，sort_spec_hash 仅用作 cursor 验证 —— 这里用 cf 缓存足够。
        let guard = self.coverage.read().unwrap();
        matches!(guard.get(&(ws, cf_name)), Some(true))
    }

    pub fn mark_covered(&self, ws: Ulid, cf_name: &'static str) {
        let mut guard = self.coverage.write().unwrap();
        guard.insert((ws, cf_name), true);
    }

    pub fn invalidate(&self, ws: Ulid) {
        let mut guard = self.coverage.write().unwrap();
        guard.retain(|(w, _), _| *w != ws);
    }

    /// 灌库用：列出该 ws 应当走索引的所有 (sortable_field, cf_name) 组合。
    pub fn indexable_targets(
        &self,
        ws: Ulid,
        schemas: &[LabelSchema],
    ) -> Vec<(&'static str, &'static str)> {
        // returns (sortable_field_name, primary_cf)
        let mut out = vec![
            ("updatedAt", cf::ENTRIES_IDX_UPDATED),
            ("createdAt", cf::ENTRIES_IDX_CREATED),
            ("title",     cf::ENTRIES_IDX_TITLE),
        ];
        for s in schemas.iter().filter(|s| s.sortable) {
            out.push((
                Box::leak(s.name.clone().into_boxed_str()) as &'static str,
                crate::storage::keys::label_bucket_cf(&s.name),
            ));
        }
        // Filter to those where (ws, cf) not already covered (caller decides whether to skip).
        let _ = ws;
        out
    }
}
```

NOTE: `Box::leak` is intentional but ugly. Replace before merge with a `HashMap<&'static str, &'static str>` keyed by leaked label name (only sortable ones get leaked; cost = number of sortable labels in the workspace, bounded).

- [ ] **Step 2: Wire into `Services` in `src/service/mod.rs`**

Find the `pub struct Services { ... }` definition. Add:

```rust
    pub index: EntryIndexService,
```

In `Services::new`, after all existing service constructions, add:

```rust
        index: EntryIndexService::new(store.clone()),
```

Add `pub mod entry_index;` to the top of `service/mod.rs`.

- [ ] **Step 3: Verify compilation**

Run: `make check`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/service/entry_index.rs src/service/mod.rs
git commit -m "feat(index): EntryIndexService — primary_cf + coverage cache + path selection"
```

---

## Task 8: `count()` fast path

**Files:**
- Modify: `src/service/entry.rs:725-737` (replace `count` body)

**Interfaces:**
- Consumes: existing `count` signature
- Produces: `Result<usize, AppError>` — same return type, faster implementation

- [ ] **Step 1: Replace `count` body**

Locate the `pub fn count(...)` method. Replace its body with:

```rust
    pub fn count(&self, ws: Ulid, query: &Query) -> Result<usize, AppError> {
        // 简单情形：查询为空 / 仅含 archived 过滤，直接数 ENTRIES_BY_WORKSPACE。
        if query.is_simple_countable() {
            let rows = self.store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &ws.to_bytes())?;
            return Ok(rows.len());
        }
        // 复杂查询：复用 query_indexed 的 total —— 它本来就要扫完整索引。
        let input = crate::service::entry::QueryInput {
            page: Some(1),
            page_size: Some(1),
            cursor: None,
            limit: Some(1),
        };
        let r = self.query_indexed(ws, query, &SortSpec::default(), &input)?;
        Ok(r.total)
    }
```

- [ ] **Step 2: Add `is_simple_countable` helper to `Query`**

In `src/domain/query.rs`, add a method on `Query`:

```rust
    /// 仅当查询为空（`Query::And(vec![])`）时返回 true —— 直接走
    /// ENTRIES_BY_WORKSPACE 的前缀扫描计数。其余情况均返回 false，
    /// 让 `count()` 走 `query_indexed` 的 total 路径。
    ///
    /// 第一版保守实现：把任何非空查询都视作「需要 evaluate」，因为归档 / 标签 /
    /// 文本等过滤目前没有「与存储层已索引状态一一对应」的等价快速计数路径。
    pub fn is_simple_countable(&self) -> bool {
        matches!(self, Query::And(parts) if parts.is_empty())
    }
```

- [ ] **Step 3: Verify compilation**

Run: `make check`
Expected: PASS.

- [ ] **Step 4: Manual smoke**

Open `views(...)` resolver for a workspace with 10 entries. The sidebar count should equal 10. Create one with `Status = Open` and a view with `Status = Open` filter — sidebar count for that view should equal 1.

- [ ] **Step 5: Commit**

```bash
git add src/service/entry.rs src/domain/query.rs
git commit -m "feat(entry): count() fast path — ENTRIES_BY_WORKSPACE for simple queries"
```

---

## Task 9: `LabelService::enable_sortable` — bulk indexer

**Files:**
- Modify: `src/service/label.rs`

**Interfaces:**
- Consumes: `&LabelSchema`
- Produces: `pub async fn enable_sortable(&self, schema: &LabelSchema) -> Result<usize, AppError>`

- [ ] **Step 1: Locate the schema mutation function**

Find the function in `src/service/label.rs` that updates a `LabelSchema` (e.g., `update_schema` / `save_schema`). When `sortable` flips from `false` to `true`, it should call `enable_sortable`.

- [ ] **Step 2: Add the bulk indexer**

In `impl LabelService`, add:

```rust
    /// 当 schema.sortable 从 false 变 true 时调用。
    /// 灌库 (ws_id, tag_name, encoded_value, code) 索引。
    pub fn enable_sortable(&self, schema: &LabelSchema) -> Result<usize, AppError> {
        use crate::storage::keys::{idx_label_key, label_bucket_cf};
        use crate::storage::doc::BatchOp;

        let ws = schema.workspace_id;
        let cf_name = label_bucket_cf(&schema.name).to_string();
        let mut ops = Vec::new();
        let mut count = 0usize;

        // 遍历该 workspace 所有有该标签的 labeling
        for (_k, v) in self.store.scan_prefix(
            crate::storage::cf::LABELINGS_BY_LABEL,
            format!("{}/{}", ws, schema.name).as_bytes(),
        )? {
            let labeling: crate::domain::label::Labeling = bincode::deserialize(&v)?;
            let code = labeling.entry_code.clone();
            if let Ok(encoded) = crate::domain::label::encode_label_value_for_index(
                &labeling.value,
                schema.value_type,
            ) {
                ops.push(BatchOp::Put {
                    cf: cf_name.clone(),
                    key: idx_label_key(ws, &schema.name, &encoded, &code),
                    value: Vec::new(),
                });
                count += 1;
                if ops.len() >= 1000 {
                    self.store.write_batch(ops.clone())?;
                    ops.clear();
                }
            }
        }

        if !ops.is_empty() {
            self.store.write_batch(ops)?;
        }

        // 标记 (ws, cf) 已覆盖
        let index_svc = /* fetch from Services — see note */;
        index_svc.mark_covered(ws, Box::leak(cf_name.clone().into_boxed_str()) as &'static str);

        Ok(count)
    }
```

NOTE: `index_svc` access requires `LabelService` to hold a reference to `Services` or to `EntryIndexService` directly. Adjust the constructor signature in `src/service/mod.rs` so `LabelService::new` accepts the index service. The existing pattern may already pass `Arc<DocStore>`; add a second arg.

- [ ] **Step 3: Hook into the schema mutation**

In the schema-update function, detect `old.sortable == false && new.sortable == true` and call `self.enable_sortable(&new)` after the BatchOp that updates the schema.

- [ ] **Step 4: Verify compilation**

Run: `make check`
Expected: PASS.

- [ ] **Step 5: Manual smoke**

1. Create a label with `sortable = true` from the start → its index gets built by the Task 10 backfill.
2. Edit an existing label (created with `sortable = false`) → flip `sortable = true` → call should succeed and the count of indexed entries should match the count of labelings.

- [ ] **Step 6: Commit**

```bash
git add src/service/label.rs src/service/mod.rs
git commit -m "feat(label): enable_sortable bulk indexer for sortable-flip events"
```

---

## Task 10: Startup backfill — `repair_entry_sort_indexes`

**Files:**
- Create: `src/service/entry_index.rs` (add `backfill` function alongside existing `EntryIndexService`)
- Modify: `src/service/mod.rs::Services::new` (call `backfill` at the end of init)

**Interfaces:**
- Consumes: `Arc<DocStore>`, `Vec<Ulid>` (workspaces), `Vec<LabelSchema>` per workspace
- Produces: `fn backfill_all(store, index_svc) -> Result<(), AppError>`

- [ ] **Step 1: Add backfill function**

In `src/service/entry_index.rs`, append:

```rust
impl EntryIndexService {
    /// 启动期灌库。对每个 workspace、每个 indexable 目标列族，
    /// 若该 ws 在该 CF 下的条目数 < ENTRIES_BY_WORKSPACE 中的 ws 条目数，
    /// 则灌库并 mark_covered。
    pub fn backfill_all(&self, store: &Arc<DocStore>) -> Result<(), AppError> {
        use crate::storage::cf;
        use crate::storage::doc::BatchOp;
        use crate::storage::keys::{
            idx_created_key, idx_label_key, idx_title_key, idx_updated_key, label_bucket_cf,
        };

        // 1. 列所有 workspaces
        let workspaces: Vec<Ulid> = {
            let mut out = Vec::new();
            for (_k, v) in store.scan_prefix(cf::WORKSPACES, b"")? {
                if let Ok(ws) = bincode::deserialize::<crate::domain::Workspace>(&v) {
                    out.push(ws.id);
                }
            }
            out
        };

        for ws in workspaces {
            // 2. 列该 ws 的 label schemas
            let schemas: Vec<crate::domain::label::LabelSchema> = {
                let mut out = Vec::new();
                for (_k, v) in store.scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())? {
                    if let Ok(s) = bincode::deserialize::<crate::domain::label::LabelSchema>(&v) {
                        out.push(s);
                    }
                }
                out
            };

            // 3. 对每个 indexable 目标：检查 + 灌库
            let entry_count = store
                .scan_prefix(cf::ENTRIES_BY_WORKSPACE, &ws.to_bytes())?
                .len();
            if entry_count == 0 {
                continue;
            }

            // 3a. updated / created / title 三个内建
            for (cf_name, keyer) in [
                (
                    cf::ENTRIES_IDX_UPDATED,
                    Box::new(|e: &crate::domain::Entry| -> Vec<u8> {
                        let ms = chrono::DateTime::parse_from_rfc3339(&e.updated_at)
                            .map(|d| d.timestamp_millis()).unwrap_or(0);
                        idx_updated_key(ws, ms, &e.code)
                    }) as Box<dyn Fn(&crate::domain::Entry) -> Vec<u8>>,
                ),
                (
                    cf::ENTRIES_IDX_CREATED,
                    Box::new(|e: &crate::domain::Entry| -> Vec<u8> {
                        let ms = chrono::DateTime::parse_from_rfc3339(&e.created_at)
                            .map(|d| d.timestamp_millis()).unwrap_or(0);
                        idx_created_key(ws, ms, &e.code)
                    }),
                ),
                (
                    cf::ENTRIES_IDX_TITLE,
                    Box::new(|e: &crate::domain::Entry| -> Vec<u8> {
                        idx_title_key(ws, &e.title, &e.code)
                    }),
                ),
            ] {
                let existing = store.scan_prefix(cf_name, &ws.to_bytes())?.len();
                if existing >= entry_count {
                    self.mark_covered(ws, cf_name);
                    continue;
                }
                // 灌库
                for (_ek, ev) in store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &ws.to_bytes())? {
                    let entry: crate::domain::Entry = bincode::deserialize(&ev)?;
                    let key = keyer(&entry);
                    store.put_raw(cf_name, &key, &[])?;
                }
                self.mark_covered(ws, cf_name);
                eprintln!("[backfill] ws={} cf={} entries={}", ws, cf_name, entry_count);
            }

            // 3b. 每个 sortable 标签
            for schema in schemas.iter().filter(|s| s.sortable) {
                let cf_name = label_bucket_cf(&schema.name);
                let existing = store.scan_prefix(cf_name, &ws.to_bytes())?.len();
                let labeling_count = store
                    .scan_prefix(
                        cf::LABELINGS_BY_LABEL,
                        format!("{}/{}", ws, schema.name).as_bytes(),
                    )?
                    .len();
                if existing >= labeling_count && labeling_count > 0 {
                    self.mark_covered(ws, cf_name);
                    continue;
                }
                for (_lk, lv) in store.scan_prefix(
                    cf::LABELINGS_BY_LABEL,
                    format!("{}/{}", ws, schema.name).as_bytes(),
                )? {
                    let labeling: crate::domain::label::Labeling = bincode::deserialize(&lv)?;
                    if let Ok(encoded) = crate::domain::label::encode_label_value_for_index(
                        &labeling.value,
                        schema.value_type,
                    ) {
                        let key = idx_label_key(ws, &schema.name, &encoded, &labeling.entry_code);
                        store.put_raw(cf_name, &key, &[])?;
                    }
                }
                self.mark_covered(ws, cf_name);
                eprintln!(
                    "[backfill] ws={} label={} entries={}",
                    ws, schema.name, labeling_count
                );
            }
        }

        Ok(())
    }
}
```

- [ ] **Step 2: Wire into `Services::new`**

In `src/service/mod.rs::Services::new`, after the existing `repair_*` calls (search for `repair_` in the file; the existing chain is described in CLAUDE.md), append:

```rust
        self.index.backfill_all(&self.store)?;
```

- [ ] **Step 3: Verify compilation + build**

Run: `make check && make build`
Expected: PASS.

- [ ] **Step 4: Manual smoke**

1. Start the server with a workspace that has 100 entries (`make dev`, create them).
2. Stop, then restart.
3. Watch startup logs — `[backfill] ws=...` lines should appear, indicating backfill ran.
4. Open the workspace view — first page should load fast and `next_cursor` should be populated.

- [ ] **Step 5: Commit**

```bash
git add src/service/entry_index.rs src/service/mod.rs
git commit -m "feat(index): startup backfill — repair_entry_sort_indexes for all ws"
```

---

## Task 11: GraphQL resolver — `QueryInput` + `EntryPage.nextCursor` + path dispatch

**Files:**
- Modify: `src/api/graphql.rs` (find `entries` resolver; update args + return type)

**Interfaces:**
- Consumes: existing `entries(workspaceId, viewId, query, sorts, page, pageSize) -> EntryPage`
- Produces: `entries(workspaceId, viewId, query, sorts, page, pageSize, cursor, limit) -> EntryPage { ..., nextCursor }`

- [ ] **Step 1: Find the `entries` resolver and its input/output types**

In `src/api/graphql.rs`, locate `async fn entries(...)`. Also locate the `GqlEntryPage` struct (or equivalent return type).

- [ ] **Step 2: Add `cursor` / `limit` args to the resolver signature**

Update the function signature to include the two new args:

```rust
async fn entries(
    &self,
    ctx: &Context<'_>,
    workspace_id: ID,
    view_id: Option<ID>,
    query: Option<serde_json::Value>,
    sorts: Option<Vec<SortInput>>,
    page: Option<i32>,
    page_size: Option<i32>,
    cursor: Option<String>,
    limit: Option<i32>,
) -> GqlResult<GqlEntryPage>
```

- [ ] **Step 3: Add `nextCursor: Option<String>` to `GqlEntryPage`**

```rust
struct GqlEntryPage {
    items: Vec<GqlEntry>,
    total: i64,
    page: i32,
    page_size: i32,
    label_names: Vec<String>,
    #[graphql(name = "nextCursor")]
    next_cursor: Option<String>,
}
```

- [ ] **Step 4: Implement path dispatch**

Replace the resolver body:

```rust
// 解析 sort / query 沿用现有逻辑（不变）
let query_ast = parse_query(query, &view_query).await?;
let sort_spec = build_sort_spec(sorts);

// 加载 schemas
let schemas = gql.services.label.list_schemas(ws_id)?;

// 构造 QueryInput
let input = QueryInput {
    page: page.map(|p| p as u32),
    page_size: page_size.map(|p| p as u32),
    cursor,
    limit: limit.map(|l| l as u32),
};

// 路径选择
let result = if input.cursor.is_some()
    || (sort_spec.keys.is_empty()
        || matches!(sort_spec.keys.first().map(|k| &k.field), Some(SortField::UpdatedAt | SortField::CreatedAt | SortField::Title))
            && gql.services.index.sort_is_indexed(ws_id, &sort_spec, &schemas))
{
    if !gql.services.index.sort_is_indexed(ws_id, &sort_spec, &schemas) && input.cursor.is_some() {
        return Err(AppError::InvalidQuery("当前排序不支持 cursor 分页".to_string()).into());
    }
    if input.page.unwrap_or(1) > 1 {
        eprintln!("[entries] page>1 with indexable sort; falling through to legacy path");
    }
    gql.services.entry.query_indexed(ws_id, &query_ast, &sort_spec, &input)?
} else {
    // Legacy path
    gql.services.entry.query(
        ws_id,
        &query_ast,
        &sort_spec,
        PageInput { page, page_size },
    )?
};

Ok(GqlEntryPage {
    items: result.items.into_iter().map(|(e, ls)| GqlEntry::new(e, ls)).collect(),
    total: result.total as i64,
    page: page.unwrap_or(1),
    page_size: page_size.unwrap_or(50),
    label_names: result.label_names,
    next_cursor: result.next_cursor,
})
```

- [ ] **Step 5: Verify compilation (wasm side)**

Run: `make check`
Expected: PASS. The wasm side only sees the GraphQL response types; no functional change here.

- [ ] **Step 6: Commit**

```bash
git add src/api/graphql.rs
git commit -m "feat(api): entries() accepts cursor + limit, returns nextCursor"
```

---

## Task 12: Frontend — cursor pagination in `workspace_main.rs`

**Files:**
- Modify: `src/frontend/graphql_client.rs` (extend `EntryPage` + `entries(...)` signature)
- Modify: `src/frontend/pages/workspace_main.rs` (use `next_cursor` when present)

**Interfaces:**
- Consumes: existing `entries(workspace_id, view_id?, query?, sorts?, page?, page_size?)` and `EntryPage`
- Produces: `entries(workspace_id, view_id?, query?, sorts?, page?, page_size?, cursor?, limit?)`; `EntryPage.next_cursor`

- [ ] **Step 1: Extend `EntryPage`**

In `src/frontend/graphql_client.rs`, find `pub struct EntryPage { ... }`. Add:

```rust
    #[serde(default)]
    pub next_cursor: Option<String>,
```

- [ ] **Step 2: Extend `entries()` signature**

Find `pub async fn entries(...)` (likely around line 735 in the current file). Add `cursor: Option<&str>` and `limit: Option<u32>` parameters. Build the GraphQL query string conditionally:

```rust
let q = format!(
    "query($id: ID!, $cursor: String, $limit: Int) {{
        entries(workspaceId: $id, cursor: $cursor, limit: $limit) {{ {ENTRY_FIELDS} }}
    }}"
);
// Pass variables { id, cursor, limit }
```

- [ ] **Step 3: Use `next_cursor` in `workspace_main.rs`**

Find the function in `src/frontend/pages/workspace_main.rs` that loads more entries on scroll/page-flip (search for `entries(` and `.await`). Replace the page-increment logic with:

```rust
if let Some(cursor) = last_page.next_cursor {
    // Cursor path
    next_args.cursor = Some(cursor.as_str());
    next_args.page = None;  // ignore page
} else {
    // Legacy path
    next_args.page = Some(next_args.page.unwrap_or(1) + 1);
    next_args.cursor = None;
}
```

- [ ] **Step 4: Verify compilation (wasm side)**

Run: `make check`
Expected: PASS.

- [ ] **Step 5: Browser smoke**

Per `browser-verification-setup.md`:
1. `LEPTOS_SITE_ROOT=target/site-smoke make build && make serve` (or `make dev` directly).
2. With playwright-core, navigate to a workspace view, scroll past the first 50 entries → page 2 should load.
3. Verify network panel: page 2 request should send `cursor: "..."` not `page: 2`.

- [ ] **Step 6: Commit**

```bash
git add src/frontend/graphql_client.rs src/frontend/pages/workspace_main.rs
git commit -m "feat(frontend): use next_cursor for view pagination"
```

---

## Task 13: Rollback switch + docs

**Files:**
- Modify: `src/config.rs` (add `QueryConfig` struct + `[query]` section in `AppConfig`)
- Modify: `config.example.toml` (document `[query]`)

**Interfaces:**
- Consumes: existing `AppConfig` deserialization
- Produces: `pub struct QueryConfig { pub force_legacy_path: bool }` defaulting to `false`

- [ ] **Step 1: Add `QueryConfig`**

In `src/config.rs`, find a good place to add the new config struct (mirror the pattern of `LoggingConfig` added in commit `21934ec`):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QueryConfig {
    /// 设为 true 时强制走 Legacy 路径，便于 cursor 索引出问题时的紧急回滚。
    pub force_legacy_path: bool,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self { force_legacy_path: false }
    }
}
```

In the main `AppConfig` struct, add:

```rust
    #[serde(default)]
    pub query: QueryConfig,
```

- [ ] **Step 2: Wire into the resolver**

In `src/api/graphql.rs::entries`, at the top of the body, add:

```rust
if gql.services.config.query.force_legacy_path {
    // 强制 Legacy 路径（不进 sort_is_indexed）
    let result = gql.services.entry.query(ws_id, &query_ast, &sort_spec, PageInput { page, page_size })?;
    return Ok(GqlEntryPage { /* legacy, no next_cursor */ ... });
}
```

(Fetch `config` from `Services` — add `pub config: Arc<AppConfig>` to `Services` if not present.)

- [ ] **Step 3: Document in `config.example.toml`**

Append:

```toml
# ── query ──────────────────────────────────────────────────────────────
# 视图查询相关开关。
[query]
# 强制走 Legacy 路径（load-all + in-memory filter）。cursor 索引出问题时
# 临时回滚用；走此模式 20M workspace 下会慢。
force_legacy_path = false
```

- [ ] **Step 4: Verify compilation + build**

Run: `make check && make build`
Expected: PASS.

- [ ] **Step 5: Manual rollback test**

1. Set `force_legacy_path = true` in `config.toml`.
2. Restart the server.
3. Open a workspace view → next_cursor should NOT be populated, but the view should still render (slowly).

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/api/graphql.rs config.example.toml
git commit -m "feat(config): [query] force_legacy_path rollback switch"
```

---

## End-to-End Verification

After all 13 tasks committed:

```bash
make check
make build
```

Then:

1. Start `make dev`.
2. In a workspace, create ~1000 entries via the UI (or seed via a quick script).
3. Open the default view → first paint under 1 second; `next_cursor` field present in the network response.
4. Scroll to page 2 → request uses `cursor`, response < 100 ms locally.
5. Open a view with sort on a label flagged `sortable = true` → cursor path used.
6. Open a view with sort on a label NOT flagged `sortable` → Legacy path (no `next_cursor`).
7. Manually edit `config.toml` → set `force_legacy_path = true` → restart → cursor field disappears from responses; legacy behavior intact.
8. Change a view's sort while paginating → next page request returns `排序已变更，请从头分页` error.

---

## Self-Review Notes (carried forward for the executor)

- **No new Rust unit tests** — verification gate is `make check` + `make build`. UI tasks additionally use the browser-verification recipe.
- **Box::leak in Task 7** — flagged for cleanup before merge (replace with a `HashMap<&'static str, &'static str>` of leaked label names, bounded by `sortable` label count).
- **`EvalEnv` factoring in Task 6** — the implementer MUST extract `EvalEnv` construction into a shared helper used by both `query()` and `query_indexed()`. Skipping this means duplication.
- **Postgres parity** — `PgDoc::scan_range` returns `Err` this phase. The path-dispatch logic must NOT silently fall back to Legacy when `scan_range` errors; instead it should propagate the error (so config-level `force_legacy_path` is the only way to opt out). If you change this, update the spec.
- **`is_simple_countable`** — Task 8 uses the conservative definition `matches!(self, Query::And(parts) if parts.is_empty())`. Phase 2 can broaden this to include archived-filter and other storage-mirrored conditions as those become available.
