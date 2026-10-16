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
    pub fn sort_is_indexed(
        &self,
        ws: Ulid,
        sort: &SortSpec,
        schemas: &[LabelSchema],
    ) -> bool {
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
                    if store.exists(
                        crate::storage::cf::ENTRIES_ARCHIVED,
                        entry.code.as_bytes(),
                    )? {
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
                for (_k, v) in store
                    .scan_prefix(crate::storage::cf::LABEL_SCHEMAS, &ws.to_bytes())?
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
}
