# Rodeo — 技术方案文档

> 版本: 1.0 | 日期: 2026-09-04 | 状态: Draft

## 1. 技术选型

| 层次 | 选型 | 理由 |
|------|------|------|
| 语言 | Rust | 性能、安全、类型系统，前后端统一 |
| 全栈框架 | Leptos 0.7+ | Rust WASM 全栈，SSR+水合，细粒度信号响应式 |
| HTTP 框架 | Axum 0.8+ | Leptos 服务端集成，Tokio 原生，生态成熟 |
| API 协议 | GraphQL (async-graphql 7+) | 强类型 schema，按需查询，嵌套关联，自文档化 |
| 实时通信 | Axum WebSocket + tokio::broadcast | 轻量级双向通信，独立于 GraphQL |
| 文档存储 | RocksDB 9+ (自定义 DocStore)；可选 PostgreSQL 16+（同一 DocStore 门面的另一个后端，列族模型落成 kv 表） | 嵌入式，LSM-tree 高写吞吐，Column Family 分集合 |
| 全文检索 | Tantivy 0.22+ | 嵌入式，Rust 原生，性能接近 Lucene |
| 富文本编辑 | @opentiny/tiny-editor | 轻量级 Web Component，wasm-bindgen FFI 集成 |
| 样式 | Tailwind CSS 4+ | 原子化 CSS，Leptos 原生 class 支持 |
| ID 生成 | Ulid | 26 字符，时间有序，URL 安全，无需协调 |
| 密码哈希 | Argon2 | OWASP 推荐，抗 GPU/ASIC |
| JWT | jsonwebtoken | 紧凑，无状态，适合 cookie 传递 |

## 2. 架构设计

### 2.1 整体架构

```
┌──────────────────────────────────────────────────┐
│                  Leptos WASM App                 │
│  ┌──────────┐ ┌──────────┐ ┌──────────────────┐ │
│  │  Server   │ │  Client  │ │   Shared Types   │ │
│  │  Actions  │ │  Views   │ │   & Signals      │ │
│  └─────┬─────┘ └────┬─────┘ └────────┬─────────┘ │
└────────┼────────────┼─────────────────┼───────────┘
         │            │                 │
    ┌────┴────────────┴─────────────────┴────┐
    │           Axum Server (HTTP/WS)         │
    │  ┌──────────┐ ┌──────────┐ ┌──────────┐│
    │  │ GraphQL  │ │WebSocket │ │  Static  ││
    │  │ Endpoint │ │  Hub     │ │  Assets  ││
    │  └────┬─────┘ └─────┬────┘ └──────────┘│
    └────────┼────────────┼───────────────────┘
             │            │
    ┌────────┴────────────┴───────────────────┐
    │            Service Layer                 │
    │  ┌───────┐ ┌──────┐ ┌──────┐ ┌───────┐ │
    │  │ Auth  │ │Entry │ │Label │ │ View  │ │
    │  │ Svc   │ │ Svc  │ │ Svc  │ │ Svc   │ │
    │  └───┬───┘ └──┬───┘ └──┬───┘ └───┬───┘ │
    └──────┼────────┼────────┼─────────┼─────┘
           │        │        │         │
    ┌──────┴────────┴────────┴─────────┴─────┐
    │           Storage Layer                  │
    │  ┌──────────┐ ┌─────────┐ ┌──────────┐ │
    │  │ RocksDB  │ │ Tantivy │ │  File    │ │
    │  │ DocStore │ │  Index  │ │  Store   │ │
    │  └──────────┘ └─────────┘ └──────────┘ │
    └─────────────────────────────────────────┘
```

### 2.2 架构原则

- **三层分离**：API 层（路由/协议/序列化）→ Service 层（业务逻辑/权限校验/审计）→ Storage 层（持久化/索引），层间通过 trait 接口解耦
- **单进程部署**：RocksDB + Tantivy 祀入进程，编译为单个二进制文件，无外部服务依赖
- **CRUD + Audit**：直接 CRUD 操作，同一 RocksDB 事务内追加审计日志
- **共享类型**：Leptos 的 `#[isomorphic]` 和 `#[server]` 宏让前后端共享 Rust struct，GraphQL schema 由 async-graphql derive 宏从同一 struct 生成

### 2.3 请求处理流程

```
HTTP Request
  → Axum middleware (JWT 校验 → 注入 AuthContext)
  → GraphQL resolver (权限校验 → 调用 Service)
  → Service (业务逻辑 → 构建审计日志)
  → Storage (RocksDB 事务写入 + Tantivy 索引更新)
  → Service (向 WebSocketHub 发送变更通知)
  → GraphQL response
```

## 3. 项目结构

```
rodeo/
├── Cargo.toml
├── src/
│   ├── main.rs                    # 入口：Axum + Leptos 启动
│   ├── app.rs                     # Leptos App 根组件
│   ├── config.rs                  # 配置加载 (TOML)
│   ├── error.rs                   # 统一错误类型
│   ├── api/                       # API 层
│   │   ├── mod.rs
│   │   ├── graphql/               # GraphQL schema & resolvers
│   │   │   ├── mod.rs
│   │   │   ├── schema.rs         # Schema 构建
│   │   │   ├── query.rs          # Query 根
│   │   │   ├── mutation.rs       # Mutation 根
│   │   │   ├── types/            # GraphQL 类型映射
│   │   │   │   ├── account.rs
│   │   │   │   ├── workspace.rs
│   │   │   │   ├── entry.rs
│   │   │   │   ├── label.rs
│   │   │   │   └── view.rs
│   │   │   └── subscription.rs   # (预留，当前用 WebSocket)
│   │   └── ws/                    # WebSocket
│   │       ├── mod.rs
│   │       ├── hub.rs            # WebSocketHub
│   │       ├── handler.rs        # 连接处理
│   │       └── message.rs        # 消息类型定义
│   ├── service/                   # Service 层
│   │   ├── mod.rs
│   │   ├── auth.rs               # 认证服务
│   │   ├── workspace.rs          # Workspace 服务
│   │   ├── entry.rs              # Entry 服务
│   │   ├── label.rs              # 标签服务
│   │   ├── view.rs               # 视图服务
│   │   ├── attachment.rs         # 附件服务
│   │   ├── search.rs             # 搜索服务
│   │   └── audit.rs              # 审计服务
│   ├── storage/                   # Storage 层
│   │   ├── mod.rs
│   │   ├── rocksdb/              # RocksDB DocStore
│   │   │   ├── mod.rs
│   │   │   ├── store.rs         # 通用文档存储
│   │   │   ├── cf.rs            # Column Family 管理
│   │   │   └── index.rs         # 二级索引
│   │   ├── tantivy/              # Tantivy 索引
│   │   │   ├── mod.rs
│   │   │   ├── schema.rs        # 索引 schema
│   │   │   └── writer.rs        # 写入管理
│   │   └── file/                 # 文件存储
│   │       ├── mod.rs
│   │       └── local.rs         # 本地文件系统实现
│   ├── domain/                    # 领域模型
│   │   ├── mod.rs
│   │   ├── account.rs
│   │   ├── workspace.rs
│   │   ├── entry.rs
│   │   ├── label.rs
│   │   ├── view.rs
│   │   ├── attachment.rs
│   │   └── audit.rs
│   └── frontend/                  # Leptos 前端组件
│       ├── mod.rs
│       ├── layout/
│       │   ├── app_shell.rs
│       │   ├── sidebar.rs
│       │   └── header.rs
│       ├── pages/
│       │   ├── login.rs
│       │   ├── workspace_list.rs
│       │   ├── workspace_main.rs
│       │   ├── entry_detail.rs
│       │   ├── workspace_settings.rs
│       │   └── admin.rs
│       ├── components/
│       │   ├── view_table.rs
│       │   ├── entry_row.rs
│       │   ├── entry_panel.rs
│       │   ├── label_editor.rs
│       │   ├── attachment_list.rs
│       │   ├── presence_bar.rs
│       │   ├── tiny_editor.rs    # tiny-editor FFI 封装
│       │   └── search_bar.rs
│       └── graphql/
│           ├── mod.rs
│           ├── client.rs          # GraphQL 请求封装
│           └── queries/           # GraphQL query/mutation 定义
├── style/                         # Tailwind CSS
│   └── tailwind.css
├── migrations/                    # 数据迁移脚本
├── config.toml                    # 默认配置
└── static/                        # 静态资源
    └── tiny-editor/               # tiny-editor JS/WASM
```

## 4. 数据模型详细设计

### 4.1 RocksDB Column Families 与键设计

| CF | 主键格式 | 说明 |
|----|----------|------|
| `accounts` | `account_id` (Ulid bytes) | Account 文档 |
| `accounts_email_idx` | `email` (UTF-8 bytes) → `account_id` | 邮箱唯一索引 |
| `workspaces` | `workspace_id` | Workspace 文档 |
| `workspaces_slug_idx` | `slug` → `workspace_id` | slug 唯一索引 |
| `workspace_members` | `(workspace_id, account_id)` | 成员关联 |
| `workspace_members_by_account` | `(account_id, workspace_id)` | 反向索引：用户所在 Workspace |
| `entries` | `entry_code` (16 bytes) | Entry 文档 |
| `entries_by_workspace` | `(workspace_id, updated_at DESC, entry_code)` | Workspace 内按更新时间排序 |
| `label_schemas` | `(workspace_id, label_name)` | 标签定义 |
| `labelings` | `(entry_code, label_name)` | 打标关联 |
| `labelings_by_label` | `(workspace_id, label_name, label_value, entry_code)` | 按标签值反查 Entry |
| `views` | `view_id` | 视图定义 |
| `views_by_workspace` | `(workspace_id, view_id)` | Workspace 内视图列表 |
| `attachments` | `attachment_id` | 附件元数据 |
| `attachments_by_entry` | `(entry_code, attachment_id)` | Entry 下附件列表 |
| `audit_logs` | `(timestamp DESC, audit_id)` | 审计日志，按时间倒序 |
| `audit_logs_by_resource` | `(resource_type, resource_id, timestamp DESC)` | 按资源查审计 |
| `sessions` | `session_id` | 会话数据 |

上表的列族与键是**后端的逻辑模型**，不是 RocksDB 独有的物理结构。两个文档后端实现的是同一套模型：RocksDB 后端把每个 CF 落成一个真实的 Column Family；PostgreSQL 后端把它们原样存进一张 `kv(cf, key, value)` 表——`cf` 即列族名，`key` / `value` 即字节串各占一列。`(cf, key)` 的主键（btree）同时就是前缀扫描所需的有序索引：`cf` 等值 + `key` 范围 + `ORDER BY key` 全由它满足，`scan_prefix` 的「按 key 升序、在第一个不匹配处收尾」因此逐字保留。**这不是关系建模，是同一个 K/V 模型的两个实现**；代价是 PostgreSQL 里无法直接用 SQL 查业务字段、也没有外键约束。

### 4.2 文档序列化

所有文档使用 **bincode** 序列化（Rust 原生，零开销，比 JSON 紧凑 3-5x）。索引值使用 UTF-8 字符串键以便前缀扫描。

### 4.3 RocksDB 事务

写入操作使用 `WriteBatch` 保证原子性：

```rust
fn update_entry(&self, entry: &Entry, audit: &AuditLog) -> Result<()> {
    let mut batch = WriteBatch::default();
    // 主文档
    batch.put_cf(&self.cf_entries, &entry.code, bincode::serialize(entry)?);
    // 二级索引
    batch.put_cf(&self.cf_entries_by_ws, &ws_time_key, &entry.code);
    // 审计日志
    batch.put_cf(&self.cf_audit_logs, &audit_key, bincode::serialize(audit)?);
    self.db.write(batch)?; // 原子写入
    Ok(())
}
```

`DocStore::write_batch` 接收的 `BatchOp` 批量写在两个后端语义一致：RocksDB 后端用 `WriteBatch` 一次落盘，PostgreSQL 后端把它映射为**单事务**（同一键先删后写也按语句顺序生效），因此跨列族的「文档 + 二级索引 + 审计日志」要么全成、要么全不成。

### 4.4 Entry Code 生成

16 字符全局唯一编码，字符集 `[a-zA-Z0-9]`（62 个字符），由 `Ulid` (26 chars base32) 映射到 base62 并截断为 16 字符。碰撞概率极低（62^16 ≈ 4.7×10^28），冲突时重试。

## 5. GraphQL Schema 设计

### 5.1 Query

```graphql
type Query {
  # 认证
  me: Account

  # Workspace
  workspaces: [Workspace!]!
  workspace(slug: String!): Workspace

  # Entry
  entry(code: String!): Entry
  entries(workspaceId: ID!, filter: EntryFilter, sort: SortSpec, page: PageInput): EntryConnection!

  # 标签
  labelSchemas(workspaceId: ID!): [LabelSchema!]!

  # 视图
  views(workspaceId: ID!): [View!]!
  view(id: ID!): View

  # 搜索
  search(workspaceId: ID, query: String!, filters: [SearchFilter!], page: PageInput): EntryConnection!

  # 审计
  auditLogs(workspaceId: ID, resourceType: String, resourceId: String, page: PageInput): AuditLogConnection!

  # 成员
  workspaceMembers(workspaceId: ID!): [WorkspaceMember!]!
}
```

### 5.2 Mutation

```graphql
type Mutation {
  # 认证
  login(email: String!, password: String!): AuthResult!
  logout: Boolean!

  # Account
  createAccount(input: CreateAccountInput!): Account!
  updateAccount(id: ID!, input: UpdateAccountInput!): Account!

  # Workspace
  createWorkspace(input: CreateWorkspaceInput!): Workspace!
  updateWorkspace(id: ID!, input: UpdateWorkspaceInput!): Workspace!
  deleteWorkspace(id: ID!): Boolean!
  inviteMember(workspaceId: ID!, email: String!, role: WorkspaceRole!): WorkspaceMember!
  updateMemberRole(workspaceId: ID!, accountId: ID!, role: WorkspaceRole!): WorkspaceMember!
  removeMember(workspaceId: ID!, accountId: ID!): Boolean!

  # Entry
  createEntry(workspaceId: ID!, input: CreateEntryInput!): Entry!
  updateEntry(code: String!, expectedUpdatedAt: DateTime!, input: UpdateEntryInput!): Entry!
  deleteEntry(code: String!): Boolean!

  # 标签
  createLabelSchema(workspaceId: ID!, input: CreateLabelSchemaInput!): LabelSchema!
  setLabeling(entryCode: String!, labelName: String!, value: JSON!): Labeling!
  removeLabeling(entryCode: String!, labelName: String!): Boolean!

  # 视图
  createView(workspaceId: ID!, input: CreateViewInput!): View!
  updateView(id: ID!, input: UpdateViewInput!): View!
  deleteView(id: ID!): Boolean!

  # 附件
  uploadAttachment(entryCode: String!, file: Upload!): Attachment!
  deleteAttachment(id: ID!): Boolean!
}
```

### 5.3 关键类型

```graphql
type Entry {
  code: String!
  workspace: Workspace!
  title: String!
  detail: String!           # 富文本 JSON
  detailHtml: String!       # 服务端渲染 HTML (计算字段)
  labels: [Labeling!]!
  attachments: [Attachment!]!
  createdBy: Account!
  updatedBy: Account!
  createdAt: DateTime!
  updatedAt: DateTime!
}

type Labeling {
  entryCode: String!
  labelSchema: LabelSchema!
  value: JSON!
  setBy: Account!
  setAt: DateTime!
}

type WorkspaceMember {
  account: Account!
  role: WorkspaceRole!
  joinedAt: DateTime!
}

enum WorkspaceRole {
  OWNER        # 最高权限
  MAINTAINER
  WORKER
  READER       # 最低权限
}
# 权限排序: OWNER > MAINTAINER > WORKER > READER

type View {
  id: ID!
  name: String!
  filters: [Filter!]!
  sort: SortSpec
  columns: [String!]!
  isShared: Boolean!
  owner: Account!
}

input EntryFilter {
  labelName: String
  labelValue: JSON
  assigneeId: ID
  createdAfter: DateTime
  createdBefore: DateTime
  updatedAfter: DateTime
  updatedBefore: DateTime
}
```

## 6. 实时协作实现

### 6.1 WebSocket Hub

```rust
pub struct WebSocketHub {
    // workspace_id → 订阅该 workspace 的连接集合
    subscriptions: DashMap<Ulid, Vec<ConnectionHandle>>,
    // 广播通道，每 workspace 一个
    channels: DashMap<Ulid, broadcast::Sender<WsMessage>>,
}

impl WebSocketHub {
    /// 订阅 workspace，返回 broadcast Receiver
    pub fn subscribe(&self, ws_id: Ulid, handle: ConnectionHandle) -> broadcast::Receiver<WsMessage>;

    /// 取消订阅
    pub fn unsubscribe(&self, ws_id: Ulid, handle: &ConnectionHandle);

    /// 向 workspace 所有订阅者广播消息
    pub fn broadcast(&self, ws_id: Ulid, msg: WsMessage);
}
```

### 6.2 WebSocket 消息协议

```rust
#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WsMessage {
    // 客户端 → 服务端
    #[serde(rename = "subscribe")]
    Subscribe { workspace_id: Ulid },
    #[serde(rename = "unsubscribe")]
    Unsubscribe { workspace_id: Ulid },
    #[serde(rename = "heartbeat")]
    Heartbeat,

    // 服务端 → 客户端
    #[serde(rename = "presence_update")]
    PresenceUpdate {
        account_id: Ulid,
        name: String,
        avatar_url: Option<String>,
        status: PresenceStatus,  // Online | Away | Offline
    },
    #[serde(rename = "entry_changed")]
    EntryChanged {
        code: String,
        change_type: ChangeType,  // Created | Updated | Deleted
        changed_by: Ulid,
        timestamp: DateTime,
    },
    #[serde(rename = "labeling_changed")]
    LabelingChanged {
        entry_code: String,
        label_name: String,
        label_value: Value,
        changed_by: Ulid,
    },
    #[serde(rename = "view_invalidated")]
    ViewInvalidated { view_id: Ulid },
}
```

### 6.3 连接生命周期

1. 客户端发起 WebSocket 连接，携带 JWT（query param 或首条消息）
2. 服务端验证 JWT，建立连接，注册到 Hub
3. 客户端发送 `Subscribe` 消息订阅 Workspace
4. 服务端广播 `PresenceUpdate(Online)` 给该 Workspace 其他成员
5. 连接期间双向心跳（30s 间隔），超时判定断线
6. 断线时服务端广播 `PresenceUpdate(Offline)`，清理 Hub 注册

### 6.4 Leptos 客户端集成

```rust
// 全局 WebSocket Signal
#[derive(Clone, Copy)]
struct WsContext {
    messages: Signal<Option<WsMessage>>,
    online_members: Signal<Vec<OnlineMember>>,
}

// 组件中使用
#[component]
fn EntryPanel(code: String) -> impl IntoView {
    let ws = use_context::<WsContext>().unwrap();
    let entry = create_resource(|| code.clone(), |code| fetch_entry(code));

    // 响应 WebSocket 变更通知
    create_effect(move |_| {
        if let Some(WsMessage::EntryChanged { code: changed_code, .. }) = ws.messages.get() {
            if changed_code == code {
                entry.refetch(); // 刷新 Entry 数据
            }
        }
    });

    view! { /* ... */ }
}
```

## 7. 全文检索实现

### 7.1 Tantivy 索引 Schema

```rust
fn entry_index_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_text_field("entry_code", STRING | STORED);   // 精确匹配 + 存储
    builder.add_text_field("workspace_id", STRING | FAST);    // 过滤
    builder.add_text_field("title", TEXT | STORED);           // 全文分词
    builder.add_text_field("content", TEXT);                  // 全文分词（详情纯文本）
    builder.add_text_field("labels", TEXT);                   // 标签值全文
    builder.add_date_field("updated_at", FAST);               // 排序/过滤
    builder.build()
}
```

### 7.2 索引更新

```rust
impl SearchIndex {
    /// Entry 写入后同步更新索引
    pub fn index_entry(&self, entry: &Entry, labels: &[Labeling]) -> Result<()> {
        let mut writer = self.writer.lock().unwrap();
        writer.add_document(doc!(
            self.code_field => entry.code.as_str(),
            self.ws_field => entry.workspace_id.to_string(),
            self.title_field => entry.title.as_str(),
            self.content_field => strip_rich_text(&entry.detail),
            self.labels_field => format_labels(labels),
            self.updated_field => entry.updated_at.into(),
        ));
        writer.commit()?;
        Ok(())
    }
}
```

### 7.3 搜索查询

```rust
impl SearchIndex {
    pub fn search(&self, query: &str, ws_id: Option<Ulid>, limit: usize) -> Result<Vec<String>> {
        let searcher = self.reader.searcher();
        let query_parser = QueryParser::for_index(&self.index, vec![self.title_field, self.content_field, self.labels_field], self.tokenizer.clone());

        let mut combined = query_parser.parse_query(query)?;
        if let Some(ws) = ws_id {
            let ws_query = TermQuery::new(Term::from_field_text(self.ws_field, &ws.to_string()), Boost(1.0));
            combined = BooleanQuery::intersection(vec![Box::new(combined), Box::new(ws_query)]);
        }

        let top_docs = searcher.search(&combined, &TopDocs::with_limit(limit))?;
        let codes: Vec<String> = top_docs.iter()
            .filter_map(|(_, addr)| searcher.doc(addr).ok())
            .filter_map(|doc| doc.get_first(self.code_field)?.as_text().map(|s| s.to_string()))
            .collect();
        Ok(codes)
    }
}
```

### 7.4 索引一致性

- 写操作流程：RocksDB 写入 → Tantivy 索引更新 → 通知 WebSocket
- Tantivy 写入失败不阻塞主操作（RocksDB 已持久化），错误记入 reconcile 队列
- **Reconcile**：定时任务（5min）对比 RocksDB `updated_at` 与 Tantivy 索引时间戳，修复不一致

## 8. 认证实现

### 8.1 内置账号认证

```rust
impl AuthService {
    pub async fn login(&self, email: &str, password: &str) -> Result<AuthResult> {
        let account = self.store.find_account_by_email(email).await?
            .ok_or(Error::InvalidCredentials)?;
        if !Argon2::verify_password(password, &account.password_hash)? {
            return Err(Error::InvalidCredentials);
        }
        let session = Session::new(account.id, self.config.session_ttl);
        self.store.save_session(&session).await?;
        let token = self.jwt.sign(session.id, account.id, session.expires_at)?;
        Ok(AuthResult { token, account })
    }
}
```

### 8.2 OAuth2 流程

```rust
impl AuthService {
    /// 生成授权 URL，前端跳转
    pub fn oauth_authorize_url(&self, provider: &str) -> Result<String> {
        let config = self.oauth_configs.get(provider).ok_or(Error::UnknownProvider)?;
        Ok(config.authorize_url(CsrfToken::new_random))?)
    }

    /// OAuth 回调，换取 token + userinfo，创建/关联 Account
    pub async fn oauth_callback(&self, provider: &str, code: &str) -> Result<AuthResult> {
        let config = self.oauth_configs.get(provider)?;
        let token = config.exchange_code(code).await?;
        let userinfo = config.fetch_userinfo(&token).await?;
        let account = self.find_or_create_oauth_account(provider, &userinfo).await?;
        // ... 签发 JWT，同内置登录
    }
}
```

### 8.3 Axum 中间件

```rust
async fn auth_middleware(
    mut req: Request,
    next: Next,
) -> Result<Response, ServerFnError> {
    let token = req.cookies().get("jwt").and_then(|c| c.value().to_string().into());
    let auth_ctx = if let Some(token) = token {
        match jwt.verify(&token) {
            Ok(claims) => {
                let session = store.get_session(&claims.session_id).await?;
                Some(AuthContext { account_id: claims.account_id, session_id: claims.session_id })
            }
            Err(_) => None,
        }
    } else { None };
    req.extensions().insert(auth_ctx);
    Ok(next.run(req).await)
}
```

## 9. 附件存储实现

### 9.1 存储接口

附件文件本体走 `BlobStore` 门面，底层是一个 `Box<dyn ObjectStore>`（`object_store` crate 的 trait
本身就是为动态派发设计的），本地与 S3 两个后端因此共用同一条代码路径：

```rust
pub struct BlobStore {
    store: Box<DynObjectStore>,
    /// 本地后端在构造期已确保根目录存在，探活只对远端后端有意义。
    kind: BlobBackend,
}

impl BlobStore {
    pub fn from_config(cfg: &StorageConfig) -> Result<Self, AppError>;
    /// 启动探活：一次 `list_with_delimiter`（ListObjects）。任何错误——含 `NotFound`——都判为失败：
    /// `object_store` 把 HTTP 404 一律映射成 `NotFound`，而「桶不存在」返回的也是 404，
    /// 与「探针对象不存在」不可区分，所以这里的 NotFound 必须当失败。
    pub async fn health_check(&self) -> Result<(), AppError>;
    /// 从 async-graphql 给的临时文件句柄整块读入后上传（上限 50MB）。
    pub async fn put(&self, key: &str, content: std::fs::File) -> Result<(), AppError>;
    pub async fn get(&self, key: &str) -> Result<Vec<u8>, AppError>;
    pub async fn delete(&self, key: &str) -> Result<(), AppError>;
}
```

### 9.2 两个后端

| 维度 | 本地文件系统（默认） | RustFS / S3 兼容对象存储 |
|------|----------------------|--------------------------|
| 实现 | `object_store::local::LocalFileSystem` | `object_store::aws::AmazonS3Builder`（path-style，兼容 RustFS / MinIO） |
| Cargo 特性 | `object_store` 的 `fs` | `object_store` 的 `aws` |
| 落点 | 根目录 `{data_dir}/attachments` | `storage.blob.endpoint` 指向端点上、`storage.blob.bucket` 桶内 |
| 对象键 | `{workspace_id}/{entry_code}/{id}_{safe_name}` | 同左（两端逐字相同） |
| 迁移 | 目录布局与历史版本逐字一致，切到本后端时已有附件一个字都不用搬 | 把 `{data_dir}/attachments` 下的目录 `rclone copy` / `mc mirror` 进桶即可，无需迁移脚本 |
| 启动探活 | 构造期 `create_dir_all` 根目录，`health_check` 直接返回 Ok | `list_with_delimiter` 探针；端点 / 桶 / 密钥配错在启动期就带着原因失败 |

`object_store` 固定 0.12（`default-features = false`，只开 `fs`/`aws`，不开 TLS 特性，维持 rustls 单 provider 不变式）。附件元数据仍存文档后端，本节只描述文件本体。

### 9.3 附件上传

GraphQL multipart 上传通过 `async-graphql-axum` 的 `Upload` scalar 实现；服务层把上传的临时文件整块读入后交给 `BlobStore::put`：

```rust
// AttachmentService::save 节选
let attachment = Attachment::new(entry_code, entry.workspace_id, filename, content_type, size, actor);
let key = format!("{}/{}/{}_{}", attachment.workspace_id, attachment.entry_code, attachment.id, safe_name(&attachment.filename));
self.blobs.put(&key, content).await?;   // 本地后端落 {data_dir}/attachments/<key>，rustfs 后端 PUT 到桶
// 随后把 Attachment 元数据、attachments_by_entry 索引与审计日志放进同一个 write_batch
```

## 10. 权限校验

### 10.1 GraphQL Context 扩展

```rust
pub struct GraphqlContext {
    pub auth: Option<AuthContext>,
    pub services: Arc<Services>,
    pub ws_memberships: Option<Vec<(Ulid, WorkspaceRole)>>,  // 缓存
}

impl GraphqlContext {
    pub fn require_auth(&self) -> Result<&AuthContext> {
        self.auth.as_ref().ok_or(Error::Unauthorized)
    }

    pub async fn require_role(&self, ws_id: Ulid, min_role: WorkspaceRole) -> Result<()> {
        let auth = self.require_auth()?;
        let member = self.services.workspace.get_member(ws_id, auth.account_id).await?
            .ok_or(Error::Forbidden)?;
        if member.role < min_role {
            return Err(Error::Forbidden);
        }
        Ok(())
    }
}
```

### 10.2 权限矩阵

| 操作 | Owner | Maintainer | Worker | Reader |
|------|-------|------------|--------|--------|
| 删除 Workspace | ✅ | ❌ | ❌ | ❌ |
| 邀请/移除成员 | ✅ | ✅ | ❌ | ❌ |
| 修改成员角色 | ✅ | ✅ | ❌ | ❌ |
| 创建标签定义 | ✅ | ✅ | ❌ | ❌ |
| 创建 Entry | ✅ | ✅ | ✅ | ❌ |
| 修改 Entry | ✅ | ✅ | ✅ | ❌ |
| 打标签 | ✅ | ✅ | ✅ | ❌ |
| 上传附件 | ✅ | ✅ | ✅ | ❌ |
| 创建 View | ✅ | ✅ | ✅ | ❌ |
| 浏览 Entry | ✅ | ✅ | ✅ | ✅ |

## 11. 审计日志

### 11.1 审计事件定义

```rust
pub enum AuditAction {
    // Account
    AccountLogin,
    AccountLogout,
    AccountCreated,
    AccountDisabled,
    // Workspace
    WorkspaceCreated,
    WorkspaceDeleted,
    MemberInvited,
    MemberRemoved,
    RoleChanged,
    // Entry
    EntryCreated,
    EntryUpdated,
    EntryDeleted,
    // Label
    LabelSchemaCreated,
    LabelingSet,
    LabelingRemoved,
    // View
    ViewCreated,
    ViewUpdated,
    ViewDeleted,
    // Attachment
    AttachmentUploaded,
    AttachmentDeleted,
}
```

### 11.2 审计写入

每个 Service 方法的写操作在 RocksDB `WriteBatch` 中同时写入审计日志：

```rust
impl EntryService {
    pub async fn update(&self, code: &str, expected_updated_at: DateTime, input: UpdateEntryInput, actor: Ulid) -> Result<Entry> {
        let before = self.store.get_entry(code).await?;
        if before.updated_at != expected_updated_at {
            return Err(Error::ConflictDetected); // 乐观并发冲突
        }
        let after = before.apply_update(input);
        let audit = AuditLog::new(
            AuditAction::EntryUpdated,
            actor,
            "entry", code,
            before.workspace_id,
            Some(&before),
            Some(&after),
        );
        self.store.update_entry_with_audit(&after, &audit).await?;
        self.hub.broadcast(before.workspace_id, WsMessage::entry_changed(code, ChangeType::Updated, actor));
        Ok(after)
    }
}
```

## 12. 配置

```toml
# config.toml
[server]
host = "0.0.0.0"
port = 3000
base_url = "http://localhost:3000"

[auth]
session_ttl_hours = 72
max_login_attempts = 5
lockout_minutes = 15

[auth.builtin]
enabled = true
allow_registration = false  # 管理员创建账号

[auth.oauth.github]
enabled = false
client_id = ""
client_secret = ""
authorize_url = "https://github.com/login/oauth/authorize"
token_url = "https://github.com/login/oauth/access_token"
userinfo_url = "https://api.github.com/user"

[storage]
# 四个组合都用它：tantivy 全文索引始终是本地嵌入式索引（落 {data_dir}/search），
# 所以 data_dir 在四种组合下都不可省。
data_dir = "./data"

# 文档与索引元数据：rocksdb（默认，嵌入式）| postgres
[storage.doc]
backend = "rocksdb"
# backend = "postgres" 时必填。走明文 TCP（本仓库全树禁 openssl / native-tls），
# 请把数据库放在内网，或由隧道 / 反向代理终结 TLS。
# url = "postgres://rodeo:rodeo@127.0.0.1:5432/rodeo"

# 附件文件本体：local（默认，落 {data_dir}/attachments）| rustfs
[storage.blob]
backend = "local"
# backend = "rustfs" 时下面四项必填（endpoint / bucket / access_key / secret_key）。
# 两个后端共用同一套对象键 {workspace_id}/{entry_code}/{id}_{文件名}，
# 所以切换后端 = 把 {data_dir}/attachments 下的目录原样搬进桶里，没有迁移脚本。
# endpoint = "http://127.0.0.1:9000"
# bucket = "rodeo"
# access_key = "rustfsadmin"
# secret_key = "rustfsadmin"
# region = "us-east-1"
# 内网自建 RustFS 通常是明文 http，需要打开它；公网端点保持 false。
# allow_http = true
```

`storage.doc.backend` 与 `storage.blob.backend` 相互独立，故共四种组合：

| # | `storage.doc.backend` | `storage.blob.backend` | 说明 |
| --- | --- | --- | --- |
| 1 | `rocksdb` | `local` | 默认组合，单机零外部依赖，行为与历史版本一致 |
| 2 | `postgres` | `rustfs` | 文档与附件都外置 |
| 3 | `rocksdb` | `rustfs` | 文档嵌入式、附件进桶；`data_dir` 下不产生 `attachments/` |
| 4 | `postgres` | `local` | 文档外置、附件落本地文件系统 |

四种组合下 `data_dir` 都必需（tantivy 全文索引始终是本地嵌入式，落在 `{data_dir}/search`）。

## 13. 构建与部署

### 13.1 构建命令

```bash
# 开发模式（CSR + 热重载）
cargo leptos watch

# 生产构建（SSR + WASM 优化）
cargo leptos build --release
# 产物：target/release/rodeo (服务端二进制)
#       target/site/pkg/ (WASM + JS + CSS)
```

### 13.2 部署

```bash
# 单二进制部署
./rodeo --config config.toml
# 默认组合（rocksdb + local）自动在 {data_dir} 下创建 RocksDB、Tantivy 索引、附件目录
# 首次启动自动初始化内置标签和管理员账号
```

按 §12 的四种组合部署：默认组合仍是单个二进制、零外部服务；文档走 PostgreSQL 时需先建好库（`kv` 表由启动期幂等 `CREATE TABLE IF NOT EXISTS` 建出），附件走 RustFS 时需先建好桶，且启动期会对桶做一次 ListObjects 探活。无论哪种组合，`data_dir` 都必需（tantivy 索引始终本地）。PostgreSQL 连接串走**明文 TCP**（`NoTls`，见 §9 与「已知取舍」），部署时请置于内网，或由隧道 / 反向代理终结 TLS。

### 13.3 系统要求

- Linux x86_64 / aarch64（主要）, macOS（开发）
- 默认组合无外部运行时依赖（无 JVM、无 Node、无 MongoDB）；选 PostgreSQL / RustFS 组合时需自备数据库与 S3 兼容对象存储
- 推荐内存：2GB+（RocksDB cache + Tantivy reader）

## 14. 关键依赖版本

| 依赖 | 版本 | 用途 |
|------|------|------|
| leptos | 0.7+ | 全栈 WASM 框架 |
| axum | 0.8+ | HTTP 服务 |
| async-graphql | 7+ | GraphQL |
| async-graphql-axum | 7+ | GraphQL Axum 集成 |
| rocksdb | 0.9+ | 嵌入式存储 |
| tantivy | 0.22+ | 全文检索 |
| tokio | 1.x | 异步运行时 |
| serde + bincode | 1.x | 序列化 |
| jsonwebtoken | 9+ | JWT |
| argon2 | 0.5+ | 密码哈希 |
| ulid | 1.x | ID 生成 |
| dashmap | 6+ | 并发 HashMap |
| reqwest | 0.12+ | OAuth HTTP 客户端 |
