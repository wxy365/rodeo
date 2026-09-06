# 内容管理闭环 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在现有 MVP 垂直切片上，补齐内容管理的完整闭环：Entry 编辑（乐观并发）/软删除、自定义标签 CRUD、移除打标，并把审计日志写操作铺进所有相关变更。

**Architecture:** 延续三层分离（API → Service → Storage）。新增 Audit 领域模型 + 审计 CF，补齐 `DocStore::write_batch`（文档 + 索引 + 审计原子写入）。新增 `LabelService` 承载自定义标签 CRUD；`EntryService` 增加 update/soft_delete/remove_labeling。前端在 WorkspaceMain 内加右侧详情面板，新增 `/:slug/settings` 页管理标签与查看审计。

**Tech Stack:** Rust 1.97、Leptos 0.8、Axum 0.8、async-graphql 7、RocksDB 0.22（`WriteBatch`）、bincode、chrono、ulid。

**Spec:** `spec/product_design.md` + `spec/technical_solution.md`（数据模型/CF/审计枚举已定义，本计划从中推导增量）。

## 全局约束

- Rust 2021 edition，Leptos 0.8（`RwSignal`/`Effect::new_sync`/`spawn_local` 风格，沿用现有代码）。
- 文档 bincode 序列化；索引值用 UTF-8/字节键做前缀扫描。
- 权限：`WorkspaceRole` 排序 `Owner > Maintainer > Worker > Reader`（`RequireRole` 已派生 `Ord`）。
- 乐观并发：`update_entry` 比对 `updated_at`，不一致返回 `ConflictDetected`。
- 软删除：`Entry.deleted_at` 标记，`list` 默认过滤，不物理删除。
- 本轮不做：tiny-editor、`labelings_by_label` 反查索引、Workspace/成员审计、JWT→cookie 迁移、附件、实时、搜索、OAuth。
- **注意：`Entry` 结构新增 `deleted_at` 字段会破坏旧 `data/` 下 bincode 数据反序列化。开发环境请先删除 `data/` 目录（已 gitignore，仅开发数据）。**
- **本仓库尚未 `git init`，所有「Commit」步骤为可选：如需 checkpoint，先执行 `git init && git add -A && git commit -m "chore: initial"` 一次即可。**

---

### Task 1: 领域模型扩展（Entry.deleted_at + Audit + ConflictDetected）

**Files:**
- Modify: `src/domain/entry.rs`
- Create: `src/domain/audit.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/error.rs`

**Interfaces:**
- Produces: `Entry { deleted_at: Option<DateTime<Utc>>, .. }` + `Entry::is_deleted()`；`AuditLog`、`AuditAction`、`AuditLog::new(action, actor_id, resource_type, resource_id, workspace_id, before: Option<String>, after: Option<String>)`；`AppError::ConflictDetected`。

- [ ] **Step 1: 修改 `src/domain/entry.rs` 增加 `deleted_at`**

在 `Entry` 结构体 `detail` 字段后加一行，并在 `new` 中初始化：

```rust
pub struct Entry {
    pub code: String,
    pub workspace_id: Ulid,
    pub title: String,
    pub detail: String,
    pub deleted_at: Option<DateTime<Utc>>,
    pub created_by: Ulid,
    pub updated_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Entry {
    pub fn new(workspace_id: Ulid, title: String, actor: Ulid) -> Self {
        let now = Utc::now();
        Self {
            code: generate_entry_code(),
            workspace_id,
            title,
            detail: String::new(),
            deleted_at: None,
            created_by: actor,
            updated_by: actor,
            created_at: now,
            updated_at: now,
        }
    }

    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
}
```

- [ ] **Step 2: 创建 `src/domain/audit.rs`**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// 审计操作类型（对齐 spec 11.1，本轮只发出 Entry/Label 相关变体）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AuditAction {
    AccountLogin,
    AccountLogout,
    AccountCreated,
    AccountDisabled,
    WorkspaceCreated,
    WorkspaceDeleted,
    MemberInvited,
    MemberRemoved,
    RoleChanged,
    EntryCreated,
    EntryUpdated,
    EntryDeleted,
    LabelSchemaCreated,
    LabelSchemaUpdated,
    LabelingSet,
    LabelingRemoved,
    ViewCreated,
    ViewUpdated,
    ViewDeleted,
    AttachmentUploaded,
    AttachmentDeleted,
}

/// 审计日志。before/after 为 JSON 字符串（序列化后的快照）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AuditLog {
    pub id: Ulid,
    pub action: AuditAction,
    pub actor_id: Ulid,
    pub resource_type: String,
    pub resource_id: String,
    pub workspace_id: Option<Ulid>,
    pub before: Option<String>,
    pub after: Option<String>,
    pub at: DateTime<Utc>,
}

impl AuditLog {
    pub fn new(
        action: AuditAction,
        actor_id: Ulid,
        resource_type: &str,
        resource_id: &str,
        workspace_id: Option<Ulid>,
        before: Option<String>,
        after: Option<String>,
    ) -> Self {
        Self {
            id: Ulid::new(),
            action,
            actor_id,
            resource_type: resource_type.to_string(),
            resource_id: resource_id.to_string(),
            workspace_id,
            before,
            after,
            at: Utc::now(),
        }
    }
}
```

- [ ] **Step 3: 修改 `src/domain/mod.rs` 导出**

```rust
pub mod account;
pub mod audit;
pub mod entry;
pub mod label;
pub mod workspace;

pub use account::Account;
pub use audit::{AuditAction, AuditLog};
pub use entry::{generate_entry_code, Entry};
pub use label::{LabelSchema, LabelValue, LabelValueType, Labeling};
pub use workspace::{Workspace, WorkspaceMember, WorkspaceRole};
```

- [ ] **Step 4: 修改 `src/error.rs` 增加冲突错误**

在枚举中加变体，并在 `code()` 中加分支：

```rust
    #[error("内容已被他人修改，请刷新后重试")]
    ConflictDetected,
```

```rust
            AppError::ConflictDetected => "CONFLICT",
```

- [ ] **Step 5: 跑测试验证编译 + 现有测试仍通过**

Run: `cargo test`
Expected: 全部通过（现有 8 个 + 无新失败）。`cargo test` 会顺带编译 domain 改动。

- [ ] **Step 6: 可选提交**

```bash
git add src/domain src/error.rs && git commit -m "feat(domain): add Entry.deleted_at, Audit model, ConflictDetected error"
```

---

### Task 2: 存储层（WriteBatch + 审计 CF + 键编码）

**Files:**
- Modify: `src/storage/rocksdb.rs`
- Modify: `src/storage/keys.rs`

**Interfaces:**
- Produces: `cf::{AUDIT_LOGS, AUDIT_LOGS_BY_RESOURCE, AUDIT_LOGS_BY_WORKSPACE}`；`DocStore::write_batch(Vec<BatchOp>)`；`BatchOp::{put, put_raw, delete}`；`keys::{audit_log_key, audit_by_workspace_key, audit_by_resource_key}`。

- [ ] **Step 1: 修改 `src/storage/rocksdb.rs` — CF 常量与 ALL_CFS**

在 `pub mod cf` 末尾加：

```rust
    pub const AUDIT_LOGS: &str = "audit_logs";
    pub const AUDIT_LOGS_BY_RESOURCE: &str = "audit_logs_by_resource";
    pub const AUDIT_LOGS_BY_WORKSPACE: &str = "audit_logs_by_workspace";
```

在 `ALL_CFS` 数组末尾加：

```rust
    cf::AUDIT_LOGS,
    cf::AUDIT_LOGS_BY_RESOURCE,
    cf::AUDIT_LOGS_BY_WORKSPACE,
```

顶部 import 加入 `WriteBatch`：

```rust
use rocksdb::{DBCompactionStyle, Direction, IteratorMode, Options, WriteBatch, DB};
```

- [ ] **Step 2: 修改 `src/storage/rocksdb.rs` — 新增 `BatchOp` 与 `write_batch`**

在 `DocStore` 之前加：

```rust
/// 单个批量写操作：文档/索引/审计统一原子写入。
pub enum BatchOp {
    Put { cf: &'static str, key: Vec<u8>, value: Vec<u8> },
    Delete { cf: &'static str, key: Vec<u8> },
}

impl BatchOp {
    pub fn put<T: Serialize>(cf: &'static str, key: Vec<u8>, value: &T) -> Result<Self, AppError> {
        Ok(BatchOp::Put {
            cf,
            key,
            value: bincode::serialize(value)?,
        })
    }

    pub fn put_raw(cf: &'static str, key: Vec<u8>, value: Vec<u8>) -> Self {
        BatchOp::Put { cf, key, value }
    }

    pub fn delete(cf: &'static str, key: Vec<u8>) -> Self {
        BatchOp::Delete { cf, key }
    }
}
```

在 `impl DocStore` 内加（`exists` 之后）：

```rust
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
```

- [ ] **Step 3: 修改 `src/storage/keys.rs` — 审计键编码**

顶部加 `use chrono::{DateTime, Utc};`，文件末尾加：

```rust
/// 审计主键：(时间倒序, id)。i64::MAX - millis 实现降序，前缀扫描最新在前。
pub fn audit_log_key(at: DateTime<Utc>, id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(24);
    let desc = i64::MAX - at.timestamp_millis();
    key.extend_from_slice(&desc.to_be_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}

/// (workspace_id, 时间倒序, id)：按 workspace 前缀扫描，最新在前。
pub fn audit_by_workspace_key(workspace_id: Ulid, at: DateTime<Utc>, id: Ulid) -> Vec<u8> {
    let mut key = Vec::with_capacity(40);
    key.extend_from_slice(&workspace_id.to_bytes());
    let desc = i64::MAX - at.timestamp_millis();
    key.extend_from_slice(&desc.to_be_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}

/// (resource_type \0 resource_id \0 时间倒序, id)：按资源前缀扫描。
pub fn audit_by_resource_key(resource_type: &str, resource_id: &str, at: DateTime<Utc>, id: Ulid) -> Vec<u8> {
    let mut key = Vec::new();
    key.extend_from_slice(resource_type.as_bytes());
    key.push(0);
    key.extend_from_slice(resource_id.as_bytes());
    key.push(0);
    let desc = i64::MAX - at.timestamp_millis();
    key.extend_from_slice(&desc.to_be_bytes());
    key.extend_from_slice(&id.to_bytes());
    key
}
```

- [ ] **Step 4: 加测试**

在 `src/storage/rocksdb.rs` 的 `mod tests` 内加：

```rust
    #[test]
    fn write_batch_atomic_put_and_delete() {
        let dir = temp_dir("batch");
        let store = DocStore::open(&dir).unwrap();
        let ops = vec![
            BatchOp::put_raw(cf::AUDIT_LOGS, b"k1".to_vec(), b"v1".to_vec()),
            BatchOp::put_raw(cf::AUDIT_LOGS, b"k2".to_vec(), b"v2".to_vec()),
            BatchOp::delete(cf::AUDIT_LOGS, b"gone".to_vec()),
        ];
        store.write_batch(ops).unwrap();
        assert_eq!(store.get_raw(cf::AUDIT_LOGS, b"k1").unwrap(), Some(b"v1".to_vec()));
        assert_eq!(store.get_raw(cf::AUDIT_LOGS, b"k2").unwrap(), Some(b"v2".to_vec()));
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn audit_log_key_orders_newest_first() {
        use chrono::TimeZone;
        let t1 = Utc.timestamp_millis_opt(1_000).unwrap();
        let t2 = Utc.timestamp_millis_opt(2_000).unwrap();
        let k1 = keys::audit_log_key(t1, ulid::Ulid::new());
        let k2 = keys::audit_log_key(t2, ulid::Ulid::new());
        assert!(k2 < k1, "较新的时间应产生更小的键，前缀扫描时排在前");
    }
```

`temp_dir` 需可被 `tests` 使用（已存在，无需改）。`use chrono::TimeZone;` 在测试内局部导入即可。

- [ ] **Step 5: 跑测试**

Run: `cargo test storage::rocksdb`
Expected: 新增 2 个测试通过。

- [ ] **Step 6: 可选提交**

```bash
git add src/storage && git commit -m "feat(storage): add WriteBatch + audit column families and key encoding"
```

---

### Task 3: AuditService（audit_ops + list 查询）

**Files:**
- Create: `src/service/audit.rs`
- Modify: `src/service/mod.rs`

**Interfaces:**
- Consumes: `AuditLog`（Task 1）、`BatchOp`/`keys::audit_*`（Task 2）。
- Produces: `crate::service::audit::audit_ops(&AuditLog) -> Result<Vec<BatchOp>, AppError>`（供 Entry/Label 服务复用）；`AuditService::list(workspace_id: Ulid, limit: usize) -> Result<Vec<AuditLog>, AppError>`；`Services.audit` 字段。

- [ ] **Step 1: 创建 `src/service/audit.rs`**

```rust
use std::sync::Arc;

use ulid::Ulid;

use crate::domain::AuditLog;
use crate::error::AppError;
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 生成一条审计日志的写入操作（主 CF + workspace 索引 + resource 索引）。
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
```

- [ ] **Step 2: 修改 `src/service/mod.rs`**

```rust
pub mod audit;
pub mod auth;
pub mod entry;
pub mod label;
pub mod workspace;
```

（`label` 模块在 Task 4 才创建，可先不加 `pub mod label;`，本任务只加 `pub mod audit;`。为减少后续改动，若想一次到位可先建空的 `src/service/label.rs`。）

`Services` 结构体加 `audit` 字段，`new` 中初始化：

```rust
use crate::service::audit::AuditService;

pub struct Services {
    pub store: Arc<DocStore>,
    pub config: Arc<Config>,
    pub auth: AuthService,
    pub workspace: WorkspaceService,
    pub entry: EntryService,
    pub audit: AuditService,
}

impl Services {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Self {
        let auth = AuthService::new(store.clone(), config.clone());
        let workspace = WorkspaceService::new(store.clone());
        let entry = EntryService::new(store.clone());
        let audit = AuditService::new(store.clone());
        Self {
            store,
            config,
            auth,
            workspace,
            entry,
            audit,
        }
    }
}
```

- [ ] **Step 3: 加测试**

在 `src/service/audit.rs` 末尾加 `mod tests`：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AuditAction, WorkspaceService};

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-audit-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn audit_list_returns_newest_first_and_scoped() {
        let dir = temp_dir("list");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = AuditService::new(store.clone());
        let ws_id = Ulid::new();
        let log1 = AuditLog::new(AuditAction::EntryCreated, Ulid::new(), "entry", "c1", Some(ws_id), None, None);
        let log2 = AuditLog::new(AuditAction::EntryUpdated, Ulid::new(), "entry", "c1", Some(ws_id), None, None);
        let mut ops = audit_ops(&log1).unwrap();
        ops.extend(audit_ops(&log2).unwrap());
        store.write_batch(ops).unwrap();

        let list = svc.list(ws_id, 100).unwrap();
        assert_eq!(list.len(), 2);
        // 最新在前
        assert_eq!(list[0].id, log2.id);
        assert_eq!(list[1].id, log1.id);

        // 其他 workspace 查询为空
        assert!(svc.list(Ulid::new(), 100).unwrap().is_empty());
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

（`WorkspaceService` 未用，删掉该 `use`。）

- [ ] **Step 4: 跑测试**

Run: `cargo test service::audit`
Expected: 通过。

- [ ] **Step 5: 可选提交**

```bash
git add src/service && git commit -m "feat(service): add AuditService with batch ops and scoped list"
```

---

### Task 4: LabelService（自定义标签 CRUD）

**Files:**
- Create: `src/service/label.rs`
- Modify: `src/service/mod.rs`（若上一步未加 `pub mod label;` 则此处加）

**Interfaces:**
- Consumes: `LabelSchema`/`LabelValueType`（domain）、`audit_ops`（Task 3）、`keys::label_schema_key`、`BatchOp`。
- Produces: `LabelService::{list_schemas, get_schema, create_schema, update_schema}`；`Services.label` 字段。

- [ ] **Step 1: 创建 `src/service/label.rs`**

```rust
use std::sync::Arc;

use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, LabelSchema, LabelValueType};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct LabelService {
    store: Arc<DocStore>,
}

impl LabelService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    pub fn list_schemas(&self, ws_id: Ulid) -> Result<Vec<LabelSchema>, AppError> {
        let prefix = ws_id.to_bytes();
        let rows = self.store.scan_prefix(cf::LABEL_SCHEMAS, &prefix)?;
        let mut out = Vec::new();
        for (_, v) in rows {
            out.push(bincode::deserialize(&v)?);
        }
        out.sort_by(|a: &LabelSchema, b: &LabelSchema| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get_schema(&self, ws_id: Ulid, name: &str) -> Result<Option<LabelSchema>, AppError> {
        self.store
            .get(cf::LABEL_SCHEMAS, &keys::label_schema_key(ws_id, name))
    }

    pub fn create_schema(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        name: &str,
        title: &str,
        value_type: LabelValueType,
        enum_values: Vec<String>,
    ) -> Result<LabelSchema, AppError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Internal("标签名称不能为空".to_string()));
        }
        if self.get_schema(ws_id, name)?.is_some() {
            return Err(AppError::Internal("标签名称已存在".to_string()));
        }
        let schema = LabelSchema::new(
            ws_id,
            name.to_string(),
            title.trim().to_string(),
            value_type,
            enum_values,
        );
        let audit = AuditLog::new(
            AuditAction::LabelSchemaCreated,
            actor,
            "label_schema",
            name,
            Some(ws_id),
            None,
            Some(serde_json::to_string(&schema).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::LABEL_SCHEMAS,
            keys::label_schema_key(ws_id, name),
            &schema,
        )?);
        self.store.write_batch(ops)?;
        Ok(schema)
    }

    pub fn update_schema(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        name: &str,
        title: &str,
        enum_values: Vec<String>,
    ) -> Result<LabelSchema, AppError> {
        let mut schema = self.get_schema(ws_id, name)?.ok_or(AppError::NotFound)?;
        let before = serde_json::to_string(&schema).unwrap_or_default();
        schema.title = title.trim().to_string();
        schema.enum_values = enum_values;
        let after = serde_json::to_string(&schema).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::LabelSchemaUpdated,
            actor,
            "label_schema",
            name,
            Some(ws_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::LABEL_SCHEMAS,
            keys::label_schema_key(ws_id, name),
            &schema,
        )?);
        self.store.write_batch(ops)?;
        Ok(schema)
    }
}
```

- [ ] **Step 2: 修改 `src/service/mod.rs`**

确保有 `pub mod label;`，并在 `Services` 加 `label` 字段：

```rust
use crate::service::label::LabelService;

pub struct Services {
    // ... 其他字段
    pub label: LabelService,
    pub audit: AuditService,
}
```

`new` 中：

```rust
        let label = LabelService::new(store.clone());
        let audit = AuditService::new(store.clone());
```

- [ ] **Step 3: 加测试**

在 `src/service/label.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-label-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn create_list_and_update_schema() {
        let dir = temp_dir("crud");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = LabelService::new(store.clone());
        let ws_id = Ulid::new();
        let actor = Ulid::new();

        let s = svc
            .create_schema(actor, ws_id, "Priority", "优先级", LabelValueType::Enum, vec!["High".into(), "Low".into()])
            .unwrap();
        assert_eq!(s.name, "Priority");

        // 重复名称报错
        assert!(svc
            .create_schema(actor, ws_id, "Priority", "x", LabelValueType::String, vec![])
            .is_err());

        // 列表包含
        let all = svc.list_schemas(ws_id).unwrap();
        assert!(all.iter().any(|x| x.name == "Priority"));

        // 更新 title/enum，name 不变
        let u = svc
            .update_schema(actor, ws_id, "Priority", "优先级2", vec!["High".into(), "Mid".into()])
            .unwrap();
        assert_eq!(u.name, "Priority");
        assert_eq!(u.title, "优先级2");
        assert_eq!(u.enum_values, vec!["High", "Mid"]);

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test service::label`
Expected: 通过。

- [ ] **Step 5: 可选提交**

```bash
git add src/service && git commit -m "feat(service): add LabelService for custom label schema CRUD"
```

---

### Task 5: EntryService（update / soft_delete / remove_labeling + 审计改造）

**Files:**
- Modify: `src/service/entry.rs`

**Interfaces:**
- Consumes: `AuditLog`/`AuditAction`（Task 1）、`audit_ops`（Task 3）、`BatchOp`（Task 2）。
- Produces: `EntryService::{update, soft_delete, remove_labeling}`；`create`/`set_labeling` 改为 WriteBatch + 审计；`list` 过滤已删除。

- [ ] **Step 1: 重写 `src/service/entry.rs`**

完整替换文件内容为：

```rust
use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{generate_entry_code, AuditAction, AuditLog, Entry, LabelSchema, LabelValue, Labeling};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

pub struct EntryService {
    store: Arc<DocStore>,
}

impl EntryService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    pub fn create(&self, actor: Ulid, workspace_id: Ulid, title: &str) -> Result<Entry, AppError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::Internal("标题不能为空".to_string()));
        }
        let mut entry = Entry::new(workspace_id, title.to_string(), actor);
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
        self.store.write_batch(ops)?;
        Ok(entry)
    }

    pub fn get(&self, code: &str) -> Result<Option<Entry>, AppError> {
        self.store.get(cf::ENTRIES, code.as_bytes())
    }

    /// 乐观并发更新：expected_updated_at 与当前 updated_at 不一致时返回 ConflictDetected。
    pub fn update(
        &self,
        actor: Ulid,
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
        let before = serde_json::to_string(&entry).unwrap_or_default();
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
        self.store.write_batch(ops)?;
        Ok(entry)
    }

    pub fn soft_delete(&self, actor: Ulid, code: &str) -> Result<(), AppError> {
        let mut entry = self.get(code)?.ok_or(AppError::NotFound)?;
        if entry.is_deleted() {
            return Ok(());
        }
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
        self.store.write_batch(ops)?;
        Ok(())
    }

    pub fn list(&self, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
        let prefix = workspace_id.to_bytes();
        let rows = self.store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &prefix)?;
        let mut entries = Vec::new();
        for (key, _) in rows {
            if key.len() <= 16 {
                continue;
            }
            let code = std::str::from_utf8(&key[16..]).unwrap_or("").to_string();
            if let Some(e) = self.store.get::<Entry>(cf::ENTRIES, code.as_bytes())? {
                if !e.is_deleted() {
                    entries.push(e);
                }
            }
        }
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(entries)
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
        let labeling = Labeling::new(entry_code.to_string(), label_name.to_string(), lv, actor);
        let before = self
            .store
            .get::<Labeling>(cf::LABELINGS, &keys::labeling_key(entry_code, label_name))?
            .map(|l: Labeling| serde_json::to_string(&l).unwrap_or_default());
        let after = serde_json::to_string(&labeling).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::LabelingSet,
            actor,
            "labeling",
            entry_code,
            Some(entry.workspace_id),
            before,
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::LABELINGS,
            keys::labeling_key(entry_code, label_name),
            &labeling,
        )?);
        self.store.write_batch(ops)?;
        Ok(labeling)
    }

    pub fn remove_labeling(
        &self,
        actor: Ulid,
        entry_code: &str,
        label_name: &str,
    ) -> Result<(), AppError> {
        let entry = self.get(entry_code)?.ok_or(AppError::NotFound)?;
        let key = keys::labeling_key(entry_code, label_name);
        let before = self
            .store
            .get::<Labeling>(cf::LABELINGS, &key)?
            .map(|l: Labeling| serde_json::to_string(&l).unwrap_or_default());
        let audit = AuditLog::new(
            AuditAction::LabelingRemoved,
            actor,
            "labeling",
            entry_code,
            Some(entry.workspace_id),
            before,
            None,
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::delete(cf::LABELINGS, key));
        self.store.write_batch(ops)?;
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
}
```

- [ ] **Step 2: 加测试**

在 `src/service/entry.rs` 末尾：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{WorkspaceService, WorkspaceRole};

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
        let entry_svc = EntryService::new(store.clone());
        (dir, store, entry_svc, ws.id, actor)
    }

    #[test]
    fn update_with_stale_timestamp_conflicts() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "hello").unwrap();
        // 错误的时间戳 → 冲突
        let err = svc.update(actor, &e.code, "2000-01-01T00:00:00Z", "x", "").unwrap_err();
        assert!(matches!(err, AppError::ConflictDetected));
        // 正确的时间戳 → 成功
        let expected = e.updated_at.to_rfc3339();
        let u = svc.update(actor, &e.code, &expected, "改了", "详情").unwrap();
        assert_eq!(u.title, "改了");
        assert_eq!(u.detail, "详情");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn soft_delete_filters_from_list() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "待删除").unwrap();
        assert_eq!(svc.list(ws_id).unwrap().len(), 1);
        svc.soft_delete(actor, &e.code).unwrap();
        assert!(svc.list(ws_id).unwrap().is_empty());
        let got = svc.get(&e.code).unwrap().unwrap();
        assert!(got.is_deleted());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn remove_labeling_clears_value() {
        let (dir, _store, svc, ws_id, actor) = setup();
        let e = svc.create(actor, ws_id, "任务").unwrap();
        svc.set_labeling(actor, &e.code, "Task", &serde_json::json!("Open")).unwrap();
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
}
```

- [ ] **Step 3: 跑测试**

Run: `cargo test service::entry`
Expected: 4 个测试通过。

- [ ] **Step 4: 可选提交**

```bash
git add src/service/entry.rs && git commit -m "feat(service): add entry update/soft-delete/remove-labeling with audit + optimistic concurrency"
```

---

### Task 6: GraphQL 层

**Files:**
- Modify: `src/api/graphql.rs`

**Interfaces:**
- Consumes: `services.label`（Task 4）、`services.audit`（Task 3）、`services.entry.update/soft_delete/remove_labeling`（Task 5）、`AuditLog`/`LabelValueType`。
- Produces: Query `entry`/`auditLogs`/`myRole`；Mutation `updateEntry`/`deleteEntry`/`createLabelSchema`/`updateLabelSchema`/`removeLabeling`；`GqlAuditLog`。

- [ ] **Step 1: 修改 imports 与新增类型**

顶部 import 增加：

```rust
use crate::domain::{Account, AuditLog, Entry, LabelSchema, LabelValueType, Labeling, Workspace, WorkspaceRole};
```

新增 `GqlAuditLog`（放在 `GqlAuthResult` 之后）：

```rust
#[derive(SimpleObject, Clone)]
pub struct GqlAuditLog {
    id: ID,
    action: String,
    actor_id: ID,
    resource_type: String,
    resource_id: String,
    workspace_id: Option<ID>,
    before: Option<String>,
    after: Option<String>,
    at: String,
}

impl From<AuditLog> for GqlAuditLog {
    fn from(l: AuditLog) -> Self {
        Self {
            id: l.id.to_string().into(),
            action: serde_json::to_value(&l.action)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default(),
            actor_id: l.actor_id.to_string().into(),
            resource_type: l.resource_type,
            resource_id: l.resource_id,
            workspace_id: l.workspace_id.map(|w| w.to_string().into()),
            before: l.before,
            after: l.after,
            at: l.at.to_rfc3339(),
        }
    }
}
```

- [ ] **Step 2: 修改 `label_schemas` query 使用 LabelService**

把 `src/api/graphql.rs` 中 `label_schemas` resolver 内的 `gql.services.workspace.label_schemas(ws_id)?` 改为 `gql.services.label.list_schemas(ws_id)?`。

同时删除 `src/service/workspace.rs` 中的 `label_schemas` 方法（第 119-128 行）及 `LabelSchema` import（若不再使用）。检查 `cargo build` 无 unused import 警告。

- [ ] **Step 3: 新增 Query resolver**

在 `impl Query` 内加：

```rust
    async fn entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<Option<GqlEntry>> {
        let gql = ctx.data::<GraphqlContext>()?;
        gql.require_auth()?;
        let Some(entry) = gql.services.entry.get(&code)? else {
            return Ok(None);
        };
        gql.require_member(entry.workspace_id)?;
        let labels = gql.services.entry.labelings(&code)?;
        Ok(Some(GqlEntry::new(entry, labels)))
    }

    async fn audit_logs(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        limit: Option<usize>,
    ) -> GqlResult<Vec<GqlAuditLog>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        let list = gql.services.audit.list(ws_id, limit.unwrap_or(100))?;
        Ok(list.into_iter().map(Into::into).collect())
    }

    async fn my_role(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<String> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        let auth = gql.require_auth()?;
        match gql.services.workspace.get_member(ws_id, auth.account_id)? {
            Some(m) => Ok(m.role.as_str().to_string()),
            None => Ok("none".to_string()),
        }
    }
```

- [ ] **Step 4: 新增 Mutation resolver**

在 `impl Mutation` 内加：

```rust
    async fn update_entry(
        &self,
        ctx: &Context<'_>,
        code: String,
        expected_updated_at: String,
        title: String,
        detail: String,
    ) -> GqlResult<GqlEntry> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        let updated = gql
            .services
            .entry
            .update(auth.account_id, &code, &expected_updated_at, &title, &detail)?;
        let labels = gql.services.entry.labelings(&code)?;
        Ok(GqlEntry::new(updated, labels))
    }

    async fn delete_entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        gql.services.entry.soft_delete(auth.account_id, &code)?;
        Ok(true)
    }

    async fn create_label_schema(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        title: String,
        value_type: String,
        enum_values: Vec<String>,
    ) -> GqlResult<GqlLabelSchema> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let vt = LabelValueType::from_str(&value_type)
            .ok_or_else(|| AppError::Internal("无效的标签值类型".to_string()))?;
        let schema = gql
            .services
            .label
            .create_schema(auth.account_id, ws_id, &name, &title, vt, enum_values)?;
        Ok(schema.into())
    }

    async fn update_label_schema(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        title: String,
        enum_values: Vec<String>,
    ) -> GqlResult<GqlLabelSchema> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let schema = gql
            .services
            .label
            .update_schema(auth.account_id, ws_id, &name, &title, enum_values)?;
        Ok(schema.into())
    }

    async fn remove_labeling(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        label_name: String,
    ) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        gql.services
            .entry
            .remove_labeling(auth.account_id, &entry_code, &label_name)?;
        Ok(true)
    }
```

- [ ] **Step 5: 编译验证**

Run: `cargo build`
Expected: 编译通过（会暴露 `services.label`/`services.audit` 是否已接入、`workspace.label_schemas` 删除后引用是否清理干净）。

- [ ] **Step 6: 可选提交**

```bash
git add src/api src/service/workspace.rs && git commit -m "feat(api): add entry/label/audit GraphQL resolvers"
```

---

### Task 7: 前端 graphql_client 扩展

**Files:**
- Modify: `src/frontend/graphql_client.rs`

**Interfaces:**
- Consumes: 现有 `graphql`/`get_token`。
- Produces: `entry`, `update_entry`, `delete_entry`, `remove_labeling`, `create_label_schema`, `update_label_schema`, `audit_logs`, `my_role`；`AuditLog` 结构体。

- [ ] **Step 1: 新增响应类型与函数**

在 `graphql_client.rs` 的 `LabelSchema` 之后加：

```rust
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLog {
    pub id: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub at: String,
}
```

在 `set_labeling` 之后加：

```rust
pub async fn entry(code: &str) -> Result<Option<Entry>, String> {
    let data = graphql(
        "query($c: String!) { entry(code: $c) { code title detail updatedAt labels { labelName value } } }",
        json!({ "c": code }),
    )
    .await?;
    Ok(data
        .get("entry")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
}

pub async fn update_entry(
    code: &str,
    expected_updated_at: &str,
    title: &str,
    detail: &str,
) -> Result<Entry, String> {
    let data = graphql(
        "mutation($c: String!, $e: String!, $t: String!, $d: String!) { updateEntry(code: $c, expectedUpdatedAt: $e, title: $t, detail: $d) { code title detail updatedAt labels { labelName value } } }",
        json!({ "c": code, "e": expected_updated_at, "t": title, "d": detail }),
    )
    .await?;
    serde_json::from_value(data.get("updateEntry").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn delete_entry(code: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!) { deleteEntry(code: $c) }",
        json!({ "c": code }),
    )
    .await?;
    Ok(data
        .get("deleteEntry")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn remove_labeling(entry_code: &str, label_name: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!, $n: String!) { removeLabeling(entryCode: $c, labelName: $n) }",
        json!({ "c": entry_code, "n": label_name }),
    )
    .await?;
    Ok(data
        .get("removeLabeling")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn create_label_schema(
    workspace_id: &str,
    name: &str,
    title: &str,
    value_type: &str,
    enum_values: &[String],
) -> Result<LabelSchema, String> {
    let data = graphql(
        "mutation($id: ID!, $n: String!, $t: String!, $vt: String!, $ev: [String!]!) { createLabelSchema(workspaceId: $id, name: $n, title: $t, valueType: $vt, enumValues: $ev) { name title valueType enumValues } }",
        json!({ "id": workspace_id, "n": name, "t": title, "vt": value_type, "ev": enum_values }),
    )
    .await?;
    serde_json::from_value(data.get("createLabelSchema").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn update_label_schema(
    workspace_id: &str,
    name: &str,
    title: &str,
    enum_values: &[String],
) -> Result<LabelSchema, String> {
    let data = graphql(
        "mutation($id: ID!, $n: String!, $t: String!, $ev: [String!]!) { updateLabelSchema(workspaceId: $id, name: $n, title: $t, enumValues: $ev) { name title valueType enumValues } }",
        json!({ "id": workspace_id, "n": name, "t": title, "ev": enum_values }),
    )
    .await?;
    serde_json::from_value(data.get("updateLabelSchema").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn audit_logs(workspace_id: &str) -> Result<Vec<AuditLog>, String> {
    let data = graphql(
        "query($id: ID!) { auditLogs(workspaceId: $id) { id action resourceType resourceId at } }",
        json!({ "id": workspace_id }),
    )
    .await?;
    serde_json::from_value(data.get("auditLogs").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn my_role(workspace_id: &str) -> Result<String, String> {
    let data = graphql(
        "query($id: ID!) { myRole(workspaceId: $id) }",
        json!({ "id": workspace_id }),
    )
    .await?;
    Ok(data
        .get("myRole")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string())
}
```

- [ ] **Step 2: 编译验证**

Run: `cargo build`
Expected: 编译通过（`graphql_client` 的 `#[cfg(not(target_arch="wasm32"))]` 分支下 `graphql` 为 stub，但这些类型化函数在 native 下仍会编译，签名与 serde 类型需正确）。

- [ ] **Step 3: 可选提交**

```bash
git add src/frontend/graphql_client.rs && git commit -m "feat(frontend): extend graphql client for entry/label/audit operations"
```

---

### Task 8: Entry 详情面板（右侧编辑）

**Files:**
- Modify: `src/frontend/pages.rs`

**Interfaces:**
- Consumes: Task 7 的 `entry`/`update_entry`/`delete_entry`/`remove_labeling`/`set_labeling`。
- Produces: `WorkspaceMain` 增加行选中 + `EntryPanel` 组件（编辑标题/详情/标签/删除）。

- [ ] **Step 1: 修改 `WorkspaceMain` 加入选中信号与详情面板**

在 `WorkspaceMain` 内、`title` 信号旁新增：

```rust
    let selected: RwSignal<String> = RwSignal::new(String::new());
```

将表格行的标题单元格改为可点击（`entry_row` 增加 `selected` 参数），并在 `</table>` 后渲染面板。修改 `view!` 中 tbody 的调用与末尾：

```rust
            <table class="entry-table">
                <thead><tr><th>"标题"</th><th>"Task"</th><th>"更新时间"</th></tr></thead>
                <tbody>
                    {move || match data.get() {
                        None => view! { <tr><td colspan="3">"加载中…"</td></tr> }.into_any(),
                        Some(Err(e)) => view! { <tr><td colspan="3" class="error">{e.clone()}</td></tr> }.into_any(),
                        Some(Ok((_ws, items, schemas))) => {
                            let task_opts: Vec<String> = schemas
                                .iter()
                                .find(|s| s.name == "Task")
                                .map(|s| s.enum_values.clone())
                                .unwrap_or_default();
                            items.iter().map(|e| entry_row(e, &task_opts, refresh, selected)).collect::<Vec<_>>().into_any()
                        }
                    }}
                </tbody>
            </table>

            {move || {
                let code = selected.get();
                if code.is_empty() {
                    view! { <div></div> }.into_any()
                } else {
                    let schemas = data.get().and_then(|r| r.ok()).map(|(_, _, s)| s).unwrap_or_default();
                    view! { <EntryPanel code refresh schemas /> }.into_any()
                }
            }}
```

- [ ] **Step 2: 修改 `entry_row` 标题可点击**

修改 `entry_row` 签名与标题单元格：

```rust
fn entry_row(entry: &Entry, task_opts: &[String], refresh: RwSignal<u32>, selected: RwSignal<String>) -> impl IntoView {
    // ... 原有 current/code/opts/title/updated_at 提取不变 ...
    let title = entry.title.clone();
    let code = entry.code.clone();
    view! {
        <tr>
            <td>
                <a href="#" on:click=move |ev| {
                    ev.prevent_default();
                    selected.set(code.clone());
                }>{title}</a>
            </td>
            // Task 下拉与更新时间单元格不变
        </tr>
    }
}
```

注意：原 `entry_row` 内 `code` 已用于 `set_labeling` 的闭包，需同时保留两份 `code`（一份给 on:click，一份给 on:change）。

- [ ] **Step 3: 新增 `EntryPanel` 组件**

在文件末尾加：

```rust
#[component]
fn EntryPanel(code: RwSignal<String>, refresh: RwSignal<u32>, schemas: Vec<LabelSchema>) -> impl IntoView {
    let data: RwSignal<Option<Result<Entry, String>>> = RwSignal::new(None);
    let title = RwSignal::new(String::new());
    let detail = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);

    Effect::new_sync(move |_| {
        let c = code.get();
        if c.is_empty() {
            return;
        }
        spawn_local(async move {
            match entry(&c).await {
                Ok(Some(e)) => {
                    title.set(e.title.clone());
                    detail.set(e.detail.clone());
                    data.set(Some(Ok(e)));
                }
                Ok(None) => data.set(Some(Err("条目不存在".to_string()))),
                Err(err) => data.set(Some(Err(err))),
            }
        });
    });

    let save = move |ev: SubmitEvent| {
        ev.prevent_default();
        let c = code.get();
        let Some(expected) = data.get().and_then(|r| r.ok()).map(|e| e.updated_at.clone()) else {
            return;
        };
        let t = title.get();
        let d = detail.get();
        spawn_local(async move {
            match update_entry(&c, &expected, &t, &d).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|n| *n += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let del = move |_| {
        let c = code.get();
        spawn_local(async move {
            match delete_entry(&c).await {
                Ok(true) => {
                    code.set(String::new());
                    refresh.update(|n| *n += 1);
                }
                Ok(false) => error.set(Some("删除失败".to_string())),
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let close = move |_| code.set(String::new());

    view! {
        <div class="entry-panel">
            <div class="panel-head">
                <strong>"条目详情"</strong>
                <button on:click=close>"关闭"</button>
            </div>
            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
            <form class="entry-form" on:submit=save>
                <label>"标题"</label>
                <input prop:value=title on:input=move |ev| title.set(event_target_value(&ev)) />
                <label>"详情"</label>
                <textarea rows="6" prop:value=detail on:input=move |ev| detail.set(event_target_value(&ev)) />
                <button type="submit">"保存"</button>
            </form>
            <div class="panel-labels">
                <LabelEditor code schemas refresh />
            </div>
            <button class="danger" on:click=del>"删除"</button>
        </div>
    }
}
```

- [ ] **Step 4: 新增 `LabelEditor` 组件（标签增删）**

在 `EntryPanel` 之后加：

```rust
#[component]
fn LabelEditor(code: RwSignal<String>, schemas: Vec<LabelSchema>, refresh: RwSignal<u32>) -> impl IntoView {
    view! {
        <div>
            {schemas.into_iter().map(|s| {
                let name = s.name.clone();
                let code = code;
                let is_enum = s.value_type == "enum";
                view! {
                    <div class="label-row">
                        <span class="label-title">{s.title.clone()}</span>
                        {if is_enum {
                            let opts = s.enum_values.clone();
                            view! {
                                <select on:change=move |ev| {
                                    let v = event_target_value(&ev);
                                    let c = code.get();
                                    spawn_local(async move {
                                        if v.is_empty() {
                                            let _ = remove_labeling(&c, &name).await;
                                        } else {
                                            let _ = set_labeling(&c, &name, &Value::String(v)).await;
                                        }
                                        refresh.update(|n| *n += 1);
                                    });
                                }>
                                    <option value="">"（清除）"</option>
                                    {opts.into_iter().map(|o| view! { <option value=o.clone()>{o.clone()}</option> }).collect::<Vec<_>>()}
                                </select>
                            }.into_any()
                        } else {
                            let c = code;
                            let name2 = name.clone();
                            let input = RwSignal::new(String::new());
                            view! {
                                <input placeholder="值" prop:value=input on:input=move |ev| input.set(event_target_value(&ev)) />
                                <button on:click=move |_| {
                                    let v = input.get();
                                    let c = c.get();
                                    let name = name2.clone();
                                    spawn_local(async move {
                                        let _ = set_labeling(&c, &name, &Value::String(v)).await;
                                        refresh.update(|n| *n += 1);
                                    });
                                }>"设置"</button>
                            }.into_any()
                        }}
                        <button on:click=move |_| {
                            let c = code.get();
                            let name = name.clone();
                            spawn_local(async move {
                                let _ = remove_labeling(&c, &name).await;
                                refresh.update(|n| *n += 1);
                            });
                        }>"移除"</button>
                    </div>
                }
            }).collect::<Vec<_>>()}
        </div>
    }
}
```

（`RwSignal<String>` 实现 `Copy`，闭包捕获 `code`/`refresh` 需显式 `move`；`schemas` 用 `into_iter` 消费并 `clone` 所需字段。若编译器对 `name2`/`c` 捕获报 borrow 错误，按错误提示将捕获变量 `clone` 成 owned 即可。）

- [ ] **Step 5: 更新 import**

在 `pages.rs` 顶部 `use super::graphql_client::{...}` 中加入 `delete_entry, entry, remove_labeling, update_entry`。

- [ ] **Step 6: 编译验证**

Run: `cargo build`
Expected: 编译通过。若报 borrow/闭包错误，按编译器提示调整捕获（将共享变量在闭包内 `clone`）。

- [ ] **Step 7: 可选提交**

```bash
git add src/frontend/pages.rs && git commit -m "feat(frontend): add entry detail panel with edit/label/delete"
```

---

### Task 9: Workspace 设置页（自定义标签 CRUD + 审计）与路由

**Files:**
- Modify: `src/frontend/pages.rs`
- Modify: `src/app.rs`

**Interfaces:**
- Consumes: Task 7 的 `workspace_by_slug`/`label_schemas`/`create_label_schema`/`update_label_schema`/`audit_logs`/`my_role`。
- Produces: `WorkspaceSettings` 组件；`app.rs` 新增 `/:slug/settings` 路由。

- [ ] **Step 1: 新增 `WorkspaceSettings` 组件**

在 `pages.rs` 末尾加：

```rust
#[component]
pub fn WorkspaceSettings() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let data: RwSignal<Option<Result<(Workspace, String, Vec<LabelSchema>, Vec<AuditLog>), String>>> =
        RwSignal::new(None);

    Effect::new_sync(move |_| {
        let s = slug();
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let result = async {
                    let ws = workspace_by_slug(&s)
                        .await?
                        .ok_or("工作空间不存在".to_string())?;
                    let role = my_role(&ws.id).await?;
                    let schemas = label_schemas(&ws.id).await?;
                    let logs = audit_logs(&ws.id).await?;
                    Ok::<_, String>((ws, role, schemas, logs))
                }
                .await;
                data.set(Some(result));
            });
        }
    });

    let new_name = RwSignal::new(String::new());
    let new_title = RwSignal::new(String::new());
    let new_type = RwSignal::new("enum".to_string());
    let new_enum = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let refresh = RwSignal::new(0u32);

    let create = move |ev: SubmitEvent| {
        ev.prevent_default();
        let Some(ws_id) = data.get().and_then(|r| r.ok()).map(|(w, _, _, _)| w.id.clone()) else {
            return;
        };
        let n = new_name.get();
        let t = new_title.get();
        let vt = new_type.get();
        let evals: Vec<String> = new_enum
            .get()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        spawn_local(async move {
            match create_label_schema(&ws_id, &n, &t, &vt, &evals).await {
                Ok(_) => {
                    new_name.set(String::new());
                    new_title.set(String::new());
                    new_enum.set(String::new());
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    view! {
        <div class="settings-page">
            <header class="topbar">
                <A href="/workspaces">"← 工作空间"</A>
                <h1>"设置"</h1>
            </header>

            {move || error.get().map(|e| view! { <p class="error">{e}</p> })}

            {move || match data.get() {
                None => view! { <p>"加载中…"</p> }.into_any(),
                Some(Err(e)) => view! { <p class="error">{e.clone()}</p> }.into_any(),
                Some(Ok((_ws, role, schemas, logs))) => {
                    let can_manage = role == "owner" || role == "maintainer";
                    view! {
                        <h2>"标签定义"</h2>
                        {if can_manage {
                            view! {
                                <form class="new-schema" on:submit=create>
                                    <input placeholder="名称（不可改，如 Priority）" prop:value=new_name on:input=move |ev| new_name.set(event_target_value(&ev)) />
                                    <input placeholder="显示名称" prop:value=new_title on:input=move |ev| new_title.set(event_target_value(&ev)) />
                                    <select prop:value=new_type on:change=move |ev| new_type.set(event_target_value(&ev))>
                                        <option value="enum">"Enum"</option>
                                        <option value="string">"String"</option>
                                        <option value="boolean">"Boolean"</option>
                                        <option value="integer">"Integer"</option>
                                        <option value="float">"Float"</option>
                                        <option value="null">"Null"</option>
                                    </select>
                                    <input placeholder="枚举值（逗号分隔）" prop:value=new_enum on:input=move |ev| new_enum.set(event_target_value(&ev)) />
                                    <button type="submit">"新建标签"</button>
                                </form>
                            }.into_any()
                        } else {
                            view! { <p class="hint">"仅 Maintainer 及以上可管理标签"</p> }.into_any()
                        }}

                        <ul class="schema-list">
                            {schemas.iter().map(|s| {
                                let ws_id = _ws.id.clone();
                                let name = s.name.clone();
                                let title = RwSignal::new(s.title.clone());
                                let enum_str = RwSignal::new(s.enum_values.join(","));
                                view! {
                                    <li>
                                        <span class="schema-name">{name.clone()}</span>
                                        <input prop:value=title on:input=move |ev| title.set(event_target_value(&ev)) />
                                        <input placeholder="枚举值" prop:value=enum_str on:input=move |ev| enum_str.set(event_target_value(&ev)) />
                                        <span class="muted">{s.value_type.clone()}</span>
                                        {if can_manage {
                                            let ws_id2 = ws_id.clone();
                                            let name2 = name.clone();
                                            view! {
                                                <button on:click=move |_| {
                                                    let evals: Vec<String> = enum_str.get().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                                                    let t = title.get();
                                                    let ws_id = ws_id2.clone();
                                                    let name = name2.clone();
                                                    spawn_local(async move {
                                                        if let Err(e) = update_label_schema(&ws_id, &name, &t, &evals).await {
                                                            error.set(Some(e));
                                                        }
                                                        refresh.update(|x| *x += 1);
                                                    });
                                                }>"保存"</button>
                                            }.into_any()
                                        } else {
                                            view! { <span></span> }.into_any()
                                        }}
                                    </li>
                                }
                            }).collect::<Vec<_>>()}
                        </ul>

                        <h2>"审计日志"</h2>
                        <ul class="audit-list">
                            {logs.iter().map(|l| view! {
                                <li>
                                    <span class="audit-action">{l.action.clone()}</span>
                                    <span>{l.resource_type.clone()} {l.resource_id.clone()}</span>
                                    <span class="muted">{l.at.clone()}</span>
                                </li>
                            }).collect::<Vec<_>>()}
                        </ul>
                    }
                }
            }}
        </div>
    }
}
```

（需在 `use super::graphql_client::{...}` 中加入 `audit_logs, create_label_schema, my_role, update_label_schema, AuditLog`。）

- [ ] **Step 2: 修改 `src/app.rs` 加路由与 import**

import 改为：

```rust
use crate::frontend::pages::{Home, Login, WorkspaceList, WorkspaceMain, WorkspaceSettings};
```

路由改为（settings 路由放在 `ParamSegment("slug")` 之前，保证多段匹配优先）：

```rust
                    <Route path=StaticSegment("") view=Home/>
                    <Route path=StaticSegment("login") view=Login/>
                    <Route path=StaticSegment("workspaces") view=WorkspaceList/>
                    <Route path=(ParamSegment("slug"), StaticSegment("settings")) view=WorkspaceSettings/>
                    <Route path=ParamSegment("slug") view=WorkspaceMain/>
```

- [ ] **Step 3: 编译验证（native + wasm）**

Run: `cargo build`
然后（若已安装 cargo-leptos）`cargo leptos build`，验证 wasm 目标也能编译。
Expected: 均编译通过。

- [ ] **Step 4: 手动端到端验证**

先删旧数据（避免 bincode 不兼容）：`rm -rf data`
Run: `cargo leptos dev`
浏览器走通：登录（admin@local / Admin12345）→ 进入 Workspace → 点行标题打开详情面板 → 改标题/详情/标签 → 保存 → 删除 → 进 `/:slug/settings` 查看/新建/编辑标签 + 查看审计日志。

- [ ] **Step 5: 可选提交**

```bash
git add src/frontend/pages.rs src/app.rs && git commit -m "feat(frontend): add workspace settings page with label CRUD and audit log"
```

---

## 自审结论

- **Spec 覆盖**：3.3（Entry 编辑/软删除/乐观并发）→ Task 5/6/8；3.4（自定义标签 CRUD + 移除打标）→ Task 4/6/8/9；3.9（审计日志写 + 查询）→ Task 1/2/3/6/9。
- **占位符扫描**：无 TBD/TODO；所有步骤含具体代码。
- **类型一致性**：`Entry.updated_at.to_rfc3339()` ↔ `update` 内 `parse_from_rfc3339`；`audit_log_key`(desc8+id16) 与 `audit_by_workspace_key` 后缀 24 字节读取一致；GraphQL snake_case 方法名自动映射 camelCase（`update_entry`→`updateEntry` 等），与 `graphql_client` 内查询字符串一致。
- **明确边界**：`labelings_by_label` 反查索引、Workspace/成员审计、全屏详情路由、tiny-editor、附件、实时、搜索、OAuth 均不在本轮，后续切片处理。
