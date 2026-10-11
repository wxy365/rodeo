# 实时通知 WebSocket 设计文档

> 日期：2026-10-11
> 状态：设计已与用户确认
> 范围：在已有 `MessageService` 之上叠加 WebSocket 推送通道，覆盖 4 种通知场景（@人、新评论→创建人、删Entry→创建人、删视图→管理员）。

## 1. 背景与目标

当前通知链路是「服务端写库 + 客户端 5s `gloo_timers` polling `unreadMessages` 拉计数」。这意味着通知平均 2.5s 延迟，且持续消耗无意义的轮询请求。本轮：

1. **新增 WebSocketHub**：服务端在 `MessageService.send` 写库后立即向目标用户的活跃 WS 连接推送 `NewMessage { message_id }`；客户端收到后**主动调 GraphQL `unreadMessages` 拿最新计数 + 顶部 toast**。
2. **覆盖 4 种通知场景**：
   - 编辑器 / 评论里 @具体人 → 被@者收到消息（已有 `dispatch_mentions`，仅接入 hub）
   - Entry 有新评论 → Entry 创建人收到消息（新规则）
   - 用户删除 Entry → Entry 创建人收到消息（新规则）
   - 用户删除视图 → 视图所在 workspace 的所有 Maintainer+ 收到消息（新规则）
3. **Polling 保留为兜底**：WS 断线 / 心跳超时 / 浏览器不支持 WS 时，polling 接管；WS 重连成功后客户端主动 pull 补偿。

**非本轮（本轮不做）**

- spec §3.7 / technical_solution §6 的完整 broadcast 模型（per-workspace subscribe + `presence_update` + `view_invalidated` + edit indicator）——保留 spec 文档不动，本轮只实现通知推送子集。
- 跨实例 pub/sub 路由（Rodeo 是单进程，`services.search::meilisearch.rs` 那种独立 runtime 隔离即可；不引 Redis/NATS）。
- 离线消息补发 / push notification / 邮件 —— WS 只对在线连接生效，离线消息走 polling 拉取 + 数据库中已有的 message 记录。

## 2. 关键决策

| 决策 | 选择 | 理由 |
| --- | --- | --- |
| Hub 拓扑 | **per-user** `DashMap<Ulid /* account_id */, broadcast::Sender<WsMessage>>` | 4 种场景的接收人都在服务端现成算好（创建人 / 管理员列表），不需要 per-workspace 订阅；简化协议（无 `subscribe`/`unsubscribe` 消息）；多 tab / 多设备自然支持 |
| 推送 payload | 只推 `NewMessage { message_id: Ulid }`，**不推完整 Message 体** | 复用现有 `unreadMessages` / `recent` GraphQL 查询；避免在 WS 和 GraphQL 之间重复定义 Message 形状；避免再写一遍 PII 剥离逻辑（share 公开页已经有这条规则） |
| WS 鉴权方式 | JWT 走 query param (`/api/ws?token=...`) | `axum::extract::ws::WebSocketUpgrade` 不能带自定义 header；query param 是浏览器 `new WebSocket(url)` 唯一能传的东西；与现有 `localStorage.rodeo_jwt` 协同 |
| WS 与 polling 的关系 | WS 为主，polling 兜底；WS Connected 时暂停 5s 间隔轮询 | 断线 / 心跳超时 / 老浏览器时 polling 自动恢复；客户端不需要知道 polling 存在，只在 ws-status signal 里看到 |
| 业务侧接入点 | **收敛在 `MessageService.send` 内部**；4 个新触发点只需要新加 `messages.send` 调用 | 4 个 service 不需要直接依赖 WsHub，hub 集成零侵入散到 4 个 service；将来加新通知规则同理 |
| 心跳 | 服务端 30s 单向推 `Pong`；服务端靠「写 Pong 进 Sink 时是否 BrokenPipe」判死，不依赖客户端 ack | 浏览器 WebSocket 不能可靠地定时回包，单向推是最简实现；写失败即死亡是同步面实现的判活信号 |
| 重连策略 | 客户端指数退避 1s → 2s → 4s → 8s → 16s → 30s 封顶 | 第一次断开往往瞬时，长 backoff 反而延迟恢复 |
| 鉴权失败 | 服务端在 upgrade 前返回 401，**不开 WS** | 不能在 WS 内发「鉴权失败」消息——浏览器侧没法区分「还没收到 hello」与「已经断开」；前置拒绝更干净 |
| MessageService 是否要感知 WS | 是；在 `send` 末尾 `if let Some(hub) = &self.ws_hub { hub.push_to(recipient, NewMessage{id}) }` | 4 个新业务规则零改动；ws_hub 字段在 `Services::new` 里若 WS 禁用则设为 None，service 调用处空检查 |
| WS 是否可独立开关 | 否（v1 总是开启）；后续 spec 可加 `[ws] enabled` | 单进程单 feature，引入开关带来多一个状态；本轮不做 |

## 3. 数据模型

### 3.1 `WsMessage` 协议

```rust
// src/api/ws/message.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WsMessage {
    // S → C
    #[serde(rename = "hello")]
    Hello {
        user_id: ulid::Ulid,
        server_time: i64,            // unix seconds, 客户端用来校时
    },

    #[serde(rename = "new_message")]
    NewMessage {
        message_id: ulid::Ulid,
    },

    #[serde(rename = "pong")]
    Pong,                           // 服务端单向心跳，客户端不需要回
}
```

共 3 个变体；后续加 `ViewInvalidated` / `PresenceUpdate` 时按需 append（bincode 兼容性按 serde 的 tag 处理，无需迁移）。

### 3.2 `WsHub`

```rust
// src/api/ws/hub.rs
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

pub struct WsHub {
    /// account_id → 活跃连接们的 broadcast sender
    users: DashMap<ulid::Ulid, broadcast::Sender<WsMessage>>,
}

impl WsHub {
    pub fn new() -> Arc<Self>;

    /// 给一个 account 注册一个新连接，返回 receiver
    pub fn subscribe(&self, user_id: Ulid) -> broadcast::Receiver<WsMessage>;

    /// 连接关闭时调用，清理空的 channel
    pub fn unsubscribe(&self, user_id: Ulid);

    /// 给单个用户所有连接广播
    pub fn push_to(&self, user_id: Ulid, msg: WsMessage);
}
```

每用户 channel 容量设 `128`：单用户 1 分钟内 128 条新消息已是异常，溢出时 `send` 返回 `RecvError::Lagged(N)`，handler 选择丢弃旧消息并继续（实际等于让客户端走 polling 拉一次）。

### 3.3 `ConnectionState`

```rust
// src/api/ws/handler.rs
struct ConnectionState {
    user_id: Ulid,
}
```

v1 不需要 `last_pong` 字段——服务端判活靠「推 Pong 时 Sink 是否 BrokenPipe」（见 §5.3）。`record_pong(ts)` 接口不在 v1 暴露，留作 v2 升级路径。

## 4. 模块与文件

```
src/api/ws/
├── mod.rs           # pub use WsHub; 装载到 axum router
├── hub.rs           # WsHub 结构
├── handler.rs       # upgrade 入口 + per-connection task
└── message.rs       # WsMessage 枚举

src/frontend/
├── ws.rs            # WsContext + 建立/重连 Effect
└── (各 component 通过 use_context::<WsContext>() 订阅)

src/service/
└── message.rs       # 加 ws_hub: Option<Arc<WsHub>> 字段；send() 末尾推

src/service/
└── mod.rs           # Services::new 末尾 new WsHub().into()；同时填到 message.ws_hub
```

## 5. 协议细节

### 5.1 Upgrade 入口

```
GET /api/ws?token={jwt}     HTTP/1.1
Upgrade: websocket
Connection: Upgrade
Sec-WebSocket-Key: ...
Sec-WebSocket-Version: 13
```

服务端在 upgrade 之前：
1. 解析 `token` query param，校验 JWT（复用 `AuthService::verify_token` 或裸 jsonwebtoken 校验），失败返回 `401 Unauthorized`
2. 取 `account_id`（`AuthContext` 已有路径）
3. 接受 upgrade，spawn per-connection task

### 5.2 握手后首条消息

服务端立即 push `WsMessage::Hello { user_id, server_time }`。客户端以此确认「我连上了」。

### 5.3 心跳

**v1 单向心跳**：服务端每 30s 推 `WsMessage::Pong`；客户端**不回**任何包。服务端判活只依赖「推 Pong 时 Sink 是否 BrokenPipe」——写失败即认为连接已死，断开清理。这省去客户端心跳代码；代价是「客户端到服务端单向网络断开」服务端要等下个 30s 推 Pong 时才能发现。30s 内可接受。

**客户端行为**：`WsContext` 监听 `WsStatus`，但客户端本身不需要按 30s 心跳做任何事；客户端的判活靠 WebSocket 自身的 `onclose` 事件。

### 5.4 断线与重连

服务端 close 路径：
- 客户端断开（read 端 `None`）→ 服务端 read loop 返回，清理 unsubscribe
- 心跳写失败（`BrokenPipe`）→ 服务端 close frame，清理
- 服务器主动 close（如 user 被吊销 / 服务关闭）→ 服务端发送 close code `1001 GoingAway`

客户端重连：见 §6.3。

## 6. 客户端（WASM）集成

### 6.1 `WsContext`

```rust
// src/frontend/ws.rs
#[derive(Clone, Copy)]
pub struct WsContext {
    pub status: RwSignal<WsStatus>,
    pub last_message: RwSignal<Option<Ulid>>,   // 最近一次推送的 message_id
}

#[derive(Clone, Copy, Debug)]
pub enum WsStatus {
    Connecting,
    Connected,
    Disconnected { reason: String, next_retry_ms: u64 },
}
```

`WsContext` 通过 `provide_context` 在 `app.rs` 启动 Effect 时挂上。`WsStatus` 初始 `Connecting`，后续由 Effect 内部更新。

### 6.2 Effect 启动

```rust
// src/frontend/ws.rs (伪代码)
fn ws_connect_loop() {
    let backoff_ms = [500, 1000, 2000, 4000, 8000, 16000, 30000];
    let mut idx = 0;
    loop {
        WsStatus::set(&Connecting);
        let token = localStorage_get("rodeo_jwt")?;
        match connect_and_consume(&token).await {
            Ok(()) => { idx = 0; WsStatus::set(&Connected); }
            Err(e) => {
                let wait = backoff_ms[idx.min(backoff_ms.len() - 1)];
                WsStatus::set(&Disconnected { reason: e.to_string(), next_retry_ms: wait });
                idx += 1;
                TimeoutFuture::new(wait).await;
            }
        }
    }
}
```

`connect_and_consume(token)` 建立 `WebSocket(url_with_token)`，发回的所有 `WsMessage` 进 `WsContext::last_message` signal；同时按消息类型分发：
- `Hello` → 设 status = Connected
- `NewMessage { message_id }` → 写 `last_message`，触发 GraphQL `unreadMessages` refetch

### 6.3 收到推送后的行为

`WsContext::last_message` 变化时，`WorkspaceMain` / `AppBar` 已有 Effect 订阅 `unreadMessages`，会再次 fetch。具体：

```rust
// src/frontend/pages/workspace_main.rs (新增 Effect)
let ws = use_context::<WsContext>().unwrap();
create_effect(move |_| {
    if let Some(msg_id) = ws.last_message.get() {
        // 顶部 toast
        push_toast(i18n.t("notification.new_message"));
        // 角标 refetch (已有逻辑, 取决于服务端轮询定时器是否被 WS 暂停)
        unread_messages.refetch();
    }
});
```

### 6.4 Polling 兜底

**现状**：现有 `gloo_timers` 5s `unreadMessages` 轮询。**保留这套轮询逻辑**；本轮不在 polling 这条路径上动。

**WS 与 polling 协调**：在 `WorkspaceMain` 加一个 Effect，监听 `ws.status.get()`：
- `Connected` 时：让现有的 5s 定时器**跳过下一次** tick（用一个 `skip_next_poll: RwSignal<bool>` flag）——WS 推送触发的 `unreadMessages.refetch()` 已经把数据拉过，polling 重复 fetch 是浪费
- `Disconnected` / `Connecting` 时：定时器正常每 5s 跑一次
- flag 在 WS 触发 refetch 后立即置 `true`，下一次定时器 tick 看到 flag 置位则跳过且清旗

## 7. 服务层接入（4 个）

### 7.1 收敛点：`MessageService.send`

```rust
// src/service/message.rs (示意)
pub struct MessageService {
    store: Arc<DocStore>,
    ws_hub: Option<Arc<WsHub>>,    // 新增字段
}

impl MessageService {
    pub async fn send(&self, recipient: Ulid, kind: MessageKind, ref_id: &str, actor: Ulid, body: Option<String>) -> Result<Message, AppError> {
        let msg = self.store.write_message(recipient, kind, ref_id, actor, body)?;
        if let Some(hub) = &self.ws_hub {
            hub.push_to(recipient, WsMessage::NewMessage { message_id: msg.id });
        }
        Ok(msg)
    }
}
```

`Services::new` 末尾 `let ws_hub = WsHub::new();` 同步建好，传给 `MessageService::new_with_hub(store, ws_hub.clone())`；`Services` 结构体也持有一份 `pub ws_hub: Arc<WsHub>` 给 axum 路由握手时 subscribe 用。

### 7.2 4 个触发点

| 触发点 | 文件 / 行 | 新增调用 |
| --- | --- | --- |
| 编辑器 / 评论 @具体人 | `src/service/comment.rs:112` `dispatch_mentions` 已调 `messages.send` | 无新增——`send` 内自动跟 hub |
| Entry 新评论 → 创建人 | `src/service/comment.rs` `CommentService::create` 末尾 | 新增 `self.messages.send(entry.created_by, MessageKind::NewComment, &comment.entry_code, actor, None)?;` |
| 删除 Entry → 创建人 | `src/service/entry.rs` `EntryService::delete` 末尾 | 新增 `self.messages.send(entry.created_by, MessageKind::EntryDeleted, &entry.code, actor, None)?;` |
| 删除视图 → 管理员 | `src/service/view.rs` `ViewService::delete` 末尾 | 查 workspace owner/maintainer，逐个 `self.messages.send(admin_id, MessageKind::ViewDeleted, &view.id, actor, None)?;` |

`MessageKind` enum 在 `src/domain/message.rs` 加 3 个变种（`NewComment` / `EntryDeleted` / `ViewDeleted`），已存在的 `Mention` 沿用。

### 7.3 `MessageKind` 扩展

```rust
// src/domain/message.rs
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageKind {
    Mention,           // 已有
    NewComment,        // 新增
    EntryDeleted,      // 新增
    ViewDeleted,       // 新增
}
```

bincode 兼容性：`MessageKind` 用 `#[repr(u8)]` + 显式 discriminant（沿用现有风格），新变体 append 在末尾。

## 8. 配置

**无新增配置节**。`/api/ws` 路由总是注册；JWT 鉴权复用现有 `AuthContext` 路径；WS hub 总是启动。

将来要关 WS：加 `[ws] enabled = false` + `main.rs` 不挂载 `/api/ws`；service 端 `MessageService.ws_hub = None`。本轮不做。

## 9. 验证

每个阶段独立通过：

| Stage | 范围 | 验证 |
| --- | --- | --- |
| **v0.1** | `WsHub` + upgrade 路由 + 客户端 `WsContext` + polling 兜底；`MessageService.send` 接入 hub；已有 `dispatch_mentions` 自动通 | `make check` + `make build`；浏览器：开两个 tab，A tab 评论里 @ B，B tab 角标立即 +1（不靠 5s 轮询） |
| **v0.2** | `MessageKind::NewComment` + `CommentService.create` 末尾 push 创建人 | 浏览器：A 评论别人的 entry，entry 创建人 B 立即收到通知 |
| **v0.3** | `MessageKind::EntryDeleted` + `EntryService.delete` 末尾 push 创建人 | 浏览器：A 删 B 创建的 entry，B 收到「你的 entry 已被删除」 |
| **v0.4** | `MessageKind::ViewDeleted` + `ViewService.delete` 末尾展开 workspace 管理员列表 push | 浏览器：A 删某视图，该 workspace 的 Maintainer 们各自收到通知 |

每个 stage 完成后**单 commit**，便于回退。

测试覆盖（按 CLAUDE.md「不补新 Rust 单测」偏好，仅做浏览器 playwright 验证）：
- `~/.claude/projects/.../memory/browser-verification-setup.md` 已有的 headless playwright 模板
- 验证脚本：登录 → 新开 tab 模拟同一用户另一连接 → 另一 tab 触发场景 → 验证 WS 消息到位（通过读 DOM 角标 / 控制台日志）

## 10. 风险与回退

- **WS 库版本**：axum 0.8 自带 `axum::extract::ws`，无新增 crate 风险
- **JWT 走 query param**：会出现在 server 访问日志——`/api/ws` 路由的日志需要单独 redact `token=` query；不影响 GraphQL 路由
- **bincode 兼容**：`MessageKind` append-only，按现有 `repair_*` 习惯加 idempotent 修复函数（如有必要）
- **断线风暴**：服务端只推单向 Pong 与业务消息，没有「必须 ack」的协议层消息；客户端靠 WebSocket `onclose` 自动触发重连
- **回退路径**：v0.1 hub 关闭时 `ws_hub = None`，service 端 `send` 内 `if let Some(hub) = ...` 跳过即可，所有旧 polling 行为不变——所以每 stage 都可独立回退

## 11. 与现有 spec 的关系

`spec/technical_solution.md §6` 描述的是 spec 早期版本的完整 broadcast 模型，本 spec 是其**通知推送子集**的最小化落地。spec 文档保留不动，将来要补 `presence_update` / `view_invalidated` / `edit_indicator` 时在本 spec 的 `WsMessage` enum 上 append 变体即可，bincode / serde tag 兼容性已保证。