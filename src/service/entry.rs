use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{
    generate_entry_code, AuditAction, AuditLog, Entry, LabelSchema, LabelValue, Labeling,
};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct EntryService {
    store: Arc<DocStore>,
}

impl EntryService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::WorkspaceService;

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
