use rocksdb::{DBCompactionStyle, Direction, IteratorMode, Options, DB};
use serde::{de::DeserializeOwned, Serialize};

use crate::error::AppError;

pub mod cf {
    pub const ACCOUNTS: &str = "accounts";
    pub const ACCOUNTS_EMAIL_IDX: &str = "accounts_email_idx";
    pub const WORKSPACES: &str = "workspaces";
    pub const WORKSPACES_SLUG_IDX: &str = "workspaces_slug_idx";
    pub const WORKSPACE_MEMBERS: &str = "workspace_members";
    pub const WORKSPACE_MEMBERS_BY_ACCOUNT: &str = "workspace_members_by_account";
    pub const ENTRIES: &str = "entries";
    pub const ENTRIES_BY_WORKSPACE: &str = "entries_by_workspace";
    pub const LABEL_SCHEMAS: &str = "label_schemas";
    pub const LABELINGS: &str = "labelings";
}

const ALL_CFS: &[&str] = &[
    cf::ACCOUNTS,
    cf::ACCOUNTS_EMAIL_IDX,
    cf::WORKSPACES,
    cf::WORKSPACES_SLUG_IDX,
    cf::WORKSPACE_MEMBERS,
    cf::WORKSPACE_MEMBERS_BY_ACCOUNT,
    cf::ENTRIES,
    cf::ENTRIES_BY_WORKSPACE,
    cf::LABEL_SCHEMAS,
    cf::LABELINGS,
];

/// RocksDB 文档存储：每个 CF 存一类文档（bincode 序列化）或二级索引（裸字节）。
pub struct DocStore {
    db: DB,
}

impl DocStore {
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

    /// 以 bincode 序列化写入文档。
    pub fn put<T: Serialize>(&self, cf: &str, key: &[u8], value: &T) -> Result<(), AppError> {
        let bytes = bincode::serialize(value)?;
        self.put_raw(cf, key, &bytes)
    }

    /// 读取并反序列化文档。
    pub fn get<T: DeserializeOwned>(&self, cf: &str, key: &[u8]) -> Result<Option<T>, AppError> {
        match self.get_raw(cf, key)? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
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

    pub fn exists(&self, cf: &str, key: &[u8]) -> Result<bool, AppError> {
        Ok(self.get_raw(cf, key)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
