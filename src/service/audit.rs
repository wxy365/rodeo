use std::sync::Arc;

use ulid::Ulid;

use crate::domain::AuditLog;
use crate::error::AppError;
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 生成一条审计日志的写入操作（主 CF + workspace 索引 + resource 索引）。
/// 供 Entry/Label 等业务服务在文档变更时复用，随同一 write_batch 原子落盘。
pub fn audit_ops(log: &AuditLog) -> Result<Vec<BatchOp>, AppError> {
    let mut ops = Vec::new();
    ops.push(BatchOp::put(
        cf::AUDIT_LOGS,
        keys::audit_log_key(log.at, log.id),
        log,
    )?);
    if let Some(ws) = log.workspace_id {
        ops.push(BatchOp::put_raw(
            cf::AUDIT_LOGS_BY_WORKSPACE,
            keys::audit_by_workspace_key(ws, log.at, log.id),
            Vec::new(),
        ));
    }
    ops.push(BatchOp::put_raw(
        cf::AUDIT_LOGS_BY_RESOURCE,
        keys::audit_by_resource_key(&log.resource_type, &log.resource_id, log.at, log.id),
        Vec::new(),
    ));
    Ok(ops)
}

pub struct AuditService {
    store: Arc<DocStore>,
}

impl AuditService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    /// 按 workspace 查询审计日志，最新在前。
    pub fn list(&self, workspace_id: Ulid, limit: usize) -> Result<Vec<AuditLog>, AppError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let prefix = workspace_id.to_bytes();
        let rows = self.store.scan_prefix(cf::AUDIT_LOGS_BY_WORKSPACE, &prefix)?;
        let mut out = Vec::new();
        for (key, _) in rows {
            if key.len() < 16 + 24 {
                continue;
            }
            // 索引键后缀 = audit_logs 主键 (desc 8 + id 16)。
            let log_key = &key[key.len() - 24..];
            if let Some(log) = self.store.get::<AuditLog>(cf::AUDIT_LOGS, log_key)? {
                out.push(log);
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
    use crate::domain::AuditAction;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-audit-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    /// 构造一条 at 固定的审计日志，保证写入顺序与查询排序完全确定。
    fn audit_log(
        action: AuditAction,
        resource_id: &str,
        ws: Option<Ulid>,
        at: DateTime<Utc>,
    ) -> AuditLog {
        let mut log = AuditLog::new(action, Ulid::new(), "entry", resource_id, ws, None, None);
        log.at = at;
        log
    }

    #[test]
    fn audit_ops_emits_store_workspace_and_resource_index_ops() {
        let ws = Ulid::new();
        let at = Utc::now();
        let log = audit_log(AuditAction::EntryCreated, "c1", Some(ws), at);

        let ops = audit_ops(&log).unwrap();
        assert_eq!(ops.len(), 3, "store + workspace index + resource index");

        let mut saw_store = false;
        let mut saw_ws = false;
        let mut saw_resource = false;
        for op in ops {
            let BatchOp::Put { cf, key, value } = op else {
                panic!("audit_ops must only emit Put ops");
            };
            match cf {
                c if c == cf::AUDIT_LOGS => {
                    saw_store = true;
                    assert_eq!(key, keys::audit_log_key(at, log.id));
                    // 主 CF 落整条 bincode 序列化日志，可直接读回。
                    let back: AuditLog = bincode::deserialize(&value).unwrap();
                    assert_eq!(back, log);
                }
                c if c == cf::AUDIT_LOGS_BY_WORKSPACE => {
                    saw_ws = true;
                    assert_eq!(key, keys::audit_by_workspace_key(ws, at, log.id));
                    assert_eq!(key.len(), 40, "ws 16 + desc 8 + id 16");
                }
                c if c == cf::AUDIT_LOGS_BY_RESOURCE => {
                    saw_resource = true;
                    assert_eq!(key, keys::audit_by_resource_key("entry", "c1", at, log.id));
                }
                c => panic!("unexpected cf: {c}"),
            }
        }
        assert!(saw_store, "must write to the audit_logs store CF");
        assert!(saw_ws, "must write to the workspace index CF");
        assert!(saw_resource, "must write to the resource index CF");
    }

    #[test]
    fn audit_ops_without_workspace_skips_workspace_index() {
        let log = audit_log(AuditAction::EntryUpdated, "c1", None, Utc::now());
        let ops = audit_ops(&log).unwrap();
        assert_eq!(ops.len(), 2, "store + resource index only when workspace is None");
        for op in ops {
            let BatchOp::Put { cf, .. } = op else {
                panic!("audit_ops must only emit Put ops");
            };
            assert_ne!(cf, cf::AUDIT_LOGS_BY_WORKSPACE);
        }
    }

    #[test]
    fn list_returns_newest_first_and_scoped_to_workspace() {
        let dir = temp_dir("list");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = AuditService::new(store.clone());

        let ws = Ulid::new();
        let base = Utc::now();
        let log1 = audit_log(AuditAction::EntryCreated, "c1", Some(ws), base);
        let log2 = audit_log(
            AuditAction::EntryUpdated,
            "c2",
            Some(ws),
            base + Duration::milliseconds(1),
        );

        let mut ops = audit_ops(&log1).unwrap();
        ops.extend(audit_ops(&log2).unwrap());
        store.write_batch(ops).unwrap();

        let got = svc.list(ws, 100).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], log2, "newest log must come first");
        assert_eq!(got[1], log1);

        assert!(
            svc.list(Ulid::new(), 100).unwrap().is_empty(),
            "other workspaces must see no logs"
        );

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn list_respects_limit() {
        let dir = temp_dir("limit");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = AuditService::new(store.clone());

        let ws = Ulid::new();
        let base = Utc::now();
        let l1 = audit_log(AuditAction::EntryCreated, "c1", Some(ws), base);
        let l2 = audit_log(
            AuditAction::EntryUpdated,
            "c2",
            Some(ws),
            base + Duration::milliseconds(1),
        );
        let l3 = audit_log(
            AuditAction::EntryDeleted,
            "c3",
            Some(ws),
            base + Duration::milliseconds(2),
        );

        let mut ops = audit_ops(&l1).unwrap();
        ops.extend(audit_ops(&l2).unwrap());
        ops.extend(audit_ops(&l3).unwrap());
        store.write_batch(ops).unwrap();

        let got = svc.list(ws, 2).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], l3, "limit keeps the newest logs");
        assert_eq!(got[1], l2);

        assert!(
            svc.list(ws, 0).unwrap().is_empty(),
            "limit 0 must return an empty list"
        );

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
