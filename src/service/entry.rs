use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::label::{encode_label_value_for_index, explode_label_value_for_index};
use crate::domain::query::{extract_index_hints, IndexHint, Interval};
use crate::domain::view::{SortField, SortKey, SortSpec};
use crate::domain::{
    generate_entry_code, AuditAction, AuditLog, DerivedLabel, Entry, EvalEnv, InheritanceGraph,
    LabelSchema, LabelValue, LabelValueType, Labeling, Op, Query,
};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::rule::{RuleEngine, StagedWrite};
use crate::service::search::{SearchBackend, TEXT_CANDIDATE_LIMIT};
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 字段全是 `Arc`，克隆只是复制几个指针，供 `CommentService` 持有同一份实例。
#[derive(Clone)]
pub struct EntryService {
    store: Arc<DocStore>,
    search: Option<Arc<dyn SearchBackend>>,
    rules: RuleEngine,
    messages: crate::service::message::MessageService,
    /// 条目侧调用 `purge_for_entry_ops` 把关联清理拼进自己的 write_batch，
    /// 保证「条目软删除 + 关联清理」原子提交——避免悬空关联行指向已删条目。
    relations: crate::service::relation::RelationService,
}

#[derive(Debug, Clone, Copy)]
pub struct PageInput {
    pub page: usize,
    pub page_size: usize,
}

impl Default for PageInput {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: 20,
        }
    }
}

impl PageInput {
    fn normalized(&self) -> (usize, usize) {
        let page = self.page.max(1);
        let page_size = self.page_size.clamp(1, 100);
        (page, page_size)
    }
}

/// 新版分页入参 —— 同时兼容老的 page/pageSize 与新加的 cursor/limit。
/// GraphQL resolver 按 sort 是否在索引内选择走 query() (PageInput) 还是
/// query_indexed() (QueryInput)。
#[derive(Debug, Clone, Default)]
pub struct QueryInput {
    pub page: Option<u32>,
    pub page_size: Option<u32>,
    /// 走 cursor 路径时传；与 page 二选一；同传时 cursor 优先。
    pub cursor: Option<String>,
    /// cursor 模式下的返回上限。默认 50，上限 200。
    pub limit: Option<u32>,
}

impl QueryInput {
    pub fn limit(&self) -> usize {
        self.limit
            .or(self.page_size)
            .unwrap_or(50)
            .min(200) as usize
    }
}

pub struct QueryResult {
    pub items: Vec<(Entry, Vec<Labeling>)>,
    pub total: usize,
    /// 命中的条目实际带有的标签名（去重排序），供前端提示可用标签。
    pub label_names: Vec<String>,
    /// 当前 sort 在索引内且后面还有匹配时返回；GraphQL 投影为 nextCursor。
    pub next_cursor: Option<String>,
}

impl EntryService {
    pub fn new(
        store: Arc<DocStore>,
        messages: crate::service::message::MessageService,
        relations: crate::service::relation::RelationService,
    ) -> Self {
        let rules = RuleEngine::new(store.clone());
        Self {
            store,
            search: None,
            rules,
            messages,
            relations,
        }
    }

    pub fn with_search(
        store: Arc<DocStore>,
        search: Arc<dyn SearchBackend>,
        messages: crate::service::message::MessageService,
        relations: crate::service::relation::RelationService,
    ) -> Self {
        let rules = RuleEngine::new(store.clone());
        Self {
            store,
            search: Some(search),
            rules,
            messages,
            relations,
        }
    }

    /// 让 GraphQL resolver / Agent 工具能拿到底层的 SearchBackend，
    /// 全文检索不可用（配置未启用 / 未注入）时返回 None。
    pub fn search_available(&self) -> Option<&Arc<dyn SearchBackend>> {
        self.search.as_ref()
    }

    fn reindex(&self, entry: &Entry) {
        let Some(search) = &self.search else { return };
        let labels = self.labelings(&entry.code).unwrap_or_default();
        // 评论正文也进检索：搜得到评论内容，但检索字段与标题/详情分开，不会互相干扰相关度。
        let comments =
            crate::service::search::comments_text(&self.store, &entry.code).unwrap_or_default();
        // 归档与软删除一样，都让条目退出全文检索：归档条目不该再被搜索命中。
        let out_of_play = entry.is_deleted() || self.is_archived(&entry.code).unwrap_or(false);
        if out_of_play {
            if let Err(e) = search.remove_entry(&entry.code) {
                tracing::warn!("移除检索索引失败 {}: {e}", entry.code);
            }
        } else if let Err(e) = search.index_entry(entry, &labels, &comments) {
            tracing::warn!("更新检索索引失败 {}: {e}", entry.code);
        }
    }

    pub fn create(&self, actor: Ulid, workspace_id: Ulid, title: &str) -> Result<Entry, AppError> {
        self.create_with_detail(actor, workspace_id, title, "")
    }

    /// 建条目并一次性写入正文。与 `create` 分开是因为 AI 总结要带着正文落库：
    /// 拆成 create + update 会产生两条审计，还要多走一次乐观并发校验。
    pub fn create_with_detail(
        &self,
        actor: Ulid,
        workspace_id: Ulid,
        title: &str,
        detail: &str,
    ) -> Result<Entry, AppError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::Internal("标题不能为空".to_string()));
        }
        let mut entry = Entry::new(workspace_id, title.to_string(), actor);
        entry.detail = detail.to_string();
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
        // 新条目：仅插入索引，无旧条目可删。labels 为空（label 在 set_labelings 时再建索引）。
        ops.extend(self.idx_puts_for(workspace_id, &entry, &[]));
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(entry)
    }

    pub fn get(&self, code: &str) -> Result<Option<Entry>, AppError> {
        self.store.get(cf::ENTRIES, code.as_bytes())
    }

    /// 供评论服务在评论变更后重建该条目的检索文档——评论不属于 Entry 的字段，
    /// 只能由外部触发这次重建。
    pub fn reindex_by_code(&self, code: &str) -> Result<(), AppError> {
        if let Some(entry) = self.get(code)? {
            self.reindex(&entry);
        }
        Ok(())
    }

    /// 乐观并发更新：expected_updated_at 与当前 updated_at 不一致时返回 ConflictDetected。
    pub fn update(
        &self,
        actor: Ulid,
        actor_name: &str,
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
        // updated_at / title 是索引键的一部分；写前删旧、写后插新。
        let old_labels = self.labels_for_entry(entry.workspace_id, code)?;
        let before = serde_json::to_string(&entry).unwrap_or_default();
        let old_entry = entry.clone();
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
        ops.extend(self.idx_deletes_for(entry.workspace_id, &old_entry, &old_labels));
        ops.extend(self.idx_puts_for(entry.workspace_id, &entry, &old_labels));
        self.store.write_batch(ops)?;
        // 详情里新增的 @姓名 才发消息：上次已通知过的人直接跳过，避免重复打扰。
        let previous =
            self.messages
                .notified_recipients(entry.workspace_id, &entry.code, "entry", None)?;
        self.dispatch_entry_mentions(actor, actor_name, &entry, &previous)?;
        self.reindex(&entry);
        Ok(entry)
    }

    /// 详情里 @姓名 解析 + 发消息：按工作空间成员表反查账号 id，
    /// 与已通知名单求差后逐个 send。和评论侧的 dispatch_mentions 同形，
    /// 只是 source_type / preview 取法不同——评论拿正文前 80 字，条目优先用标题。
    fn dispatch_entry_mentions(
        &self,
        actor: Ulid,
        actor_name: &str,
        entry: &Entry,
        previous_notified: &std::collections::HashSet<Ulid>,
    ) -> Result<(), AppError> {
        let members = self.messages.workspaces.list_members(entry.workspace_id)?;
        let names = crate::service::mention::extract_mention_names(
            &crate::service::mention::flat_text(&entry.detail),
        );
        let targets = crate::service::mention::resolve_mentions(&names, &members);
        let preview = if entry.title.trim().is_empty() {
            crate::service::mention::preview(&entry.detail, 80)
        } else {
            entry.title.clone()
        };
        for rid in targets {
            if rid == actor {
                continue;
            }
            if previous_notified.contains(&rid) {
                continue;
            }
            self.messages.send(
                actor,
                actor_name,
                entry.workspace_id,
                &entry.code,
                "entry",
                None,
                &preview,
                rid,
            )?;
        }
        Ok(())
    }

    /// 软删除：仅置 deleted_at，不物理移除。重复删除幂等（第二次直接 Ok）。
    ///
    /// `actor_name` 必传：删除消息里要冗余展示发起人姓名（`Message.actor_name`）。
    pub fn soft_delete(&self, actor: Ulid, actor_name: &str, code: &str) -> Result<(), AppError> {
        let mut entry = self.get(code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Ok(());
        }
        // 软删除后该 entry 不应再出现在视图里：从所有索引中抹除。
        let old_labels = self.labels_for_entry(entry.workspace_id, code)?;
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
        // 把条目关联清理一并拼进同一批：避免悬空关联行指向已删条目。
        let (_count, rel_purge) =
            self.relations
                .purge_for_entry_ops(actor, entry.workspace_id, code)?;
        ops.extend(rel_purge);
        // 删除索引：不再插入新条目索引。
        ops.extend(self.idx_deletes_for(entry.workspace_id, &entry, &old_labels));
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        // 通知创建人：删除发起人 != 创建人。
        if entry.created_by != actor {
            self.messages.send(
                actor,
                actor_name,
                entry.workspace_id,
                code,
                "entry_deleted",
                None,
                &entry.title,
                entry.created_by,
            )?;
        }
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

    /// 归档：只写标记，条目与打标全部保留，只是移出基础视图与全文检索。幂等。
    /// 已删除的条目不可归档（回到 NotFound），因为删除是比归档更彻底的状态。
    pub fn archive(&self, actor: Ulid, code: &str) -> Result<(), AppError> {
        let entry = self.get(code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        if self.is_archived(code)? {
            return Ok(());
        }
        // 归档后条目仍在索引里（evaluate 时按 archived_at 过滤），但 updated_at 变化 →
        // 索引键变了，必须先删旧再写新。
        let old_labels = self.labels_for_entry(entry.workspace_id, code)?;
        let old_entry = entry.clone();
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
        ops.extend(self.idx_deletes_for(entry.workspace_id, &old_entry, &old_labels));
        ops.extend(self.idx_puts_for(entry.workspace_id, &entry, &old_labels));
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
        // 取消归档同样 updated_at 不一定变，但保持 delete+put 对称以免漏 case。
        let old_labels = self.labels_for_entry(entry.workspace_id, code)?;
        let old_entry = entry.clone();
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
        ops.push(BatchOp::delete(
            cf::ENTRIES_ARCHIVED,
            code.as_bytes().to_vec(),
        ));
        ops.extend(self.idx_deletes_for(entry.workspace_id, &old_entry, &old_labels));
        ops.extend(self.idx_puts_for(entry.workspace_id, &entry, &old_labels));
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(())
    }

    /// 列出工作空间内已归档的条目（不含已删除），按归档时间倒序。
    pub fn list_archived(&self, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
        let rows = self
            .store
            .scan_prefix(cf::ENTRIES_BY_WORKSPACE, &workspace_id.to_bytes())?;
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
        list_entries(&self.store, workspace_id)
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
        crate::service::label::check_account_member(&self.store, entry.workspace_id, &lv)?;
        let labeling = Labeling::new(entry_code.to_string(), label_name.to_string(), lv, actor);
        let before = self
            .store
            .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(entry_code, label_name))?;
        let mut ops = labeling_set_ops(
            entry.workspace_id,
            &labeling,
            schema.value_type,
            actor,
            before.as_ref(),
        )?;
        // plan 必须在提交前算：引擎从库里取前像，用户写入先落库的话它就看不到变更了。
        let staged = StagedWrite {
            entry_code: entry_code.to_string(),
            label_name: label_name.to_string(),
            value: Some(labeling.value.clone()),
            actor,
        };
        let plan = self
            .rules
            .plan(entry.workspace_id, std::slice::from_ref(&staged))?;
        let mut affected = vec![entry_code.to_string()];
        if let Some(p) = plan {
            affected.extend(p.affected);
            // 规则动作与用户写入拼成同一批：任一步失败整体回滚，不留半成品。
            ops.extend(p.ops);
        }
        self.store.write_batch(ops)?;
        affected.sort();
        affected.dedup();
        for code in &affected {
            if let Ok(Some(e)) = self.get(code) {
                self.reindex(&e);
            }
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
                .get::<LabelSchema>(cf::LABEL_SCHEMAS, &keys::label_schema_key(ws_id, name))?
                .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {name}")))?;
            let lv = LabelValue::from_json(value, &schema)?;
            crate::service::label::check_account_member(&self.store, ws_id, &lv)?;
            resolved.push((name.clone(), lv));
        }

        let mut ops = Vec::new();
        for entry in &entries {
            for (name, lv) in &resolved {
                let vt = self
                    .store
                    .get::<LabelSchema>(cf::LABEL_SCHEMAS, &keys::label_schema_key(ws_id, name))?
                    .map(|s| s.value_type)
                    .unwrap_or(LabelValueType::Null);
                let labeling = Labeling::new(entry.code.clone(), name.clone(), lv.clone(), actor);
                let before = self
                    .store
                    .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(&entry.code, name))?;
                ops.extend(labeling_set_ops(
                    ws_id,
                    &labeling,
                    vt,
                    actor,
                    before.as_ref(),
                )?);
            }
        }
        let written = entries.len() * resolved.len();
        // 索引：标签值是标签索引键的一部分，旧条目要删、新条目要写。
        // 一次性在 write_batch 前把每个 entry 的 idx delete + idx put 拼进去。
        for entry in &entries {
            let old_labels = self.labels_for_entry(ws_id, &entry.code)?;
            ops.extend(self.idx_deletes_for(ws_id, entry, &old_labels));
            // 新 labels = 旧 labels 中匹配 resolved 的条目替换
            let new_labels: Vec<Labeling> = old_labels
                .iter()
                .map(|l| {
                    if let Some((_, lv)) = resolved.iter().find(|(n, _)| n == &l.label_name) {
                        Labeling::new(l.entry_code.clone(), l.label_name.clone(), lv.clone(), actor)
                    } else {
                        l.clone()
                    }
                })
                .collect();
            ops.extend(self.idx_puts_for(ws_id, entry, &new_labels));
        }
        let staged: Vec<StagedWrite> = entries
            .iter()
            .flat_map(|e| {
                resolved.iter().map(move |(name, lv)| StagedWrite {
                    entry_code: e.code.clone(),
                    label_name: name.clone(),
                    value: Some(lv.clone()),
                    actor,
                })
            })
            .collect();
        let plan = self.rules.plan(ws_id, &staged)?;
        let mut affected: Vec<String> = entries.iter().map(|e| e.code.clone()).collect();
        if let Some(p) = plan {
            affected.extend(p.affected);
            ops.extend(p.ops);
        }
        self.store.write_batch(ops)?;
        affected.sort();
        affected.dedup();
        for code in &affected {
            if let Ok(Some(e)) = self.get(code) {
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
        let before = self
            .store
            .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(entry_code, label_name))?;
        let vt = self
            .store
            .get::<LabelSchema>(
                cf::LABEL_SCHEMAS,
                &keys::label_schema_key(entry.workspace_id, label_name),
            )?
            .map(|s| s.value_type)
            .unwrap_or(LabelValueType::Null);
        let mut ops = labeling_ops(
            entry.workspace_id,
            entry_code,
            label_name,
            vt,
            None,
            actor,
            before.as_ref(),
        )?;
        // 索引：标签被移除后该标签索引条目要删；其他标签索引保持不变。
        let old_labels = self.labels_for_entry(entry.workspace_id, entry_code)?;
        ops.extend(self.idx_deletes_for(entry.workspace_id, &entry, &old_labels));
        let new_labels: Vec<Labeling> = old_labels
            .iter()
            .filter(|l| l.label_name != label_name)
            .cloned()
            .collect();
        ops.extend(self.idx_puts_for(entry.workspace_id, &entry, &new_labels));
        let staged = StagedWrite {
            entry_code: entry_code.to_string(),
            label_name: label_name.to_string(),
            value: None,
            actor,
        };
        let plan = self
            .rules
            .plan(entry.workspace_id, std::slice::from_ref(&staged))?;
        let mut affected = vec![entry_code.to_string()];
        if let Some(p) = plan {
            affected.extend(p.affected);
            ops.extend(p.ops);
        }
        self.store.write_batch(ops)?;
        affected.sort();
        affected.dedup();
        for code in &affected {
            if let Ok(Some(e)) = self.get(code) {
                self.reindex(&e);
            }
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
        let mut map: std::collections::HashMap<String, Vec<Labeling>> =
            std::collections::HashMap::new();
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
        if !store
            .scan_prefix(cf::LABELINGS_BY_WORKSPACE, b"")?
            .is_empty()
        {
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
                    keys::labeling_by_workspace_key(
                        entry.workspace_id,
                        &l.entry_code,
                        &l.label_name,
                    ),
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

    /// `LABELINGS_BY_LABEL` 列族为空时重建：遍历 `LABELINGS` 取每条打标，
    /// 按工作空间的 schema 查 `value_type` 后编码成索引键。已填充则直接返回 0（幂等）。
    /// 找不到 schema 或编码失败（NaN 等）跳过对应条目——宁可漏索引，也不让回填整批失败。
    /// 返回成功写入的索引条数。
    pub fn labelings_by_label_backfill(&self, store: &DocStore) -> Result<usize, AppError> {
        if !store.scan_prefix(cf::LABELINGS_BY_LABEL, b"")?.is_empty() {
            return Ok(0);
        }
        // 按工作空间缓存 schema，避免同一个 workspace 内每条打标都回查一次。
        let mut schema_cache: std::collections::HashMap<
            Ulid,
            std::collections::HashMap<String, LabelValueType>,
        > = std::collections::HashMap::new();
        let mut ops = Vec::new();
        let mut count = 0;
        for (_, lv) in store.scan_prefix(cf::LABELINGS, b"")? {
            let l: Labeling = bincode::deserialize(&lv)?;
            let entry: Entry = match store.get::<Entry>(cf::ENTRIES, l.entry_code.as_bytes())? {
                Some(e) => e,
                None => continue,
            };
            let ws = entry.workspace_id;
            let cache = schema_cache.entry(ws).or_insert_with(|| {
                match store.scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes()) {
                    Ok(rows) => rows
                        .into_iter()
                        .filter_map(|(_, sv)| bincode::deserialize::<LabelSchema>(&sv).ok())
                        .map(|s| (s.name.clone(), s.value_type))
                        .collect(),
                    Err(_) => std::collections::HashMap::new(),
                }
            });
            let vt = match cache.get(&l.label_name) {
                Some(vt) => *vt,
                None => continue,
            };
            for op in label_index_puts(ws, &l.entry_code, &l.label_name, &l.value, vt) {
                ops.push(op);
                count += 1;
            }
        }
        if !ops.is_empty() {
            store.write_batch(ops)?;
        }
        Ok(count)
    }

    /// 该 workspace 内 sortable=true 的 LabelSchema 列表。每个写路径灌索引时取一次，
    /// 缓存不必要：写路径每写一次就扫一次的成本相对 ENTRIES 主写入可忽略。
    fn sortable_schemas(&self, ws: Ulid) -> Vec<LabelSchema> {
        self.store
            .scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())
            .ok()
            .map(|rows| {
                rows.into_iter()
                    .filter_map(|(_, v)| bincode::deserialize::<LabelSchema>(&v).ok())
                    .filter(|s| s.sortable)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 单个 entry 的 idx BatchOp 列表（写完后插入）。给定 entry 当前状态 + 该 entry 的
    /// 打标列表，生成 3 个内建字段 + 每个 sortable 标签的索引条目。
    fn idx_puts_for(
        &self,
        ws: Ulid,
        entry: &Entry,
        labels: &[Labeling],
    ) -> Vec<BatchOp> {
        let schemas = self.sortable_schemas(ws);
        self.idx_batch_puts(ws, entry, labels, &schemas)
    }

    /// 单个 entry 的 idx BatchOp 列表（写完前删除旧条目）。与 idx_puts_for 配对；
    /// updated_at / title / label 值变化时必须先删旧再写新。
    fn idx_deletes_for(
        &self,
        ws: Ulid,
        entry: &Entry,
        labels: &[Labeling],
    ) -> Vec<BatchOp> {
        let schemas = self.sortable_schemas(ws);
        self.idx_batch_deletes(ws, entry, labels, &schemas)
    }

    /// 按 entry_code 取该 entry 的所有打标。用于 idx_deletes_for 在写前捕获旧 label 状态。
    fn labels_for_entry(&self, ws: Ulid, code: &str) -> Result<Vec<Labeling>, AppError> {
        let mut out = Vec::new();
        for (_, v) in self.store.scan_prefix(cf::LABELINGS_BY_WORKSPACE, &ws.to_bytes())? {
            let l: Labeling = bincode::deserialize(&v)?;
            if l.entry_code == code {
                out.push(l);
            }
        }
        Ok(out)
    }

    /// 侧栏计数。简单查询（空 AND）走 ENTRIES_BY_WORKSPACE 字节长度；
    /// 复杂查询走 query_indexed 的 total —— 它本来就要扫完整索引。
    pub fn count(&self, ws: Ulid, query: &Query) -> Result<usize, AppError> {
        if query.is_simple_countable() {
            // ENTRIES_BY_WORKSPACE 的 key 是 (ws_id, code)，bincode 长度的代价
            // 比反序列化每个 Entry 小一个数量级。Phase 2 可加 ENTRIES_COUNT_BY_WORKSPACE
            // 维护精确计数。
            let rows = self
                .store
                .scan_prefix(cf::ENTRIES_BY_WORKSPACE, &ws.to_bytes())?;
            return Ok(rows.len());
        }
        let input = QueryInput {
            page: Some(1),
            page_size: Some(1),
            cursor: None,
            limit: Some(1),
        };
        let r = self.query_indexed(ws, query, &SortSpec::default(), &input)?;
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
        // 标签定义必须先于 hint 提取加载：Account 类型据此跳过索引，
        // 否则 `Assignee has "<email>"` 这种 want 走 ULID 校验会失败，
        // 候选全集空、整批条目被错杀在 hint 层。
        let all_schemas: Vec<LabelSchema> = self
            .store
            .scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())?
            .into_iter()
            .filter_map(|(_, v)| bincode::deserialize::<LabelSchema>(&v).ok())
            .collect();
        // 取出顶层 AND 里全部可被二级索引加速的标签条件。
        let hints = extract_index_hints(query, &all_schemas);
        let all_rows: Vec<Entry> = self.list(ws)?;
        // 先按 hints 计算候选 entry_code 集合；多个 hint 取交集。
        // 集合与全量等大时跳过过滤（退化回原路径），集合为空时 `query` 自然没命中。
        let rows: Vec<Entry> = if hints.is_empty() {
            all_rows
        } else {
            let base: HashSet<String> = all_rows.iter().map(|e| e.code.clone()).collect();
            match resolve_candidates_from_hints(&self.store, ws, &hints, &base)? {
                Some(codes) if codes.len() < base.len() => all_rows
                    .into_iter()
                    .filter(|e| codes.contains(&e.code))
                    .collect(),
                _ => all_rows,
            }
        };

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

        // 账号解析只在查询树或排序链真的引用 CreatedBy / UpdatedBy 时才扫表。
        // 查询侧用 `contains_account_field`（遍历 Query 树），排序侧只看 sort keys：
        // 两路任一命中即预加载，否则不付出「全账号表扫一次」的代价。
        let sort_needs_account = sort
            .keys
            .iter()
            .any(|k| matches!(k.field, SortField::CreatedBy | SortField::UpdatedBy));
        let accounts: std::collections::HashMap<ulid::Ulid, (String, String)> =
            if query.contains_account_field() || sort_needs_account {
                let mut m = std::collections::HashMap::new();
                for (_, v) in self.store.scan_prefix(cf::ACCOUNTS, b"")? {
                    let a: crate::domain::Account = bincode::deserialize(&v)?;
                    m.insert(a.id, (a.name, a.email));
                }
                m
            } else {
                std::collections::HashMap::new()
            };
        // 标签定义已在 query 顶部取回（先于 hint 提取）：本处只把它们映射成
        // `label_of` 用的 (值类型, 时间格式) 表，外加一份给继承图。
        // 标签名 → (值类型, 时间格式)，供时间型标签的 >/< 比较取布局。
        let schemas: std::collections::HashMap<
            String,
            (crate::domain::LabelValueType, Option<String>),
        > = all_schemas
            .iter()
            .map(|s| (s.name.clone(), (s.value_type, s.format.clone())))
            .collect();
        let account_of = |id: ulid::Ulid| accounts.get(&id).cloned();
        let label_of = |name: &str| schemas.get(name).cloned();

        let empty: Vec<Labeling> = Vec::new();
        let labels_of = |code: &str| -> &[Labeling] {
            labels_map.get(code).map(Vec::as_slice).unwrap_or(&empty)
        };
        // 继承推导只在表达式真的写了 `L4+` 时才做——否则一次全空间扫描白算。
        let empty_derived: Vec<DerivedLabel> = Vec::new();
        let derived_map: std::collections::HashMap<String, Vec<DerivedLabel>> =
            if query.contains_inherited_label() {
                let graph = InheritanceGraph::build(&all_schemas);
                rows.iter()
                    .map(|e| (e.code.clone(), graph.derive(labels_of(&e.code))))
                    .filter(|(_, d)| !d.is_empty())
                    .collect()
            } else {
                std::collections::HashMap::new()
            };
        // 标签值历史变更：仅在表达式用到 Changed/ChangedValue 时才扫审计。
        // 单 workspace 内 labeling 审计量级小（每条 entry 几十到几百条），
        // 全量入内存按 entry_code 索引，与 `labels_map` 同构。
        let audit_map: std::collections::HashMap<String, Vec<AuditLog>> =
            if query.contains_label_change() {
                let codes: HashSet<String> = rows.iter().map(|e| e.code.clone()).collect();
                load_label_change_audits(&self.store, ws, &codes)?
            } else {
                std::collections::HashMap::new()
            };
        let empty_audit: Vec<AuditLog> = Vec::new();
        let audit_of = |code: &str| {
            audit_map
                .get(code)
                .map(Vec::as_slice)
                .unwrap_or(&empty_audit)
        };
        // Changed：区间 `interval` 内至少一条 `LabelingSet` / `LabelingRemoved` 事件，
        // 且日志涉及的标签名 == name。区间判定由 `Interval::contains` 负责（开闭端点语义）。
        let changed_during = |code: &str, name: &str, interval: &Interval| -> bool {
            let logs = audit_of(code);
            logs.iter().any(|l| {
                audit_label_name(l).as_deref() == Some(name) && interval.contains(l.at)
            })
        };
        // ChangedValue：区间 `interval` 内至少一条事件，其 before.value == from
        // 且 after.value == to。`from` / `to` 任一为 `None` 表示该侧通配（任意值匹配）。
        // `audit_label_before_value` / `audit_label_value` 已处理 LabelingSet /
        // LabelingRemoved 两种 action。
        let changed_value = |code: &str,
                             name: &str,
                             from: Option<&serde_json::Value>,
                             to: Option<&serde_json::Value>,
                             interval: &Interval|
         -> bool {
            let logs = audit_of(code);
            logs.iter().any(|l| {
                audit_label_name(l).as_deref() == Some(name)
                    && interval.contains(l.at)
                    && from.is_none_or(|f| audit_label_before_value(l) == *f)
                    && to.is_none_or(|t| audit_label_value(l) == *t)
            })
        };
        let mut matched: Vec<Entry> = rows
            .into_iter()
            .filter(|e| {
                let labels = labels_of(&e.code);
                let text_ok = |kw: &str| match &text_hits {
                    Some((keyword, set)) => kw == keyword && set.contains(&e.code),
                    None => false,
                };
                let derived = derived_map
                    .get(&e.code)
                    .map(Vec::as_slice)
                    .unwrap_or(&empty_derived);
                let env = EvalEnv {
                    text_hit: &text_ok,
                    account_of: &account_of,
                    label_of: &label_of,
                    event: None,
                    derived,
                    changed_during: Some(&changed_during),
                    changed_value: Some(&changed_value),
                };
                query.evaluate(e, labels, &env)
            })
            .collect();

        // 命中集合里出现过的标签名（跨分页，去重排序）。
        let mut label_names: Vec<String> = matched
            .iter()
            .flat_map(|e| labels_of(&e.code).iter().map(|l| l.label_name.clone()))
            .collect();
        // `L4+` 引用的标签也进列集合：否则纯靠继承命中的条目在表格里没有可展示的列，
        // 「继承来的要有所区别」也就无从谈起。
        label_names.extend(query.inherited_label_names());
        label_names.sort();
        label_names.dedup();

        sort_rows(&mut matched, sort, &labels_map, &schemas, &accounts);
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
        Ok(QueryResult {
            items,
            total,
            label_names,
            next_cursor: None,
        })
    }

    /// 走复合索引的 cursor 查询路径。仅当 sort 命中索引（`EntryIndexService::sort_is_indexed`
    /// 返回 true）时由 resolver 调用；否则走 `query()`。
    ///
    /// 实现概要：
    /// 1. 根据 SortField 选主索引 CF；
    /// 2. 解码 cursor（若有）+ hash 校验；
    /// 3. 从 cursor 位置起反向/正向扫 limit 条 key，每条 key 解码出 entry_code；
    /// 4. 对每条命中的 entry 反查 ENTRIES 主表 + 该 entry 的 labels；
    /// 5. 走 `query.evaluate(...)` 过滤；命中且未到 limit 就 append 到 results；
    /// 6. limit 满后再扫一轮「是否还有匹配」以决定 next_cursor 的有无。
    ///
    /// 索引覆盖范围内不再做 workspace 维度的全量预加载 —— labels / accounts /
    /// audit 只为匹配到的 entry 按需加载。复用了 `query()` 中的 EvalEnv 构造逻辑
    /// （TODO Phase 2：把 EvalEnv 构造抽成共享 helper，避免 query / query_indexed 双
    /// 份维护的字段漂移风险）。
    #[allow(clippy::too_many_lines)]
    pub fn query_indexed(
        &self,
        ws: Ulid,
        query: &Query,
        sort: &SortSpec,
        input: &QueryInput,
    ) -> Result<QueryResult, AppError> {
        use crate::domain::view::SortField;
        use crate::storage::keys::{ws_end_exclusive, ws_start};

        // 1. 主索引 CF + 方向
        let primary_field = sort
            .keys
            .first()
            .map(|k| k.field.clone())
            .unwrap_or(SortField::UpdatedAt);
        let descending = sort.keys.first().map(|k| k.desc).unwrap_or(true);
        let primary_cf: &'static str = match &primary_field {
            SortField::UpdatedAt => cf::ENTRIES_IDX_UPDATED,
            SortField::CreatedAt => cf::ENTRIES_IDX_CREATED,
            SortField::Title => cf::ENTRIES_IDX_TITLE,
            // Phase 1 暂不支持 sortable 标签的索引：key 布局里 tag_name 变长、
            // 解析 entry_code 需要反向匹配，跟 ENTRIES 主表的全量反查成本几乎
            // 等价于「退化到 Legacy 路径」。Phase 2 加 u16 tag_name_len 前缀解决。
            SortField::Label(_) => {
                return Err(AppError::InvalidQuery(
                    "排序字段不在 cursor 路径索引范围内（Phase 1 仅内建字段）".to_string(),
                ));
            }
            _ => {
                return Err(AppError::InvalidQuery(
                    "排序字段不在 cursor 路径索引范围内".to_string(),
                ))
            }
        };

        // 2. cursor 解码 + 起点 key 构造
        let sort_hash = crate::service::cursor::sort_spec_hash(sort);
        let ws_end = ws_end_exclusive(ws); // None 表示 ws 是全 0xFF，扫到 CF 末尾
        let ws_end_vec = ws_end.clone().unwrap_or_default();
        let mut scan_start = ws_start(ws);
        let mut cursor_code: Option<String> = None;
        if let Some(c) = &input.cursor {
            let decoded = crate::service::cursor::decode_cursor(c)?;
            if decoded.ws != ws {
                return Err(AppError::InvalidQuery("cursor workspace 不匹配".to_string()));
            }
            if decoded.sort_hash != sort_hash {
                return Err(AppError::InvalidQuery(
                    "排序已变更，请从头分页".to_string(),
                ));
            }
            // resume key = 上次 emit 的 code 之后的字节。本轮第一版：从 cursor_code 起续扫，
            // 跳过所有 < cursor_code 的 entry_code（自身已在上页 emit）。
            // 多键排序的二级排序条件本轮先按 cursor_code tiebreak 处理（精确 resume 需要
            // 把 last_values 折回 sort_value bytes 再构造复合 key，超出 Phase 1 范围）。
            cursor_code = Some(decoded.last_code.clone());
        }

        // 3. EvalEnv 构造（标签定义 / accounts / 文本命中 / 审计 等）
        let all_schemas: Vec<LabelSchema> = self
            .store
            .scan_prefix(cf::LABEL_SCHEMAS, &ws.to_bytes())?
            .into_iter()
            .filter_map(|(_, v)| bincode::deserialize::<LabelSchema>(&v).ok())
            .collect();
        let sort_needs_account = sort
            .keys
            .iter()
            .any(|k| matches!(k.field, SortField::CreatedBy | SortField::UpdatedBy));
        let accounts: std::collections::HashMap<Ulid, (String, String)> =
            if query.contains_account_field() || sort_needs_account {
                let mut m = std::collections::HashMap::new();
                for (_, v) in self.store.scan_prefix(cf::ACCOUNTS, b"")? {
                    let a: crate::domain::Account = bincode::deserialize(&v)?;
                    m.insert(a.id, (a.name, a.email));
                }
                m
            } else {
                std::collections::HashMap::new()
            };
        let schemas: std::collections::HashMap<
            String,
            (crate::domain::LabelValueType, Option<String>),
        > = all_schemas
            .iter()
            .map(|s| (s.name.clone(), (s.value_type, s.format.clone())))
            .collect();
        let account_of = |id: Ulid| accounts.get(&id).cloned();
        let label_of = |name: &str| schemas.get(name).cloned();
        let text_hits: Option<std::collections::HashSet<String>> =
            match query.first_text_keyword() {
                Some(keyword) => {
                    let search = self
                        .search
                        .as_ref()
                        .ok_or_else(|| AppError::Internal("全文检索不可用".to_string()))?;
                    Some(
                        search
                            .search(ws, &keyword, TEXT_CANDIDATE_LIMIT)
                            .or_else(|_| search.search(ws, &keyword, 10_000))?
                            .into_iter()
                            .collect(),
                    )
                }
                None => None,
            };
        let text_ok = |kw: &str| -> bool {
            match &text_hits {
                Some(set) => set.contains(kw),
                None => false,
            }
        };
        // audit / 继承 / Changed-闭包：本轮 query_indexed 仅支持 cursor 路径下
        // 的「简单」filter（不依赖 Changed / ChangedValue / 继承标签）。复杂 filter
        // 仍走 Legacy path。判定：contains_inherited_label / contains_label_change。
        if query.contains_inherited_label() || query.contains_label_change() {
            return Err(AppError::InvalidQuery(
                "当前 cursor 路径暂不支持 Changed / 继承标签条件，请改用 page 分页".to_string(),
            ));
        }

        // 4. 流式扫描
        let limit = input.limit();
        let mut matched: Vec<(Entry, Vec<Labeling>)> = Vec::with_capacity(limit);
        let mut total: usize = 0;
        let mut last_emitted_code: Option<String> = None;
        let mut last_emitted_field: Option<SortField> = None;
        let mut has_more_in_index = true;
        let mut cursor_key_set = false;

        loop {
            // 反向扫描：[ws_start, cursor_key) —— desc 排序下从最新的往前
            // 正向扫描：[cursor_key, ws_end) —— asc 排序下从最老的往后
            let batch = if descending {
                self.store
                    .scan_range_reverse(primary_cf, &scan_start, &cursor_key_set_state(
                        &scan_start,
                        &cursor_code,
                    ), 500)?
            } else {
                let end_key = ws_end_vec.clone();
                self.store.scan_range(primary_cf, &scan_start, &end_key, 500)?
            };
            if batch.is_empty() {
                has_more_in_index = false;
                break;
            }
            for (k, _) in &batch {
                let code = decode_code_from_idx_key(&primary_field, &ws.to_bytes(), k);
                if let Some(prev) = &cursor_code {
                    if !cursor_key_set && code.as_str() <= prev.as_str() {
                        // 跳过 cursor_code 自身及之前的条目
                        continue;
                    }
                }
                cursor_key_set = true;
                let entry = match self
                    .store
                    .get::<Entry>(cf::ENTRIES, &keys::entry_by_workspace_key(ws, &code))?
                {
                    Some(e) => e,
                    None => continue, // 主表已删但索引还在 —— 跳过
                };
                let labels = self.labels_for_entry(ws, &code)?;
                let text_kw = query.first_text_keyword();
                let env = EvalEnv {
                    text_hit: &text_ok,
                    account_of: &account_of,
                    label_of: &label_of,
                    event: None,
                    derived: &[],
                    changed_during: None,
                    changed_value: None,
                };
                // text_ok 闭包要求 keyword 参数；这里把它包成「无视 kw 都用 set 命中」的版本
                let text_hit_for_entry = |_kw: &str| -> bool {
                    text_kw
                        .as_ref()
                        .map(|kw| text_ok(kw))
                        .unwrap_or(false)
                };
                let env_for_entry = EvalEnv {
                    text_hit: &text_hit_for_entry,
                    account_of: &account_of,
                    label_of: &label_of,
                    event: None,
                    derived: &[],
                    changed_during: None,
                    changed_value: None,
                };
                if query.evaluate(&entry, &labels, &env_for_entry) {
                    total += 1;
                    if matched.len() < limit {
                        matched.push((entry, labels));
                        last_emitted_code = Some(code);
                        last_emitted_field = Some(primary_field.clone());
                    }
                }
            }
            // 推进 cursor 到 batch 最后一条之后
            if let Some((last_k, _)) = batch.last() {
                scan_start = last_k.clone();
            }
            if matched.len() >= limit {
                // 多扫一轮确认还有匹配
                let peek = if descending {
                    self.store.scan_range_reverse(
                        primary_cf,
                        &ws_start(ws),
                        &scan_start,
                        1,
                    )?
                } else {
                    self.store.scan_range(
                        primary_cf,
                        &scan_start,
                        &ws_end_vec,
                        1,
                    )?
                };
                has_more_in_index = !peek.is_empty();
                break;
            }
            if batch.len() < 500 {
                has_more_in_index = false;
                break;
            }
        }

        // 5. 多键排序：内存里对 limit 条做 tiebreaker 排序
        if sort.keys.len() > 1 {
            let mut to_sort: Vec<&mut (Entry, Vec<Labeling>)> = matched.iter_mut().collect();
            // 第二级排序键按 labels_of 闭包 + sort_spec.keys[1..]
            // 这里 Phase 1 简化：只对 limit 条做稳定 in-place 排序（按 entry 的
            // updated_at 二级键；多标签二级排序是 Phase 2）。
            to_sort.sort_by(|a, b| {
                let av = match &sort.keys.get(1).map(|k| &k.field) {
                    Some(SortField::UpdatedAt) | None => a.0.updated_at.timestamp_millis(),
                    _ => 0,
                };
                let bv = match &sort.keys.get(1).map(|k| &k.field) {
                    Some(SortField::UpdatedAt) | None => b.0.updated_at.timestamp_millis(),
                    _ => 0,
                };
                if sort.keys.get(1).map(|k| k.desc).unwrap_or(true) {
                    bv.cmp(&av)
                } else {
                    av.cmp(&bv)
                }
            });
        }

        // 6. next_cursor
        let next_cursor = if has_more_in_index {
            if let (Some(code), Some(field)) = (last_emitted_code, last_emitted_field) {
                let entry = &matched
                    .last()
                    .map(|(e, _)| e.clone())
                    .unwrap_or_else(|| unreachable!());
                let labels = &matched.last().map(|(_, l)| l.clone()).unwrap_or_default();
                let value = crate::service::cursor::cursor_value_for_entry(&field, entry, labels);
                let c = crate::domain::cursor::Cursor {
                    ws,
                    sort_hash,
                    last_values: vec![value],
                    last_code: code,
                    backward: false,
                };
                Some(crate::service::cursor::encode_cursor(&c))
            } else {
                None
            }
        } else {
            None
        };

        // 7. label_names 汇总
        let mut label_names: Vec<String> = matched
            .iter()
            .flat_map(|(_, labels)| labels.iter().map(|l| l.label_name.clone()))
            .collect();
        label_names.extend(query.inherited_label_names());
        label_names.sort();
        label_names.dedup();

        Ok(QueryResult {
            items: matched,
            total,
            label_names,
            next_cursor,
        })
    }

    /// 给定 entry 当前状态 + 该 entry 的打标，生成「插入/更新索引」的 BatchOp 列表。
    /// 调用方把返回值 `extend` 进自己的 BatchOp vec，与主写入走同一事务。
    pub fn idx_batch_puts(
        &self,
        ws: Ulid,
        entry: &Entry,
        labels: &[Labeling],
        schemas: &[LabelSchema],
    ) -> Vec<BatchOp> {
        use crate::storage::keys::{
            idx_created_key, idx_label_key, idx_title_key, idx_updated_key, label_bucket_cf,
        };
        let mut ops = Vec::with_capacity(3 + labels.len());
        ops.push(BatchOp::put_raw(
            cf::ENTRIES_IDX_UPDATED,
            idx_updated_key(ws, entry.updated_at.timestamp_millis(), &entry.code),
            Vec::new(),
        ));
        ops.push(BatchOp::put_raw(
            cf::ENTRIES_IDX_CREATED,
            idx_created_key(ws, entry.created_at.timestamp_millis(), &entry.code),
            Vec::new(),
        ));
        ops.push(BatchOp::put_raw(
            cf::ENTRIES_IDX_TITLE,
            idx_title_key(ws, &entry.title, &entry.code),
            Vec::new(),
        ));
        for schema in schemas.iter().filter(|s| s.sortable) {
            if let Some(l) = labels.iter().find(|l| l.label_name == schema.name) {
                if let Ok(encoded) = crate::domain::label::encode_label_value_for_index(
                    &l.value, schema.value_type,
                ) {
                    ops.push(BatchOp::put_raw(
                        label_bucket_cf(&schema.name),
                        idx_label_key(ws, &schema.name, &encoded, &entry.code),
                        Vec::new(),
                    ));
                }
            }
        }
        ops
    }

    /// 与 `idx_batch_puts` 对称：删除该 entry 的所有索引条目。
    /// updated_at / title / label 值变化时必须先调用此方法再调用 idx_batch_puts，
    /// 否则旧索引条目留在库里指向已经不存在的位置。
    pub fn idx_batch_deletes(
        &self,
        ws: Ulid,
        entry: &Entry,
        labels: &[Labeling],
        schemas: &[LabelSchema],
    ) -> Vec<BatchOp> {
        use crate::storage::keys::{
            idx_created_key, idx_label_key, idx_title_key, idx_updated_key, label_bucket_cf,
        };
        let mut ops = Vec::with_capacity(3 + labels.len());
        ops.push(BatchOp::delete(
            cf::ENTRIES_IDX_UPDATED,
            idx_updated_key(ws, entry.updated_at.timestamp_millis(), &entry.code),
        ));
        ops.push(BatchOp::delete(
            cf::ENTRIES_IDX_CREATED,
            idx_created_key(ws, entry.created_at.timestamp_millis(), &entry.code),
        ));
        ops.push(BatchOp::delete(
            cf::ENTRIES_IDX_TITLE,
            idx_title_key(ws, &entry.title, &entry.code),
        ));
        for schema in schemas.iter().filter(|s| s.sortable) {
            if let Some(l) = labels.iter().find(|l| l.label_name == schema.name) {
                if let Ok(encoded) = crate::domain::label::encode_label_value_for_index(
                    &l.value, schema.value_type,
                ) {
                    ops.push(BatchOp::delete(
                        label_bucket_cf(&schema.name),
                        idx_label_key(ws, &schema.name, &encoded, &entry.code),
                    ));
                }
            }
        }
        ops
    }
}

/// 工作空间内的「在视图内」条目：不含已删除，也不含已归档。
/// 抽成自由函数是为了让 `RuleEngine` 的目标圈定与视图列表口径完全一致——
/// 规则引擎不能依赖 `EntryService`（会形成循环依赖）。
pub fn list_entries(store: &DocStore, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
    let prefix = workspace_id.to_bytes();
    let rows = store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &prefix)?;
    let mut entries = Vec::new();
    for (key, _) in rows {
        if key.len() <= 16 {
            continue;
        }
        let code = std::str::from_utf8(&key[16..]).unwrap_or("").to_string();
        if let Some(e) = store.get::<Entry>(cf::ENTRIES, code.as_bytes())? {
            let archived = store
                .get_raw(cf::ENTRIES_ARCHIVED, code.as_bytes())?
                .is_some();
            if !e.is_deleted() && !archived {
                entries.push(e);
            }
        }
    }
    entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(entries)
}

/// 一次性按工作空间加载所有 `LabelingSet` / `LabelingRemoved` 审计，
/// 按 entry_code（= resource_id）索引成 `HashMap<entry_code, Vec<AuditLog>>`。
///
/// 索引键 `cf::AUDIT_LOGS_BY_RESOURCE` 的形状是 `(resource_type \0 resource_id \0 desc8 \0 id16)`；
/// 这里前缀用 `"labeling\0"` 把同一 type 的所有条目一次性扫出来，再按 workspace 过滤——
/// 单 workspace 内的 labeling 审计量级小（与 `labels_map` 同构），全量入内存足够。
fn load_label_change_audits(
    store: &DocStore,
    workspace_id: Ulid,
    entry_codes: &HashSet<String>,
) -> Result<std::collections::HashMap<String, Vec<AuditLog>>, AppError> {
    let prefix = b"labeling\0".to_vec();
    let rows = store.scan_prefix(cf::AUDIT_LOGS_BY_RESOURCE, &prefix)?;
    let mut by_code: std::collections::HashMap<String, Vec<AuditLog>> =
        std::collections::HashMap::new();
    for (key, _) in rows {
        // 主键 tail = desc8 + id16，从索引键末尾截取。
        if key.len() < 24 {
            continue;
        }
        let log_key = &key[key.len() - 24..];
        let Some(log) = store.get::<AuditLog>(cf::AUDIT_LOGS, log_key)? else {
            continue;
        };
        if log.workspace_id != Some(workspace_id) {
            continue;
        }
        if !matches!(
            log.action,
            AuditAction::LabelingSet | AuditAction::LabelingRemoved
        ) {
            continue;
        }
        if !entry_codes.contains(&log.resource_id) {
            continue;
        }
        by_code
            .entry(log.resource_id.clone())
            .or_default()
            .push(log);
    }
    Ok(by_code)
}

/// 从 audit 的 before/after JSON 快照里提取被变更标签的 `label_name`。
/// 审计快照是 `Labeling` 的 serde_json 序列化结果——字段是 snake_case
///（`label_name` / `entry_code` / `value` / `set_by` / `set_at`）。
/// 解析失败（理论上不会发生，审计与打标同事务写入）→ 返回 None，调用方跳过该条。
///
/// 返回的是 owned `String`——`serde_json::Value` 是局部变量，引用它会撞 borrow checker；
/// 单 workspace 内每次 evaluate 调用的次数与 audit 条数同量级（通常几十），String 分配可接受。
fn audit_label_name(log: &AuditLog) -> Option<String> {
    let s = log.after.as_deref().or(log.before.as_deref())?;
    let v: serde_json::Value = serde_json::from_str(s).ok()?;
    v.get("label_name")
        .and_then(|x| x.as_str())
        .map(str::to_string)
}

/// 从 audit 快照的 JSON 里反序列化 `Labeling`，取其 `value` 并走
/// `LabelValue::to_json()` 还原成与表达式里 `from` / `to` 同形状的 `serde_json::Value`。
///
/// `LabelingSet` 的 before 可能是 None（首次打标）：返回 `Value::Null`。
/// `LabelingRemoved` 的 after 必然 None：返回 `Value::Null`，与「值变为缺失」对齐。
///
/// 用于 Op::ChangedValue 的「前值匹配」：用户写 `from "open" to "closed"` 时，
/// 要在审计里找出 before.value == "open" 且 after.value == "closed" 的事件。
fn audit_label_value_from(s: &str) -> Option<serde_json::Value> {
    let labeling: Labeling = serde_json::from_str(s).ok()?;
    Some(labeling.value.to_json())
}

/// `audit_label_before_value` / `audit_label_value` 的薄包装：分别按
/// `log.before` / `log.after` 抽取 LabelValue 并归一化为 JSON。
/// 任一侧缺失（LabelingSet 首次打标 / LabelingRemoved 移除）→ 视为 `Value::Null`。
fn audit_label_before_value(log: &AuditLog) -> serde_json::Value {
    if log.action == AuditAction::LabelingRemoved {
        // 移除操作没有 after；before 仍是旧值。
        return log
            .before
            .as_deref()
            .and_then(audit_label_value_from)
            .unwrap_or(serde_json::Value::Null);
    }
    // LabelingSet：before 可能为 None（首次打标）。
    log.before
        .as_deref()
        .and_then(audit_label_value_from)
        .unwrap_or(serde_json::Value::Null)
}

/// 用于 Op::ChangedValue 的「后值匹配」：与 `audit_label_before_value` 配对。
fn audit_label_value(log: &AuditLog) -> serde_json::Value {
    if log.action == AuditAction::LabelingRemoved {
        return serde_json::Value::Null;
    }
    if let Some(v) = log.after.as_deref().and_then(audit_label_value_from) {
        return v;
    }
    // 兜底：理论上 LabelingSet 一定有 after。
    log.before
        .as_deref()
        .and_then(audit_label_value_from)
        .unwrap_or(serde_json::Value::Null)
}

/// 把 `IndexHint` 列表转成 entry_code 候选集合：多个 hint 取交集。
/// schema 缺失 / 值与类型不匹配 / Float NaN 时该 hint 跳过（视作不约束）。
/// 返回 `None` 表示 hints 为空或全部跳过，调用方应走全扫。
fn resolve_candidates_from_hints(
    store: &DocStore,
    ws: Ulid,
    hints: &[IndexHint],
    base_codes: &HashSet<String>,
) -> Result<Option<HashSet<String>>, AppError> {
    if hints.is_empty() {
        return Ok(None);
    }
    let mut combined: Option<HashSet<String>> = None;
    for hint in hints {
        let schema = match store.get::<LabelSchema>(
            cf::LABEL_SCHEMAS,
            &keys::label_schema_key(ws, &hint.label_name),
        )? {
            Some(s) => s,
            None => continue,
        };
        let codes = codes_for_hint(store, ws, hint, &schema, base_codes);
        combined = Some(match combined {
            None => codes,
            Some(prev) => prev.intersection(&codes).cloned().collect(),
        });
        if matches!(&combined, Some(s) if s.is_empty()) {
            return Ok(combined);
        }
    }
    Ok(combined)
}

fn codes_for_hint(
    store: &DocStore,
    ws: Ulid,
    hint: &IndexHint,
    schema: &LabelSchema,
    base_codes: &HashSet<String>,
) -> HashSet<String> {
    match hint.op {
        Op::Eq => match resolve_value_for_index(&hint.value, schema) {
            Ok(enc) => codes_with_exact_value(store, ws, &hint.label_name, &enc),
            Err(_) => HashSet::new(),
        },
        Op::Ne => match resolve_value_for_index(&hint.value, schema) {
            Ok(enc) => {
                let hits = codes_with_exact_value(store, ws, &hint.label_name, &enc);
                base_codes - &hits
            }
            Err(_) => base_codes.clone(),
        },
        Op::In => {
            let mut out = HashSet::new();
            if let Some(arr) = hint.value.as_array() {
                for v in arr {
                    if let Ok(enc) = resolve_value_for_index(v, schema) {
                        out.extend(codes_with_exact_value(store, ws, &hint.label_name, &enc));
                    }
                }
            }
            out
        }
        Op::NotIn => {
            let mut hits = HashSet::new();
            if let Some(arr) = hint.value.as_array() {
                for v in arr {
                    if let Ok(enc) = resolve_value_for_index(v, schema) {
                        hits.extend(codes_with_exact_value(store, ws, &hint.label_name, &enc));
                    }
                }
            }
            base_codes - &hits
        }
        Op::Gt | Op::Ge | Op::Lt | Op::Le => match resolve_value_for_index(&hint.value, schema) {
            Ok(threshold) => codes_in_range(store, ws, &hint.label_name, hint.op, &threshold),
            Err(_) => HashSet::new(),
        },
        _ => HashSet::new(),
    }
}

/// 把查询里的 JSON 值按 schema 转成 `LabelValue`，再编码成索引字节。
fn resolve_value_for_index(
    v: &serde_json::Value,
    schema: &LabelSchema,
) -> Result<Vec<u8>, AppError> {
    let lv = LabelValue::from_json(v, schema)?;
    encode_label_value_for_index(&lv, schema.value_type)
}

/// 严格相等：键前缀到编码后的字节完全匹配；scan_prefix 返回的 key 末尾 16 字节即 entry_code。
fn codes_with_exact_value(store: &DocStore, ws: Ulid, name: &str, enc: &[u8]) -> HashSet<String> {
    let mut prefix = keys::labeling_by_label_prefix(ws, name);
    prefix.extend_from_slice(enc);
    let mut out = HashSet::new();
    if let Ok(rows) = store.scan_prefix(cf::LABELINGS_BY_LABEL, &prefix) {
        for (k, _) in rows {
            if let Some(c) = extract_entry_code_from_key(&k) {
                out.insert(c);
            }
        }
    }
    out
}

/// 范围扫描：取整个 `(ws, name)` 前缀，内存里按编码字节序比较。
/// 索引条目量远小于工作空间条目量时这是 O(标签值集合)；比逐条 evaluate 仍快得多。
fn codes_in_range(
    store: &DocStore,
    ws: Ulid,
    name: &str,
    op: Op,
    threshold: &[u8],
) -> HashSet<String> {
    let prefix = keys::labeling_by_label_prefix(ws, name);
    let mut out = HashSet::new();
    if let Ok(rows) = store.scan_prefix(cf::LABELINGS_BY_LABEL, &prefix) {
        for (k, _) in rows {
            if k.len() < prefix.len() + 16 {
                continue;
            }
            let code_start = k.len() - 16;
            let enc = &k[prefix.len()..code_start];
            let matched = match op {
                Op::Gt => enc > threshold,
                Op::Ge => enc >= threshold,
                Op::Lt => enc < threshold,
                Op::Le => enc <= threshold,
                _ => false,
            };
            if matched {
                if let Ok(c) = std::str::from_utf8(&k[code_start..]) {
                    out.insert(c.to_string());
                }
            }
        }
    }
    out
}

fn extract_entry_code_from_key(key: &[u8]) -> Option<String> {
    if key.len() < 16 {
        return None;
    }
    std::str::from_utf8(&key[key.len() - 16..])
        .ok()
        .map(String::from)
}

/// 构造「写 / 删一个标签」的全套 ops：Labeling 两条（主键 + 工作空间索引）+ 一条标签审计
/// + LABELINGS_BY_LABEL 的 Put/Delete。索引键依赖 schema 的 `value_type`，调用方必须传入。
/// 索引写入失败（如 NaN、值与类型不匹配）只跳过索引条目，主写入照常完成——
/// 没索引的标签最多退回全扫，不会丢数据。
/// `before` 由调用方给定：`EntryService` 从库里读，`RuleEngine` 从内存后置状态读——
/// 引擎的写入尚未提交，读库拿到的是前像。
pub fn labeling_ops(
    ws: Ulid,
    code: &str,
    label_name: &str,
    value_type: LabelValueType,
    value: Option<&LabelValue>,
    actor: Ulid,
    before: Option<&Labeling>,
) -> Result<Vec<BatchOp>, AppError> {
    match value {
        Some(lv) => labeling_set_ops(
            ws,
            &Labeling::new(code.to_string(), label_name.to_string(), lv.clone(), actor),
            value_type,
            actor,
            before,
        ),
        None => {
            let audit = AuditLog::new(
                AuditAction::LabelingRemoved,
                actor,
                "labeling",
                code,
                Some(ws),
                before.map(|l| serde_json::to_string(l).unwrap_or_default()),
                None,
            );
            let mut ops = audit_ops(&audit)?;
            ops.push(BatchOp::delete(
                cf::LABELINGS,
                keys::labeling_key(code, label_name),
            ));
            ops.push(BatchOp::delete(
                cf::LABELINGS_BY_WORKSPACE,
                keys::labeling_by_workspace_key(ws, code, label_name),
            ));
            // `before` 已知旧值，按其编码算出旧索引键逐条删；编码失败时跳过对应条目。
            if let Some(prev) = before {
                for op in label_index_deletes(ws, code, label_name, &prev.value, value_type) {
                    ops.push(op);
                }
            }
            Ok(ops)
        }
    }
}

/// cursor 路径续扫时跳过 cursor_code 自身及之前的 key：scan_range_reverse 的上界
/// 就是当前 batch 末尾的 key —— cursor_code 不必参与 end key 计算。
fn cursor_key_set_state(start: &[u8], _cursor_code: &Option<String>) -> Vec<u8> {
    // Phase 1 简化：start 就是 ws_start；cursor_code 在外层逐 key 跳过。
    start.to_vec()
}

/// 从索引 key 的尾部解出 entry_code。Phase 1 仅内建三字段索引：
/// - UpdatedAt/CreatedAt：key = (ws_id, desc_ms[8B], code)
/// - Title：key = (ws_id, title_utf8, code)
fn decode_code_from_idx_key(
    primary_field: &crate::domain::view::SortField,
    ws_bytes: &[u8],
    key: &[u8],
) -> String {
    use crate::domain::view::SortField;
    let after_ws = &key[ws_bytes.len()..];
    match primary_field {
        SortField::UpdatedAt | SortField::CreatedAt => {
            String::from_utf8_lossy(&after_ws[8..]).into_owned()
        }
        SortField::Title => String::from_utf8_lossy(after_ws).into_owned(),
        _ => String::new(),
    }
}

/// 写入路径的 ops：接收已构造好的 `Labeling`，调用方返回给上层的就是落库的那个对象，
/// 审计快照也因此与返回值逐字节一致（若在内部另构造一份，`set_at` 会差几微秒）。
fn labeling_set_ops(
    ws: Ulid,
    labeling: &Labeling,
    value_type: LabelValueType,
    actor: Ulid,
    before: Option<&Labeling>,
) -> Result<Vec<BatchOp>, AppError> {
    let audit = AuditLog::new(
        AuditAction::LabelingSet,
        actor,
        "labeling",
        &labeling.entry_code,
        Some(ws),
        before.map(|l| serde_json::to_string(l).unwrap_or_default()),
        Some(serde_json::to_string(labeling).unwrap_or_default()),
    );
    let mut ops = audit_ops(&audit)?;
    ops.push(BatchOp::put(
        cf::LABELINGS,
        keys::labeling_key(&labeling.entry_code, &labeling.label_name),
        labeling,
    )?);
    ops.push(BatchOp::put(
        cf::LABELINGS_BY_WORKSPACE,
        keys::labeling_by_workspace_key(ws, &labeling.entry_code, &labeling.label_name),
        labeling,
    )?);
    // 旧值不同则先把旧索引键删掉（更新场景下编码不同会变成「写入新的 + 漏删旧的」）。
    if let Some(prev) = before {
        if prev.value != labeling.value {
            for op in label_index_deletes(
                ws,
                &labeling.entry_code,
                &labeling.label_name,
                &prev.value,
                value_type,
            ) {
                ops.push(op);
            }
        }
    }
    for op in label_index_puts(
        ws,
        &labeling.entry_code,
        &labeling.label_name,
        &labeling.value,
        value_type,
    ) {
        ops.push(op);
    }
    Ok(ops)
}

/// 新打标对应的索引 Put 集合：列表类型每个元素一条记录，编码失败时静默跳过。
fn label_index_puts(
    ws: Ulid,
    code: &str,
    name: &str,
    lv: &LabelValue,
    vt: LabelValueType,
) -> Vec<BatchOp> {
    let mut ops = Vec::new();
    for item in explode_label_value_for_index(lv) {
        if let Ok(encoded) = encode_label_value_for_index(&item, vt) {
            ops.push(BatchOp::put_raw(
                cf::LABELINGS_BY_LABEL,
                keys::labeling_by_label_value_key(ws, name, &encoded, code),
                Vec::new(),
            ));
        }
    }
    ops
}

/// 旧打标对应的索引 Delete 集合：与 `label_index_puts` 对称。
fn label_index_deletes(
    ws: Ulid,
    code: &str,
    name: &str,
    lv: &LabelValue,
    vt: LabelValueType,
) -> Vec<BatchOp> {
    let mut ops = Vec::new();
    for item in explode_label_value_for_index(lv) {
        if let Ok(encoded) = encode_label_value_for_index(&item, vt) {
            ops.push(BatchOp::delete(
                cf::LABELINGS_BY_LABEL,
                keys::labeling_by_label_value_key(ws, name, &encoded, code),
            ));
        }
    }
    ops
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

/// 按 `SortSpec` 对条目就地排序：按键顺序依次比较，全部相等则保持原序
/// （`sort_by` 是稳定排序，条目的原始顺序来自扫描，稳定可复现）。
///
/// **缺失值永远排在最后，与升降序无关**——字典型排序里把空值翻到最前通常不是
/// 用户想要的，而「空值永远最后」是一条不需要额外开关的确定规则。
fn sort_rows(
    rows: &mut [Entry],
    sort: &SortSpec,
    labels_map: &std::collections::HashMap<String, Vec<Labeling>>,
    schemas: &std::collections::HashMap<String, (LabelValueType, Option<String>)>,
    // 账号 id → (姓名, 邮箱)，用于按 CreatedBy / UpdatedBy 排序时映射成展示名。
    // 不引用账号字段的代码路径传空 map 即可，行为退化为按 id 比较。
    accounts: &std::collections::HashMap<Ulid, (String, String)>,
) {
    rows.sort_by(|a, b| {
        for k in &sort.keys {
            match key_ordering(a, b, k, labels_map, schemas, accounts) {
                // 这两支不参与 desc 翻转：缺失永远最后。
                KeyOrder::AMissing => return Ordering::Greater,
                KeyOrder::BMissing => return Ordering::Less,
                KeyOrder::BothMissing => continue,
                KeyOrder::Values(o) => {
                    let o = if k.desc { o.reverse() } else { o };
                    if o != Ordering::Equal {
                        return o;
                    }
                }
            }
        }
        Ordering::Equal
    });
}

enum KeyOrder {
    /// a 在这个键上没有值，b 有 → a 排后面。
    AMissing,
    /// b 没有值 → b 排后面。
    BMissing,
    BothMissing,
    Values(Ordering),
}

fn key_ordering(
    a: &Entry,
    b: &Entry,
    k: &SortKey,
    labels_map: &std::collections::HashMap<String, Vec<Labeling>>,
    schemas: &std::collections::HashMap<String, (LabelValueType, Option<String>)>,
    accounts: &std::collections::HashMap<Ulid, (String, String)>,
) -> KeyOrder {
    let SortField::Label(name) = &k.field else {
        // 内置列恒有值，不存在缺失这一支。
        // 账号字段展示名做大小写不敏感比较；账号已删除时退回 id 以保次序稳定。
        // `accounts` 里只有排序字段需要时才会被预加载（见 `query()` 的 sort 旁路），
        // 不引用账号字段的代码路径直接拿空 map，行为退化为按 Ulid 比较——比乱排好。
        let account_key = |e: &Entry, which: &'static str| -> String {
            let id = match which {
                "createdBy" => e.created_by.clone(),
                _ => e.updated_by.clone(),
            };
            accounts
                .get(&id)
                .map(|(name, email)| {
                    if name.trim().is_empty() {
                        email.to_lowercase()
                    } else {
                        name.to_lowercase()
                    }
                })
                .unwrap_or_else(|| id.to_string())
        };
        let o = match k.field {
            SortField::UpdatedAt => a.updated_at.cmp(&b.updated_at),
            SortField::CreatedAt => a.created_at.cmp(&b.created_at),
            // 标题大小写不敏感，与改动前的行为一致。
            SortField::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortField::CreatedBy => account_key(a, "createdBy").cmp(&account_key(b, "createdBy")),
            SortField::UpdatedBy => account_key(a, "updatedBy").cmp(&account_key(b, "updatedBy")),
            SortField::Label(_) => unreachable!(),
        };
        return KeyOrder::Values(o);
    };
    let va = label_value_of(a, name, labels_map);
    let vb = label_value_of(b, name, labels_map);
    match (va, vb) {
        (None, None) => KeyOrder::BothMissing,
        (None, Some(_)) => KeyOrder::AMissing,
        (Some(_), None) => KeyOrder::BMissing,
        (Some(x), Some(y)) => {
            // 值类型取标签声明；声明取不到（schema 已被删）时退到字符串比较。
            let vt = schemas
                .get(name)
                .map(|(t, _)| *t)
                .unwrap_or(LabelValueType::String);
            KeyOrder::Values(compare_values(x, y, vt))
        }
    }
}

fn label_value_of<'a>(
    e: &Entry,
    name: &str,
    labels_map: &'a std::collections::HashMap<String, Vec<Labeling>>,
) -> Option<&'a LabelValue> {
    labels_map
        .get(&e.code)?
        .iter()
        .find(|l| l.label_name == name)
        .map(|l| &l.value)
}

fn compare_values(a: &LabelValue, b: &LabelValue, vt: LabelValueType) -> Ordering {
    // 多值标签逐元素按字符串比较，字典序。
    if let (LabelValue::EnumList(x), LabelValue::EnumList(y)) = (a, b) {
        return x
            .iter()
            .map(String::as_str)
            .cmp(y.iter().map(String::as_str));
    }
    match vt {
        LabelValueType::Integer | LabelValueType::Float | LabelValueType::Currency => {
            match (as_number(a), as_number(b)) {
                (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
                // 按声明类型解析不出来（脏数据）：退到字符串比较，好过认定两者相等。
                _ => display_key(a).cmp(&display_key(b)),
            }
        }
        LabelValueType::Boolean => {
            matches!(a, LabelValue::Bool(true)).cmp(&matches!(b, LabelValue::Bool(true)))
        }
        // enum / string / email / account / date / time / datetime，以及无值标签：
        // 一律按存下来的字符串比较。无值标签的 `LabelValue::Null` 两边都相等，
        // 于是「有 < 无」由缺失规则自然给出。
        _ => display_key(a).cmp(&display_key(b)),
    }
}

fn as_number(v: &LabelValue) -> Option<f64> {
    match v {
        LabelValue::Int(i) => Some(*i as f64),
        LabelValue::Float(f) | LabelValue::Currency(f) => Some(*f),
        LabelValue::String(s)
        | LabelValue::Enum(s)
        | LabelValue::Date(s)
        | LabelValue::Time(s)
        | LabelValue::DateTime(s)
        | LabelValue::Email(s) => s.parse().ok(),
        _ => None,
    }
}

fn display_key(v: &LabelValue) -> String {
    match v {
        LabelValue::Null => String::new(),
        LabelValue::Bool(b) => b.to_string(),
        LabelValue::Int(i) => i.to_string(),
        LabelValue::Float(f) | LabelValue::Currency(f) => f.to_string(),
        LabelValue::String(s)
        | LabelValue::Enum(s)
        | LabelValue::Date(s)
        | LabelValue::Time(s)
        | LabelValue::DateTime(s)
        | LabelValue::Email(s)
        | LabelValue::Account(s) => s.clone(),
        LabelValue::EnumList(xs) | LabelValue::AccountList(xs) => xs.join(","),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::search::TantivyBackend;
    use crate::service::WorkspaceService;

    use crate::domain::query::{Condition, Field, Op, Query};
    use crate::domain::view::{SortField, SortKey, SortSpec};

    fn temp_search() -> (String, std::sync::Arc<TantivyBackend>) {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-entry-search-{}", Ulid::new()));
        let dir = p.to_string_lossy().into_owned();
        let idx = std::sync::Arc::new(TantivyBackend::open(&dir).unwrap());
        (dir, idx)
    }

    #[test]
    fn query_filters_by_label_and_pages() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let msg_svc = crate::service::message::MessageService::new(
            store.clone(),
            WorkspaceService::new(store.clone()),
            None,
        );
        let rel_svc = crate::service::relation::RelationService::new(store.clone());
        let svc = EntryService::with_search(store.clone(), search, msg_svc, rel_svc);
        for i in 0..5 {
            let e = svc.create(actor, ws_id, &format!("条目{i}")).unwrap();
            if i % 2 == 0 {
                svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null))
                    .unwrap();
            }
        }
        let q = Query::Cond(Condition {
            field: Field::Label("Task".into()),
            op: Op::Present,
            value: None,
        });
        let page = PageInput {
            page: 1,
            page_size: 2,
        };
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
        let msg_svc = crate::service::message::MessageService::new(
            store.clone(),
            WorkspaceService::new(store.clone()),
            None,
        );
        let rel_svc = crate::service::relation::RelationService::new(store.clone());
        let svc = EntryService::with_search(store.clone(), search, msg_svc, rel_svc);
        let a = svc.create(actor, ws_id, "找回密码失败").unwrap();
        svc.update(
            actor,
            "tester",
            &a.code,
            &a.updated_at.to_rfc3339(),
            "找回密码失败",
            "验证码收不到",
        )
        .unwrap();
        let b = svc.create(actor, ws_id, "找回密码失败").unwrap();
        svc.set_labeling(actor, &a.code, "Task", &serde_json::json!(null))
            .unwrap();

        let q = Query::And(vec![
            Query::Cond(Condition {
                field: Field::Label("Task".into()),
                op: Op::Present,
                value: None,
            }),
            Query::Cond(Condition {
                field: Field::Text,
                op: Op::Contains,
                value: Some(serde_json::json!("密码")),
            }),
        ]);
        let r = svc
            .query(ws_id, &q, &SortSpec::default(), PageInput::default())
            .unwrap();
        assert_eq!(r.total, 1);
        assert_eq!(r.items[0].0.code, a.code, "b 未打标，应被过滤掉");
        let _ = b;
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_reports_label_names_of_matched_entries() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let msg_svc = crate::service::message::MessageService::new(
            store.clone(),
            WorkspaceService::new(store.clone()),
            None,
        );
        let rel_svc = crate::service::relation::RelationService::new(store.clone());
        let svc = EntryService::with_search(store.clone(), search, msg_svc, rel_svc);
        let a = svc.create(actor, ws_id, "甲").unwrap();
        svc.set_labeling(actor, &a.code, "Task", &serde_json::json!(null))
            .unwrap();
        svc.set_labeling(actor, &a.code, "Bug", &serde_json::json!(null))
            .unwrap();
        let b = svc.create(actor, ws_id, "乙").unwrap();
        svc.set_labeling(actor, &b.code, "Task", &serde_json::json!(null))
            .unwrap();

        // 不过滤时两个标签都出现；跨分页汇总，去重排序。
        let r = svc
            .query(
                ws_id,
                &Query::all(),
                &SortSpec::default(),
                PageInput::default(),
            )
            .unwrap();
        assert_eq!(r.label_names, vec!["Bug".to_string(), "Task".to_string()]);

        // 过滤到只剩「乙」时，只有它带过的 Task。
        let only_b = Query::Cond(Condition {
            field: Field::Text,
            op: Op::Contains,
            value: Some(serde_json::json!("乙")),
        });
        let r = svc
            .query(ws_id, &only_b, &SortSpec::default(), PageInput::default())
            .unwrap();
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.label_names, vec!["Task".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn query_sorts_by_title_both_directions() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let msg_svc = crate::service::message::MessageService::new(
            store.clone(),
            WorkspaceService::new(store.clone()),
            None,
        );
        let rel_svc = crate::service::relation::RelationService::new(store.clone());
        let svc = EntryService::with_search(store.clone(), search, msg_svc, rel_svc);
        for t in ["b", "a", "c"] {
            svc.create(actor, ws_id, t).unwrap();
        }
        let asc = SortSpec {
            keys: vec![SortKey {
                field: SortField::Title,
                desc: false,
            }],
        };
        let r = svc
            .query(ws_id, &Query::all(), &asc, PageInput::default())
            .unwrap();
        let titles: Vec<String> = r.items.into_iter().map(|(e, _)| e.title).collect();
        assert_eq!(titles, vec!["a", "b", "c"]);

        // desc 是默认排序方向，必须单独覆盖。
        let desc = SortSpec {
            keys: vec![SortKey {
                field: SortField::Title,
                desc: true,
            }],
        };
        let r = svc
            .query(ws_id, &Query::all(), &desc, PageInput::default())
            .unwrap();
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
        let msg_svc = crate::service::message::MessageService::new(store.clone(), ws_svc, None);
        let rel_svc = crate::service::relation::RelationService::new(store.clone());
        let entry_svc = EntryService::new(store.clone(), msg_svc, rel_svc);
        (dir, store, entry_svc, ws.id, actor)
    }

    #[test]
    fn update_with_stale_timestamp_conflicts() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "hello").unwrap();
        // 错误的时间戳 → 冲突
        let err = svc
            .update(actor, "tester", &e.code, "2000-01-01T00:00:00Z", "x", "")
            .unwrap_err();
        assert!(matches!(err, AppError::ConflictDetected));
        // 正确的时间戳 → 成功
        let expected = e.updated_at.to_rfc3339();
        let u = svc
            .update(actor, "tester", &e.code, &expected, "改了", "详情")
            .unwrap();
        assert_eq!(u.title, "改了");
        assert_eq!(u.detail, "详情");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn soft_delete_filters_from_list() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "待删除").unwrap();
        assert_eq!(svc.list(ws_id).unwrap().len(), 1);
        svc.soft_delete(actor, "tester", &e.code).unwrap();
        assert!(svc.list(ws_id).unwrap().is_empty());
        let got = svc.get(&e.code).unwrap().unwrap();
        assert!(got.is_deleted());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_removes_from_list_but_keeps_entry_and_is_reversible() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "陈旧条目").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null))
            .unwrap();
        assert_eq!(svc.list(ws_id).unwrap().len(), 1);

        svc.archive(actor, &e.code).unwrap();
        assert!(
            svc.list(ws_id).unwrap().is_empty(),
            "归档后不该出现在默认列表"
        );
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
        assert!(
            after["archived_at"].is_string(),
            "after 快照必须携带 archived_at"
        );
        assert_eq!(
            list.iter()
                .filter(|l| l.action == AuditAction::EntryArchived)
                .count(),
            1,
            "重复归档只留一条审计"
        );

        svc.unarchive(actor, &e.code).unwrap();
        let list2 = audit.list(ws_id, 100).unwrap();
        assert!(list2
            .iter()
            .any(|l| l.action == AuditAction::EntryUnarchived));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archive_and_delete_are_independent() {
        let (dir, store, svc, ws_id, actor) = setup();
        let a = svc.create(actor, ws_id, "先归档再删除").unwrap();
        svc.archive(actor, &a.code).unwrap();
        svc.soft_delete(actor, "tester", &a.code).unwrap();
        assert!(
            svc.list_archived(ws_id).unwrap().is_empty(),
            "已删除的不该出现在归档列表"
        );

        // 已删除的条目不能再归档。
        let b = svc.create(actor, ws_id, "先删除").unwrap();
        svc.soft_delete(actor, "tester", &b.code).unwrap();
        assert!(matches!(
            svc.archive(actor, &b.code),
            Err(AppError::NotFound)
        ));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn archived_entries_are_excluded_from_fulltext_search() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let (_sdir, search) = temp_search();
        let msg_svc = crate::service::message::MessageService::new(
            store.clone(),
            WorkspaceService::new(store.clone()),
            None,
        );
        let rel_svc = crate::service::relation::RelationService::new(store.clone());
        let svc = EntryService::with_search(store.clone(), search.clone(), msg_svc, rel_svc);
        let e = svc.create(actor, ws_id, "独一无二的关键词").unwrap();
        assert_eq!(search.num_docs(), 1);

        svc.archive(actor, &e.code).unwrap();
        assert_eq!(search.num_docs(), 0, "归档后从检索索引移除");
        assert!(search.search(ws_id, "关键词", 10).unwrap().is_empty());

        svc.unarchive(actor, &e.code).unwrap();
        assert_eq!(search.num_docs(), 1, "取消归档后重新入索引");
        assert_eq!(
            search.search(ws_id, "关键词", 10).unwrap(),
            vec![e.code.clone()]
        );
        std::fs::remove_dir_all(&_sdir).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_backfill_skips_archived_entries() {
        let (dir, store, _svc, ws_id, actor) = setup();
        let msg_svc = crate::service::message::MessageService::new(
            store.clone(),
            WorkspaceService::new(store.clone()),
            None,
        );
        let rel_svc = crate::service::relation::RelationService::new(store.clone());
        let svc = EntryService::new(store.clone(), msg_svc, rel_svc);
        let keep = svc.create(actor, ws_id, "保留的条目").unwrap();
        let gone = svc.create(actor, ws_id, "归档的条目").unwrap();
        svc.archive(actor, &gone.code).unwrap();

        // 空索引回填：按 CF 里的归档标记跳过归档条目，而不是照单全收。
        let (sdir, fresh) = temp_search();
        assert_eq!(fresh.backfill(&store).unwrap(), 1, "只回填未归档条目");
        assert!(fresh.search(ws_id, "归档", 10).unwrap().is_empty());
        assert_eq!(
            fresh.search(ws_id, "保留", 10).unwrap(),
            vec![keep.code.clone()]
        );
        std::fs::remove_dir_all(&sdir).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_labeling_clears_value() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null))
            .unwrap();
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
        let u = svc
            .update(actor, "tester", &e.code, &expected, "改了", "详情")
            .unwrap();
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
            .update(actor, "tester", &e.code, "2000-01-01T00:00:00Z", "x", "")
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
        svc.soft_delete(actor, "tester", &e.code).unwrap();
        let expected = e.updated_at.to_rfc3339();
        let err = svc
            .update(actor, "tester", &e.code, &expected, "x", "")
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound), "已删除条目不可再更新");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn soft_delete_records_entry_deleted_audit_and_is_idempotent() {
        let (dir, store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "待删").unwrap();
        svc.soft_delete(actor, "tester", &e.code).unwrap();

        let audit = crate::service::audit::AuditService::new(store.clone());
        let list = audit.list(ws_id, 100).unwrap();
        let log = list
            .iter()
            .find(|l| l.action == AuditAction::EntryDeleted)
            .expect("必须产生 EntryDeleted 审计");
        assert_eq!(log.resource_id, e.code);
        let after: serde_json::Value = serde_json::from_str(log.after.as_deref().unwrap()).unwrap();
        assert!(
            after["deleted_at"].is_string(),
            "after 快照必须携带 deleted_at"
        );

        // 重复删除幂等：不新增审计、不报错
        svc.soft_delete(actor, "tester", &e.code).unwrap();
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
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null))
            .unwrap();
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
                    default_value: None,
                    links: vec![],
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
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null))
            .unwrap();

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
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!(null))
            .unwrap();
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
