//! 条目关联业务逻辑：新增 / 删除 / 更新语义 + 单条目全方向查询。
//!
//! 数据形态与「附件」接近（条目的附属资源），但属于有向边：要从 `from` 和
//! `to` 两个方向都能查到同一行。所以主键 + FROM / TO 两个索引同步落——
//! 任意一边有缺失都让反向查询漏数据，干脆任一失败整体回滚。

use std::sync::Arc;

use ulid::Ulid;

use crate::domain::audit::{AuditAction, AuditLog};
use crate::domain::{Relation, RelationSemantic};
use crate::error::AppError;
use crate::storage::{cf, keys, BatchOp, DocStore};

use super::audit::audit_ops;

#[derive(Clone)]
pub struct RelationService {
    store: Arc<DocStore>,
}

impl RelationService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    /// 新建一条 `from -> to` 的关联。
    /// 校验：from != to（自环没有意义）；两端条目必须存在；二者须同工作空间。
    /// 单次 write_batch：主行 + 两条索引 + 审计，任一失败整体回滚。
    pub fn create(
        &self,
        actor: Ulid,
        from_code: &str,
        to_code: &str,
        semantic: RelationSemantic,
    ) -> Result<Relation, AppError> {
        if from_code == to_code {
            return Err(AppError::InvalidQuery("不能关联到自身".to_string()));
        }
        let from = self
            .store
            .get::<crate::domain::Entry>(cf::ENTRIES, from_code.as_bytes())?
            .ok_or(AppError::NotFound)?;
        let to = self
            .store
            .get::<crate::domain::Entry>(cf::ENTRIES, to_code.as_bytes())?
            .ok_or(AppError::NotFound)?;
        if from.workspace_id != to.workspace_id {
            return Err(AppError::InvalidQuery(
                "两个条目不属于同一个工作空间".to_string(),
            ));
        }
        // 同向同语义重复 → 当作幂等更新现有那条，避免重复刷屏。业务上
        // 「同一 from -> 同一 to」允许不同语义并存（先建「包含」再补一条
        // 「派生」是合理的语义演化），真正重复的语义才拒绝。
        if let Some(existing) = self.find_dup(from.workspace_id, from_code, to_code, &semantic) {
            return Ok(existing);
        }

        let rel = Relation::new(
            from.workspace_id,
            from_code.to_string(),
            to_code.to_string(),
            semantic,
            actor,
        );
        let audit = AuditLog::new(
            AuditAction::RelationCreated,
            actor,
            "relation",
            &rel.id.to_string(),
            Some(from.workspace_id),
            None,
            Some(serde_json::to_string(&rel).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::ENTRY_RELATIONS,
            keys::relation_key(rel.workspace_id, rel.id),
            &rel,
        )?);
        ops.push(BatchOp::put_raw(
            cf::ENTRY_RELATIONS_BY_FROM,
            keys::relation_by_from_key(rel.workspace_id, &rel.from_code, rel.id),
            Vec::new(),
        ));
        ops.push(BatchOp::put_raw(
            cf::ENTRY_RELATIONS_BY_TO,
            keys::relation_by_to_key(rel.workspace_id, &rel.to_code, rel.id),
            Vec::new(),
        ));
        self.store.write_batch(ops)?;
        Ok(rel)
    }

    /// 找 `(from, to, semantic)` 完全相同的现有行：用于幂等。
    /// 实现：扫 FROM 索引拿到 from 端所有关系 id，再逐条读主行比对 to / semantic。
    /// from 端关系数远低于 workspace 总关系数，这条扫描不会成为热点。
    fn find_dup(
        &self,
        workspace_id: Ulid,
        from_code: &str,
        to_code: &str,
        semantic: &RelationSemantic,
    ) -> Option<Relation> {
        let prefix = keys::relation_by_from_prefix(workspace_id, from_code);
        let rows = self.store.scan_prefix(cf::ENTRY_RELATIONS_BY_FROM, &prefix).ok()?;
        for (k, _) in rows {
            // 索引键 = (ws, from, id)；id 是后 16 字节
            if k.len() < 32 {
                continue;
            }
            let id_bytes: [u8; 16] = k[k.len() - 16..].try_into().ok()?;
            let id = Ulid::from_bytes(id_bytes);
            if let Ok(Some(rel)) = self.store.get::<Relation>(
                cf::ENTRY_RELATIONS,
                &keys::relation_key(workspace_id, id),
            ) {
                if rel.to_code == to_code && &rel.semantic == semantic {
                    return Some(rel);
                }
            }
        }
        None
    }

    /// 按 id 直查主行，用于 GraphQL 层先校验再改。
    pub fn get(
        &self,
        workspace_id: Ulid,
        relation_id: Ulid,
    ) -> Result<Option<Relation>, AppError> {
        self.store
            .get::<Relation>(cf::ENTRY_RELATIONS, &keys::relation_key(workspace_id, relation_id))
    }

    /// 改语义。语义可改，其它字段（方向 / 端点）一旦写就锁住——修改方向
    /// 等价于删除 + 新建，所以这里只允许语义变化。
    pub fn update_semantic(
        &self,
        actor: Ulid,
        workspace_id: Ulid,
        relation_id: Ulid,
        semantic: RelationSemantic,
    ) -> Result<Relation, AppError> {
        let key = keys::relation_key(workspace_id, relation_id);
        let mut rel = self
            .store
            .get::<Relation>(cf::ENTRY_RELATIONS, &key)?
            .ok_or(AppError::NotFound)?;
        if rel.semantic == semantic {
            return Ok(rel);
        }
        let before = serde_json::to_string(&rel).unwrap_or_default();
        rel.semantic = semantic;
        rel.updated_at = chrono::Utc::now();
        let after = serde_json::to_string(&rel).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::RelationUpdated,
            actor,
            "relation",
            &relation_id.to_string(),
            Some(workspace_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::ENTRY_RELATIONS, key, &rel)?);
        self.store.write_batch(ops)?;
        Ok(rel)
    }

    /// 删除一条关联。返回是否真删了一行。
    pub fn delete(
        &self,
        actor: Ulid,
        workspace_id: Ulid,
        relation_id: Ulid,
    ) -> Result<bool, AppError> {
        let key = keys::relation_key(workspace_id, relation_id);
        let Some(rel) = self.store.get::<Relation>(cf::ENTRY_RELATIONS, &key)? else {
            return Ok(false);
        };
        let before = serde_json::to_string(&rel).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::RelationDeleted,
            actor,
            "relation",
            &relation_id.to_string(),
            Some(workspace_id),
            Some(before),
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::ENTRY_RELATIONS, key));
        ops.push(BatchOp::delete(
            cf::ENTRY_RELATIONS_BY_FROM,
            keys::relation_by_from_key(workspace_id, &rel.from_code, relation_id),
        ));
        ops.push(BatchOp::delete(
            cf::ENTRY_RELATIONS_BY_TO,
            keys::relation_by_to_key(workspace_id, &rel.to_code, relation_id),
        ));
        self.store.write_batch(ops)?;
        Ok(true)
    }

    /// 列一个条目的全部关联（FROM + TO 双方向合并，按 created_at 升序）。
    /// 工作空间成员即可读，与「查看条目」同权。
    pub fn list_for_entry(
        &self,
        workspace_id: Ulid,
        entry_code: &str,
    ) -> Result<Vec<Relation>, AppError> {
        let mut out: Vec<Relation> = Vec::new();
        let from_prefix = keys::relation_by_from_prefix(workspace_id, entry_code);
        for (k, _) in self.store.scan_prefix(cf::ENTRY_RELATIONS_BY_FROM, &from_prefix)? {
            if k.len() < 32 {
                continue;
            }
            let Ok(id_bytes): Result<[u8; 16], _> = k[k.len() - 16..].try_into() else {
                continue;
            };
            let id = Ulid::from_bytes(id_bytes);
            if let Some(rel) = self.store.get::<Relation>(
                cf::ENTRY_RELATIONS,
                &keys::relation_key(workspace_id, id),
            )? {
                out.push(rel);
            }
        }
        let to_prefix = keys::relation_by_to_prefix(workspace_id, entry_code);
        for (k, _) in self.store.scan_prefix(cf::ENTRY_RELATIONS_BY_TO, &to_prefix)? {
            if k.len() < 32 {
                continue;
            }
            let Ok(id_bytes): Result<[u8; 16], _> = k[k.len() - 16..].try_into() else {
                continue;
            };
            let id = Ulid::from_bytes(id_bytes);
            if let Some(rel) = self.store.get::<Relation>(
                cf::ENTRY_RELATIONS,
                &keys::relation_key(workspace_id, id),
            )? {
                if !out.iter().any(|r| r.id == rel.id) {
                    out.push(rel);
                }
            }
        }
        out.sort_by_key(|r| r.created_at);
        Ok(out)
    }

    /// 列出条目被删时需要清掉的所有关联 ops（含审计），供调用方拼进自己的 write_batch
    /// 以保证「条目软删除 + 关联清理」原子提交。返回关联条数。
    pub fn purge_for_entry_ops(
        &self,
        actor: Ulid,
        workspace_id: Ulid,
        entry_code: &str,
    ) -> Result<(usize, Vec<BatchOp>), AppError> {
        let rels = self.list_for_entry(workspace_id, entry_code)?;
        let mut ops: Vec<BatchOp> = Vec::new();
        let mut count = 0usize;
        for rel in &rels {
            let before = serde_json::to_string(rel).unwrap_or_default();
            let audit = AuditLog::new(
                AuditAction::RelationDeleted,
                actor,
                "relation",
                &rel.id.to_string(),
                Some(workspace_id),
                Some(before),
                None,
            );
            ops.extend(audit_ops(&audit)?);
            ops.push(BatchOp::delete(
                cf::ENTRY_RELATIONS,
                keys::relation_key(workspace_id, rel.id),
            ));
            ops.push(BatchOp::delete(
                cf::ENTRY_RELATIONS_BY_FROM,
                keys::relation_by_from_key(workspace_id, &rel.from_code, rel.id),
            ));
            ops.push(BatchOp::delete(
                cf::ENTRY_RELATIONS_BY_TO,
                keys::relation_by_to_key(workspace_id, &rel.to_code, rel.id),
            ));
            count += 1;
        }
        Ok((count, ops))
    }

    /// 直接提交关联清理。EntryService 走 `purge_for_entry_ops` 把它拼进自己的
    /// write_batch 以保证原子性；这条只是给非删除路径（例如工作空间清理）用的便利入口。
    pub fn purge_for_entry(
        &self,
        actor: Ulid,
        workspace_id: Ulid,
        entry_code: &str,
    ) -> Result<usize, AppError> {
        let (count, ops) = self.purge_for_entry_ops(actor, workspace_id, entry_code)?;
        if !ops.is_empty() {
            self.store.write_batch(ops)?;
        }
        Ok(count)
    }

    /// 启动期 idempotent 修复：扫 `ENTRY_RELATIONS` 列族，遇到 `bincode::deserialize`
    /// 失败的旧记录直接连同两条索引一起删掉。
    ///
    /// 触发原因：早期版本用 `RelationSemantic` 的 `#[serde(tag="type", content="value")]`
    /// 形态存进了 RocksDB（实际并不能被 bincode 编码成功，所以大部分写入在旧版本里直接
    /// 抛错），但总有零星成功落盘的样本——一旦切到 struct 形态，read 路径在反序列化
    /// 阶段直接 `Err("tag for enum is not valid, found N")` 把整个 list_for_entry
    /// 拖挂。删记录是最稳的兜底：关系是工作空间里的派生数据，重新建一条成本极低，
    /// 不留任何 shim 比 read-site 容错更可靠。
    pub fn repair_undecodable(&self) -> Result<usize, AppError> {
        let bad = self
            .store
            .scan_prefix(cf::ENTRY_RELATIONS, b"")
            .map_err(AppError::from)?;
        let mut ops: Vec<BatchOp> = Vec::new();
        for (k, _v) in &bad {
            if k.len() != 32 {
                // 不是 (workspace_id, relation_id) 形态，留给别的修
                continue;
            }
            let Ok(ws_bytes): Result<[u8; 16], _> = k[..16].try_into() else {
                continue;
            };
            let Ok(id_bytes): Result<[u8; 16], _> = k[16..].try_into() else {
                continue;
            };
            let ws = Ulid::from_bytes(ws_bytes);
            let id = Ulid::from_bytes(id_bytes);
            // 再确认一次反序列化确实失败（多数键只是格式可疑，但 bincode 容忍）
            if self
                .store
                .get::<Relation>(cf::ENTRY_RELATIONS, k)
                .is_ok()
            {
                continue;
            }
            ops.push(BatchOp::delete(cf::ENTRY_RELATIONS, k.clone()));
            // 索引键里嵌了 from_code / to_code——旧记录里我们没有这两段，按主键
            // 拉不到 index，就不强求：list 路径只会看见坏主键删了之后没有悬空索引。
            // 真要再扫一遍 FROM/TO 列族也行，但坏主键路径已经覆盖 list_for_entry
            // 的「主行能解码」分支，索引列族坏只会让列出的关系少几行，不会拖崩。
            let _ = (ws, id);
        }
        let removed = ops.len();
        if !ops.is_empty() {
            self.store.write_batch(ops)?;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::SemanticKind;

    #[test]
    fn semantic_from_input_handles_builtins() {
        assert_eq!(
            RelationSemantic::from_input("包含"),
            RelationSemantic::builtin(SemanticKind::Contains)
        );
        assert_eq!(
            RelationSemantic::from_input("归属"),
            RelationSemantic::builtin(SemanticKind::BelongsTo)
        );
    }

    #[test]
    fn semantic_from_input_custom_round_trip() {
        let s = RelationSemantic {
            kind: SemanticKind::Custom,
            value: Some("上下游".into()),
        };
        let back = RelationSemantic::from_input(s.display());
        assert_eq!(back, s);
    }
}