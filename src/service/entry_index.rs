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
use crate::storage::{cf, DocStore};

pub struct EntryIndexService {
    #[allow(dead_code)]
    store: Arc<DocStore>,
    /// (ws_id, primary_cf_name) → 是否已灌库完成。
    coverage: RwLock<HashMap<(Ulid, &'static str), bool>>,
}

impl EntryIndexService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self {
            store,
            coverage: RwLock::new(HashMap::new()),
        }
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
}
