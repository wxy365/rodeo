//! 后端无关的存储契约。
//!
//! `cf` 与 `BatchOp` 描述的是「应用需要什么样的存储」——若干命名集合、二进制键、
//! 跨集合的原子批量写——而不是某一种数据库的形状。RocksDB 用列族实现它，
//! PostgreSQL 后端用一张 `kv(cf, key, value)` 表实现它。门面 [`DocStore`] 在此之上
//! 提供 bincode 编解码，于是上层 ~30 处 `Arc<DocStore>` 不需要知道后端是什么。

use serde::{de::DeserializeOwned, Serialize};

use crate::config::{DocBackend, StorageConfig};
use crate::error::AppError;

use super::pg::PgDoc;
use super::rocksdb::RocksDoc;

pub mod cf {
    pub const ACCOUNTS: &str = "accounts";
    pub const ACCOUNTS_EMAIL_IDX: &str = "accounts_email_idx";
    /// 令牌版本：account id → u64 大端。登出时自增，使该账号所有已签发 JWT 立即失效。
    /// 与软删除同理，用独立 CF 而不是给 Account 加字段，避免 bincode 结构变更。
    pub const ACCOUNT_TOKEN_VERSION: &str = "account_token_version";
    pub const WORKSPACES: &str = "workspaces";
    pub const WORKSPACES_SLUG_IDX: &str = "workspaces_slug_idx";
    /// 软删除标记：workspace id → 删除时间（RFC3339）。单独一个 CF 而不是给 Workspace
    /// 加字段，避免 bincode 结构变更导致存量工作空间读不出来。
    pub const WORKSPACES_DELETED: &str = "workspaces_deleted";
    pub const WORKSPACE_MEMBERS: &str = "workspace_members";
    pub const WORKSPACE_MEMBERS_BY_ACCOUNT: &str = "workspace_members_by_account";
    /// 待接受的邀请：(workspace_id, account_id) → `Invite`。接受之前不写成员关系，
    /// 所以「邀请中」和「已是成员」是两套互不干扰的记录，`Invite` 也就不需要 status 字段。
    pub const INVITES: &str = "invites";
    /// 反向索引：(account_id, workspace_id) → 空值，供「我收到的邀请」前缀扫描。
    pub const INVITES_BY_ACCOUNT: &str = "invites_by_account";
    pub const ENTRIES: &str = "entries";
    pub const ENTRIES_BY_WORKSPACE: &str = "entries_by_workspace";
    /// 归档标记：entry code → 归档时间（RFC3339）。与软删除同理，用独立 CF 而不是给
    /// Entry 加字段，避免 bincode 结构变更导致存量条目读不出来。归档不删除数据，
    /// 只是把条目移出基础视图，可随时取消归档。
    pub const ENTRIES_ARCHIVED: &str = "entries_archived";
    pub const LABEL_SCHEMAS: &str = "label_schemas";
    pub const LABELINGS: &str = "labelings";
    pub const AUDIT_LOGS: &str = "audit_logs";
    pub const AUDIT_LOGS_BY_RESOURCE: &str = "audit_logs_by_resource";
    pub const AUDIT_LOGS_BY_WORKSPACE: &str = "audit_logs_by_workspace";
    pub const VIEWS: &str = "views";
    pub const VIEWS_BY_WORKSPACE: &str = "views_by_workspace";
    pub const DEFAULT_VIEWS: &str = "default_views";
    pub const LABELINGS_BY_WORKSPACE: &str = "labelings_by_workspace";
    /// 工作空间级 AI 配置：workspace id → `WorkspaceAiConfig`（bincode）。
    /// 与 `WORKSPACES_DELETED` / `ENTRIES_ARCHIVED` 同理，用独立列族而不是给 `Workspace`
    /// 加字段——加字段会让存量工作空间反序列化失败。
    pub const WORKSPACE_AI: &str = "workspace_ai";
    /// 自动化规则：rule id → `AutomationRule`（bincode）。
    pub const AUTOMATION_RULES: &str = "automation_rules";
    /// 工作空间下的规则索引：(workspace_id, rule_id) → 空值，供前缀扫描。
    pub const AUTOMATION_RULES_BY_WORKSPACE: &str = "automation_rules_by_workspace";
    pub const COMMENTS: &str = "comments";
    /// 附件主键：`attachment_id`（ULID 16 字节）→ `Attachment`（bincode）。
    /// 用 id 单键而非 (entry_code, id)，因为下载路由手里只有 id，必须能直取。
    pub const ATTACHMENTS: &str = "attachments";
    /// 附件索引：(entry_code, attachment_id) → 空值，供按条目前缀扫描。
    pub const ATTACHMENTS_BY_ENTRY: &str = "attachments_by_entry";
    /// 内联图片标记：`attachment_id`（ULID 16 字节）→ 空值。
    /// 编辑器里粘贴上传的图片仍是附件（要下载、要按角色删），但不属于「附件」这一栏，
    /// 所以不进附件列表。不给 `Attachment` 加字段是因为那是 bincode 结构变更，
    /// 存量附件会读不出来——与 `ENTRIES_ARCHIVED` / `WORKSPACES_DELETED` 同一套取舍。
    pub const INLINE_ATTACHMENTS: &str = "inline_attachments";
}

/// 单个批量写操作：文档/索引/审计统一原子写入。
pub enum BatchOp {
    Put { cf: &'static str, key: Vec<u8>, value: Vec<u8> },
    Delete { cf: &'static str, key: Vec<u8> },
}

impl BatchOp {
    pub fn put<T: Serialize>(cf: &'static str, key: Vec<u8>, value: &T) -> Result<Self, AppError> {
        Ok(BatchOp::Put { cf, key, value: bincode::serialize(value)? })
    }

    pub fn put_raw(cf: &'static str, key: Vec<u8>, value: Vec<u8>) -> Self {
        BatchOp::Put { cf, key, value }
    }

    pub fn delete(cf: &'static str, key: Vec<u8>) -> Self {
        BatchOp::Delete { cf, key }
    }
}

enum Backend {
    Rocks(RocksDoc),
    Pg(PgDoc),
}

/// 文档存储统一门面。
pub struct DocStore {
    backend: Backend,
}

impl DocStore {
    /// RocksDB 后端（spec 的默认组合）。这个签名刻意保留：8 个文件里约 30 处
    /// 既有测试都是 `DocStore::open(&dir)`，让它们一个字都不用改。
    pub fn open(path: &str) -> Result<Self, AppError> {
        Ok(Self { backend: Backend::Rocks(RocksDoc::open(path)?) })
    }

    /// 按配置选后端。四种组合由这里的两层 match 决定，服务层无感。
    pub fn from_config(cfg: &StorageConfig) -> Result<Self, AppError> {
        let backend = match cfg.doc.backend {
            DocBackend::Rocksdb => Backend::Rocks(RocksDoc::open(&cfg.data_dir)?),
            DocBackend::Postgres => {
                let url = cfg.doc.url.trim();
                if url.is_empty() {
                    return Err(AppError::Storage(
                        "storage.doc.backend = \"postgres\" 时必须配置 storage.doc.url".to_string(),
                    ));
                }
                Backend::Pg(PgDoc::open(url)?)
            }
        };
        Ok(Self { backend })
    }

    pub fn put<T: Serialize>(&self, cf: &str, key: &[u8], value: &T) -> Result<(), AppError> {
        let bytes = bincode::serialize(value)?;
        self.put_raw(cf, key, &bytes)
    }

    pub fn get<T: DeserializeOwned>(&self, cf: &str, key: &[u8]) -> Result<Option<T>, AppError> {
        match self.get_raw(cf, key)? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
    }

    pub fn put_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), AppError> {
        match &self.backend {
            Backend::Rocks(s) => s.put_raw(cf, key, value),
            Backend::Pg(s) => s.put_raw(cf, key, value),
        }
    }

    pub fn get_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, AppError> {
        match &self.backend {
            Backend::Rocks(s) => s.get_raw(cf, key),
            Backend::Pg(s) => s.get_raw(cf, key),
        }
    }

    /// 前缀扫描：按 key 升序返回所有以 `prefix` 开头的键值对。
    pub fn scan_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        match &self.backend {
            Backend::Rocks(s) => s.scan_prefix(cf, prefix),
            Backend::Pg(s) => s.scan_prefix(cf, prefix),
        }
    }

    pub fn delete(&self, cf: &str, key: &[u8]) -> Result<(), AppError> {
        match &self.backend {
            Backend::Rocks(s) => s.delete(cf, key),
            Backend::Pg(s) => s.delete(cf, key),
        }
    }

    pub fn exists(&self, cf: &str, key: &[u8]) -> Result<bool, AppError> {
        Ok(self.get_raw(cf, key)?.is_some())
    }

    /// 原子写入跨多个 CF 的批量操作（文档变更 + 二级索引 + 审计日志一次落盘）。
    pub fn write_batch(&self, ops: Vec<BatchOp>) -> Result<(), AppError> {
        match &self.backend {
            Backend::Rocks(s) => s.write_batch(ops),
            Backend::Pg(s) => s.write_batch(ops),
        }
    }
}
