//! 自动化规则：DSL 校验、CRUD、以及把规则动作接进标签写入的引擎。

use std::sync::Arc;

use ulid::Ulid;

use crate::domain::rule::{
    ActionTarget, AutomationRule, LabelEvent, LabelWrite, RuleAction, ValueSource, WriteOp,
};
use crate::domain::{AuditAction, AuditLog, Entry, LabelSchema, LabelValue, Labeling, Query};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::entry::{labeling_ops, list_entries};
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

// ---------- 执行引擎 ----------

/// 用户直接写入产生的事件层级。
pub const USER_LEVEL: u8 = 0;
/// 规则动作最多再触发几轮（level 1..=MAX_LEVEL）。到顶后静默截断。
pub const MAX_LEVEL: u8 = 3;

/// 一次待提交的标签变更。`value = None` 表示删除。
#[derive(Debug, Clone)]
pub struct StagedWrite {
    pub entry_code: String,
    pub label_name: String,
    pub value: Option<LabelValue>,
    pub actor: Ulid,
}

/// 引擎算出的补充批次：规则动作的写入 + 它们的审计，外加需要 reindex 的条目。
/// 与用户写入拼成**一个** `write_batch` 提交，任一步失败整体回滚。
pub struct RulePlan {
    pub ops: Vec<BatchOp>,
    pub affected: Vec<String>,
}

/// 标签后置状态：条目 → (标签名 → Labeling)。规则求值看的是「写入之后」的状态，
/// 而写入尚未提交，因此引擎自己维护一份内存视图。
struct Overlay {
    labels: std::collections::HashMap<String, std::collections::HashMap<String, Labeling>>,
}

impl Overlay {
    fn load(store: &DocStore, ws: Ulid) -> Result<Self, AppError> {
        let mut labels: std::collections::HashMap<String, std::collections::HashMap<String, Labeling>> =
            std::collections::HashMap::new();
        for (_, v) in store.scan_prefix(cf::LABELINGS_BY_WORKSPACE, &ws.to_bytes())? {
            if let Ok(l) = bincode::deserialize::<Labeling>(&v) {
                labels
                    .entry(l.entry_code.clone())
                    .or_default()
                    .insert(l.label_name.clone(), l);
            }
        }
        Ok(Self { labels })
    }

    fn get(&self, code: &str, name: &str) -> Option<&Labeling> {
        self.labels.get(code).and_then(|m| m.get(name))
    }

    fn value_of(&self, code: &str, name: &str) -> Option<LabelValue> {
        self.get(code, name).map(|l| l.value.clone())
    }

    fn labels_of(&self, code: &str) -> Vec<Labeling> {
        self.labels
            .get(code)
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    fn apply(&mut self, w: &StagedWrite) {
        let entry = self.labels.entry(w.entry_code.clone()).or_default();
        match &w.value {
            Some(v) => {
                entry.insert(
                    w.label_name.clone(),
                    Labeling::new(w.entry_code.clone(), w.label_name.clone(), v.clone(), w.actor),
                );
            }
            None => {
                entry.remove(&w.label_name);
            }
        }
    }
}

/// 工作空间标签 schema 的按名索引，外加一张继承推导图。
struct Schemas {
    by_name: std::collections::HashMap<String, LabelSchema>,
    graph: crate::domain::InheritanceGraph,
}

impl Schemas {
    fn new(list: Vec<LabelSchema>) -> Self {
        let graph = crate::domain::InheritanceGraph::build(&list);
        Self {
            by_name: list.into_iter().map(|s| (s.name.clone(), s)).collect(),
            graph,
        }
    }

    fn get(&self, name: &str) -> Option<&LabelSchema> {
        self.by_name.get(name)
    }

    fn label_of(&self, name: &str) -> Option<(crate::domain::LabelValueType, Option<String>)> {
        self.by_name.get(name).map(|s| (s.value_type, s.format.clone()))
    }
}

/// 规则求值不走全文检索——校验已禁止 `text` 条件，走不到；恒假即可。
/// 用具名函数而非内联闭包：`&dyn Fn(&str) -> bool` 的期望类型传不进闭包字面量的参数。
fn no_text_hit(_: &str) -> bool {
    false
}

#[derive(Clone)]
pub struct RuleEngine {
    store: Arc<DocStore>,
}

impl RuleEngine {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    /// 给定一次请求内的用户标签写入，算出规则动作要补的全部 ops。
    /// 没有启用规则时返回 `Ok(None)`——常态写入因此不为规则付出任何代价。
    /// **必须在用户写入提交之前调用**：`before` 取自库里的前像。
    pub fn plan(&self, ws: Ulid, user_writes: &[StagedWrite]) -> Result<Option<RulePlan>, AppError> {
        let rules: Vec<AutomationRule> = RuleService::new(self.store.clone())
            .list(ws)?
            .into_iter()
            .filter(|r| r.enabled)
            .collect();
        if rules.is_empty() {
            return Ok(None);
        }

        let schemas = Schemas::new(RuleService::new(self.store.clone()).schemas(ws)?);
        let candidates = list_entries(&self.store, ws)?;
        let mut overlay = Overlay::load(&self.store, ws)?;
        // 触发条件与动作目标都可能引用 CreatedBy / UpdatedBy；目标侧漏算会让账号条件
        // 恒为假，规则静默圈不到条目，所以两边都要看。
        let needs_accounts = rules.iter().any(|r| {
            r.trigger.contains_account_field()
                || matches!(&r.action.target, ActionTarget::Query(q) if q.contains_account_field())
        });
        let accounts = if needs_accounts {
            self.accounts()?
        } else {
            std::collections::HashMap::new()
        };

        // level 0：用户写入全部落库（现有语义不变），但只有真正变化的成为事件。
        let mut events = collect_diff(ws, &overlay, user_writes, USER_LEVEL);
        let mut affected: Vec<String> = user_writes.iter().map(|w| w.entry_code.clone()).collect();
        for w in user_writes {
            overlay.apply(w);
        }

        let now = chrono::Utc::now();
        let mut ops: Vec<BatchOp> = Vec::new();
        let mut level = USER_LEVEL;

        while level < MAX_LEVEL && !events.is_empty() {
            let next = level + 1;
            // (规则下标, 写入)：下标用于把最终生效的写入归给产出它的规则。
            let mut staged: Vec<(usize, StagedWrite)> = Vec::new();
            // (规则下标, 它自己命中的事件)：审计要的正是后者，不是整层的事件。
            let mut fired: Vec<(usize, Vec<&LabelEvent>)> = Vec::new();

            for (ri, rule) in rules.iter().enumerate() {
                // 不用 `filter` 收集：命中判断里的存储错误要经 `?` 向上抛，
                // 闭包里做不到。
                let mut matched: Vec<&LabelEvent> = Vec::new();
                for ev in &events {
                    if self.trigger_matches(rule, ev, &overlay, &schemas, &accounts)? {
                        matched.push(ev);
                    }
                }
                if matched.is_empty() {
                    continue;
                }
                // 圈定结果在层内可复用：层内 overlay 冻结，目标集合不会变。
                let target_codes = match &rule.action.target {
                    ActionTarget::EventSource => Vec::new(),
                    ActionTarget::Query(q) => {
                        self.match_entries(q, &candidates, &overlay, &schemas, &accounts)
                    }
                };
                for ev in &matched {
                    let codes: Vec<String> = match &rule.action.target {
                        ActionTarget::EventSource => vec![ev.entry_code.clone()],
                        ActionTarget::Query(_) => target_codes.clone(),
                    };
                    for code in codes {
                        for w in &rule.action.writes {
                            let value = match resolve_value(rule, w, ev, &schemas, now)? {
                                // $old 遇上新增、$new 遇上删除：该事件不产生这条写入。
                                // 这不是错误——删除标签的操作不该因为规则引用旧值而失败。
                                ActionValue::Skip => continue,
                                ActionValue::Value(v) => v,
                            };
                            staged.push((
                                ri,
                                StagedWrite {
                                    entry_code: code.clone(),
                                    label_name: w.label_name.clone(),
                                    value,
                                    actor: ev.actor,
                                },
                            ));
                        }
                    }
                }
                fired.push((ri, matched));
            }

            // 同层同 (entry, label)：后者覆盖前者；与层初值相同的丢弃（级联的安全阀）。
            let collapsed = collapse(&overlay, &staged);

            // (entry, label) → 产出最终生效写入的规则下标。一张表把归属查询摊平：
            // 否则每条写入都要扫一遍 `collapsed`，命中规则多、目标集合大时是三重乘积。
            let owner: std::collections::HashMap<(String, String), usize> = collapsed
                .iter()
                .map(|(ri, w)| ((w.entry_code.clone(), w.label_name.clone()), *ri))
                .collect();
            // 规则命中就落一条 RuleApplied，只记它自己命中的事件。
            // 放在 `collapsed.is_empty()` 之前：写入全被去重丢掉时规则确实命中过，
            // 审计不能缺席（此时每条写入 `applied: false`）。
            let mut audits: Vec<BatchOp> = Vec::new();
            for (ri, matched) in fired {
                let own: Vec<(&StagedWrite, bool)> = staged
                    .iter()
                    .filter(|(r, _)| *r == ri)
                    .map(|(_, w)| {
                        let applied = owner
                            .get(&(w.entry_code.clone(), w.label_name.clone()))
                            .is_some_and(|o| *o == ri);
                        (w, applied)
                    })
                    .collect();
                audits.extend(audit_ops(&rule_audit(&rules[ri], next, &matched, &own))?);
            }

            if collapsed.is_empty() {
                ops.extend(audits);
                break;
            }

            let mut next_events = Vec::new();
            for (_, w) in &collapsed {
                let before = overlay.get(&w.entry_code, &w.label_name).cloned();
                ops.extend(labeling_ops(
                    ws,
                    &w.entry_code,
                    &w.label_name,
                    w.value.as_ref(),
                    w.actor,
                    before.as_ref(),
                )?);
                next_events.push(LabelEvent {
                    workspace_id: ws,
                    entry_code: w.entry_code.clone(),
                    label_name: w.label_name.clone(),
                    old: before.map(|l| l.value),
                    new: w.value.clone(),
                    actor: w.actor,
                    level: next,
                });
                overlay.apply(w);
                affected.push(w.entry_code.clone());
            }

            // 审计仍排在标签写入之后，op 顺序与改动前一致。
            ops.extend(audits);

            events = next_events;
            level = next;
        }

        affected.sort();
        affected.dedup();
        Ok(Some(RulePlan { ops, affected }))
    }

    fn accounts(&self) -> Result<std::collections::HashMap<Ulid, (String, String)>, AppError> {
        let mut m = std::collections::HashMap::new();
        for (_, v) in self.store.scan_prefix(cf::ACCOUNTS, b"")? {
            let a: crate::domain::Account = bincode::deserialize(&v)?;
            m.insert(a.id, (a.name, a.email));
        }
        Ok(m)
    }

    /// 事件源条目：触发条件里的条目字段求的是它的写入后状态。
    /// 条目可能已被归档 / 软删除（列表口径里没有），此时直接读库补上，不静默跳过。
    fn entry_of(&self, code: &str) -> Result<Option<Entry>, AppError> {
        self.store.get::<Entry>(cf::ENTRIES, code.as_bytes())
    }

    /// 条目不存在（`Ok(None)`）才算不命中；读库 / 解码失败必须向上抛——
    /// 静默当作不命中，会让一条坏 `Entry` 悄悄关掉该条目的全部规则。
    fn trigger_matches(
        &self,
        rule: &AutomationRule,
        ev: &LabelEvent,
        overlay: &Overlay,
        schemas: &Schemas,
        accounts: &std::collections::HashMap<Ulid, (String, String)>,
    ) -> Result<bool, AppError> {
        let Some(entry) = self.entry_of(&ev.entry_code)? else {
            return Ok(false);
        };
        let labels = overlay.labels_of(&ev.entry_code);
        // 触发条件写了 `L4+` 才算继承——规则引擎的 overlay 就是写入后的状态，
        // 推导要基于它，而不是库里的前像。
        let derived = if rule.trigger.contains_inherited_label() {
            schemas.graph.derive(&labels)
        } else {
            Vec::new()
        };
        let env = crate::domain::EvalEnv {
            text_hit: &no_text_hit,
            account_of: &|id| accounts.get(&id).cloned(),
            label_of: &|n| schemas.label_of(n),
            event: Some(ev),
            derived: &derived,
        };
        Ok(rule.trigger.evaluate(&entry, &labels, &env))
    }

    fn match_entries(
        &self,
        q: &Query,
        candidates: &[Entry],
        overlay: &Overlay,
        schemas: &Schemas,
        accounts: &std::collections::HashMap<Ulid, (String, String)>,
    ) -> Vec<String> {
        candidates
            .iter()
            .filter(|e| {
                let labels = overlay.labels_of(&e.code);
                let derived = if q.contains_inherited_label() {
                    schemas.graph.derive(&labels)
                } else {
                    Vec::new()
                };
                let env = crate::domain::EvalEnv {
                    text_hit: &no_text_hit,
                    account_of: &|id| accounts.get(&id).cloned(),
                    label_of: &|n| schemas.label_of(n),
                    event: None,
                    derived: &derived,
                };
                q.evaluate(e, &labels, &env)
            })
            .map(|e| e.code.clone())
            .collect()
    }
}

/// 按 (entry, label) 归并一层内的写入，返回真正发生变化的那些事件。
fn collect_diff(ws: Ulid, overlay: &Overlay, writes: &[StagedWrite], level: u8) -> Vec<LabelEvent> {
    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    // 同一个 key 只保留最后一次写入（同批里后者覆盖前者），事件顺序则按首次出现排。
    let mut last: std::collections::HashMap<(String, String), &StagedWrite> =
        std::collections::HashMap::new();
    for w in writes {
        last.insert((w.entry_code.clone(), w.label_name.clone()), w);
    }
    for w in writes {
        let key = (w.entry_code.clone(), w.label_name.clone());
        if !seen.insert(key.clone()) {
            continue;
        }
        let w = last[&key];
        let old = overlay.value_of(&w.entry_code, &w.label_name);
        if old != w.value {
            out.push(LabelEvent {
                workspace_id: ws,
                entry_code: w.entry_code.clone(),
                label_name: w.label_name.clone(),
                old,
                new: w.value.clone(),
                actor: w.actor,
                level,
            });
        }
    }
    out
}

/// 同层归并：同 (entry, label) 后者覆盖前者；与层初值相同者丢弃。
fn collapse(overlay: &Overlay, staged: &[(usize, StagedWrite)]) -> Vec<(usize, StagedWrite)> {
    let mut order: Vec<(String, String)> = Vec::new();
    let mut last: std::collections::HashMap<(String, String), (usize, StagedWrite)> =
        std::collections::HashMap::new();
    for (ri, w) in staged {
        let key = (w.entry_code.clone(), w.label_name.clone());
        if !last.contains_key(&key) {
            order.push(key.clone());
        }
        last.insert(key, (*ri, w.clone()));
    }
    let mut out = Vec::new();
    for key in order {
        let (ri, w) = last.remove(&key).unwrap();
        if overlay.value_of(&w.entry_code, &w.label_name) != w.value {
            out.push((ri, w));
        }
    }
    out
}

/// 一条动作在某个事件下解析出的结果。
/// 「缺值」与「删除」必须分开：前者是 $old 无旧值 / $new 无新值，跳过该条写入；
/// 后者是规则真的要删掉这个标签（`Value(None)`），不能一并当作跳过。
enum ActionValue {
    Skip,
    Value(Option<LabelValue>),
}

/// 算出动作要写入的值。`Skip` = 该事件不产生这条写入。
fn resolve_value(
    rule: &AutomationRule,
    w: &LabelWrite,
    ev: &LabelEvent,
    schemas: &Schemas,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ActionValue, AppError> {
    if w.op == WriteOp::Remove {
        // 删除不取事件值，也就不存在缺值。
        return Ok(ActionValue::Value(None));
    }
    let Some(schema) = schemas.get(&w.label_name) else {
        return Err(AppError::RuleFailed(format!(
            "规则「{}」写标签「{}」失败：标签不存在",
            rule.name, w.label_name
        )));
    };
    let bad = |why: &str| {
        AppError::RuleFailed(format!(
            "规则「{}」写标签「{}」失败：{why}",
            rule.name, w.label_name
        ))
    };
    match w.value.as_ref() {
        Some(ValueSource::Literal(v)) => Ok(ActionValue::Value(Some(
            LabelValue::from_json(v, schema).map_err(|_| bad("字面量的值不合法"))?,
        ))),
        Some(ValueSource::Now) => Ok(ActionValue::Value(Some(
            now_value(schema, now).map_err(|e| bad(&e))?,
        ))),
        // $old / $new 按目标标签的类型重新校验：跨类型转发（如把枚举值写进整数标签）
        // 在运行时被拦下，整体回滚，不留半成品。
        Some(ValueSource::New) => match &ev.new {
            Some(v) => Ok(ActionValue::Value(Some(
                LabelValue::from_json(&v.to_json(), schema)
                    .map_err(|_| bad("事件的新值与目标标签类型不兼容"))?,
            ))),
            None => Ok(ActionValue::Skip),
        },
        Some(ValueSource::Old) => match &ev.old {
            Some(v) => Ok(ActionValue::Value(Some(
                LabelValue::from_json(&v.to_json(), schema)
                    .map_err(|_| bad("事件的旧值与目标标签类型不兼容"))?,
            ))),
            None => Ok(ActionValue::Skip),
        },
        None => Err(bad("写入动作缺少值来源")),
    }
}

/// `now` 只对时间型标签有意义：按 schema 的布局渲染成存储串。
fn now_value(schema: &LabelSchema, now: chrono::DateTime<chrono::Utc>) -> Result<LabelValue, String> {
    use crate::domain::LabelValueType::*;
    use chrono::{Datelike, Timelike};
    if !matches!(schema.value_type, Date | Time | DateTime) {
        return Err("now 只能写入日期 / 时间 / 日期时间型标签".to_string());
    }
    let layout = schema
        .format
        .as_deref()
        .unwrap_or_else(|| crate::domain::default_layout(schema.value_type));
    let t = crate::golayout::YmdHms {
        year: now.year(),
        month: now.month(),
        day: now.day(),
        hour: now.hour(),
        minute: now.minute(),
        second: now.second(),
    };
    let s = crate::golayout::format(layout, t);
    LabelValue::from_json(&serde_json::Value::String(s), schema)
        .map_err(|_| "当前时间无法按标签的布局渲染".to_string())
}

/// 一条 RuleApplied 审计：规则本次命中了哪些事件、它算了哪些写入、哪些真正生效。
/// `events` 只含**本规则**命中的事件，不是整层的事件。
fn rule_audit(
    rule: &AutomationRule,
    level: u8,
    events: &[&LabelEvent],
    writes: &[(&StagedWrite, bool)],
) -> AuditLog {
    let triggers: Vec<serde_json::Value> = events
        .iter()
        .map(|ev| {
            serde_json::json!({
                "entryCode": ev.entry_code,
                "labelName": ev.label_name,
                "old": ev.old.as_ref().map(LabelValue::to_json),
                "new": ev.new.as_ref().map(LabelValue::to_json),
            })
        })
        .collect();
    let writes: Vec<serde_json::Value> = writes
        .iter()
        .map(|(w, applied)| {
            serde_json::json!({
                "entryCode": w.entry_code,
                "labelName": w.label_name,
                "value": w.value.as_ref().map(LabelValue::to_json),
                "applied": applied,
            })
        })
        .collect();
    let after = serde_json::json!({
        "ruleId": rule.id.to_string(),
        "ruleName": rule.name,
        "level": level,
        "triggers": triggers,
        "writes": writes,
    });
    // actor 用规则创建者：RuleApplied 是规则自身的动作记录，与触发者无关
    // （被规则写入的标签另有 LabelingSet 审计，那里记的是触发者）。
    AuditLog::new(
        AuditAction::RuleApplied,
        rule.created_by,
        RESOURCE_TYPE,
        &rule.id.to_string(),
        Some(rule.workspace_id),
        None,
        Some(after.to_string()),
    )
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
            default_value: None,
            links: Vec::new(),
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
            derived: &[],
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
                    default_value: None,
                    links: vec![],
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

    /// 引擎测试：覆盖分层推进、级联收敛与审计。
    mod engine {
        use super::*;
        use crate::service::entry::labeling_ops;
        use crate::service::rule::RuleEngine;
        use crate::service::EntryService;
        use crate::storage::DocStore;
        use std::sync::Arc;

        /// 建一个只有 schema + 条目的临时工作空间。
        fn setup() -> (String, Arc<DocStore>, Ulid, Ulid) {
            let mut dir = std::env::temp_dir();
            dir.push(format!("rodeo-rule-test-{}", Ulid::new()));
            let path = dir.to_string_lossy().to_string();
            let store = Arc::new(DocStore::open(&path).unwrap());
            let ws = Ulid::new();
            let actor = Ulid::new();
            for s in schemas() {
                let mut s = s;
                s.workspace_id = ws;
                store
                    .put(cf::LABEL_SCHEMAS, &keys::label_schema_key(ws, &s.name), &s)
                    .unwrap();
            }
            (path, store, ws, actor)
        }

        /// 建一条「Status 变 Finished 就写 FinishedAt = now」的规则。
        fn rule_set_finished_at(store: &Arc<DocStore>, ws: Ulid, actor: Ulid) -> AutomationRule {
            let svc = RuleService::new(store.clone());
            svc.create(
                actor,
                ws,
                "完成时间",
                true,
                r#"$label = "Status" AND $new = "Finished""#,
                true,
                None,
                vec![LabelWrite {
                    label_name: "FinishedAt".into(),
                    op: WriteOp::Set,
                    value: Some(ValueSource::Now),
                }],
            )
            .unwrap()
        }

        /// 直接落一个标签（不走规则），用于构造前像。
        fn set(store: &Arc<DocStore>, ws: Ulid, code: &str, name: &str, lv: LabelValue, actor: Ulid) {
            let before = store
                .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(code, name))
                .unwrap();
            let ops = labeling_ops(ws, code, name, Some(&lv), actor, before.as_ref()).unwrap();
            store.write_batch(ops).unwrap();
        }

        fn get(store: &Arc<DocStore>, code: &str, name: &str) -> Option<LabelValue> {
            store
                .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(code, name))
                .unwrap()
                .map(|l| l.value)
        }

        /// 走一遍「用户写入 + 引擎 plan + 同批提交」的真实路径，返回引擎报告的受影响条目。
        /// 顺序必须是先 plan 再提交：引擎的 `before` 取自库里的前像，
        /// 用户写入若先落库，`collect_diff` 看到的就是「值没变」。
        fn apply(store: &Arc<DocStore>, ws: Ulid, writes: &[StagedWrite]) -> Vec<String> {
            let mut ops = Vec::new();
            for w in writes {
                let before = store
                    .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(&w.entry_code, &w.label_name))
                    .unwrap();
                ops.extend(
                    labeling_ops(
                        ws,
                        &w.entry_code,
                        &w.label_name,
                        w.value.as_ref(),
                        w.actor,
                        before.as_ref(),
                    )
                    .unwrap(),
                );
            }
            let engine = RuleEngine::new(store.clone());
            let affected = match engine.plan(ws, writes).unwrap() {
                Some(p) => {
                    ops.extend(p.ops);
                    p.affected
                }
                None => Vec::new(),
            };
            store.write_batch(ops).unwrap();
            affected
        }

        #[test]
        fn status_finished_writes_finished_at() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            rule_set_finished_at(&store, ws, actor);
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let got = get(&store, &entry.code, "FinishedAt");
            assert!(got.is_some(), "Status=Finished 应触发 FinishedAt 写入");
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn rewrites_with_unchanged_value_do_not_fire() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            rule_set_finished_at(&store, ws, actor);
            // 先真的写一次 Finished，让规则触发。
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let first = get(&store, &entry.code, "FinishedAt");
            // 再把同一个值写一遍：值没变，不产生事件，规则不该重跑。
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            assert_eq!(get(&store, &entry.code, "FinishedAt"), first, "重复写同值不得刷新 FinishedAt");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// 同层两条规则写同一个标签：按规则 id 升序，后者覆盖前者。
        /// 这条用例实际验证的不是层数上限——收敛靠的是「同层覆盖 + 写同值即丢弃」，
        /// 层数上限由 `cascade_is_capped_at_max_level` 覆盖。
        #[test]
        fn same_level_writes_to_one_label_are_last_write_wins() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            // 两条规则都因 Priority 事件触发（`Priority` 条件对任意 Priority 值成立），
            // 同一层里先后写 Priority = 1、2。
            let svc = RuleService::new(store.clone());
            svc.create(actor, ws, "P+A", true, "Priority", true, None, vec![LabelWrite {
                label_name: "Priority".into(), op: WriteOp::Set,
                value: Some(ValueSource::Literal(serde_json::json!(1))),
            }]).unwrap();
            // `Ulid::new()` 只是 from_datetime(now())，毫秒内的低 80 位是随机的、并不单调，
            // 两条规则若落在同一毫秒，id 相对顺序就是随机的；sleep 隔开时间戳才能保证 id 升序 = 创建顺序。
            std::thread::sleep(std::time::Duration::from_millis(2));
            svc.create(actor, ws, "P+B", true, "Priority", true, None, vec![LabelWrite {
                label_name: "Priority".into(), op: WriteOp::Set,
                value: Some(ValueSource::Literal(serde_json::json!(2))),
            }]).unwrap();
            // 层内后者覆盖前者 → 2；下一轮两条规则再算出的仍是 2，与当前值相同被丢弃，
            // 级联就此收敛。
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Priority".into(),
                value: Some(LabelValue::Int(0)),
                actor,
            }]);
            assert_eq!(get(&store, &entry.code, "Priority"), Some(LabelValue::Int(2)));
            std::fs::remove_dir_all(&dir).ok();
        }

        #[test]
        fn rule_audit_records_trigger_and_writes() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            rule_set_finished_at(&store, ws, actor);
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let audits = crate::service::AuditService::new(store.clone()).list(ws, 100).unwrap();
            let applied = audits
                .iter()
                .find(|a| a.action == AuditAction::RuleApplied)
                .expect("规则命中应写一条 RuleApplied");
            let after = applied.after.as_deref().unwrap_or("");
            assert!(after.contains("FinishedAt"), "审计应记录规则写了哪些标签: {after}");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// 层数上限必须真的截断级联。上面那条自触发用例其实是被「写同值即丢弃」拦下的，
        /// 换成每轮都写新值的链才轮得到上限说话：少一层会多写一次 Priority=2。
        #[test]
        fn cascade_is_capped_at_max_level() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            let svc = RuleService::new(store.clone());
            let write = |name: &str, op: WriteOp, value: Option<ValueSource>| LabelWrite {
                label_name: name.into(),
                op,
                value,
            };
            // 链：Status=Finished → FinishedAt=now → Priority=1 → Status=InProgress → Priority=2。
            svc.create(actor, ws, "R1", true, r#"$label = "Status" AND $new = "Finished""#, true, None,
                vec![write("FinishedAt", WriteOp::Set, Some(ValueSource::Now))]).unwrap();
            svc.create(actor, ws, "R2", true, r#"$label = "FinishedAt""#, true, None,
                vec![write("Priority", WriteOp::Set, Some(ValueSource::Literal(serde_json::json!(1))))]).unwrap();
            svc.create(actor, ws, "R3", true, r#"$label = "Priority""#, true, None,
                vec![write("Status", WriteOp::Set, Some(ValueSource::Literal(serde_json::json!("InProgress"))))]).unwrap();
            svc.create(actor, ws, "R4", true, r#"$label = "Status" AND $new = "InProgress""#, true, None,
                vec![write("Priority", WriteOp::Set, Some(ValueSource::Literal(serde_json::json!(2))))]).unwrap();
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            assert_eq!(
                get(&store, &entry.code, "Priority"),
                Some(LabelValue::Int(1)),
                "第 3 层之后不得再跑第 4 轮"
            );
            assert_eq!(
                get(&store, &entry.code, "Status"),
                Some(LabelValue::Enum("InProgress".into())),
                "第 3 层自身的写入照常生效"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// 规则写出的值等于该标签的当前值：不是变更，既不写标签也不写标签审计
        /// （规则命中本身仍有一条 `RuleApplied`，见 `noop_only_rule_still_audits`）。
        /// A→B、B→A 这类互相触发的规则正靠这条收敛。
        #[test]
        fn noop_rule_write_is_dropped_without_audit() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            // 触发条件是任意 Status 事件，动作把 $new 原样写回 Status：值没变。
            RuleService::new(store.clone())
                .create(
                    actor,
                    ws,
                    "原样回写",
                    true,
                    r#"$label = "Status""#,
                    true,
                    None,
                    vec![LabelWrite {
                        label_name: "Status".into(),
                        op: WriteOp::Set,
                        value: Some(ValueSource::New),
                    }],
                )
                .unwrap();
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let sets = crate::service::AuditService::new(store.clone())
                .list(ws, 100)
                .unwrap()
                .into_iter()
                .filter(|a| a.action == AuditAction::LabelingSet)
                .count();
            assert_eq!(sets, 1, "只有用户那一次写入该有 LabelingSet 审计");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// 删除动作必须真的删掉标签：`Remove` 与「值来源缺值」是两回事，不能一起被跳过。
        #[test]
        fn remove_action_deletes_the_label() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            set(&store, ws, &entry.code, "Priority", LabelValue::Int(5), actor);
            RuleService::new(store.clone())
                .create(
                    actor,
                    ws,
                    "完成即清优先级",
                    true,
                    r#"$label = "Status" AND $new = "Finished""#,
                    true,
                    None,
                    vec![LabelWrite {
                        label_name: "Priority".into(),
                        op: WriteOp::Remove,
                        value: None,
                    }],
                )
                .unwrap();
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            assert_eq!(get(&store, &entry.code, "Priority"), None, "Remove 动作应删掉该标签");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// $old 遇上新增事件：跳过该条写入，不写标签也不报错（整个请求照常提交）。
        #[test]
        fn missing_event_value_skips_the_write_without_failing() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            RuleService::new(store.clone())
                .create(
                    actor,
                    ws,
                    "记录旧状态",
                    true,
                    r#"$label = "Status""#,
                    true,
                    None,
                    vec![LabelWrite {
                        label_name: "FinishedAt".into(),
                        op: WriteOp::Set,
                        value: Some(ValueSource::Old),
                    }],
                )
                .unwrap();
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            assert_eq!(get(&store, &entry.code, "FinishedAt"), None, "$old 缺值应跳过该条写入");
            assert_eq!(
                get(&store, &entry.code, "Status"),
                Some(LabelValue::Enum("Finished".into())),
                "跳过规则动作不得影响用户写入"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// $new 遇上删除事件：与 $old 遇上新增对称，同样跳过该条写入。
        #[test]
        fn missing_new_on_delete_event_skips_the_write() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            set(&store, ws, &entry.code, "Status", LabelValue::Enum("Finished".into()), actor);
            RuleService::new(store.clone())
                .create(
                    actor,
                    ws,
                    "转发新状态",
                    true,
                    r#"$label = "Status""#,
                    true,
                    None,
                    vec![LabelWrite {
                        label_name: "FinishedAt".into(),
                        op: WriteOp::Set,
                        value: Some(ValueSource::New),
                    }],
                )
                .unwrap();
            // 用户删除 Status：没有新值可转发。
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: None,
                actor,
            }]);
            assert_eq!(get(&store, &entry.code, "FinishedAt"), None, "$new 在删除事件里无值，应跳过该条写入");
            assert_eq!(get(&store, &entry.code, "Status"), None, "删标签的请求照常生效");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// 命中但写入全被去重丢弃：标签没有变更，但规则确实命中过，仍要留一条 RuleApplied，
        /// 并如实记 `applied: false`。
        #[test]
        fn noop_only_rule_still_audits() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            // 触发条件是任意 Status 事件，动作把 $new 原样写回 Status：值没变。
            RuleService::new(store.clone())
                .create(
                    actor,
                    ws,
                    "原样回写",
                    true,
                    r#"$label = "Status""#,
                    true,
                    None,
                    vec![LabelWrite {
                        label_name: "Status".into(),
                        op: WriteOp::Set,
                        value: Some(ValueSource::New),
                    }],
                )
                .unwrap();
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let audits = crate::service::AuditService::new(store.clone()).list(ws, 100).unwrap();
            let applied = audits
                .iter()
                .find(|a| a.action == AuditAction::RuleApplied)
                .expect("命中过就该有一条 RuleApplied");
            let after: serde_json::Value =
                serde_json::from_str(applied.after.as_deref().unwrap_or("null")).unwrap();
            assert_eq!(after["level"], serde_json::json!(1), "命中发生在第 1 层");
            assert_eq!(
                after["triggers"].as_array().map(Vec::len),
                Some(1),
                "审计要记下命中的事件: {after}"
            );
            let writes = after["writes"].as_array().expect("writes 是数组");
            assert_eq!(writes.len(), 1, "算过一条写入就要记一条: {after}");
            assert_eq!(writes[0]["applied"], serde_json::json!(false), "被去重丢弃的写入记 applied=false");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// 命中但一条写入都没落下（$old 遇上新增）：审计不能缺席，只是 `writes` 为空。
        /// 这条路径上 `staged` 是空的，引擎必须走到审计之后才 break。
        #[test]
        fn skipped_write_still_audits_with_empty_writes() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            RuleService::new(store.clone())
                .create(
                    actor,
                    ws,
                    "记录旧状态",
                    true,
                    r#"$label = "Status""#,
                    true,
                    None,
                    vec![LabelWrite {
                        label_name: "FinishedAt".into(),
                        op: WriteOp::Set,
                        value: Some(ValueSource::Old),
                    }],
                )
                .unwrap();
            // 新增 Status 时 $old 无值，动作整体跳过，`staged` 为空。
            apply(&store, ws, &[StagedWrite {
                entry_code: entry.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);
            let audits = crate::service::AuditService::new(store.clone()).list(ws, 100).unwrap();
            let applied = audits
                .iter()
                .find(|a| a.action == AuditAction::RuleApplied)
                .expect("命中过就该有一条 RuleApplied");
            let after: serde_json::Value =
                serde_json::from_str(applied.after.as_deref().unwrap_or("null")).unwrap();
            assert_eq!(after["level"], serde_json::json!(1), "命中发生在第 1 层");
            assert_eq!(
                after["triggers"].as_array().map(Vec::len),
                Some(1),
                "审计要记下命中的事件: {after}"
            );
            assert!(
                after["writes"].as_array().is_some_and(|w| w.is_empty()),
                "写入全被跳过时 writes 应为空数组: {after}"
            );
            std::fs::remove_dir_all(&dir).ok();
        }

        /// 一条规则只该在审计里记下**它自己**命中的事件：同层别的规则命中的事件不得混进来。
        #[test]
        fn audit_triggers_list_only_the_rules_own_matched_events() {
            let (dir, store, ws, actor) = setup();
            let entry = EntryService::new(store.clone()).create(actor, ws, "任务").unwrap();
            let svc = RuleService::new(store.clone());
            let set_write = |name: &str, value: ValueSource| LabelWrite {
                label_name: name.into(),
                op: WriteOp::Set,
                value: Some(value),
            };
            // 两条规则各认一个标签的事件，两个事件同处层 0。
            svc.create(
                actor,
                ws,
                "记完成时间",
                true,
                r#"$label = "Status" AND $new = "Finished""#,
                true,
                None,
                vec![set_write("FinishedAt", ValueSource::Now)],
            )
            .unwrap();
            svc.create(
                actor,
                ws,
                "转进行中",
                true,
                r#"$label = "Priority""#,
                true,
                None,
                vec![set_write("Status", ValueSource::Literal(serde_json::json!("InProgress")))],
            )
            .unwrap();
            apply(&store, ws, &[
                StagedWrite {
                    entry_code: entry.code.clone(),
                    label_name: "Status".into(),
                    value: Some(LabelValue::Enum("Finished".into())),
                    actor,
                },
                StagedWrite {
                    entry_code: entry.code.clone(),
                    label_name: "Priority".into(),
                    value: Some(LabelValue::Int(1)),
                    actor,
                },
            ]);
            let audits = crate::service::AuditService::new(store.clone()).list(ws, 100).unwrap();
            let mut by_rule = std::collections::HashMap::new();
            for a in audits.iter().filter(|a| a.action == AuditAction::RuleApplied) {
                let after: serde_json::Value =
                    serde_json::from_str(a.after.as_deref().unwrap_or("null")).unwrap();
                by_rule.insert(after["ruleName"].as_str().unwrap_or("").to_string(), after);
            }
            assert_eq!(by_rule.len(), 2, "两条规则各写一条 RuleApplied");
            for (rule_name, label) in [("记完成时间", "Status"), ("转进行中", "Priority")] {
                let after = by_rule
                    .get(rule_name)
                    .unwrap_or_else(|| panic!("规则「{rule_name}」缺 RuleApplied 审计"));
                let triggers = after["triggers"].as_array().unwrap();
                assert_eq!(triggers.len(), 1, "规则「{rule_name}」只该记自己命中的那一个事件: {after}");
                assert_eq!(triggers[0]["labelName"].as_str(), Some(label));
            }
            std::fs::remove_dir_all(&dir).ok();
        }

        /// `ActionTarget::Query` 按表达式圈定目标：只写命中的条目。
        /// 目标集合取自本层**之前**的 overlay——同层别的规则刚写下的标签它看不到，
        /// 所以事件源 E1 不会被 R1 同层写的 Priority=1 拉进 R2 的目标集合。
        #[test]
        fn query_target_scopes_writes_to_matching_entries_and_is_frozen_within_level() {
            let (dir, store, ws, actor) = setup();
            let svc = EntryService::new(store.clone());
            let e1 = svc.create(actor, ws, "任务一").unwrap();
            let e2 = svc.create(actor, ws, "任务二").unwrap();
            let e3 = svc.create(actor, ws, "任务三").unwrap();
            // 只有 E2 命中 `Priority >= 1`；E3 的 Priority=0 与事件源 E1 都不命中。
            set(&store, ws, &e2.code, "Priority", LabelValue::Int(5), actor);
            set(&store, ws, &e3.code, "Priority", LabelValue::Int(0), actor);

            let rules = RuleService::new(store.clone());
            let set_write = |name: &str, value: ValueSource| LabelWrite {
                label_name: name.into(),
                op: WriteOp::Set,
                value: Some(value),
            };
            rules
                .create(
                    actor,
                    ws,
                    "标记优先级",
                    true,
                    r#"$label = "Status" AND $new = "Finished""#,
                    true,
                    None,
                    vec![set_write("Priority", ValueSource::Literal(serde_json::json!(1)))],
                )
                .unwrap();
            rules
                .create(
                    actor,
                    ws,
                    "记完成时间",
                    true,
                    r#"$label = "Status" AND $new = "Finished""#,
                    false,
                    Some("Priority >= 1"),
                    vec![set_write("FinishedAt", ValueSource::Now)],
                )
                .unwrap();

            let affected = apply(&store, ws, &[StagedWrite {
                entry_code: e1.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);

            assert!(get(&store, &e2.code, "FinishedAt").is_some(), "圈定命中的条目应被写入");
            assert_eq!(
                get(&store, &e1.code, "FinishedAt"),
                None,
                "事件源不得被同层刚写下的 Priority 拉进目标集合"
            );
            assert_eq!(get(&store, &e3.code, "FinishedAt"), None, "不命中的条目不得被写入");
            assert_eq!(get(&store, &e1.code, "Priority"), Some(LabelValue::Int(1)));

            let mut want = vec![e1.code.clone(), e2.code.clone()];
            want.sort();
            assert_eq!(affected, want, "affected 应覆盖用户写入与规则写入的条目，不含 E3");
            std::fs::remove_dir_all(&dir).ok();
        }

        /// `affected` 要覆盖引擎在每一层写过的条目——Task 4 靠它决定 reindex 谁。
        #[test]
        fn affected_covers_entries_written_at_every_level() {
            let (dir, store, ws, actor) = setup();
            let svc = EntryService::new(store.clone());
            let e1 = svc.create(actor, ws, "任务一").unwrap();
            let e2 = svc.create(actor, ws, "任务二").unwrap();
            set(&store, ws, &e2.code, "Priority", LabelValue::Int(5), actor);
            let rules = RuleService::new(store.clone());
            let set_write = |name: &str, value: ValueSource| LabelWrite {
                label_name: name.into(),
                op: WriteOp::Set,
                value: Some(value),
            };
            // 层 1：E1 的 Status=Finished → E1 写 FinishedAt。
            // 层 2：FinishedAt 事件 → 表达式圈到 E2（Priority=5）→ 改 E2 的 Priority。
            rules
                .create(
                    actor,
                    ws,
                    "记完成时间",
                    true,
                    r#"$label = "Status" AND $new = "Finished""#,
                    true,
                    None,
                    vec![set_write("FinishedAt", ValueSource::Now)],
                )
                .unwrap();
            rules
                .create(
                    actor,
                    ws,
                    "压优先级",
                    true,
                    r#"$label = "FinishedAt""#,
                    false,
                    Some("Priority >= 1"),
                    vec![set_write("Priority", ValueSource::Literal(serde_json::json!(2)))],
                )
                .unwrap();

            let affected = apply(&store, ws, &[StagedWrite {
                entry_code: e1.code.clone(),
                label_name: "Status".into(),
                value: Some(LabelValue::Enum("Finished".into())),
                actor,
            }]);

            assert_eq!(
                get(&store, &e2.code, "Priority"),
                Some(LabelValue::Int(2)),
                "第 2 层应圈到 E2 并改它的 Priority"
            );
            let mut want = vec![e1.code.clone(), e2.code.clone()];
            want.sort();
            assert_eq!(affected, want, "affected 应列出每一层被写过的条目");
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
