//! RocksDB 后端实现。列族创建、迭代器、WriteBatch 只在这个文件里出现。

use rocksdb::{DBCompactionStyle, Direction, IteratorMode, Options, WriteBatch, DB};

use crate::error::AppError;

use super::doc::{cf, BatchOp};

const ALL_CFS: &[&str] = &[
    cf::ACCOUNTS,
    cf::ACCOUNTS_EMAIL_IDX,
    cf::ACCOUNT_TOKEN_VERSION,
    cf::ACCOUNT_STATUS,
    cf::WORKSPACES,
    cf::WORKSPACES_SLUG_IDX,
    cf::WORKSPACES_DELETED,
    cf::WORKSPACE_MEMBERS,
    cf::WORKSPACE_MEMBERS_BY_ACCOUNT,
    cf::INVITES,
    cf::INVITES_BY_ACCOUNT,
    cf::ENTRIES,
    cf::ENTRIES_BY_WORKSPACE,
    cf::ENTRIES_ARCHIVED,
    cf::LABEL_SCHEMAS,
    cf::LABELINGS,
    cf::AUDIT_LOGS,
    cf::AUDIT_LOGS_BY_RESOURCE,
    cf::AUDIT_LOGS_BY_WORKSPACE,
    cf::VIEWS,
    cf::VIEWS_BY_WORKSPACE,
    cf::DEFAULT_VIEWS,
    cf::LABELINGS_BY_WORKSPACE,
    cf::WORKSPACE_AI,
    cf::AUTOMATION_RULES,
    cf::AUTOMATION_RULES_BY_WORKSPACE,
    cf::COMMENTS,
    cf::ATTACHMENTS,
    cf::ATTACHMENTS_BY_ENTRY,
    cf::INLINE_ATTACHMENTS,
    cf::VIEW_TIMELINE,
    cf::MESSAGES_BY_RECIPIENT,
];

/// RocksDB 文档存储：每个 CF 存一类文档（bincode 序列化）或二级索引（裸字节）。
pub(crate) struct RocksDoc {
    db: DB,
}

impl RocksDoc {
    pub fn open(path: &str) -> Result<Self, AppError> {
        std::fs::create_dir_all(path).map_err(|e| AppError::Storage(e.to_string()))?;
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        opts.set_compaction_style(DBCompactionStyle::Level);
        let db = DB::open_cf(&opts, path, ALL_CFS).map_err(|e| AppError::Storage(e.to_string()))?;
        Ok(Self { db })
    }

    fn handle(&self, name: &str) -> Result<&rocksdb::ColumnFamily, AppError> {
        self.db
            .cf_handle(name)
            .ok_or_else(|| AppError::Storage(format!("缺失 column family: {name}")))
    }

    pub fn put_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), AppError> {
        let h = self.handle(cf)?;
        self.db.put_cf(h, key, value).map_err(Into::into)
    }

    pub fn get_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, AppError> {
        let h = self.handle(cf)?;
        self.db.get_cf(h, key).map_err(Into::into)
    }

    /// 前缀扫描：按 key 升序返回所有以 `prefix` 开头的键值对。
    pub fn scan_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        let h = self.handle(cf)?;
        let iter = self
            .db
            .iterator_cf(h, IteratorMode::From(prefix, Direction::Forward));
        let mut out = Vec::new();
        for item in iter {
            let (k, v) = item.map_err(|e| AppError::Storage(e.to_string()))?;
            if !k.starts_with(prefix) {
                break;
            }
            out.push((k.to_vec(), v.to_vec()));
        }
        Ok(out)
    }

    pub fn delete(&self, cf: &str, key: &[u8]) -> Result<(), AppError> {
        let h = self.handle(cf)?;
        self.db.delete_cf(h, key).map_err(Into::into)
    }

    /// 原子写入跨多个 CF 的批量操作（文档变更 + 二级索引 + 审计日志一次落盘）。
    pub fn write_batch(&self, ops: Vec<BatchOp>) -> Result<(), AppError> {
        let mut batch = WriteBatch::default();
        for op in &ops {
            match op {
                BatchOp::Put { cf, key, value } => {
                    let h = self.handle(cf)?;
                    batch.put_cf(h, key.as_slice(), value.as_slice());
                }
                BatchOp::Delete { cf, key } => {
                    let h = self.handle(cf)?;
                    batch.delete_cf(h, key.as_slice());
                }
            }
        }
        self.db.write(batch).map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{cf, keys, DocStore};
    use chrono::{TimeZone, Utc};

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-test-{name}-{}", ulid::Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn put_get_roundtrip() {
        let dir = temp_dir("putget");
        let store = DocStore::open(&dir).unwrap();
        store.put(cf::ACCOUNTS, b"k1", &42u64).unwrap();
        let v: Option<u64> = store.get(cf::ACCOUNTS, b"k1").unwrap();
        assert_eq!(v, Some(42));
        assert!(store.get::<u64>(cf::ACCOUNTS, b"missing").unwrap().is_none());
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prefix_scan_orders_by_key() {
        let dir = temp_dir("scan");
        let store = DocStore::open(&dir).unwrap();
        store.put_raw(cf::ENTRIES_BY_WORKSPACE, b"w/aaa", b"1").unwrap();
        store.put_raw(cf::ENTRIES_BY_WORKSPACE, b"w/bbb", b"2").unwrap();
        store.put_raw(cf::ENTRIES_BY_WORKSPACE, b"x/ccc", b"3").unwrap();

        let got = store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, b"w/").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, b"w/aaa");
        assert_eq!(got[1].0, b"w/bbb");
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reopens_with_existing_cfs() {
        let dir = temp_dir("reopen");
        {
            let store = DocStore::open(&dir).unwrap();
            store.put_raw(cf::LABELINGS, b"c/Task", b"x").unwrap();
        }
        let store = DocStore::open(&dir).unwrap();
        assert_eq!(store.get_raw(cf::LABELINGS, b"c/Task").unwrap(), Some(b"x".to_vec()));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_batch_atomic_put_and_delete() {
        let dir = temp_dir("batch");
        let store = DocStore::open(&dir).unwrap();
        store.put_raw(cf::AUDIT_LOGS, b"gone", b"g0").unwrap();
        let ops = vec![
            BatchOp::put_raw(cf::AUDIT_LOGS, b"k1".to_vec(), b"v1".to_vec()),
            BatchOp::put_raw(cf::AUDIT_LOGS, b"k2".to_vec(), b"v2".to_vec()),
            BatchOp::delete(cf::AUDIT_LOGS, b"gone".to_vec()),
        ];
        store.write_batch(ops).unwrap();
        assert_eq!(store.get_raw(cf::AUDIT_LOGS, b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(store.get_raw(cf::AUDIT_LOGS, b"k2").unwrap(), Some(b"v2".to_vec()));
        assert_eq!(
            store.get_raw(cf::AUDIT_LOGS, b"gone").unwrap(),
            None,
            "delete inside the batch must remove the pre-existing key"
        );
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_batch_atomic_across_column_families() {
        let dir = temp_dir("batch_multi_cf");
        let store = DocStore::open(&dir).unwrap();
        let ops = vec![
            BatchOp::put_raw(cf::ENTRIES, b"entry/TESTCODE0001".to_vec(), b"{entry}".to_vec()),
            BatchOp::put_raw(cf::AUDIT_LOGS, b"audit/1".to_vec(), b"{audit}".to_vec()),
            BatchOp::put_raw(cf::AUDIT_LOGS_BY_RESOURCE, b"idx/1".to_vec(), b"audit/1".to_vec()),
        ];
        store.write_batch(ops).unwrap();
        assert_eq!(
            store.get_raw(cf::ENTRIES, b"entry/TESTCODE0001").unwrap(),
            Some(b"{entry}".to_vec())
        );
        assert_eq!(store.get_raw(cf::AUDIT_LOGS, b"audit/1").unwrap(), Some(b"{audit}".to_vec()));
        assert_eq!(
            store.get_raw(cf::AUDIT_LOGS_BY_RESOURCE, b"idx/1").unwrap(),
            Some(b"audit/1".to_vec())
        );
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn audit_log_key_orders_newest_first() {
        let t1 = Utc.timestamp_millis_opt(1_000).unwrap();
        let t2 = Utc.timestamp_millis_opt(2_000).unwrap();
        let k1 = keys::audit_log_key(t1, ulid::Ulid::new());
        let k2 = keys::audit_log_key(t2, ulid::Ulid::new());
        assert!(k2 < k1, "较新的时间应产生更小的键，前缀扫描时排在前");
    }

    #[test]
    fn audit_by_workspace_key_orders_newest_first() {
        let ws = ulid::Ulid::new();
        let t_old = Utc.timestamp_millis_opt(1_000).unwrap();
        let t_new = Utc.timestamp_millis_opt(2_000).unwrap();
        let old = keys::audit_by_workspace_key(ws, t_old, ulid::Ulid::new());
        let new = keys::audit_by_workspace_key(ws, t_new, ulid::Ulid::new());
        assert!(new < old, "较新的时间应排在前面");
        assert!(old.starts_with(&ws.to_bytes()), "键必须以 workspace_id 开头以便前缀扫描");
        assert!(new.starts_with(&ws.to_bytes()));
    }

    #[test]
    fn audit_by_resource_key_orders_newest_first() {
        let t_old = Utc.timestamp_millis_opt(1_000).unwrap();
        let t_new = Utc.timestamp_millis_opt(2_000).unwrap();
        let prefix: &[u8] = b"entry\0TESTCODE0001\0";
        let old = keys::audit_by_resource_key("entry", "TESTCODE0001", t_old, ulid::Ulid::new());
        let new = keys::audit_by_resource_key("entry", "TESTCODE0001", t_new, ulid::Ulid::new());
        assert!(new < old, "较新的时间应排在前面");
        assert!(old.starts_with(prefix), "键必须以 resource_type\\0resource_id\\0 开头");
        assert!(new.starts_with(prefix));
    }
}
