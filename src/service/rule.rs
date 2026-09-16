//! 自动化规则：DSL 校验、CRUD、以及把规则动作接进标签写入的引擎。

use std::sync::Arc;

use ulid::Ulid;

use crate::domain::rule::{ActionTarget, AutomationRule, LabelWrite, RuleAction, ValueSource, WriteOp};
use crate::domain::{AuditAction, AuditLog, LabelSchema, LabelValue, Query};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 每个工作空间的规则数上限。规则求值是同步的，且目标表达式要扫全表，
/// 数量失控会拖慢每一次打标。
pub const MAX_RULES_PER_WORKSPACE: usize = 50;

const RESOURCE_TYPE: &str = "rule";

/// 规则的增删改查与保存时校验。执行不在这里——见 `RuleEngine`。
pub struct RuleService {
    store: Arc<DocStore>,
}

impl RuleService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    pub fn list(&self, ws: Ulid) -> Result<Vec<AutomationRule>, AppError> {
        let rows = self
            .store
            .scan_prefix(cf::AUTOMATION_RULES_BY_WORKSPACE, &ws.to_bytes())?;
        let mut out = Vec::new();
        for (key, _) in rows {
            // 复合键为 (workspace_id, rule_id)，各 16 字节。
            if key.len() < 32 {
                continue;
            }
            let id = Ulid::from_bytes(key[16..32].try_into().unwrap());
            match self.store.get::<AutomationRule>(cf::AUTOMATION_RULES, &keys::rule_key(id)) {
                Ok(Some(r)) => out.push(r),
                Ok(None) => {}
                // 读不出来的记录（旧编码残留）不该拖垮整个规则列表：跳过并记一笔。
                Err(e) => tracing::warn!("跳过无法读取的规则 {id}: {e}"),
            }
        }
        // 按创建顺序（Ulid 时间有序）排列，求值顺序因此可预期。
        out.sort_by_key(|r| r.id);
        Ok(out)
    }

    pub fn get(&self, id: Ulid) -> Result<Option<AutomationRule>, AppError> {
        self.store.get(cf::AUTOMATION_RULES, &keys::rule_key(id))
    }

    /// 解析 + 按规则规则校验触发条件，供编辑器实时校验。
    pub fn parse_trigger(&self, ws: Ulid, expr: &str) -> Result<Query, AppError> {
        let q = Query::parse(expr)?;
        q.validate_for_rule(&self.schemas(ws)?, true)?;
        Ok(q)
    }

    pub fn create(
        &self,
        actor: Ulid,
        ws: Ulid,
        name: &str,
        enabled: bool,
        trigger_expr: &str,
        target_event_source: bool,
        target_expr: Option<&str>,
        writes: Vec<LabelWrite>,
    ) -> Result<AutomationRule, AppError> {
        if self.list(ws)?.len() >= MAX_RULES_PER_WORKSPACE {
            return Err(AppError::InvalidQuery(format!(
                "自动化规则数量已达上限（{MAX_RULES_PER_WORKSPACE} 条）"
            )));
        }
        let (trigger, action) = self.build(
            ws,
            name,
            trigger_expr,
            target_event_source,
            target_expr,
            writes,
        )?;
        let rule = AutomationRule::new(ws, name.trim().to_string(), enabled, trigger, action, actor);
        let audit = AuditLog::new(
            AuditAction::RuleCreated,
            actor,
            RESOURCE_TYPE,
            &rule.id.to_string(),
            Some(ws),
            None,
            Some(serde_json::to_string(&rule).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::AUTOMATION_RULES, keys::rule_key(rule.id).to_vec(), &rule)?);
        ops.push(BatchOp::put_raw(
            cf::AUTOMATION_RULES_BY_WORKSPACE,
            keys::rule_by_workspace_key(ws, rule.id).to_vec(),
            Vec::new(),
        ));
        self.store.write_batch(ops)?;
        Ok(rule)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &self,
        actor: Ulid,
        id: Ulid,
        name: &str,
        enabled: bool,
        trigger_expr: &str,
        target_event_source: bool,
        target_expr: Option<&str>,
        writes: Vec<LabelWrite>,
    ) -> Result<AutomationRule, AppError> {
        let mut rule = self.get(id)?.ok_or(AppError::NotFound)?;
        let (trigger, action) = self.build(
            rule.workspace_id,
            name,
            trigger_expr,
            target_event_source,
            target_expr,
            writes,
        )?;
        let before = serde_json::to_string(&rule).unwrap_or_default();
        rule.name = name.trim().to_string();
        rule.enabled = enabled;
        rule.trigger = trigger;
        rule.action = action;
        rule.updated_at = chrono::Utc::now();
        let audit = AuditLog::new(
            AuditAction::RuleUpdated,
            actor,
            RESOURCE_TYPE,
            &id.to_string(),
            Some(rule.workspace_id),
            Some(before),
            Some(serde_json::to_string(&rule).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::AUTOMATION_RULES, keys::rule_key(id).to_vec(), &rule)?);
        self.store.write_batch(ops)?;
        Ok(rule)
    }

    pub fn delete(&self, actor: Ulid, id: Ulid) -> Result<(), AppError> {
        let rule = self.get(id)?.ok_or(AppError::NotFound)?;
        let audit = AuditLog::new(
            AuditAction::RuleDeleted,
            actor,
            RESOURCE_TYPE,
            &id.to_string(),
            Some(rule.workspace_id),
            Some(serde_json::to_string(&rule).unwrap_or_default()),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::AUTOMATION_RULES, keys::rule_key(id).to_vec()));
        ops.push(BatchOp::delete(
            cf::AUTOMATION_RULES_BY_WORKSPACE,
            keys::rule_by_workspace_key(rule.workspace_id, id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 解析并校验一次提交，返回规范化的触发条件与动作。
    fn build(
        &self,
        ws: Ulid,
        name: &str,
        trigger_expr: &str,
        target_event_source: bool,
        target_expr: Option<&str>,
        writes: Vec<LabelWrite>,
    ) -> Result<(Query, RuleAction), AppError> {
        // 空名会让规则在列表和审计快照里无法辨认，且创建/更新两条路径都会落库，
        // 放在这个唯一入口校验，两条路径自然一致。
        if name.trim().is_empty() {
            return Err(AppError::InvalidQuery("规则名不能为空".to_string()));
        }
        let schemas = self.schemas(ws)?;
        let trigger = Query::parse(trigger_expr)?;
        trigger.validate_for_rule(&schemas, true)?;
        if writes.is_empty() {
            return Err(AppError::InvalidQuery("规则至少要有一个标签动作".to_string()));
        }
        let target = if target_event_source {
            ActionTarget::EventSource
        } else {
            let expr = target_expr.unwrap_or("").trim();
            if expr.is_empty() {
                return Err(AppError::InvalidQuery(
                    "动作目标为「表达式圈定」时必须填写表达式".to_string(),
                ));
            }
            let q = Query::parse(expr)?;
            q.validate_for_rule(&schemas, false)?;
            ActionTarget::Query(q)
        };
        for w in &writes {
            let schema = schemas
                .iter()
                .find(|s| s.name == w.label_name)
                .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {}", w.label_name)))?;
            match (w.op, &w.value) {
                (WriteOp::Remove, Some(_)) => {
                    return Err(AppError::InvalidQuery(format!(
                        "删除动作不能带值（标签 {}）",
                        w.label_name
                    )))
                }
                (WriteOp::Remove, None) => {}
                (WriteOp::Set, None) => {
                    return Err(AppError::InvalidQuery(format!(
                        "写入动作必须指定值来源（标签 {}）",
                        w.label_name
                    )))
                }
                (WriteOp::Set, Some(ValueSource::Literal(v))) => {
                    // 字面量不合法在保存时就报出来，不留到运行时才发现。
                    LabelValue::from_json(v, schema).map_err(|_| {
                        AppError::InvalidQuery(format!(
                            "标签 {} 的值不合法: {v}",
                            w.label_name
                        ))
                    })?;
                }
                (WriteOp::Set, Some(_)) => {}
            }
        }
        Ok((
            trigger,
            RuleAction {
                target,
                writes,
            },
        ))
    }

    pub(crate) fn schemas(&self, ws: Ulid) -> Result<Vec<LabelSchema>, AppError> {
        let mut out = Vec::new();
        for (_, v) in self.store.scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())? {
            if let Ok(s) = bincode::deserialize::<LabelSchema>(&v) {
                out.push(s);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::rule::{LabelEvent, ValueSource, WriteOp};
    use crate::domain::{Entry, EvalEnv, LabelSchema, LabelValue, LabelValueType, Labeling, Query};
    use crate::service::{label::LabelSchemaInput, LabelService};
    use ulid::Ulid;

    fn schema(name: &str, vt: LabelValueType) -> LabelSchema {
        LabelSchema {
            workspace_id: Ulid::new(),
            name: name.into(),
            title: name.into(),
            value_type: vt,
            enum_values: vec!["InProgress".into(), "Finished".into()],
            color: None,
            value_colors: Vec::new(),
            multi: false,
            format: None,
            currency_symbol: None,
            unit: None,
        }
    }

    fn schemas() -> Vec<LabelSchema> {
        vec![
            schema("Status", LabelValueType::Enum),
            schema("FinishedAt", LabelValueType::DateTime),
            schema("Priority", LabelValueType::Integer),
        ]
    }

    fn entry() -> Entry {
        Entry::new(Ulid::new(), "找回密码失败".to_string(), Ulid::new())
    }

    fn event(old: Option<LabelValue>, new: Option<LabelValue>) -> LabelEvent {
        LabelEvent {
            workspace_id: Ulid::new(),
            entry_code: "E1".into(),
            label_name: "Status".into(),
            old,
            new,
            actor: Ulid::new(),
            level: 0,
        }
    }

    /// 在给定事件下求值一个触发条件。
    fn matches(expr: &str, ev: &LabelEvent, labels: &[Labeling]) -> bool {
        let q = Query::parse(expr).expect("表达式应能解析");
        let e = entry();
        let schemas = schemas();
        // 具名闭包与 `src/domain/query.rs` 既有测试同形；内联 `&|_| false` 会因
        // 参数类型无法推断而编译失败。
        let never = |_: &str| false;
        let no_acct = |_: Ulid| None;
        let label_of = |n: &str| {
            schemas
                .iter()
                .find(|s| s.name == n)
                .map(|s| (s.value_type, s.format.clone()))
        };
        let env = EvalEnv {
            text_hit: &never,
            account_of: &no_acct,
            label_of: &label_of,
            event: Some(ev),
        };
        q.evaluate(&e, labels, &env)
    }

    #[test]
    fn event_fields_match_label_and_new_value() {
        let ev = event(Some(LabelValue::Enum("InProgress".into())), Some(LabelValue::Enum("Finished".into())));
        assert!(matches(r#"$label = "Status" AND $new = "Finished""#, &ev, &[]));
        assert!(matches(r#"$label = "Status" AND $old = "InProgress""#, &ev, &[]));
        assert!(!matches(r#"$new = "Aborted""#, &ev, &[]));
    }

    #[test]
    fn absent_sugar_distinguishes_insert_and_delete() {
        let added = event(None, Some(LabelValue::Enum("Finished".into())));
        let removed = event(Some(LabelValue::Enum("Finished".into())), None);
        assert!(matches("!$old", &added, &[]));
        assert!(!matches("!$new", &added, &[]));
        assert!(matches("!$new", &removed, &[]));
        assert!(!matches("!$old", &removed, &[]));
    }

    #[test]
    fn event_value_compares_with_the_changed_labels_own_layout() {
        // FinishedAt 是 DateTime 且未配 format，走 default_layout，
        // 事件值 "2026-09-16 10:00:00" 与字面量比较按时刻而非字符串。
        let ev = LabelEvent {
            label_name: "FinishedAt".into(),
            old: None,
            new: Some(LabelValue::DateTime("2026-09-16 10:00:00".into())),
            ..event(None, None)
        };
        assert!(matches(r#"$new > "2026-09-16 09:00:00""#, &ev, &[]));
        assert!(!matches(r#"$new > "2026-09-16 11:00:00""#, &ev, &[]));
    }

    #[test]
    fn entry_conditions_see_the_event_source_entry() {
        let ev = event(None, None);
        let labels = vec![Labeling::new("E1".into(), "Priority".into(), LabelValue::Int(5), Ulid::new())];
        assert!(matches("Priority >= 3", &ev, &labels));
        assert!(!matches("Priority >= 9", &ev, &labels));
    }

    #[test]
    fn to_expr_round_trips_event_fields() {
        for expr in [
            r#"$label = "Status" AND $new = "Finished""#,
            "!$old",
            r#"$new > "2026-09-16 09:00:00""#,
        ] {
            let q = Query::parse(expr).unwrap();
            let again = Query::parse(&q.to_expr()).expect("to_expr 的输出必须能再次解析");
            assert_eq!(q, again, "表达式往返失败: {expr}");
        }
    }

    #[test]
    fn views_reject_event_fields_and_rules_reject_text() {
        let schemas = schemas();
        let ev_field = Query::parse(r#"$label = "Status""#).unwrap();
        assert!(ev_field.validate(&schemas).is_err(), "视图不得使用事件字段");
        assert!(ev_field.validate_for_rule(&schemas, true).is_ok(), "触发条件允许事件字段");
        assert!(
            ev_field.validate_for_rule(&schemas, false).is_err(),
            "动作目标是对条目的过滤，不得使用事件字段"
        );
        let text = Query::parse(r#"text ~ "报错""#).unwrap();
        assert!(text.validate_for_rule(&schemas, true).is_err(), "规则不得使用全文条件");
    }

    #[test]
    fn unknown_event_field_is_rejected_with_readable_message() {
        let err = Query::parse("$nope = 1").unwrap_err();
        assert!(err.to_string().contains("nope"), "错误信息应指出未知字段: {err}");
    }

    #[test]
    fn empty_name_is_rejected_on_create_and_update() {
        // 服务测试要真落库，用临时目录起一个 DocStore（同 view/label 的测试）。
        let mut dir = std::env::temp_dir();
        dir.push(format!("rodeo-rule-name-{}", Ulid::new()));
        let dir = dir.to_string_lossy().into_owned();
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = RuleService::new(store.clone());
        let actor = Ulid::new();
        let ws = Ulid::new();
        // 动作里的标签必须真实存在，否则会在名字之前的校验上先失败。
        LabelService::new(store.clone())
            .create_schema(
                actor,
                ws,
                LabelSchemaInput {
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
                },
            )
            .unwrap();
        let writes = || {
            vec![LabelWrite {
                label_name: "Status".into(),
                op: WriteOp::Set,
                value: Some(ValueSource::Literal(serde_json::json!("Done"))),
            }]
        };

        for bad in ["", "   ", "\t\n"] {
            let err = svc
                .create(actor, ws, bad, true, r#"$new = "Done""#, true, None, writes())
                .unwrap_err();
            assert!(
                matches!(err, AppError::InvalidQuery(_)),
                "名字 {bad:?} 应被拒: {err}"
            );
        }
        assert!(svc.list(ws).unwrap().is_empty(), "被拒的规则不得留下索引残留");

        let rule = svc
            .create(actor, ws, "  有效名  ", true, r#"$new = "Done""#, true, None, writes())
            .unwrap();
        assert_eq!(rule.name, "有效名", "合法名字应去除首尾空白后落库");

        let err = svc
            .update(actor, rule.id, "  ", true, r#"$new = "Done""#, true, None, writes())
            .unwrap_err();
        assert!(matches!(err, AppError::InvalidQuery(_)), "更新为空名应被拒: {err}");
        assert_eq!(
            svc.get(rule.id).unwrap().unwrap().name,
            "有效名",
            "被拒的更新不得改动已存库的规则"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
