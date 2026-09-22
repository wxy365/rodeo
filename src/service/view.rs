use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, LabelSchema, Query, SortSpec, TitleColorRule, View};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::label::check_color;
use crate::storage::{cf, keys, BatchOp, DocStore};

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

    /// 基础视图的 id：每个 workspace 一条指针，指向那个「始终存在、不可删除」的视图。
    /// 代码沿用 `default` 一词（`is_default` / `DEFAULT_VIEWS`），界面上一律叫「基础视图」。
    pub fn default_view_id(&self, ws: Ulid) -> Result<Option<Ulid>, AppError> {
        let key = keys::default_view_key(ws);
        match self.store.get_raw(cf::DEFAULT_VIEWS, &key)? {
            Some(bytes) if bytes.len() == 16 => {
                Ok(Ulid::from_bytes(bytes.as_slice().try_into().unwrap()).into())
            }
            _ => Ok(None),
        }
    }

    /// 确保 workspace 有基础视图：不存在（或指针失效）就新建一个「包含全部条目」的基础视图。
    /// 幂等——已有基础视图时直接返回，不产生多余的审计记录。
    pub fn ensure_default(&self, actor: Ulid, ws: Ulid) -> Result<View, AppError> {
        if let Some(id) = self.default_view_id(ws)? {
            if let Some(v) = self.get(id)? {
                return Ok(v);
            }
        }
        let view = self.build(ws, "基础视图", Query::all(), SortSpec::default(), vec![], true, vec![], actor);
        self.store.put(cf::VIEWS, &keys::view_key(view.id), &view)?;
        self.store.put_raw(
            cf::VIEWS_BY_WORKSPACE,
            &keys::view_by_workspace_key(ws, view.id),
            &[],
        )?;
        self.store
            .put_raw(cf::DEFAULT_VIEWS, &keys::default_view_key(ws), &view.id.to_bytes())?;
        if let Err(e) = self.audit(actor, &view, AuditAction::ViewCreated, None) {
            tracing::warn!("基础视图审计写入失败: {e}");
        }
        Ok(view)
    }

    /// 把存量基础视图的名字从「默认视图」改成「基础视图」。幂等——改完再扫不会命中。
    /// 只动名字：名字是服务端播种的固定文案，没有用户意图在里面。
    /// 返回修好的条数。
    pub fn repair_default_view_name(&self) -> Result<usize, AppError> {
        let mut ops = Vec::new();
        for (key, value) in self.store.scan_prefix(cf::DEFAULT_VIEWS, b"")? {
            if key.len() != 16 || value.len() != 16 {
                continue;
            }
            let id = Ulid::from_bytes(value.as_slice().try_into().unwrap());
            // 指针悬空（视图已被清掉）留给 ensure_default 补建，这里不碰。
            let Some(mut view) = self.get(id)? else { continue };
            if view.name != "默认视图" {
                continue;
            }
            view.name = "基础视图".to_string();
            view.updated_at = Utc::now();
            ops.push(BatchOp::put(cf::VIEWS, keys::view_key(id).to_vec(), &view)?);
        }
        let repaired = ops.len();
        if repaired > 0 {
            self.store.write_batch(ops)?;
            tracing::info!("修复 {repaired} 个基础视图的名称");
        }
        Ok(repaired)
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
        let view = self.build(ws, name, query, sort, columns, is_shared, title_colors, actor);
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

    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        ws: Ulid,
        name: &str,
        query: Query,
        sort: SortSpec,
        columns: Vec<String>,
        is_shared: bool,
        title_colors: Vec<TitleColorRule>,
        owner: Ulid,
    ) -> View {
        let now = Utc::now();
        View {
            id: Ulid::new(),
            workspace_id: ws,
            name: name.trim().to_string(),
            query,
            sort,
            columns,
            is_shared,
            owner_id: owner,
            created_at: now,
            updated_at: now,
            title_colors,
        }
    }

    fn audit(
        &self,
        actor: Ulid,
        view: &View,
        action: AuditAction,
        before: Option<String>,
    ) -> Result<(), AppError> {
        let log = AuditLog::new(
            action,
            actor,
            "view",
            &view.id.to_string(),
            Some(view.workspace_id),
            before,
            Some(serde_json::to_string(view).unwrap_or_default()),
        );
        self.store.write_batch(audit_ops(&log)?)
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
            match self.store.get::<View>(cf::VIEWS, &keys::view_key(id)) {
                Ok(Some(v)) if v.is_shared || v.owner_id == actor => out.push(v),
                Ok(_) => {}
                // 读不出来的记录（旧编码残留）不该拖垮整个视图列表：跳过并记一笔。
                Err(e) => tracing::warn!("跳过无法读取的视图 {id}: {e}"),
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        // 基础视图永远置顶，方便用户一眼找到「全部内容」的入口。
        if let Some(def) = self.default_view_id(ws)? {
            if let Some(i) = out.iter().position(|v| v.id == def) {
                let v = out.remove(i);
                out.insert(0, v);
            }
        }
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
        let is_default = self.default_view_id(view.workspace_id)? == Some(id);
        let before = serde_json::to_string(&view).unwrap_or_default();
        if is_default {
            // 基础视图只认列与标题色：名字是固定概念，查询条件在它上面只作临时过滤
            // （要留存请「另存为新视图」），排序同样是 ad-hoc 的——三者都不该被这次调用改写。
            view.columns = columns;
            view.title_colors = title_colors;
        } else {
            view.name = name.trim().to_string();
            view.query = query;
            view.sort = sort;
            view.columns = columns;
            view.title_colors = title_colors;
        }
        // 基础视图必须对所有成员可见，否则他人列表里就少了这个入口。
        view.is_shared = is_shared || is_default;
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
        if self.default_view_id(view.workspace_id)? == Some(id) {
            return Err(AppError::InvalidQuery(
                "基础视图不可删除；需要留存的筛选请另存为新视图".to_string(),
            ));
        }
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
    use crate::domain::SortField;
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
            .create(actor, ws, "全部内容", Query::all(), SortSpec::default(), vec!["Task".into()], false, vec![])
            .unwrap();
        assert_eq!(v.name, "全部内容");
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
    fn view_with_value_conditions_survives_save_and_read_back() {
        use crate::domain::{Condition, Field, LabelValueType, Op};
        use crate::service::LabelService;

        let (dir, store, svc, ws, actor) = setup();
        // 带值条件需要 enum 标签；内置 Task/Bug 是无值标签，只能 present/absent。
        LabelService::new(store.clone())
            .create_schema(
                actor,
                ws,
                crate::service::label::LabelSchemaInput {
                    name: "Status".into(),
                    title: "状态".into(),
                    value_type: LabelValueType::Enum,
                    enum_values: vec!["Open".into(), "Done".into()],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap();
        let cond = |op| {
            Query::Cond(Condition {
                field: Field::Label("Status".into()),
                op,
                value: Some(serde_json::json!("Open")),
            })
        };
        // 既覆盖条件本身，也覆盖标题色规则里的条件——两者都内联了带值的 AST。
        let v = svc
            .create(
                actor,
                ws,
                "带值条件",
                cond(Op::Eq),
                SortSpec::default(),
                vec![],
                false,
                vec![TitleColorRule { query: cond(Op::Ne), color: "#ff0000".into() }],
            )
            .unwrap();

        // 列表 / 单读 / 更新都要能把落库的视图反序列化回来：这里曾是「保存视图」报错的位置。
        let listed = svc.list(actor, ws).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].query, v.query);
        assert_eq!(listed[0].title_colors, v.title_colors);
        assert_eq!(svc.get(v.id).unwrap().unwrap().query, v.query);

        let u = svc
            .update(actor, v.id, "改名", cond(Op::Eq), SortSpec::default(), vec![], false, vec![])
            .unwrap();
        assert_eq!(u.name, "改名");
        assert_eq!(svc.list(actor, ws).unwrap()[0].query, cond(Op::Eq));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_view_is_idempotent_pinned_and_undeletable() {
        use crate::domain::{Condition, Field, Op};

        let (dir, _store, svc, ws, actor) = setup();
        let d = svc.ensure_default(actor, ws).unwrap();
        assert_eq!(d.name, "基础视图");
        assert!(d.is_shared, "基础视图对全员可见");

        // 幂等：再次调用返回同一个视图，不重复创建。
        let again = svc.ensure_default(actor, ws).unwrap();
        assert_eq!(again.id, d.id);

        // 新建普通视图后，基础视图仍在且置顶。
        svc.create(actor, ws, "A-视图", Query::all(), SortSpec::default(), vec![], false, vec![])
            .unwrap();
        let list = svc.list(actor, ws).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, d.id, "基础视图置顶");

        // 不可删除。
        assert!(svc.delete(actor, d.id).is_err());

        // 基础视图只接受列配置：改名、改查询条件、改排序都被挡下。
        let q = Query::Cond(Condition {
            field: Field::Label("Task".into()),
            op: Op::Present,
            value: None,
        });
        let u = svc
            .update(actor, d.id, "全部", q, SortSpec { field: SortField::Title, desc: false }, vec!["Task".into()], false, vec![])
            .unwrap();
        assert!(u.is_shared);
        assert_eq!(u.name, "基础视图", "基础视图不可改名");
        assert_eq!(u.query, Query::all(), "基础视图恒为全部条目，过滤只在临时态");
        assert_eq!(u.sort.field, SortField::UpdatedAt, "基础视图不可改排序");
        assert_eq!(u.columns, vec!["Task"], "但列仍可配置");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_skips_unreadable_view_records() {
        let (dir, store, svc, ws, actor) = setup();
        let good = svc
            .create(actor, ws, "好视图", Query::all(), SortSpec::default(), vec![], false, vec![])
            .unwrap();
        // 伪造一条旧编码残留：索引齐全、正文读不出来。
        let bad = Ulid::new();
        store
            .put_raw(cf::VIEWS, &keys::view_key(bad), b"not a valid view")
            .unwrap();
        store
            .put_raw(cf::VIEWS_BY_WORKSPACE, &keys::view_by_workspace_key(ws, bad), &[])
            .unwrap();

        let listed = svc.list(actor, ws).unwrap();
        assert_eq!(listed.len(), 1, "坏记录应被跳过而不是让整个列表失败");
        assert_eq!(listed[0].id, good.id);
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
