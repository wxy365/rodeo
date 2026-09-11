use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, LabelSchema, Query, SortSpec, TitleColorRule, View};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::label::check_color;
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 校验标题颜色规则的取色格式（`#rrggbb`）。
pub struct ViewService {
    store: Arc<DocStore>,
}

impl ViewService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    fn schemas(&self, ws: Ulid) -> Result<Vec<LabelSchema>, AppError> {
        let rows = self.store.scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())?;
        rows.into_iter()
            .map(|(_, v)| bincode::deserialize::<LabelSchema>(&v).map_err(Into::into))
            .collect()
    }

    fn validate(
        &self,
        ws: Ulid,
        name: &str,
        query: &Query,
        columns: &[String],
        title_colors: &[TitleColorRule],
    ) -> Result<(), AppError> {
        if name.trim().is_empty() {
            return Err(AppError::InvalidQuery("视图名称不能为空".to_string()));
        }
        let schemas = self.schemas(ws)?;
        for c in columns {
            if !schemas.iter().any(|s| &s.name == c) {
                return Err(AppError::InvalidQuery(format!("列引用了不存在的标签: {c}")));
            }
        }
        query.validate(&schemas)?;
        // 标题色规则：颜色格式 + 条件 schema 校验（§7）。
        for r in title_colors {
            check_color(&r.color)?;
            r.query.validate(&schemas)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        actor: Ulid,
        ws: Ulid,
        name: &str,
        query: Query,
        sort: SortSpec,
        columns: Vec<String>,
        is_shared: bool,
        title_colors: Vec<TitleColorRule>,
    ) -> Result<View, AppError> {
        self.validate(ws, name, &query, &columns, &title_colors)?;
        let now = Utc::now();
        let view = View {
            id: Ulid::new(),
            workspace_id: ws,
            name: name.trim().to_string(),
            query,
            sort,
            columns,
            is_shared,
            owner_id: actor,
            created_at: now,
            updated_at: now,
            title_colors,
        };
        let audit = AuditLog::new(
            AuditAction::ViewCreated,
            actor,
            "view",
            &view.id.to_string(),
            Some(ws),
            None,
            Some(serde_json::to_string(&view).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::VIEWS, keys::view_key(view.id).to_vec(), &view)?);
        ops.push(BatchOp::put_raw(
            cf::VIEWS_BY_WORKSPACE,
            keys::view_by_workspace_key(ws, view.id).to_vec(),
            Vec::new(),
        ));
        self.store.write_batch(ops)?;
        Ok(view)
    }

    pub fn list(&self, actor: Ulid, ws: Ulid) -> Result<Vec<View>, AppError> {
        let rows = self.store.scan_prefix(cf::VIEWS_BY_WORKSPACE, &ws.to_bytes())?;
        let mut out = Vec::new();
        for (key, _) in rows {
            // 复合键为 (workspace_id, view_id)，各 16 字节。
            if key.len() < 32 {
                continue;
            }
            let id = Ulid::from_bytes(key[16..32].try_into().unwrap());
            if let Some(v) = self.store.get::<View>(cf::VIEWS, &keys::view_key(id))? {
                if v.is_shared || v.owner_id == actor {
                    out.push(v);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get(&self, id: Ulid) -> Result<Option<View>, AppError> {
        self.store.get(cf::VIEWS, &keys::view_key(id))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        actor: Ulid,
        id: Ulid,
        name: &str,
        query: Query,
        sort: SortSpec,
        columns: Vec<String>,
        is_shared: bool,
        title_colors: Vec<TitleColorRule>,
    ) -> Result<View, AppError> {
        let mut view = self.get(id)?.ok_or(AppError::NotFound)?;
        self.validate(view.workspace_id, name, &query, &columns, &title_colors)?;
        let before = serde_json::to_string(&view).unwrap_or_default();
        view.name = name.trim().to_string();
        view.query = query;
        view.sort = sort;
        view.columns = columns;
        view.is_shared = is_shared;
        view.title_colors = title_colors;
        view.updated_at = Utc::now();
        let after = serde_json::to_string(&view).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::ViewUpdated,
            actor,
            "view",
            &id.to_string(),
            Some(view.workspace_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::VIEWS, keys::view_key(id).to_vec(), &view)?);
        self.store.write_batch(ops)?;
        Ok(view)
    }

    pub fn delete(&self, actor: Ulid, id: Ulid) -> Result<(), AppError> {
        let view = self.get(id)?.ok_or(AppError::NotFound)?;
        let audit = AuditLog::new(
            AuditAction::ViewDeleted,
            actor,
            "view",
            &id.to_string(),
            Some(view.workspace_id),
            Some(serde_json::to_string(&view).unwrap_or_default()),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::VIEWS, keys::view_key(id).to_vec()));
        ops.push(BatchOp::delete(
            cf::VIEWS_BY_WORKSPACE,
            keys::view_by_workspace_key(view.workspace_id, id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::query::Query;
    use crate::service::WorkspaceService;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-view-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    fn setup() -> (String, Arc<DocStore>, ViewService, Ulid, Ulid) {
        let dir = temp_dir("setup");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let ws_svc = WorkspaceService::new(store.clone());
        let actor = Ulid::new();
        let ws = ws_svc.create(actor, "测试", None, "").unwrap();
        let svc = ViewService::new(store.clone());
        (dir, store, svc, ws.id, actor)
    }

    #[test]
    fn create_list_and_delete_with_audit() {
        let (dir, store, svc, ws, actor) = setup();
        let v = svc
            .create(actor, ws, "全部任务", Query::all(), SortSpec::default(), vec!["Task".into()], false, vec![])
            .unwrap();
        assert_eq!(v.name, "全部任务");
        assert!(!v.is_shared);

        let mine = svc.list(actor, ws).unwrap();
        assert_eq!(mine.len(), 1);

        // 他人看不到非共享视图
        assert!(svc.list(Ulid::new(), ws).unwrap().is_empty());

        // 共享视图对所有人可见
        let shared = svc
            .create(actor, ws, "看板", Query::all(), SortSpec::default(), vec![], true, vec![])
            .unwrap();
        assert_eq!(svc.list(Ulid::new(), ws).unwrap().len(), 1);
        assert_eq!(svc.list(Ulid::new(), ws).unwrap()[0].id, shared.id);

        svc.delete(actor, v.id).unwrap();
        assert!(svc.get(v.id).unwrap().is_none());

        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&crate::domain::AuditAction::ViewCreated));
        assert!(actions.contains(&crate::domain::AuditAction::ViewDeleted));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_rejects_unknown_column_and_bad_query() {
        let (dir, _store, svc, ws, actor) = setup();
        let bad_col = svc.create(actor, ws, "x", Query::all(), SortSpec::default(), vec!["Nope".into()], false, vec![]);
        assert!(matches!(bad_col.unwrap_err(), AppError::InvalidQuery(_)));

        let bad_q = Query::Cond(crate::domain::Condition {
            field: crate::domain::Field::Label("Nope".into()),
            op: crate::domain::Op::Present,
            value: None,
        });
        assert!(matches!(
            svc.create(actor, ws, "x", bad_q, SortSpec::default(), vec![], false, vec![]).unwrap_err(),
            AppError::InvalidQuery(_)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_changes_fields_and_audits() {
        let (dir, store, svc, ws, actor) = setup();
        let v = svc.create(actor, ws, "a", Query::all(), SortSpec::default(), vec![], false, vec![]).unwrap();
        let u = svc
            .update(actor, v.id, "b", Query::all(), SortSpec::default(), vec!["Task".into()], true, vec![])
            .unwrap();
        assert_eq!(u.name, "b");
        assert!(u.is_shared);
        assert_eq!(u.columns, vec!["Task"]);

        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&crate::domain::AuditAction::ViewUpdated));
        std::fs::remove_dir_all(&dir).ok();
    }
}
