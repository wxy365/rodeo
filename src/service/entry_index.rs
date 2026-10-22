//! 索引覆盖率缓存 + path 选择。
//!
//! 设计要点：
//! - 缓存键 `(ws, primary_cf) -> bool`：该 (workspace, 主索引列族) 是否灌库完成。
//! - 启动期灌库（Task 10）后调 `mark_covered` 把所有 (ws, cf) 标记为 true。
//! - 写路径（Task 5）正常双写，新条目不会让已标记为 true 的 ws 变 false。
//! - `invalidate(ws)` 留作「schema 变更触发全量重灌」时的逃生口。
//!
//! Phase 1 仅内建三字段（UpdatedAt / CreatedAt / Title）走 cursor 路径；
//! SortField::Label 的 sortable 标签索引灌库逻辑先写但不读（待 Phase 2 解码）。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ulid::Ulid;

use crate::domain::label::LabelSchema;
use crate::domain::view::{SortField, SortSpec};
use crate::error::AppError;
use crate::storage::{cf, BatchOp, DocStore};

pub struct EntryIndexService {
    #[allow(dead_code)]
    store: Arc<DocStore>,
    /// (ws_id, primary_cf_name) → 是否已灌库完成。
    coverage: RwLock<HashMap<(Ulid, &'static str), bool>>,
}

impl EntryIndexService {
    pub fn new(store: Arc<DocStore>) -> Arc<Self> {
        Arc::new(Self {
            store,
            coverage: RwLock::new(HashMap::new()),
        })
    }

    /// SortSpec 主排序字段对应的索引 CF；不在 Phase 1 范围内的返回 None。
    pub fn primary_cf(sort: &SortSpec, schemas: &[LabelSchema]) -> Option<&'static str> {
        let field = sort.keys.first().map(|k| &k.field)?;
        match field {
            SortField::UpdatedAt => Some(cf::ENTRIES_IDX_UPDATED),
            SortField::CreatedAt => Some(cf::ENTRIES_IDX_CREATED),
            SortField::Title => Some(cf::ENTRIES_IDX_TITLE),
            // Phase 1: SortField::Label 索引未支持 cursor 路径（key 布局歧义）。
            SortField::Label(name) => {
                let s = schemas.iter().find(|s| &s.name == name)?;
                if !s.sortable {
                    return None;
                }
                // 返回桶名仅用于灌库标记 —— cursor 路径不读它。
                Some(crate::storage::keys::label_bucket_cf(name))
            }
            SortField::CreatedBy | SortField::UpdatedBy => None,
        }
    }

    /// sort 是否可走 cursor 路径：(1) 主字段有索引 CF；(2) (ws, primary_cf) 已灌完。
    pub fn sort_is_indexed(&self, ws: Ulid, sort: &SortSpec, schemas: &[LabelSchema]) -> bool {
        let Some(cf_name) = Self::primary_cf(sort, schemas) else {
            return false;
        };
        // Phase 1：SortField::Label 即使 sortable=true 也不进 cursor 路径。
        if matches!(
            sort.keys.first().map(|k| &k.field),
            Some(SortField::Label(_))
        ) {
            return false;
        }
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

    /// 启动期灌库。对每个 workspace × 每个 sortable 字段，比对主表条目数与索引
    /// 条目数；不足则灌库并 mark_covered。已覆盖的 workspace 跳过（idempotent）。
    pub fn backfill_all(&self, store: &Arc<DocStore>) -> Result<(), AppError> {
        use crate::domain::Entry;
        use crate::storage::keys::{
            idx_created_key, idx_label_key, idx_title_key, idx_updated_key, label_bucket_cf,
        };

        // 列所有 workspaces
        let workspaces: Vec<Ulid> = {
            let mut out = Vec::new();
            for (_k, v) in store.scan_prefix(crate::storage::cf::WORKSPACES, b"")? {
                if let Ok(ws) = bincode::deserialize::<crate::domain::Workspace>(&v) {
                    out.push(ws.id);
                }
            }
            out
        };

        for ws in workspaces {
            // 该 ws 内全部活跃 entry：扫 `cf::ENTRIES`（含完整数据），按 workspace 过滤、
            // 跳过已软删 / 已归档。`ENTRIES_BY_WORKSPACE` 是「存在性索引」，value 为空，
            // 从它上面 deserialize 会撞上 bincode `unexpected end of file`。
            let active_entries: Vec<Entry> = {
                let mut out = Vec::new();
                for (_ek, ev) in store.scan_prefix(crate::storage::cf::ENTRIES, b"")? {
                    let Ok(entry) = bincode::deserialize::<Entry>(&ev) else {
                        continue;
                    };
                    if entry.workspace_id != ws {
                        continue;
                    }
                    if entry.is_deleted() {
                        continue;
                    }
                    if store.exists(crate::storage::cf::ENTRIES_ARCHIVED, entry.code.as_bytes())? {
                        continue;
                    }
                    out.push(entry);
                }
                out
            };
            if active_entries.is_empty() {
                continue;
            }
            let entry_count = active_entries.len();

            // 列该 ws 的 schemas
            let schemas: Vec<crate::domain::label::LabelSchema> = {
                let mut out = Vec::new();
                for (_k, v) in
                    store.scan_prefix(crate::storage::cf::LABEL_SCHEMAS, &ws.to_bytes())?
                {
                    if let Ok(s) = bincode::deserialize::<crate::domain::label::LabelSchema>(&v) {
                        out.push(s);
                    }
                }
                out
            };

            // 内建三字段：updated / created / title
            for (cf_name, key_fn) in [
                (
                    cf::ENTRIES_IDX_UPDATED,
                    Box::new(|e: &Entry| -> Vec<u8> {
                        idx_updated_key(ws, e.updated_at.timestamp_millis(), &e.code)
                    }) as Box<dyn Fn(&Entry) -> Vec<u8>>,
                ),
                (
                    cf::ENTRIES_IDX_CREATED,
                    Box::new(|e: &Entry| -> Vec<u8> {
                        idx_created_key(ws, e.created_at.timestamp_millis(), &e.code)
                    }),
                ),
                (
                    cf::ENTRIES_IDX_TITLE,
                    Box::new(|e: &Entry| -> Vec<u8> { idx_title_key(ws, &e.title, &e.code) }),
                ),
            ] {
                let existing = store.scan_prefix(cf_name, &ws.to_bytes())?.len();
                if existing >= entry_count {
                    self.mark_covered(ws, cf_name);
                    continue;
                }
                let mut ops = Vec::new();
                for entry in &active_entries {
                    ops.push(BatchOp::put_raw(cf_name, key_fn(entry), Vec::new()));
                    if ops.len() >= 1000 {
                        store.write_batch(std::mem::take(&mut ops))?;
                    }
                }
                if !ops.is_empty() {
                    store.write_batch(ops)?;
                }
                self.mark_covered(ws, cf_name);
            }

            // 每个 sortable 标签：灌 label bucket
            for schema in schemas.iter().filter(|s| s.sortable) {
                let cf_name = label_bucket_cf(&schema.name);
                let prefix = format!("{ws}/{}", schema.name);
                // 索引行数上限：从 `LABELINGS_BY_LABEL` 取（与既有短路一致；
                // 该 CF 含历史已删条目的悬空行，但用作「是否有必要灌库」的探测
                // 信号已经足够）。
                let labeling_count = store
                    .scan_prefix(crate::storage::cf::LABELINGS_BY_LABEL, prefix.as_bytes())?
                    .len();
                if labeling_count == 0 {
                    continue;
                }
                // 收集该 ws + schema.name 的活跃打标：从 `cf::LABELINGS`（含完整
                // Labeling）按 entry.code 前缀扫；外层 active_entries 已经把已删
                // / 已归档 entry 滤掉，所以这里只看活跃 entry 的 LABELINGS。
                let mut active_labelings: Vec<crate::domain::Labeling> = Vec::new();
                for entry in &active_entries {
                    for (_lk, lv) in
                        store.scan_prefix(crate::storage::cf::LABELINGS, entry.code.as_bytes())?
                    {
                        let Ok(l) = bincode::deserialize::<crate::domain::Labeling>(&lv) else {
                            continue;
                        };
                        if l.label_name == schema.name {
                            active_labelings.push(l);
                        }
                    }
                }
                let existing = store.scan_prefix(cf_name, &ws.to_bytes())?.len();
                if existing >= active_labelings.len() {
                    self.mark_covered(ws, cf_name);
                    continue;
                }
                let mut ops = Vec::new();
                for labeling in &active_labelings {
                    if let Ok(encoded) = crate::domain::label::encode_label_value_for_index(
                        &labeling.value,
                        schema.value_type,
                    ) {
                        ops.push(BatchOp::put_raw(
                            cf_name,
                            idx_label_key(ws, &schema.name, &encoded, &labeling.entry_code),
                            Vec::new(),
                        ));
                        if ops.len() >= 1000 {
                            store.write_batch(std::mem::take(&mut ops))?;
                        }
                    }
                }
                if !ops.is_empty() {
                    store.write_batch(ops)?;
                }
                self.mark_covered(ws, cf_name);
            }
        }
        Ok(())
    }

    /// 启动期 orphan-index 清理：把索引行还指向「已软删 / 已归档」entry 的那些行删掉。
    ///
    /// 背景：`5f03940 fix(service): soft_delete 用 old_entry 拼索引删除键` 修了新写
    /// 路径——之后删除的 entry 一定会把索引行一起撤掉。但在那次 commit 之前已被软删的
    /// entry 仍带着孤儿索引行，对应的键当时是按「删除前 updated_at / title」写入的；
    /// 现在的 entry 行里 `updated_at` 已经被刷新成删除时刻、`deleted_at` 已置位，两
    /// 边对不上，重做 idx_delete 也拼不出原键。
    ///
    /// 这层 repair 不重建键，而是直接正向扫三个 builtin CF + 所有 sortable 标签桶，
    /// 对每条索引行反查 `cf::ENTRIES`，遇到 `is_deleted || archived` 就把这条 delete
    /// 掉；和记不记得旧键无关、idempotent、只对确实死掉的 entry 下手。
    ///
    /// 同时清掉三类 builtin CF 里「键与条目当前状态对不上」的**陈旧行**：评论 / 附件
    /// 推进过 updated_at、但旧写入路径没同步索引（2026-10 修复前），或历史重复写入
    /// 留下的第二行。判据是重新拼一遍该条目此刻应有的键、与扫描到的键逐字节比较；
    /// 陈旧行必须删，否则 `query_indexed` 会把同一个 code 发射两次（列表重复条目）。
    pub fn repair_orphan_index_rows(&self, store: &Arc<DocStore>) -> Result<(), AppError> {
        use crate::domain::Entry;
        use std::collections::{HashMap, HashSet};

        // 一次性把所有 entry（含已删）抓进内存：后续判断「这条索引行有没有死人指着」
        // 就是 O(1) HashMap 查表。entry 体小（9 个字段），ws 上千条也是 MB 量级以下。
        let entries_by_code: HashMap<String, Entry> = {
            let mut m = HashMap::new();
            for (_ek, ev) in store.scan_prefix(cf::ENTRIES, b"")? {
                if let Ok(e) = bincode::deserialize::<Entry>(&ev) {
                    m.insert(e.code.clone(), e);
                }
            }
            m
        };

        // 「code 是否已死」闭包：entry 已经软删、或对应的 ENTRIES_ARCHIVED 行存在、
        // 或 entry 根本不存在（外力清掉了，索引行留着更没用）都算死。
        let archived_codes: HashSet<String> = {
            let mut s = HashSet::new();
            for (k, _) in store.scan_prefix(cf::ENTRIES_ARCHIVED, b"")? {
                if let Ok(code) = std::str::from_utf8(&k) {
                    s.insert(code.to_string());
                }
            }
            s
        };
        let is_dead = |code: &str| -> bool {
            match entries_by_code.get(code) {
                Some(e) => e.is_deleted() || archived_codes.contains(code),
                None => true,
            }
        };

        // helper: 从索引键尾部取 16 字节当 entry code（base62 16 字符恒定）。
        // 长度 < 16 + 1 + 16 的视为无效键（防误删：scan 时偶尔扫到边界外的前缀）。
        let code_from_tail = |k: &[u8]| -> Option<String> {
            if k.len() < 16 + 1 + 16 {
                return None;
            }
            std::str::from_utf8(&k[k.len() - 16..])
                .ok()
                .map(str::to_string)
        };

        // 三个 builtin 索引 CF：键格式 (ws[16B], sort_field..., code[16B])。
        // 死条目的行直接删；活条目则重算「此刻应有的键」，不一致的陈旧行删掉并
        // 按当前状态补写正确行——同一 (ws, code) 在 UPDATED CF 里留两行会让
        // query_indexed 逐行发射，列表里出现两条一模一样的 Entry。
        // 补写而不是只删，是为了兜住「陈旧行挤掉了正常行」的情形：只删的话，
        // backfill_all 的覆盖率标记仍是 true，该条目会从 cursor 列表里彻底消失。
        use crate::storage::keys::{idx_created_key, idx_title_key, idx_updated_key};
        let mut cleared_builtin = 0usize;
        let mut rebuilt_stale = 0usize;
        for cf_name in [
            cf::ENTRIES_IDX_UPDATED,
            cf::ENTRIES_IDX_CREATED,
            cf::ENTRIES_IDX_TITLE,
        ] {
            let mut ops = Vec::new();
            for (k, _) in store.scan_prefix(cf_name, b"")? {
                let Some(code) = code_from_tail(&k) else {
                    continue;
                };
                if is_dead(&code) {
                    cleared_builtin += 1;
                    ops.push(BatchOp::delete(cf_name, k.to_vec()));
                } else {
                    // is_dead 为假 ⇒ entries_by_code 里必有这条 entry。
                    let entry = entries_by_code
                        .get(&code)
                        .expect("is_dead 为假时 entry 必然存在");
                    let expected = match cf_name {
                        cf::ENTRIES_IDX_UPDATED => idx_updated_key(
                            entry.workspace_id,
                            entry.updated_at.timestamp_millis(),
                            &code,
                        ),
                        cf::ENTRIES_IDX_CREATED => idx_created_key(
                            entry.workspace_id,
                            entry.created_at.timestamp_millis(),
                            &code,
                        ),
                        cf::ENTRIES_IDX_TITLE => {
                            idx_title_key(entry.workspace_id, &entry.title, &code)
                        }
                        _ => continue,
                    };
                    if k.as_slice() != expected.as_slice() {
                        rebuilt_stale += 1;
                        ops.push(BatchOp::delete(cf_name, k.to_vec()));
                        ops.push(BatchOp::put_raw(cf_name, expected, Vec::new()));
                    }
                }
                if ops.len() >= 1000 {
                    store.write_batch(std::mem::take(&mut ops))?;
                }
            }
            if !ops.is_empty() {
                store.write_batch(ops)?;
            }
        }

        // Label bucket CF：每个 sortable label 一个，键 (ws[16B], tag[N], encoded[M], code[16B])。
        // 跨 ws 共享同一个 CF（按 schema.name 经 FNV-1a 哈希映射到 16 个桶）；
        // 找出所有 sortable schema 名，按桶去重后只扫一次。
        // `label_bucket_cf` 返回 `&'static str`，正好喂给 `BatchOp::delete (&'static str)`。
        let mut cleared_label = 0usize;
        let mut scanned: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
        for (_k, v) in store.scan_prefix(cf::LABEL_SCHEMAS, b"")? {
            let Ok(schema) = bincode::deserialize::<crate::domain::label::LabelSchema>(&v) else {
                continue;
            };
            if !schema.sortable {
                continue;
            }
            let cf_name: &'static str = crate::storage::keys::label_bucket_cf(&schema.name);
            if !scanned.insert(cf_name) {
                continue;
            }
            let mut ops = Vec::new();
            for (k, _) in store.scan_prefix(cf_name, b"")? {
                let Some(code) = code_from_tail(&k) else {
                    continue;
                };
                if is_dead(&code) {
                    ops.push(BatchOp::delete(cf_name, k.to_vec()));
                    cleared_label += 1;
                    if ops.len() >= 1000 {
                        store.write_batch(std::mem::take(&mut ops))?;
                    }
                }
            }
            if !ops.is_empty() {
                store.write_batch(ops)?;
            }
        }
        if cleared_builtin + rebuilt_stale + cleared_label > 0 {
            tracing::info!(
                builtin = cleared_builtin,
                rebuilt_stale = rebuilt_stale,
                label_buckets = cleared_label,
                "orphan-index repair: 已清理悬空 / 陈旧索引行"
            );
        }
        Ok(())
    }
}
