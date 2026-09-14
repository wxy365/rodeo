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
    /// 命中的条目实际带有的标签名（去重排序），供前端提示可用标签。
    pub label_names: Vec<String>,
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
        // 归档与软删除一样，都让条目退出全文检索：归档条目不该再被搜索命中。
        let out_of_play = entry.is_deleted() || self.is_archived(&entry.code).unwrap_or(false);
        if out_of_play {
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

    /// 是否处于归档状态。
    pub fn is_archived(&self, code: &str) -> Result<bool, AppError> {
        Ok(self.archived_at(code)?.is_some())
    }

    /// 归档时间（RFC3339）；未归档时为 None。
    pub fn archived_at(&self, code: &str) -> Result<Option<String>, AppError> {
        let Some(raw) = self.store.get_raw(cf::ENTRIES_ARCHIVED, code.as_bytes())? else {
            return Ok(None);
        };
        Ok(Some(String::from_utf8_lossy(&raw).into_owned()))
    }

    /// 归档：只写标记，条目与打标全部保留，只是移出默认视图与全文检索。幂等。
    /// 已删除的条目不可归档（回到 NotFound），因为删除是比归档更彻底的状态。
    pub fn archive(&self, actor: Ulid, code: &str) -> Result<(), AppError> {
        let entry = self.get(code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        if self.is_archived(code)? {
            return Ok(());
        }
        let stamp = Utc::now().to_rfc3339();
        let audit = AuditLog::new(
            AuditAction::EntryArchived,
            actor,
            "entry",
            code,
            Some(entry.workspace_id),
            Some(entry_snapshot(&entry, None)),
            Some(entry_snapshot(&entry, Some(&stamp))),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put_raw(
            cf::ENTRIES_ARCHIVED,
            code.as_bytes().to_vec(),
            stamp.into_bytes(),
        ));
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(())
    }

    /// 取消归档。未归档时直接返回（幂等）。
    pub fn unarchive(&self, actor: Ulid, code: &str) -> Result<(), AppError> {
        let entry = self.get(code)?.ok_or(AppError::NotFound)?;
        let Some(stamp) = self.archived_at(code)? else {
            return Ok(());
        };
        let audit = AuditLog::new(
            AuditAction::EntryUnarchived,
            actor,
            "entry",
            code,
            Some(entry.workspace_id),
            Some(entry_snapshot(&entry, Some(&stamp))),
            Some(entry_snapshot(&entry, None)),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::ENTRIES_ARCHIVED, code.as_bytes().to_vec()));
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(())
    }

    /// 列出工作空间内已归档的条目（不含已删除），按归档时间倒序。
    pub fn list_archived(&self, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
        let rows = self.store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &workspace_id.to_bytes())?;
        let mut entries = Vec::new();
        for (key, _) in rows {
            if key.len() <= 16 {
                continue;
            }
            let code = std::str::from_utf8(&key[16..]).unwrap_or("").to_string();
            let Some(e) = self.store.get::<Entry>(cf::ENTRIES, code.as_bytes())? else {
                continue;
            };
            if e.is_deleted() || !self.is_archived(&code)? {
                continue;
            }
            entries.push(e);
        }
        entries.sort_by(|a, b| {
            self.archived_at(&b.code)
                .ok()
                .flatten()
                .cmp(&self.archived_at(&a.code).ok().flatten())
        });
        Ok(entries)
    }

    /// 工作空间内的「在视图内」条目：不含已删除，也不含已归档。
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
                if !e.is_deleted() && !self.is_archived(&code)? {
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

    /// 批量给多个条目写多个标签值。所有条目必须同属一个工作空间；全部取值先按 schema
    /// 校验，再合并成一次 `write_batch`，因此任一取值非法时整批不写入，不会只写一半。
    /// 返回实际写入的 Labeling 条数（条目数 × 标签数）。
    pub fn set_labelings(
        &self,
        actor: Ulid,
        entry_codes: &[String],
        labelings: &[(String, serde_json::Value)],
    ) -> Result<usize, AppError> {
        if entry_codes.is_empty() {
            return Err(AppError::InvalidQuery("未选择任何条目".to_string()));
        }
        if labelings.is_empty() {
            return Err(AppError::InvalidQuery("未填写任何标签".to_string()));
        }
        let mut entries = Vec::with_capacity(entry_codes.len());
        for code in entry_codes {
            entries.push(self.get(code)?.ok_or(AppError::NotFound)?);
        }
        let ws_id = entries[0].workspace_id;
        if entries.iter().any(|e| e.workspace_id != ws_id) {
            return Err(AppError::InvalidQuery(
                "选中的条目不属于同一个工作空间".to_string(),
            ));
        }
        // 先把所有取值解析出来；这一步失败则什么都没写。
        let mut resolved = Vec::with_capacity(labelings.len());
        for (name, value) in labelings {
            let schema = self
                .store
                .get::<LabelSchema>(
                    cf::LABEL_SCHEMAS,
                    &keys::label_schema_key(ws_id, name),
                )?
                .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {name}")))?;
            resolved.push((name.clone(), LabelValue::from_json(value, &schema)?));
        }

        let mut ops = Vec::new();
        for entry in &entries {
            for (name, lv) in &resolved {
                let labeling =
                    Labeling::new(entry.code.clone(), name.clone(), lv.clone(), actor);
                let before = self
                    .store
                    .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(&entry.code, name))?
                    .map(|l: Labeling| serde_json::to_string(&l).unwrap_or_default());
                let after = serde_json::to_string(&labeling).unwrap_or_default();
                let audit = AuditLog::new(
                    AuditAction::LabelingSet,
                    actor,
                    "labeling",
                    &entry.code,
                    Some(ws_id),
                    before,
                    Some(after),
                );
                ops.extend(audit_ops(&audit)?);
                ops.push(BatchOp::put(
                    cf::LABELINGS,
                    keys::labeling_key(&entry.code, name),
                    &labeling,
                )?);
                ops.push(BatchOp::put(
                    cf::LABELINGS_BY_WORKSPACE,
                    keys::labeling_by_workspace_key(ws_id, &entry.code, name),
                    &labeling,
                )?);
            }
        }
        let written = entries.len() * resolved.len();
        self.store.write_batch(ops)?;
        for entry in &entries {
            if let Ok(Some(e)) = self.get(&entry.code) {
                self.reindex(&e);
            }
        }
        Ok(written)
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

    /// `labelings_by_workspace` 列族为空时，遍历 `ENTRIES`（取 workspace_id）+
    /// `LABELINGS` 重建次级索引；已填充则直接返回 0（幂等）。
    /// 返回回填的 labeling 条数。
    pub fn labelings_by_workspace_backfill(&self, store: &DocStore) -> Result<usize, AppError> {
        if !store.scan_prefix(cf::LABELINGS_BY_WORKSPACE, b"")?.is_empty() {
            return Ok(0);
        }
        let mut ops = Vec::new();
        let mut count = 0;
        for (_, ev) in store.scan_prefix(cf::ENTRIES, b"")? {
            let entry: Entry = bincode::deserialize(&ev)?;
            for (_, lv) in store.scan_prefix(cf::LABELINGS, entry.code.as_bytes())? {
                let l: Labeling = bincode::deserialize(&lv)?;
                ops.push(BatchOp::put(
                    cf::LABELINGS_BY_WORKSPACE,
                    keys::labeling_by_workspace_key(entry.workspace_id, &l.entry_code, &l.label_name),
                    &l,
                )?);
                count += 1;
            }
        }
        if !ops.is_empty() {
            store.write_batch(ops)?;
        }
        Ok(count)
    }

    /// 侧栏计数：复用 `query` 的 total（只取一页一条，避免重复过滤逻辑）。
    pub fn count(&self, ws: Ulid, query: &Query) -> Result<usize, AppError> {
        let r = self.query(
            ws,
            query,
            &SortSpec::default(),
            PageInput { page: 1, page_size: 1 },
        )?;
        Ok(r.total)
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

        // 一次取回工作空间全部打标：既给过滤用，也用来汇总「本视图条目带到的标签」。
        let labels_map = self.labelings_by_workspace(ws)?;
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
        let labels_of = |code: &str| -> &[Labeling] {
            labels_map.get(code).map(Vec::as_slice).unwrap_or(&empty)
        };
        let mut matched: Vec<Entry> = rows
            .into_iter()
            .filter(|e| {
                let labels = labels_of(&e.code);
                let text_ok = |kw: &str| match &text_hits {
                    Some((keyword, set)) => kw == keyword && set.contains(&e.code),
                    None => false,
                };
                query.evaluate(e, labels, &text_ok)
            })
            .collect();

        // 命中集合里出现过的标签名（跨分页，去重排序）。
        let mut label_names: Vec<String> = matched
            .iter()
            .flat_map(|e| labels_of(&e.code).iter().map(|l| l.label_name.clone()))
            .collect();
        label_names.sort();
        label_names.dedup();

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
            let labels = labels_of(&e.code).to_vec();
            items.push((e, labels));
        }
        Ok(QueryResult { items, total, label_names })
    }
}

/// 归档审计快照：条目本身不变，只是补上/清掉 archived_at，便于前端 diff 出
/// 「归档时间: null → 2026-…」。
fn entry_snapshot(entry: &Entry, archived_at: Option<&str>) -> String {
    serde_json::json!({
        "code": entry.code,
        "title": entry.title,
        "archived_at": archived_at,
    })
    .to_string()
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
                svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null)).unwrap();
            }
        }
        let q = Query::Cond(Condition {
            field: Field::Label("Task".into()), op: Op::Present, value: None,
        });
        let page = PageInput { page: 1, page_size: 2 };
        let r = svc.query(ws_id, &q, &SortSpec::default(), page).unwrap();
        assert_eq!(r.total, 3, "3 条被打了 Task");
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
        svc.set_labeling(actor, &a.code, "Task", &serde_json::json!(null)).unwrap();

        let q = Query::And(vec![
            Query::Cond(Condition { field: Field::Label("Task".into()), op: Op::Present, value: None }),
            Query::Cond(Condition { field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("密码")) }),
        ]);
        let r = svc.query(ws_id, &q, &SortSpec::default(), PageInput::default()).unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].0.code, a.code, "b 未打标，应被过滤掉");
        let _ = b;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_reports_label_names_of_matched_entries() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        let a = svc.create(actor, ws_id, "甲").unwrap();
        svc.set_labeling(actor, &a.code, "Task", &serde_json::json!(null)).unwrap();
        svc.set_labeling(actor, &a.code, "Bug", &serde_json::json!(null)).unwrap();
        let b = svc.create(actor, ws_id, "乙").unwrap();
        svc.set_labeling(actor, &b.code, "Task", &serde_json::json!(null)).unwrap();

        // 不过滤时两个标签都出现；跨分页汇总，去重排序。
        let r = svc.query(ws_id, &Query::all(), &SortSpec::default(), PageInput::default()).unwrap();
        assert_eq!(r.label_names, vec!["Bug".to_string(), "Task".to_string()]);

        // 过滤到只剩「乙」时，只有它带过的 Task。
        let only_b = Query::Cond(Condition {
            field: Field::Text, op: Op::Contains, value: Some(serde_json::json!("乙")),
        });
        let r = svc.query(ws_id, &only_b, &SortSpec::default(), PageInput::default()).unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.label_names, vec!["Task".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_sorts_by_title_both_directions() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search);
        for t in ["b", "a", "c"] {
            svc.create(actor, ws_id, t).unwrap();
        }
        let asc = SortSpec { field: SortField::Title, desc: false };
        let r = svc.query(ws_id, &Query::all(), &asc, PageInput::default()).unwrap();
        let titles: Vec<String> = r.items.into_iter().map(|(e, _)| e.title).collect();
        assert_eq!(titles, vec!["a", "b", "c"]);

        // desc 是默认排序方向，必须单独覆盖。
        let desc = SortSpec { field: SortField::Title, desc: true };
        let r = svc.query(ws_id, &Query::all(), &desc, PageInput::default()).unwrap();
        let titles: Vec<String> = r.items.into_iter().map(|(e, _)| e.title).collect();
        assert_eq!(titles, vec!["c", "b", "a"]);
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
    fn archive_removes_from_list_but_keeps_entry_and_is_reversible() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "陈旧条目").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null)).unwrap();
        assert_eq!(svc.list(ws_id).unwrap().len(), 1);

        svc.archive(actor, &e.code).unwrap();
        assert!(svc.list(ws_id).unwrap().is_empty(), "归档后不该出现在默认列表");
        assert!(svc.is_archived(&e.code).unwrap());
        assert!(svc.get(&e.code).unwrap().is_some(), "归档不删除数据");
        assert_eq!(svc.labelings(&e.code).unwrap().len(), 1, "打标保留");
        // 归档列表里能看到。
        let archived = svc.list_archived(ws_id).unwrap();
        assert_eq!(archived.len(), 1);
        assert_eq!(archived[0].code, e.code);

        svc.unarchive(actor, &e.code).unwrap();
        assert!(!svc.is_archived(&e.code).unwrap());
        assert_eq!(svc.list(ws_id).unwrap().len(), 1);
        assert!(svc.list_archived(ws_id).unwrap().is_empty());

        // 幂等：重复归档/取消归档都不报错。
        svc.archive(actor, &e.code).unwrap();
        svc.archive(actor, &e.code).unwrap();
        svc.unarchive(actor, &e.code).unwrap();
        svc.unarchive(actor, &e.code).unwrap();
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_records_audit_once_and_is_idempotent() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "待归档").unwrap();
        svc.archive(actor, &e.code).unwrap();
        svc.archive(actor, &e.code).unwrap();

        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        let log = list
            .iter()
            .find(|l| l.action == AuditAction::EntryArchived)
            .expect("必须产生 EntryArchived 审计");
        assert_eq!(log.resource_id, e.code);
        let after: serde_json::Value = serde_json::from_str(log.after.as_deref().unwrap()).unwrap();
        assert!(after["archived_at"].is_string(), "after 快照必须携带 archived_at");
        assert_eq!(
            list.iter().filter(|l| l.action == AuditAction::EntryArchived).count(),
            1,
            "重复归档只留一条审计"
        );

        svc.unarchive(actor, &e.code).unwrap();
        let list2 = audit.list(ws_id, 100).unwrap();
        assert!(list2.iter().any(|l| l.action == AuditAction::EntryUnarchived));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_and_delete_are_independent() {
        let (dir, store, svc, ws_id, actor) = setup();
        let a = svc.create(actor, ws_id, "先归档再删除").unwrap();
        svc.archive(actor, &a.code).unwrap();
        svc.soft_delete(actor, &a.code).unwrap();
        assert!(svc.list_archived(ws_id).unwrap().is_empty(), "已删除的不该出现在归档列表");

        // 已删除的条目不能再归档。
        let b = svc.create(actor, ws_id, "先删除").unwrap();
        svc.soft_delete(actor, &b.code).unwrap();
        assert!(matches!(svc.archive(actor, &b.code), Err(AppError::NotFound)));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archived_entries_are_excluded_from_fulltext_search() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let svc = EntryService::with_search(store.clone(), search.clone());
        let e = svc.create(actor, ws_id, "独一无二的关键词").unwrap();
        assert_eq!(search.num_docs(), 1);

        svc.archive(actor, &e.code).unwrap();
        assert_eq!(search.num_docs(), 0, "归档后从检索索引移除");
        assert!(search.search(ws_id, "关键词", 10).unwrap().is_empty());

        svc.unarchive(actor, &e.code).unwrap();
        assert_eq!(search.num_docs(), 1, "取消归档后重新入索引");
        assert_eq!(search.search(ws_id, "关键词", 10).unwrap(), vec![e.code.clone()]);
        std::fs::remove_dir_all(&_sdir).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_backfill_skips_archived_entries() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let svc = EntryService::new(store.clone());
        let keep = svc.create(actor, ws_id, "保留的条目").unwrap();
        let gone = svc.create(actor, ws_id, "归档的条目").unwrap();
        svc.archive(actor, &gone.code).unwrap();

        // 空索引回填：按 CF 里的归档标记跳过归档条目，而不是照单全收。
        let (sdir, fresh) = temp_search();
        assert_eq!(fresh.backfill(&store).unwrap(), 1, "只回填未归档条目");
        assert!(fresh.search(ws_id, "归档", 10).unwrap().is_empty());
        assert_eq!(fresh.search(ws_id, "保留", 10).unwrap(), vec![keep.code.clone()]);
        std::fs::remove_dir_all(&sdir).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_labeling_clears_value() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null)).unwrap();
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
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null)).unwrap();
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

    /// 造一个枚举标签 Status（Open/Done），用于验证取值校验。
    fn add_status_schema(store: &Arc<DocStore>, ws_id: Ulid, actor: Ulid) {
        crate::service::LabelService::new(store.clone())
            .create_schema(
                actor,
                ws_id,
                crate::service::label::LabelSchemaInput {
                    name: "Status".into(),
                    title: "状态".into(),
                    value_type: crate::domain::LabelValueType::Enum,
                    enum_values: vec!["Open".to_string(), "Done".to_string()],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                },
            )
            .unwrap();
    }

    #[test]
    fn batch_set_labelings_writes_all_cross_product_and_audits() {
        let (dir, store, svc, ws_id, actor) = setup();
        add_status_schema(&store, ws_id, actor);
        let a = svc.create(actor, ws_id, "甲").unwrap();
        let b = svc.create(actor, ws_id, "乙").unwrap();
        let pairs = vec![
            ("Task".to_string(), serde_json::json!(null)),
            ("Status".to_string(), serde_json::json!("Open")),
        ];
        let codes = vec![a.code.clone(), b.code.clone()];

        let written = svc.set_labelings(actor, &codes, &pairs).unwrap();
        assert_eq!(written, 4, "2 条目 × 2 标签");

        for code in &codes {
            let labels = svc.labelings(code).unwrap();
            assert_eq!(labels.len(), 2, "{code} 应写入两个标签");
            let status = labels.iter().find(|l| l.label_name == "Status").unwrap();
            assert_eq!(status.value.to_json(), serde_json::json!("Open"));
        }

        let audit = crate::service::audit::AuditService::new(store.clone());
        let sets = audit
            .list(ws_id, 100)
            .unwrap()
            .into_iter()
            .filter(|l| l.action == AuditAction::LabelingSet)
            .count();
        assert_eq!(sets, 4, "每条 labeling 都要有自己的审计");

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn batch_set_labelings_is_atomic_when_a_value_is_invalid() {
        let (dir, store, svc, ws_id, actor) = setup();
        add_status_schema(&store, ws_id, actor);
        let a = svc.create(actor, ws_id, "甲").unwrap();
        let b = svc.create(actor, ws_id, "乙").unwrap();
        let codes = vec![a.code.clone(), b.code.clone()];
        // 第二个标签取值不在枚举内：整批都不应写入，也不该留下审计。
        let pairs = vec![
            ("Task".to_string(), serde_json::json!(null)),
            ("Status".to_string(), serde_json::json!("Nope")),
        ];

        assert!(svc.set_labelings(actor, &codes, &pairs).is_err());
        for code in &codes {
            assert!(svc.labelings(code).unwrap().is_empty(), "{code} 不应被写入");
        }
        // 列表里会有建 schema 留下的审计，这里只关心有没有 LabelingSet 漏出来。
        let audit = crate::service::audit::AuditService::new(store.clone());
        let sets = audit
            .list(ws_id, 100)
            .unwrap()
            .into_iter()
            .filter(|l| l.action == AuditAction::LabelingSet)
            .count();
        assert_eq!(sets, 0, "整批失败不应留下 LabelingSet 审计");

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn batch_set_labelings_rejects_cross_workspace_selection() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let a = svc.create(actor, ws_id, "甲").unwrap();
        // 另一个工作空间的条目：与 a 混选必须被拒绝，否则等于越权改别的空间。
        let other = crate::domain::Workspace::new(
            "别的".to_string(),
            "other".to_string(),
            String::new(),
            actor,
        );
        let b = svc.create(actor, other.id, "乙").unwrap();
        let codes = vec![a.code.clone(), b.code.clone()];
        let pairs = vec![("Task".to_string(), serde_json::json!(null))];

        assert!(svc.set_labelings(actor, &codes, &pairs).is_err());
        assert!(svc.labelings(&a.code).unwrap().is_empty());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn labeling_index_tracks_set_and_remove() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null)).unwrap();

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
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null)).unwrap();
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
