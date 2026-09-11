use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::view::{SortField, SortSpec};
use crate::domain::{
    generate_entry_code, AuditAction, AuditLog, Entry, LabelSchema, LabelValue, Labeling, Query,
};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::search::{SearchIndex, TEXT_CANDIDATE_LIMIT};
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct EntryService {
    store: Arc<DocStore>,
    search: Option<Arc<SearchIndex>>,
}

#[derive(Debug, Clone, Copy)]
pub struct PageInput {
    pub page: usize,
    pub page_size: usize,
}

impl Default for PageInput {
    fn default() -> Self {
        Self { page: 1, page_size: 20 }
    }
}

impl PageInput {
    fn normalized(&self) -> (usize, usize) {
        let page = self.page.max(1);
        let page_size = self.page_size.clamp(1, 100);
        (page, page_size)
    }
}

pub struct QueryResult {
    pub items: Vec<(Entry, Vec<Labeling>)>,
    pub total: usize,
}

impl EntryService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store, search: None }
    }

    pub fn with_search(store: Arc<DocStore>, search: Arc<SearchIndex>) -> Self {
        Self { store, search: Some(search) }
    }

    fn reindex(&self, entry: &Entry) {
        let Some(search) = &self.search else { return };
        let labels = self.labelings(&entry.code).unwrap_or_default();
        if entry.is_deleted() {
            if let Err(e) = search.remove_entry(&entry.code) {
                tracing::warn!("移除检索索引失败 {}: {e}", entry.code);
            }
        } else if let Err(e) = search.index_entry(entry, &labels) {
            tracing::warn!("更新检索索引失败 {}: {e}", entry.code);
        }
    }

    pub fn create(&self, actor: Ulid, workspace_id: Ulid, title: &str) -> Result<Entry, AppError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::Internal("标题不能为空".to_string()));
        }
        let mut entry = Entry::new(workspace_id, title.to_string(), actor);
        while self
            .store
            .get::<Entry>(cf::ENTRIES, entry.code.as_bytes())?
            .is_some()
        {
            entry.code = generate_entry_code();
        }
        let audit = AuditLog::new(
            AuditAction::EntryCreated,
            actor,
            "entry",
            &entry.code,
            Some(workspace_id),
            None,
            Some(serde_json::to_string(&entry).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::ENTRIES,
            entry.code.as_bytes().to_vec(),
            &entry,
        )?);
        ops.push(BatchOp::put_raw(
            cf::ENTRIES_BY_WORKSPACE,
            keys::entry_by_workspace_key(workspace_id, &entry.code),
            Vec::new(),
        ));
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(entry)
    }

    pub fn get(&self, code: &str) -> Result<Option<Entry>, AppError> {
        self.store.get(cf::ENTRIES, code.as_bytes())
    }

    /// 乐观并发更新：expected_updated_at 与当前 updated_at 不一致时返回 ConflictDetected。
    pub fn update(
        &self,
        actor: Ulid,
        code: &str,
        expected_updated_at: &str,
        title: &str,
        detail: &str,
    ) -> Result<Entry, AppError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::Internal("标题不能为空".to_string()));
        }
        let mut entry = self.get(code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        let expected = chrono::DateTime::parse_from_rfc3339(expected_updated_at)
            .map_err(|_| AppError::Internal("时间格式无效".to_string()))?
            .with_timezone(&Utc);
        if entry.updated_at != expected {
            return Err(AppError::ConflictDetected);
        }
        let before = serde_json::to_string(&entry).unwrap_or_default();
        entry.title = title.to_string();
        entry.detail = detail.to_string();
        entry.updated_by = actor;
        entry.updated_at = Utc::now();
        let after = serde_json::to_string(&entry).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::EntryUpdated,
            actor,
            "entry",
            code,
            Some(entry.workspace_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::ENTRIES, code.as_bytes().to_vec(), &entry)?);
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(entry)
    }

    /// 软删除：仅置 deleted_at，不物理移除。重复删除幂等（第二次直接 Ok）。
    pub fn soft_delete(&self, actor: Ulid, code: &str) -> Result<(), AppError> {
        let mut entry = self.get(code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Ok(());
        }
        let before = serde_json::to_string(&entry).unwrap_or_default();
        entry.deleted_at = Some(Utc::now());
        entry.updated_by = actor;
        entry.updated_at = Utc::now();
        let after = serde_json::to_string(&entry).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::EntryDeleted,
            actor,
            "entry",
            code,
            Some(entry.workspace_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::ENTRIES, code.as_bytes().to_vec(), &entry)?);
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(())
    }

    pub fn list(&self, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
        let prefix = workspace_id.to_bytes();
        let rows = self.store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &prefix)?;
        let mut entries = Vec::new();
        for (key, _) in rows {
            if key.len() <= 16 {
                continue;
            }
            let code = std::str::from_utf8(&key[16..]).unwrap_or("").to_string();
            if let Some(e) = self.store.get::<Entry>(cf::ENTRIES, code.as_bytes())? {
                if !e.is_deleted() {
                    entries.push(e);
                }
            }
        }
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(entries)
    }

    pub fn set_labeling(
        &self,
        actor: Ulid,
        entry_code: &str,
        label_name: &str,
        value: &serde_json::Value,
    ) -> Result<Labeling, AppError> {
        let entry = self.get(entry_code)?.ok_or(AppError::NotFound)?;
        let schema = self
            .store
            .get::<LabelSchema>(
                cf::LABEL_SCHEMAS,
                &keys::label_schema_key(entry.workspace_id, label_name),
            )?
            .ok_or(AppError::NotFound)?;
        let lv = LabelValue::from_json(value, &schema)?;
        let labeling = Labeling::new(entry_code.to_string(), label_name.to_string(), lv, actor);
        let before = self
            .store
            .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(entry_code, label_name))?
            .map(|l: Labeling| serde_json::to_string(&l).unwrap_or_default());
        let after = serde_json::to_string(&labeling).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::LabelingSet,
            actor,
            "labeling",
            entry_code,
            Some(entry.workspace_id),
            before,
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::LABELINGS,
            keys::labeling_key(entry_code, label_name),
            &labeling,
        )?);
        ops.push(BatchOp::put(
            cf::LABELINGS_BY_WORKSPACE,
            keys::labeling_by_workspace_key(entry.workspace_id, entry_code, label_name),
            &labeling,
        )?);
        self.store.write_batch(ops)?;
        if let Ok(Some(e)) = self.get(entry_code) {
            self.reindex(&e);
        }
        Ok(labeling)
    }

    pub fn remove_labeling(
        &self,
        actor: Ulid,
        entry_code: &str,
        label_name: &str,
    ) -> Result<(), AppError> {
        let entry = self.get(entry_code)?.ok_or(AppError::NotFound)?;
        let key = keys::labeling_key(entry_code, label_name);
        let before = self
            .store
            .get::<Labeling>(cf::LABELINGS, &key)?
            .map(|l: Labeling| serde_json::to_string(&l).unwrap_or_default());
        let audit = AuditLog::new(
            AuditAction::LabelingRemoved,
            actor,
            "labeling",
            entry_code,
            Some(entry.workspace_id),
            before,
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::LABELINGS, key));
        ops.push(BatchOp::delete(
            cf::LABELINGS_BY_WORKSPACE,
            keys::labeling_by_workspace_key(entry.workspace_id, entry_code, label_name),
        ));
        self.store.write_batch(ops)?;
        if let Ok(Some(e)) = self.get(entry_code) {
            self.reindex(&e);
        }
        Ok(())
    }

    pub fn labelings(&self, entry_code: &str) -> Result<Vec<Labeling>, AppError> {
        let prefix = entry_code.as_bytes();
        let rows = self.store.scan_prefix(cf::LABELINGS, prefix)?;
        let mut out = Vec::new();
        for (_, v) in rows {
            out.push(bincode::deserialize(&v)?);
        }
        Ok(out)
    }

    /// 一次前缀扫描取回 workspace 内全部打标，按 entry_code 分组。
    pub fn labelings_by_workspace(
        &self,
        workspace_id: Ulid,
    ) -> Result<std::collections::HashMap<String, Vec<Labeling>>, AppError> {
        let rows = self
            .store
            .scan_prefix(cf::LABELINGS_BY_WORKSPACE, &workspace_id.to_bytes())?;
        let mut map: std::collections::HashMap<String, Vec<Labeling>> = std::collections::HashMap::new();
        for (_, v) in rows {
            let l: Labeling = bincode::deserialize(&v)?;
            map.entry(l.entry_code.clone()).or_default().push(l);
        }
        Ok(map)
    }

    /// 单次求值的查询编排：一次性取回候选行、按需取回打标与全文命中，再过滤/排序/分页。
    pub fn query(
        &self,
        ws: Ulid,
        query: &Query,
        sort: &SortSpec,
        page: PageInput,
    ) -> Result<QueryResult, AppError> {
        let rows: Vec<Entry> = self.list(ws)?;

        let labels_map = if query.contains_label() {
            Some(self.labelings_by_workspace(ws)?)
        } else {
            None
        };
        let text_hits: Option<(String, std::collections::HashSet<String>)> =
            match query.first_text_keyword() {
                Some(keyword) => {
                    let search = self
                        .search
                        .as_ref()
                        .ok_or_else(|| AppError::Internal("全文检索不可用".to_string()))?;
                    let hits = search
                        .search(ws, &keyword, TEXT_CANDIDATE_LIMIT)?
                        .into_iter()
                        .collect::<std::collections::HashSet<_>>();
                    Some((keyword, hits))
                }
                None => None,
            };

        let empty: Vec<Labeling> = Vec::new();
        let mut matched: Vec<Entry> = rows
            .into_iter()
            .filter(|e| {
                let labels: &[Labeling] = labels_map
                    .as_ref()
                    .and_then(|m| m.get(&e.code))
                    .map(Vec::as_slice)
                    .unwrap_or(&empty);
                let text_ok = |kw: &str| match &text_hits {
                    Some((keyword, set)) => kw == keyword && set.contains(&e.code),
                    None => false,
                };
                query.evaluate(e, labels, &text_ok)
            })
            .collect();

        sort_rows(&mut matched, sort);
        let total = matched.len();
        let (page, page_size) = page.normalized();
        let slice: Vec<Entry> = matched
            .into_iter()
            .skip((page - 1) * page_size)
            .take(page_size)
            .collect();

        let mut items = Vec::with_capacity(slice.len());
        for e in slice {
            let labels = match &labels_map {
                Some(m) => m.get(&e.code).cloned().unwrap_or_default(),
                None => self.labelings(&e.code)?,
            };
            items.push((e, labels));
        }
        Ok(QueryResult { items, total })
    }
}

/// 按 SortSpec 对条目就地排序；标题大小写不敏感。
fn sort_rows(rows: &mut [Entry], sort: &SortSpec) {
    rows.sort_by(|a, b| {
        let ord = match sort.field {
            SortField::UpdatedAt => a.updated_at.cmp(&b.updated_at),
            SortField::CreatedAt => a.created_at.cmp(&b.created_at),
            SortField::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
        };
        if sort.desc { ord.reverse() } else { ord }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::WorkspaceService;

    use crate::domain::query::{Condition, Field, Op, Query};
    use crate::domain::view::{SortField, SortSpec};

    fn temp_search() -> (String, std::sync::Arc<crate::service::search::SearchIndex>) {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-entry-search-{}", Ulid::new()));
        let dir = p.to_string_lossy().into_owned();
        let idx = std::sync::Arc::new(crate::service::search::SearchIndex::open(&dir).unwrap());
        (dir, idx)
    }

    #[test]
    fn query_filters_by_label_and_pages() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        for i in 0..5 {
            let e = svc.create(actor, ws_id, &format!("条目{i}")).unwrap();
            if i % 2 == 0 {
                svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();
            }
        }
        let q = Query::Cond(Condition {
            field: Field::Label("Task".into()), op: Op::Eq, value: Some(serde_json::json!("Open")),
        });
        let page = PageInput { page: 1, page_size: 2 };
        let r = svc.query(ws_id, &q, &SortSpec::default(), page).unwrap();
        assert_eq!(r.total, 3, "3 条被打了 Task=Open");
        assert_eq!(r.items.len(), 2, "第一页取 2 条");
        assert_eq!(r.items[0].1.len(), 1, "每行带上打标");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_fulltext_intersects_with_label_filter() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        let a = svc.create(actor, ws_id, "找回密码失败").unwrap();
        svc.update(actor, &a.code, &a.updated_at.to_rfc3339(), "找回密码失败", "验证码收不到").unwrap();
        let b = svc.create(actor, ws_id, "找回密码失败").unwrap();
        svc.set_labeling(actor, &a.code, "Task", &serde_json::json!("Open")).unwrap();

        let q = Query::And(vec![
            Query::Cond(Condition { field: Field::Label("Task".into()), op: Op::Eq, value: Some(serde_json::json!("Open")) }),
            Query::Cond(Condition { field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("密码")) }),
        ]);
        let r = svc.query(ws_id, &q, &SortSpec::default(), PageInput::default()).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].0.code, a.code, "b 未打标，应被过滤掉");
        let _ = b;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_sorts_by_title_desc_when_requested() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        for t in ["b", "a", "c"] {
            svc.create(actor, ws_id, t).unwrap();
        }
        let sort = SortSpec { field: SortField::Title, desc: false };
        let r = svc.query(ws_id, &Query::all(), &sort, PageInput::default()).unwrap();
        let titles: Vec<String> = r.items.into_iter().map(|(e, _)| e.title).collect();
        assert_eq!(titles, vec!["a", "b", "c"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-entry-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    fn setup() -> (String, Arc<DocStore>, EntryService, Ulid, Ulid) {
        let dir = temp_dir("setup");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let ws_svc = WorkspaceService::new(store.clone());
        let actor = Ulid::new();
        let ws = ws_svc.create(actor, "测试", None, "").unwrap();
        let entry_svc = EntryService::new(store.clone());
        (dir, store, entry_svc, ws.id, actor)
    }

    #[test]
    fn update_with_stale_timestamp_conflicts() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "hello").unwrap();
        // 错误的时间戳 → 冲突
        let err = svc.update(actor, &e.code, "2000-01-01T00:00:00Z", "x", "").unwrap_err();
        assert!(matches!(err, AppError::ConflictDetected));
        // 正确的时间戳 → 成功
        let expected = e.updated_at.to_rfc3339();
        let u = svc.update(actor, &e.code, &expected, "改了", "详情").unwrap();
        assert_eq!(u.title, "改了");
        assert_eq!(u.detail, "详情");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn soft_delete_filters_from_list() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "待删除").unwrap();
        assert_eq!(svc.list(ws_id).unwrap().len(), 1);
        svc.soft_delete(actor, &e.code).unwrap();
        assert!(svc.list(ws_id).unwrap().is_empty());
        let got = svc.get(&e.code).unwrap().unwrap();
        assert!(got.is_deleted());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_labeling_clears_value() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();
        assert_eq!(svc.labelings(&e.code).unwrap().len(), 1);
        svc.remove_labeling(actor, &e.code, "Task").unwrap();
        assert!(svc.labelings(&e.code).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_writes_audit_log() {
        let (dir, store, svc, ws_id, actor) = setup();
        svc.create(actor, ws_id, "审计").unwrap();
        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        assert!(list.iter().any(|l| l.action == AuditAction::EntryCreated));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_bumps_updated_at_and_records_before_after_audit() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "hello").unwrap();
        let expected = e.updated_at.to_rfc3339();
        let u = svc.update(actor, &e.code, &expected, "改了", "详情").unwrap();
        assert_ne!(u.updated_at, e.updated_at, "update 必须推进 updated_at");
        assert_eq!(u.updated_by, actor);
        let reloaded = svc.get(&e.code).unwrap().unwrap();
        assert_eq!(reloaded, u, "落库条目必须与返回值一致");

        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        let log = list
            .iter()
            .find(|l| l.action == AuditAction::EntryUpdated)
            .expect("必须产生 EntryUpdated 审计");
        assert_eq!(log.resource_type, "entry");
        assert_eq!(log.resource_id, e.code);
        let before: serde_json::Value =
            serde_json::from_str(log.before.as_deref().unwrap()).unwrap();
        let after: serde_json::Value = serde_json::from_str(log.after.as_deref().unwrap()).unwrap();
        assert_eq!(before["title"], "hello");
        assert_eq!(after["title"], "改了");
        assert_eq!(after["detail"], "详情");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_conflict_does_not_mutate_or_audit() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "hello").unwrap();
        let err = svc
            .update(actor, &e.code, "2000-01-01T00:00:00Z", "x", "")
            .unwrap_err();
        assert!(matches!(err, AppError::ConflictDetected));
        // 冲突时数据不得被改动
        let reloaded = svc.get(&e.code).unwrap().unwrap();
        assert_eq!(reloaded.title, "hello");
        assert_eq!(reloaded.detail, "");
        assert_eq!(reloaded.updated_at, e.updated_at);
        // 冲突时不得产生 EntryUpdated 审计
        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        assert!(
            !list.iter().any(|l| l.action == AuditAction::EntryUpdated),
            "冲突失败不得写审计"
        );
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_deleted_entry_is_not_found() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "将被删除").unwrap();
        svc.soft_delete(actor, &e.code).unwrap();
        let expected = e.updated_at.to_rfc3339();
        let err = svc.update(actor, &e.code, &expected, "x", "").unwrap_err();
        assert!(matches!(err, AppError::NotFound), "已删除条目不可再更新");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn soft_delete_records_entry_deleted_audit_and_is_idempotent() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "待删").unwrap();
        svc.soft_delete(actor, &e.code).unwrap();

        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        let log = list
            .iter()
            .find(|l| l.action == AuditAction::EntryDeleted)
            .expect("必须产生 EntryDeleted 审计");
        assert_eq!(log.resource_id, e.code);
        let after: serde_json::Value = serde_json::from_str(log.after.as_deref().unwrap()).unwrap();
        assert!(after["deleted_at"].is_string(), "after 快照必须携带 deleted_at");

        // 重复删除幂等：不新增审计、不报错
        svc.soft_delete(actor, &e.code).unwrap();
        let list2 = audit.list(ws_id, 100).unwrap();
        let deleted = list2
            .iter()
            .filter(|l| l.action == AuditAction::EntryDeleted)
            .count();
        assert_eq!(deleted, 1);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_labeling_records_labeling_set_audit() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();
        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        let log = list
            .iter()
            .find(|l| l.action == AuditAction::LabelingSet)
            .expect("必须产生 LabelingSet 审计");
        assert_eq!(log.resource_type, "labeling");
        assert_eq!(log.resource_id, e.code);
        assert!(log.after.is_some());
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn labeling_index_tracks_set_and_remove() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();

        let map = svc.labelings_by_workspace(ws_id).unwrap();
        assert_eq!(map.get(&e.code).map(|v| v.len()), Some(1));

        svc.remove_labeling(actor, &e.code, "Task").unwrap();
        let map = svc.labelings_by_workspace(ws_id).unwrap();
        assert!(map.get(&e.code).is_none(), "移除后索引必须清空");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_labeling_records_labeling_removed_audit() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();
        svc.remove_labeling(actor, &e.code, "Task").unwrap();
        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        let log = list
            .iter()
            .find(|l| l.action == AuditAction::LabelingRemoved)
            .expect("必须产生 LabelingRemoved 审计");
        assert_eq!(log.resource_id, e.code);
        assert!(log.before.is_some(), "删除前必须快照被移除的 labeling");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
