# 多选条目 AI 总结 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 勾选多条 Entry，一键发给大模型生成 Markdown 总结，并把总结作为一条新 Entry 建库、自动全屏打开。

**Architecture:** 服务端一份 `[ai]` 配置（base_url / api_key / model），走 Chat Completions 协议；工作空间级的「场景 / 语气」名称+提示词存进新列族 `WORKSPACE_AI`；`summarizeEntries` 变更把「组装提示词 → 调模型 → 建条目」串起来，任何失败都发生在建条目之前。前端在批量操作栏加入口，设置页加一个配置标签页。

**Tech Stack:** Rust / Leptos 0.8（SSR + hydrate）/ async-graphql 7 / RocksDB / reqwest（仅 ssr）/ gloo-net（wasm 侧 GraphQL 客户端）

**Spec:** `docs/superpowers/specs/2026-09-14-ai-entry-summary-design.md`

## Global Constraints

- **不新增后端单元测试**（用户明确要求）。每个任务的门是两条编译命令，都必须 exit 0：
  - **门 A（SSR）**：`cargo check --lib`
  - **门 B（WASM hydrate）**：`cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown`
- 前端改动另需在浏览器实测（用户已提供真实网关，见 Task 10）。
- **提交纪律**：每个任务结束提交一次，commit message 用仓库现有风格（`feat(scope): ...` / `fix(scope): ...`，祈使句英文）。
- **中文注释与中文用户可见文案**，与仓库既有风格一致；注释解释「为什么」，不解释「是什么」。
- `reqwest` 只能出现在 `ssr` feature 下。`src/lib.rs` 已经把 `service` / `api` / `domain` / `storage` / `config` / `error` 全部 `#[cfg(feature = "ssr")]` 门控，所以写在这些模块里的 `reqwest` 代码天然不会进 wasm 构建——**不要**把 AI 客户端代码放进 `src/frontend/`。
- `AuditAction` 按 bincode 变体序号编码，**本计划不新增该枚举变体**（新变体只能追加在末尾，见 `src/domain/audit.rs:31`）。AI 配置变更复用 `AuditAction::WorkspaceUpdated`，用 `resource_type = "workspace_ai"` 区分。
- `AppError` 是 `Serialize + Deserialize` 并被跨进程传递，新变体同样只追加。
- 新增配置段只改 `config.example.toml`；**`config.toml` 已被 gitignore**（`.gitignore:5`），真实密钥只写在那里，永远不进仓库。
- 协议选择：**Chat Completions**（`POST {base_url}/chat/completions`），不是 OpenAI Responses。用户实际要接的网关（自建 / vLLM / one-api / DeepSeek / 通义）走前者。

## File Structure

| 文件 | 职责 | 动作 |
|---|---|---|
| `src/config.rs` | `[ai]` 配置段与 `enabled()` 判定 | 修改 |
| `src/error.rs` | `AiNotConfigured` / `Ai(String)` 两个变体与 code 映射 | 修改 |
| `src/domain/ai.rs` | `NamedPrompt` / `WorkspaceAiConfig` 两个纯数据类型 | 新建 |
| `src/domain/mod.rs` | 注册并转出上面的类型 | 修改 |
| `src/storage/rocksdb.rs` | 新列族 `cf::WORKSPACE_AI` 与 `ALL_CFS` 注册 | 修改 |
| `src/service/ai.rs` | `AiService`（配置读写 + 提示词组装）、`AiClient`（HTTP 调用）、`parse_completion` / `derive_title` 两个纯函数 | 新建 |
| `src/service/entry.rs` | `create_with_detail`，正文与标题一次写入 | 修改 |
| `src/service/mod.rs` | 注册 `ai` 模块，`Services` 增加 `ai` 与 `ai_client` | 修改 |
| `src/api/graphql.rs` | 三个新接口 + 两个新输出类型 + 一个输入类型 | 修改 |
| `Cargo.toml` | `reqwest` 依赖（仅 ssr） | 修改 |
| `config.example.toml` | `[ai]` 示例段 | 修改 |
| `src/frontend/graphql_client.rs` | 三个客户端函数 + 两个响应类型 | 修改 |
| `src/frontend/pages/workspace_main.rs` | 批量栏「AI 总结」按钮 + 生成弹窗 | 修改 |
| `src/frontend/ai_prompt_editor.rs` | 「名称 + 提示词」行编辑器组件（场景 / 语气共用） | 新建 |
| `src/frontend/mod.rs` | 注册 `ai_prompt_editor` | 修改 |
| `src/frontend/pages/settings.rs` | 「AI 总结」标签页 | 修改 |

`src/frontend/pages/settings.rs` 已有 1152 行、`workspace_main.rs` 已有 2082 行，所以可复用且成块的编辑 UI（提示词行编辑器）单独成文件；弹窗与入口沿用这两个文件里既有的内联写法（`.dmodal` / `.dmbox` / `.batchbar`），不另起抽象。

**不新增 CSS**：全部复用 `.dmodal` / `.dmbox` / `.btn` / `.btn.pri` / `.btn.sm` / `.inp` / `.fld` / `.stack` / `.mut` / `.error` / `.set-nav` / `.it`，需要横向排布时用既有的内联 `style="display:flex;gap:8px"` 写法。

---

### Task 1: `[ai]` 配置段 + reqwest 依赖 + 两个错误变体

**Files:**
- Modify: `Cargo.toml:41`（依赖区）、`Cargo.toml:57-81`（`ssr` feature 列表）
- Modify: `config.example.toml`
- Modify: `src/config.rs`
- Modify: `src/error.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  - `Config { server, auth, storage, ai: AiConfig }`
  - `AiConfig { base_url: String, api_key: String, model: String, timeout_seconds: u64 }`，`AiConfig::default()` 与 `AiConfig::enabled() -> bool`
  - `AppError::AiNotConfigured` / `AppError::Ai(String)`，code 分别为 `AI_NOT_CONFIGURED` / `AI_ERROR`

- [ ] **Step 1: 加 reqwest 依赖（仅 ssr）**

`Cargo.toml` 的 `[dependencies]` 里，紧跟 `tantivy` 那行之后加：

```toml
# 服务端调用大模型（仅 ssr）。default-features = false 是为了不引入 openssl，
# 与仓库其余依赖一样走 rustls。
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"], optional = true }
```

`[features]` 的 `ssr = [ ... ]` 列表里，在 `"dep:tantivy",` 之后加一行：

```toml
    "dep:reqwest",
```

注意：**不要**把 `reqwest` 加进 `hydrate` 或 `[target.'cfg(target_arch = "wasm32")'.dependencies]`——wasm 侧不发这个请求，服务端代发。

- [ ] **Step 2: `config.example.toml` 加 `[ai]` 段**

在文件末尾追加：

```toml

[ai]
# 兼容 Chat Completions 协议的服务（OpenAI 官方 / 自建网关 / vLLM / one-api / DeepSeek / 通义）。
base_url = "https://api.openai.com/v1"
# api_key 或 model 留空即视为未启用：生成总结时返回「服务端未配置 AI 模型」。
api_key = ""
model = ""
timeout_seconds = 60
```

- [ ] **Step 3: `src/config.rs` 加 `AiConfig`**

`Config` 结构体加一个字段（`storage` 之后）：

```rust
    #[serde(default)]
    pub ai: AiConfig,
```

新增结构体与默认值函数（放在 `StorageConfig` 定义之后）：

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct AiConfig {
    #[serde(default = "default_ai_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_ai_timeout")]
    pub timeout_seconds: u64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            base_url: default_ai_base_url(),
            api_key: String::new(),
            model: String::new(),
            timeout_seconds: default_ai_timeout(),
        }
    }
}

impl AiConfig {
    /// 密钥与模型缺一都发不出可用请求，故两者任一为空即视为「未启用」。
    /// 前端据此收到明确的 `AI_NOT_CONFIGURED`，而不是一个费解的 HTTP 错误。
    pub fn enabled(&self) -> bool {
        !self.api_key.trim().is_empty() && !self.model.trim().is_empty()
    }
}
```

对应的默认值函数（放在文件末尾其它 `default_*` 旁边）：

```rust
fn default_ai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_ai_timeout() -> u64 {
    60
}
```

`impl Default for Config` 里补上 `ai: AiConfig::default(),`。

- [ ] **Step 4: `src/error.rs` 加两个变体**

`AppError` 枚举尾部（`Internal(String)` 之后）加：

```rust
    #[error("服务端未配置 AI 模型")]
    AiNotConfigured,
    #[error("{0}")]
    Ai(String),
```

`AppError::code()` 的 match 里对应加：

```rust
            AppError::AiNotConfigured => "AI_NOT_CONFIGURED",
            AppError::Ai(_) => "AI_ERROR",
```

- [ ] **Step 5: 跑编译门**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

`cargo check --lib` 会真的去解析并下载 `reqwest`；若这一步报「failed to select a version」，把版本放宽成 `"0.12"` 之外的最新 `0.13` 再试（本地缓存里 `reqwest-0.12.28` 与 `reqwest-0.13.2` 都在）。

- [ ] **Step 6: 确认 wasm 侧没有被牵连**

Run: `cargo tree --no-default-features --features hydrate --target wasm32-unknown-unknown -i reqwest`

期望：报 `package ID specification \`reqwest\` did not match any packages`（即 wasm 构建里根本没有 reqwest）。

- [ ] **Step 7: 提交**

```bash
git add Cargo.toml Cargo.lock config.example.toml src/config.rs src/error.rs
git commit -m "feat(config): add [ai] section, reqwest dep and AI error variants"
```

---

### Task 2: `NamedPrompt` / `WorkspaceAiConfig` + 新列族 `WORKSPACE_AI`

**Files:**
- Create: `src/domain/ai.rs`
- Modify: `src/domain/mod.rs`
- Modify: `src/storage/rocksdb.rs:38`（`cf` 模块尾）、`src/storage/rocksdb.rs:41-64`（`ALL_CFS`）

**Interfaces:**
- Consumes: 无
- Produces:
  - `crate::domain::NamedPrompt { name: String, prompt: String }`（`Clone + PartialEq + Serialize + Deserialize`）
  - `crate::domain::WorkspaceAiConfig { scenarios: Vec<NamedPrompt>, tones: Vec<NamedPrompt> }`（额外实现 `Default`）
  - `crate::storage::cf::WORKSPACE_AI`

- [ ] **Step 1: 建 `src/domain/ai.rs`**

```rust
use serde::{Deserialize, Serialize};

/// 「名称 + 提示词」条目。场景与语气共用这一种形状：名称是用户在界面上选的东西，
/// 提示词是拼进 system 消息的那段文字。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NamedPrompt {
    pub name: String,
    pub prompt: String,
}

/// 工作空间级的 AI 总结配置。整体以 bincode 存进 `cf::WORKSPACE_AI`，
/// 键是 workspace id——所以结构体一旦落过库，新增字段必须带 `#[serde(default)]`。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceAiConfig {
    pub scenarios: Vec<NamedPrompt>,
    pub tones: Vec<NamedPrompt>,
}
```

- [ ] **Step 2: 注册模块**

`src/domain/mod.rs`：`pub mod account;` 之前加 `pub mod ai;`；转出行里加

```rust
pub use ai::{NamedPrompt, WorkspaceAiConfig};
```

- [ ] **Step 3: 加列族常量并注册**

`src/storage/rocksdb.rs` 的 `pub mod cf` 里，`LABELINGS_BY_WORKSPACE` 之后加：

```rust
    /// 工作空间级 AI 配置：workspace id → `WorkspaceAiConfig`（bincode）。
    /// 与 `WORKSPACES_DELETED` / `ENTRIES_ARCHIVED` 同理，用独立列族而不是给 `Workspace`
    /// 加字段——加字段会让存量工作空间反序列化失败。
    pub const WORKSPACE_AI: &str = "workspace_ai";
```

`ALL_CFS` 数组末尾（`cf::LABELINGS_BY_WORKSPACE,` 之后）加 `cf::WORKSPACE_AI,`。

**这一步不能漏**：`DocStore::open` 用 `ALL_CFS` 建库，漏注册会在读写该列族时报「缺失 column family」而不是编译错误。

- [ ] **Step 4: 跑编译门**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

- [ ] **Step 5: 确认列族真的建出来了**

Run: `cargo run --bin rodeo -- /tmp/rodeo-cf-probe.toml` 会缺配置文件而走默认值，改用现成的开发库更省事——

```bash
ls data/                                                        # 沿用现有开发库
rm -rf /tmp/rodeo-cf-probe && cargo run --bin rodeo -- /dev/null &   # 起服务，读到默认配置
sleep 3 && kill %1 && ls /tmp/rodeo-cf-probe 2>/dev/null
```

更直接的做法：起一次服务（`make dev` 或 `cargo run`），确认启动日志里没有「缺失 column family」的报错；`data/` 下 RocksDB 会自动补上新列族（`create_missing_column_families(true)`）。

- [ ] **Step 6: 提交**

```bash
git add src/domain/ai.rs src/domain/mod.rs src/storage/rocksdb.rs
git commit -m "feat(domain): add workspace AI config types and WORKSPACE_AI column family"
```

---

### Task 3: `EntryService::create_with_detail`

**Files:**
- Modify: `src/service/entry.rs:71-107`

**Interfaces:**
- Consumes: `Entry::new(workspace_id, title, actor)`（既有）
- Produces:
  - `EntryService::create_with_detail(&self, actor: Ulid, workspace_id: Ulid, title: &str, detail: &str) -> Result<Entry, AppError>`
  - `EntryService::create(&self, actor: Ulid, workspace_id: Ulid, title: &str) -> Result<Entry, AppError>`（签名不变，改为薄封装）

**为什么要加**：现有 `create` 只收标题，`Entry::new` 把 `detail` 置为空串；而 `update` 要求 `expected_updated_at` 走乐观并发。AI 总结必须把正文与标题一次写入——先 create 再 update 会留下两条审计，还多一次没有意义的并发校验。

- [ ] **Step 1: 把 `create` 拆成薄封装 + 新的 `create_with_detail`**

把 `src/service/entry.rs` 里现有的 `pub fn create(...)` 整段替换为：

```rust
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
        self.store.write_batch(ops)?;
        self.reindex(&entry);
        Ok(entry)
    }
```

除了 `entry.detail = detail.to_string();` 这一行，其余与原来的 `create` 逐字相同。

- [ ] **Step 2: 跑编译门**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

既有调用点（`src/api/graphql.rs` 的 `create_entry` 等）不受影响，因为它们仍调 `create`。

- [ ] **Step 3: 跑既有测试确认没回归**

Run: `cargo test --lib service::entry`
期望：全绿（`create` 的既有行为一字未改；仓库里所有 `create` 断言都必须继续通过）。

注意：本任务**不新增**测试，只跑既有的——用户明确要求不写新单测。

- [ ] **Step 4: 提交**

```bash
git add src/service/entry.rs
git commit -m "feat(service): add create_with_detail for one-shot title and body writes"
```

---

### Task 4: `AiService` 数据层（配置读写 + 提示词组装）

**Files:**
- Create: `src/service/ai.rs`
- Modify: `src/service/mod.rs`

**Interfaces:**
- Consumes: `cf::WORKSPACE_AI`（Task 2）、`AppError::AiNotConfigured`（Task 1）、`AuditLog` / `audit_ops`（既有）、`strip_rich_text`（既有，`src/service/search.rs:30`）
- Produces:
  - `crate::service::ai::Prompt { instructions: String, input: String }`
  - `AiService::new(store: Arc<DocStore>) -> Self`
  - `AiService::get_config(&self, ws: Ulid) -> Result<WorkspaceAiConfig, AppError>`
  - `AiService::update_config(&self, actor: Ulid, ws: Ulid, scenarios: Vec<NamedPrompt>, tones: Vec<NamedPrompt>) -> Result<WorkspaceAiConfig, AppError>`
  - `AiService::build_prompt(&self, ws: Ulid, codes: &[String], scenario: Option<&str>, tone: Option<&str>) -> Result<Prompt, AppError>`
  - `Services { ..., ai: AiService }`

- [ ] **Step 1: 建 `src/service/ai.rs` 的数据层**

```rust
use std::sync::Arc;

use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, Entry, Labeling, NamedPrompt, WorkspaceAiConfig};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::service::search::strip_rich_text;
use crate::storage::{cf, BatchOp, DocStore};

/// 内置基础模板。输入形态与输出形态都在这里钉死：模型必须只吐 Markdown 正文，
/// 否则 §8 的标题抽取会退化成「第一行是一句客套话」。
const BASE_INSTRUCTIONS: &str = "你会收到若干条任务/问题跟踪条目（含编码、标题、标签与详情）。\
请阅读全部条目，输出一份结构化的 Markdown 总结，建议包含：概览、关键要点、风险与待办。\
只输出 Markdown 正文本身，不要任何前言、结语或代码块包裹。";

/// 待发送给模型的提示词：system 消息与 user 消息各一条。
pub struct Prompt {
    pub instructions: String,
    pub input: String,
}

pub struct AiService {
    store: Arc<DocStore>,
}

impl AiService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    /// 没配置过就是默认值（空列表）——空配置是合法状态，不该报错。
    pub fn get_config(&self, ws: Ulid) -> Result<WorkspaceAiConfig, AppError> {
        Ok(self
            .store
            .get::<WorkspaceAiConfig>(cf::WORKSPACE_AI, &ws.to_bytes())?
            .unwrap_or_default())
    }

    /// 整体替换场景与语气。审计复用 `WorkspaceUpdated` + resource_type "workspace_ai"：
    /// `AuditAction` 按 bincode 变体序号编码，不值得为一个配置项新增变体。
    pub fn update_config(
        &self,
        actor: Ulid,
        ws: Ulid,
        scenarios: Vec<NamedPrompt>,
        tones: Vec<NamedPrompt>,
    ) -> Result<WorkspaceAiConfig, AppError> {
        let before = serde_json::to_string(&self.get_config(ws)?).unwrap_or_default();
        let cfg = WorkspaceAiConfig {
            scenarios: normalize(scenarios, "场景")?,
            tones: normalize(tones, "语气")?,
        };
        let audit = AuditLog::new(
            AuditAction::WorkspaceUpdated,
            actor,
            "workspace_ai",
            &ws.to_string(),
            Some(ws),
            Some(before),
            Some(serde_json::to_string(&cfg).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(cf::WORKSPACE_AI, ws.to_bytes().to_vec(), &cfg)?);
        self.store.write_batch(ops)?;
        Ok(cfg)
    }

    /// 同步：把选中的条目与选中的场景/语气组装成待发送的提示词。
    /// 越权、已删除、已归档的条目在这里就被拒掉——调用方因此可以在发请求与建条目之前
    /// 拿到确定的结果，不会留下半成品。
    pub fn build_prompt(
        &self,
        ws: Ulid,
        codes: &[String],
        scenario: Option<&str>,
        tone: Option<&str>,
    ) -> Result<Prompt, AppError> {
        if codes.is_empty() {
            return Err(AppError::InvalidQuery("未选择任何条目".to_string()));
        }
        let cfg = self.get_config(ws)?;
        let mut instructions = BASE_INSTRUCTIONS.to_string();
        if let Some(p) = pick_prompt(&cfg.scenarios, scenario, "场景")? {
            instructions.push_str("\n\n");
            instructions.push_str(p);
        }
        if let Some(p) = pick_prompt(&cfg.tones, tone, "语气")? {
            instructions.push_str("\n\n");
            instructions.push_str(p);
        }

        let mut blocks = Vec::with_capacity(codes.len());
        for code in codes {
            let entry = self.entry_in(ws, code)?;
            blocks.push(render_entry_block(&entry, &self.labelings_of(code)?));
        }
        Ok(Prompt {
            instructions,
            input: blocks.join("\n\n---\n\n"),
        })
    }

    /// 取条目并校验归属与状态：跨工作空间、已删除、已归档都不可总结。
    fn entry_in(&self, ws: Ulid, code: &str) -> Result<Entry, AppError> {
        let entry = self
            .store
            .get::<Entry>(cf::ENTRIES, code.as_bytes())?
            .ok_or(AppError::NotFound)?;
        if entry.workspace_id != ws || entry.is_deleted() {
            return Err(AppError::NotFound);
        }
        if self
            .store
            .get_raw(cf::ENTRIES_ARCHIVED, code.as_bytes())?
            .is_some()
        {
            return Err(AppError::InvalidQuery(format!("条目已归档: {code}")));
        }
        Ok(entry)
    }

    /// 标签键是「条目编码 + 标签名」直接拼接，没有分隔符，所以前缀扫描可能捞到前缀相同的
    /// 其它条目（如 ABC 与 ABCX），无法只靠键区分——必须按值里的 entry_code 精确比对。
    fn labelings_of(&self, code: &str) -> Result<Vec<Labeling>, AppError> {
        let mut out = Vec::new();
        for (_, v) in self.store.scan_prefix(cf::LABELINGS, code.as_bytes())? {
            let l: Labeling = bincode::deserialize(&v)?;
            if l.entry_code == code {
                out.push(l);
            }
        }
        Ok(out)
    }
}

/// 名称去空白后为空的行直接丢弃（用户加了行又没填）。名称是界面上的唯一标识，不能为空；
/// 提示词允许为空——等价于这条没配。
fn normalize(list: Vec<NamedPrompt>, kind: &str) -> Result<Vec<NamedPrompt>, AppError> {
    let mut out = Vec::with_capacity(list.len());
    for p in list {
        let name = p.name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::InvalidQuery(format!("{kind}名称不能为空")));
        }
        out.push(NamedPrompt {
            name,
            prompt: p.prompt.trim().to_string(),
        });
    }
    Ok(out)
}

/// 未选（None / 空串）返回 None；选了但配置里找不到就报错——静默忽略会让用户以为
/// 场景生效了，实际没有。命中了但提示词为空，等价于没选。
fn pick_prompt<'a>(
    list: &'a [NamedPrompt],
    chosen: Option<&str>,
    kind: &str,
) -> Result<Option<&'a str>, AppError> {
    let Some(name) = chosen.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let hit = list
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| AppError::InvalidQuery(format!("{kind}不存在: {name}")))?;
    if hit.prompt.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(hit.prompt.as_str()))
}

/// 每条条目一段：编码、标题、标签、正文纯文本。正文复用检索侧的富文本剥离，
/// 免得把编辑器内部的 JSON 结构喂给模型。
fn render_entry_block(entry: &Entry, labels: &[Labeling]) -> String {
    let mut out = format!("【{}】{}\n", entry.code, entry.title);
    if !labels.is_empty() {
        let mut parts: Vec<String> = labels.iter().map(render_label).collect();
        parts.sort();
        out.push_str("标签：");
        out.push_str(&parts.join("、"));
        out.push('\n');
    }
    let body = strip_rich_text(&entry.detail);
    if !body.trim().is_empty() {
        out.push_str(&body);
    }
    out
}

fn render_label(l: &Labeling) -> String {
    match l.value.to_json() {
        serde_json::Value::Null => l.label_name.clone(),
        serde_json::Value::Array(items) => format!(
            "{}={}",
            l.label_name,
            items.iter().map(scalar_text).collect::<Vec<_>>().join("/")
        ),
        other => format!("{}={}", l.label_name, scalar_text(&other)),
    }
}

fn scalar_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
```

- [ ] **Step 2: 在 `Services` 里聚合**

`src/service/mod.rs`：模块列表加 `pub mod ai;`（保持字母序，放 `pub mod audit;` 之前或之后都行，按现有顺序插在 `pub mod audit;` 之后即可），转出行加 `pub use ai::AiService;`。

`Services` 结构体加字段：

```rust
    pub ai: AiService,
```

`Services::new` 里初始化：

```rust
            ai: AiService::new(store.clone()),
```

- [ ] **Step 3: 跑编译门**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

- [ ] **Step 4: 提交**

```bash
git add src/service/ai.rs src/service/mod.rs
git commit -m "feat(service): add AiService with workspace config and prompt assembly"
```

---

### Task 5: `AiClient`（Chat Completions）+ 响应解析 + 标题推导

**Files:**
- Modify: `src/service/ai.rs`（追加）
- Modify: `src/service/mod.rs`（`ai_client` 字段）

**Interfaces:**
- Consumes: `Prompt`、`AiConfig`（Task 1）、`AppError::Ai`（Task 1）
- Produces:
  - `AiClient::from_config(cfg: &AiConfig) -> Result<Option<AiClient>, AppError>`（未启用返回 `Ok(None)`）
  - `AiClient::complete(&self, prompt: &Prompt) -> Result<String, AppError>`
  - `crate::service::ai::parse_completion(text: &str) -> Result<String, AppError>`
  - `crate::service::ai::derive_title(markdown: &str, count: usize) -> String`
  - `Services { ..., ai_client: Option<AiClient> }`

- [ ] **Step 1: 在 `src/service/ai.rs` 追加客户端**

在 `AiService` 的 `impl` 之后追加（`normalize` 等自由函数之前的位置都可以）：

```rust
pub struct AiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl AiClient {
    /// 未配置密钥或模型时返回 `None`：调用方据此报 `AiNotConfigured`，
    /// 而不是发一个注定 401 的请求。
    ///
    /// 客户端只在这里建一次（挂在 `Services` 上），连接池与超时因此跨请求复用。
    pub fn from_config(cfg: &crate::config::AiConfig) -> Result<Option<Self>, AppError> {
        if !cfg.enabled() {
            return Ok(None);
        }
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.timeout_seconds))
            .build()
            .map_err(|e| AppError::Ai(format!("HTTP 客户端初始化失败: {e}")))?;
        Ok(Some(Self {
            http,
            // 去掉末尾斜杠，免得拼出 `//chat/completions`——不少网关对此直接 404。
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
        }))
    }

    /// 一次性返回（不做流式）。失败一律落在 `AppError::Ai` 上，带上尽量可读的上游信息。
    pub async fn complete(&self, prompt: &Prompt) -> Result<String, AppError> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = serde_json::json!({
            "model": self.model,
            "messages": [
                { "role": "system", "content": prompt.instructions },
                { "role": "user", "content": prompt.input },
            ],
        });
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Ai(format!("调用模型失败: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AppError::Ai(format!("读取模型响应失败: {e}")))?;
        if !status.is_success() {
            return Err(AppError::Ai(format!(
                "模型返回 {status}: {}",
                upstream_error(&text)
            )));
        }
        parse_completion(&text)
    }
}

/// 解析 Chat Completions 响应：取 `choices[0].message.content`。
/// 顶层 `error` 非空、`choices` 缺失或为空、内容为 null/空白，都按失败处理——
/// 宁可报错，也不要把空串当总结建出一条空条目。
fn parse_completion(text: &str) -> Result<String, AppError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| AppError::Ai("模型响应不是合法 JSON".to_string()))?;
    if let Some(msg) = v.get("error").and_then(error_message) {
        return Err(AppError::Ai(msg));
    }
    let content = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Ai("模型没有返回内容".to_string()))?;
    Ok(content.to_string())
}

fn error_message(err: &serde_json::Value) -> Option<String> {
    err.get("message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .or_else(|| err.as_str().map(str::to_string))
}

/// 从错误响应体里尽量抠出可读信息；抠不出就退回原文截断。
fn upstream_error(text: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(m) = v.get("error").and_then(error_message) {
            return m;
        }
    }
    truncate_chars(text.trim(), 200)
}

/// 标题抽取：优先取第一个 Markdown 标题行，其次取首个非空行的前 40 个字符，
/// 都没有就兜底「AI 总结（N 条）」。按字符截断而不是按字节——标题基本是中文。
pub fn derive_title(markdown: &str, count: usize) -> String {
    for line in markdown.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix('#') else {
            continue;
        };
        let title = rest.trim_start_matches('#').trim().trim_end_matches('#').trim();
        if !title.is_empty() {
            return title.to_string();
        }
    }
    if let Some(line) = markdown.lines().map(str::trim).find(|l| !l.is_empty()) {
        return truncate_chars(line, 40);
    }
    format!("AI 总结（{count} 条）")
}

/// 按字符截断，超出时补省略号，避免截出来看不出被截过。
fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}
```

- [ ] **Step 2: 把客户端挂到 `Services`**

`src/service/mod.rs`：`Services` 加字段

```rust
    pub ai_client: Option<AiClient>,
```

转出行加 `pub use ai::{AiClient, AiService};`。

`Services::new` 里，在构造 `Self { ... }` **之前**建客户端（`config` 随后会被移动进结构体）：

```rust
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Result<Self, AppError> {
        let search = Arc::new(SearchIndex::open(&format!("{}/search", config.data_dir()))?);
        // 未配置就保持 None，启动阶段不做任何网络操作。
        let ai_client = AiClient::from_config(&config.ai)?;
        let services = Self {
            auth: AuthService::new(store.clone(), config.clone()),
            workspace: WorkspaceService::new(store.clone()),
            entry: EntryService::with_search(store.clone(), search.clone()),
            label: LabelService::new(store.clone()),
            audit: AuditService::new(store.clone()),
            view: ViewService::new(store.clone()),
            ai: AiService::new(store.clone()),
            ai_client,
            search,
            store,
            config,
        };
        services.search.backfill(&services.store)?;
        services.entry.labelings_by_workspace_backfill(&services.store)?;
        Ok(services)
    }
```

- [ ] **Step 3: 跑编译门**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

- [ ] **Step 4: 提交**

```bash
git add src/service/ai.rs src/service/mod.rs
git commit -m "feat(service): add AiClient speaking Chat Completions with title derivation"
```

---

### Task 6: GraphQL 三个接口

**Files:**
- Modify: `src/api/graphql.rs`

**Interfaces:**
- Consumes: `AiService` / `AiClient`（Task 4、5）、`derive_title`（Task 5）、`create_with_detail`（Task 3）、`NamedPrompt` / `WorkspaceAiConfig`（Task 2）
- Produces:
  - 查询 `workspaceAiConfig(workspaceId: ID!): GqlWorkspaceAiConfig!`
  - 变更 `updateWorkspaceAiConfig(workspaceId: ID!, scenarios: [NamedPromptInput!]!, tones: [NamedPromptInput!]!): GqlWorkspaceAiConfig!`
  - 变更 `summarizeEntries(workspaceId: ID!, codes: [String!]!, scenario: String, tone: String): GqlEntry!`
  - 输出类型 `GqlNamedPrompt { name, prompt }`、`GqlWorkspaceAiConfig { scenarios, tones }`；输入类型 `NamedPromptInput { name, prompt }`

- [ ] **Step 1: 加输出与输入类型**

在 `src/api/graphql.rs` 的 `GqlLabeling` 实现之后、`GqlEntry` 定义之前插入：

```rust
#[derive(SimpleObject, Clone)]
pub struct GqlNamedPrompt {
    name: String,
    prompt: String,
}

impl From<NamedPrompt> for GqlNamedPrompt {
    fn from(p: NamedPrompt) -> Self {
        Self {
            name: p.name,
            prompt: p.prompt,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlWorkspaceAiConfig {
    scenarios: Vec<GqlNamedPrompt>,
    tones: Vec<GqlNamedPrompt>,
}

impl From<WorkspaceAiConfig> for GqlWorkspaceAiConfig {
    fn from(c: WorkspaceAiConfig) -> Self {
        Self {
            scenarios: c.scenarios.into_iter().map(Into::into).collect(),
            tones: c.tones.into_iter().map(Into::into).collect(),
        }
    }
}

/// 「名称 + 提示词」输入行。整体替换语义：提交什么就是什么。
#[derive(async_graphql::InputObject)]
pub struct NamedPromptInput {
    name: String,
    prompt: String,
}

impl NamedPromptInput {
    fn into_named(self) -> NamedPrompt {
        NamedPrompt {
            name: self.name,
            prompt: self.prompt,
        }
    }
}
```

文件顶部的 `use crate::domain::{...}` 里补上 `NamedPrompt, WorkspaceAiConfig`；再补一行

```rust
use crate::service::ai::derive_title;
```

- [ ] **Step 2: 加查询**

`#[Object] impl Query` 里，`label_schemas` 之后加：

```rust
    /// 场景 / 语气配置。成员即可读（与 `labelSchemas` 一致）——生成弹窗要用它渲染下拉框。
    async fn workspace_ai_config(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
    ) -> GqlResult<GqlWorkspaceAiConfig> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        Ok(gql.services.ai.get_config(ws_id)?.into())
    }
```

- [ ] **Step 3: 加两个变更**

`#[Object] impl Mutation` 里，`create_entry` 之后加：

```rust
    /// 整体替换场景与语气：Maintainer 及以上（与标签管理一致）。
    async fn update_workspace_ai_config(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        scenarios: Vec<NamedPromptInput>,
        tones: Vec<NamedPromptInput>,
    ) -> GqlResult<GqlWorkspaceAiConfig> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let cfg = gql.services.ai.update_config(
            auth.account_id,
            ws_id,
            scenarios.into_iter().map(NamedPromptInput::into_named).collect(),
            tones.into_iter().map(NamedPromptInput::into_named).collect(),
        )?;
        Ok(cfg.into())
    }

    /// 生成总结并新建条目（会写 Entry，故要 Worker 及以上）。
    ///
    /// 顺序是有意的：先确认模型配置存在（最快、最可操作的错误），再组装提示词
    /// （这一步会校验选中条目的归属与状态），最后才发请求、建条目。
    /// 任何失败都发生在 `create_with_detail` 之前，不会留下半成品条目。
    async fn summarize_entries(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        codes: Vec<String>,
        scenario: Option<String>,
        tone: Option<String>,
    ) -> GqlResult<GqlEntry> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Worker)?;

        let client = gql
            .services
            .ai_client
            .as_ref()
            .ok_or(AppError::AiNotConfigured)?;
        let prompt = gql
            .services
            .ai
            .build_prompt(ws_id, &codes, scenario.as_deref(), tone.as_deref())?;
        let summary = client.complete(&prompt).await?;

        let title = derive_title(&summary, codes.len());
        let entry =
            gql.services
                .entry
                .create_with_detail(auth.account_id, ws_id, &title, &summary)?;
        gql_entry(gql, entry, vec![])
    }
```

- [ ] **Step 4: 跑编译门**

Run: `cargo check --lib` → 期望 exit 0（`GqlResult` 与 `AppError` 之间的 `?` 依赖既有的 `From<AppError>` 实现，仓库里已经这么用了）
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

- [ ] **Step 5: 起服务确认三个接口进了 schema**

```bash
make dev &            # 或 cargo run
sleep 20
curl -s http://localhost:3000/api/graphql \
  -H 'Content-Type: application/json' \
  -d '{"query":"{ __schema { queryType { fields { name } } mutationType { fields { name } } } }"}' \
  | grep -o 'workspaceAiConfig\|updateWorkspaceAiConfig\|summarizeEntries'
```

期望：三行输出各出现一次。

- [ ] **Step 6: 提交**

```bash
git add src/api/graphql.rs
git commit -m "feat(api): expose workspaceAiConfig and summarizeEntries over GraphQL"
```

---

### Task 7: 前端 GraphQL 客户端函数

**Files:**
- Modify: `src/frontend/graphql_client.rs`

**Interfaces:**
- Consumes: 既有 `graphql()`、`ENTRY_FIELDS`、`Entry`
- Produces:
  - `pub struct NamedPrompt { name: String, prompt: String }`（`Clone + PartialEq + Deserialize`）
  - `pub struct WorkspaceAiConfig { scenarios: Vec<NamedPrompt>, tones: Vec<NamedPrompt> }`（额外 `Default`）
  - `workspace_ai_config(workspace_id: &str) -> Result<WorkspaceAiConfig, String>`
  - `update_workspace_ai_config(workspace_id: &str, scenarios: &Value, tones: &Value) -> Result<WorkspaceAiConfig, String>`
  - `summarize_entries(workspace_id: &str, codes: &[String], scenario: Option<&str>, tone: Option<&str>) -> Result<Entry, String>`

- [ ] **Step 1: 加响应类型**

在 `pub struct Entry { ... }` 之前（响应类型区）加：

```rust
/// 「名称 + 提示词」行。字段都是单词，不需要 rename。
#[derive(Clone, serde::Deserialize, PartialEq)]
pub struct NamedPrompt {
    pub name: String,
    pub prompt: String,
}

#[derive(Clone, serde::Deserialize, Default, PartialEq)]
pub struct WorkspaceAiConfig {
    pub scenarios: Vec<NamedPrompt>,
    pub tones: Vec<NamedPrompt>,
}
```

- [ ] **Step 2: 加三个请求函数**

在 `pub async fn label_schemas(...)` 附近加（`NamedPrompt` 字段只请求 `name prompt`，避免前端类型跟着服务端结构漂移）：

```rust
const AI_CONFIG_FIELDS: &str = "scenarios { name prompt } tones { name prompt }";

pub async fn workspace_ai_config(workspace_id: &str) -> Result<WorkspaceAiConfig, String> {
    let q = format!(
        "query($id: ID!) {{ workspaceAiConfig(workspaceId: $id) {{ {AI_CONFIG_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("workspaceAiConfig").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// `scenarios` / `tones` 是 `[{name, prompt}]` 数组；整体替换语义。
pub async fn update_workspace_ai_config(
    workspace_id: &str,
    scenarios: &Value,
    tones: &Value,
) -> Result<WorkspaceAiConfig, String> {
    let q = format!(
        "mutation($id: ID!, $s: [NamedPromptInput!]!, $t: [NamedPromptInput!]!) {{ \
         updateWorkspaceAiConfig(workspaceId: $id, scenarios: $s, tones: $t) {{ {AI_CONFIG_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({ "id": workspace_id, "s": scenarios, "t": tones }),
    )
    .await?;
    serde_json::from_value(data.get("updateWorkspaceAiConfig").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 生成总结并新建条目，返回那条新条目（调用方据此直接全屏打开）。
/// 生成可能要几十秒，服务端超时由 `[ai] timeout_seconds` 控制。
pub async fn summarize_entries(
    workspace_id: &str,
    codes: &[String],
    scenario: Option<&str>,
    tone: Option<&str>,
) -> Result<Entry, String> {
    let q = format!(
        "mutation($id: ID!, $c: [String!]!, $s: String, $t: String) {{ \
         summarizeEntries(workspaceId: $id, codes: $c, scenario: $s, tone: $t) {{ {ENTRY_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({ "id": workspace_id, "c": codes, "s": scenario, "t": tone }),
    )
    .await?;
    serde_json::from_value(data.get("summarizeEntries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}
```

- [ ] **Step 3: 跑编译门**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

- [ ] **Step 4: 提交**

```bash
git add src/frontend/graphql_client.rs
git commit -m "feat(frontend): add AI summary GraphQL client calls"
```

---

### Task 8: 批量栏「AI 总结」入口 + 生成弹窗

**Files:**
- Modify: `src/frontend/pages/workspace_main.rs`

**Interfaces:**
- Consumes: `workspace_ai_config` / `summarize_entries` / `NamedPrompt`（Task 7）
- Produces: 无（终端 UI 改动）

- [ ] **Step 1: 补 import**

`use crate::frontend::graphql_client::{...}` 的列表里加 `summarize_entries, workspace_ai_config, NamedPrompt`（保持字母序，与现有列表一致）。

- [ ] **Step 2: 加状态与两个处理函数**

在 `let batch_busy = RwSignal::new(false);` 之后（`// ---- 归档 ----` 之前）插入：

```rust
    // ---- AI 总结 ----
    // 场景 / 语气下拉数据在打开弹窗时才拉，和「已归档」弹窗一个路数：
    // 没打开过的用户不该为这个功能付一次请求。
    let show_ai = RwSignal::new(false);
    let ai_loading = RwSignal::new(false);
    let ai_scenarios = RwSignal::new(Vec::<NamedPrompt>::new());
    let ai_tones = RwSignal::new(Vec::<NamedPrompt>::new());
    let ai_scenario = RwSignal::new(String::new());
    let ai_tone = RwSignal::new(String::new());
    let ai_busy = RwSignal::new(false);
    let ai_error = RwSignal::new(None::<String>);
```

在 `apply_batch` 闭包之后插入两个处理函数：

```rust
    let open_ai = move |_| {
        show_ai.set(true);
        ai_error.set(None);
        ai_scenario.set(String::new());
        ai_tone.set(String::new());
        let Some(ws_id) = data.get_untracked().and_then(|r| r.ok()).map(|(w, _, _)| w.id) else {
            return;
        };
        ai_loading.set(true);
        spawn_local(async move {
            match workspace_ai_config(&ws_id).await {
                Ok(cfg) => {
                    ai_scenarios.set(cfg.scenarios);
                    ai_tones.set(cfg.tones);
                }
                Err(e) => ai_error.set(Some(e)),
            }
            ai_loading.set(false);
        });
    };

    let apply_ai = move |_| {
        let codes = batch_selected.get_untracked();
        if codes.is_empty() {
            return;
        }
        let Some(ws_id) = data.get_untracked().and_then(|r| r.ok()).map(|(w, _, _)| w.id) else {
            return;
        };
        let scenario = ai_scenario.get_untracked();
        let tone = ai_tone.get_untracked();
        ai_busy.set(true);
        ai_error.set(None);
        spawn_local(async move {
            let result = summarize_entries(
                &ws_id,
                &codes,
                (!scenario.is_empty()).then_some(scenario.as_str()),
                (!tone.is_empty()).then_some(tone.as_str()),
            )
            .await;
            match result {
                Ok(created) => {
                    ai_busy.set(false);
                    show_ai.set(false);
                    batch_selected.set(Vec::new());
                    // 生成的是新条目：直接选中并全屏打开，用户不必再去列表里翻。
                    // 用 created.code 而不是等列表重查后再定位——列表分页位置不可预测。
                    selected.set(created.code.clone());
                    fullscreen.set(true);
                    refresh.update(|n| *n += 1);
                }
                Err(e) => {
                    ai_busy.set(false);
                    ai_error.set(Some(e));
                }
            }
        });
    };
```

- [ ] **Step 3: 批量栏加按钮**

`.batchbar` 里「批量设置标签」之后加：

```rust
                                        <button class="btn sm" disabled=move || batch_busy.get() || ai_busy.get()
                                            on:click=open_ai>"AI 总结"</button>
```

- [ ] **Step 4: 加弹窗**

在 `show_batch` 那个弹窗块之后插入：

```rust
            {move || show_ai.get().then(|| view! {
                <div class="dmodal" on:click=move |_| show_ai.set(false)>
                    <div class="panel dmbox" style="max-width:520px" on:click=|ev| ev.stop_propagation()>
                        <h3>"AI 总结"</h3>
                        <p class="mut">{move || format!(
                            "把选中的 {} 个条目交给模型，生成一条新条目并全屏打开。",
                            batch_selected.get().len()
                        )}</p>
                        {move || if ai_loading.get() {
                            view! { <p class="mut">"正在读取场景与语气…"</p> }.into_any()
                        } else {
                            let scenarios = ai_scenarios.get();
                            let tones = ai_tones.get();
                            let none_configured = scenarios.is_empty() && tones.is_empty();
                            view! {
                                <div class="stack">
                                    {none_configured.then(|| view! {
                                        <p class="mut">"尚未配置场景与语气；不指定也可以直接生成，配置入口在工作空间设置页。"</p>
                                    })}
                                    <label class="fld">
                                        <span>"场景"</span>
                                        <select class="inp" prop:value=move || ai_scenario.get()
                                            on:change=move |ev| ai_scenario.set(event_target_value(&ev))>
                                            <option value="">"（不指定）"</option>
                                            {scenarios.into_iter().map(|p| {
                                                let v = p.name.clone();
                                                view! { <option value=v>{p.name}</option> }
                                            }).collect::<Vec<_>>()}
                                        </select>
                                    </label>
                                    <label class="fld">
                                        <span>"语气"</span>
                                        <select class="inp" prop:value=move || ai_tone.get()
                                            on:change=move |ev| ai_tone.set(event_target_value(&ev))>
                                            <option value="">"（不指定）"</option>
                                            {tones.into_iter().map(|p| {
                                                let v = p.name.clone();
                                                view! { <option value=v>{p.name}</option> }
                                            }).collect::<Vec<_>>()}
                                        </select>
                                    </label>
                                </div>
                            }.into_any()
                        }}
                        {move || ai_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <div style="display:flex;gap:8px;justify-content:flex-end">
                            <button class="btn" on:click=move |_| show_ai.set(false)>"取消"</button>
                            <button class="btn pri" disabled=move || ai_busy.get() || ai_loading.get()
                                on:click=apply_ai>
                                {move || if ai_busy.get() { "生成中…" } else { "生成" }}
                            </button>
                        </div>
                    </div>
                </div>
            })}
```

- [ ] **Step 5: 跑编译门 + 浏览器实测未配置路径**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

然后 `make dev`，在浏览器里：

1. 进任一工作空间，勾选 2 条条目 → 批量栏出现「AI 总结」。
2. 点开弹窗：`config.toml` 里 `api_key` 为空，故应看到「尚未配置场景与语气…」，两个下拉只有「（不指定）」。
3. 点「生成」：应显示「服务端未配置 AI 模型」，**弹窗不关闭**，控制台无 page error。
4. 关掉弹窗，确认列表条目数没变（没有偷偷建出一条空条目）。

用 headless Chromium 走同一套断言也可以，选择器沿用既有约定：`.batchbar button`（文案「AI 总结」）、`.dmodal .dmbox`、`.dmbox .error`。

- [ ] **Step 6: 提交**

```bash
git add src/frontend/pages/workspace_main.rs
git commit -m "feat(frontend): add AI summary entry and dialog to the batch bar"
```

---

### Task 9: 设置页「AI 总结」标签页

**Files:**
- Create: `src/frontend/ai_prompt_editor.rs`
- Modify: `src/frontend/mod.rs:1-8`（模块列表）
- Modify: `src/frontend/pages/settings.rs`

**Interfaces:**
- Consumes: `workspace_ai_config` / `update_workspace_ai_config` / `NamedPrompt`（Task 7）
- Produces:
  - `crate::frontend::ai_prompt_editor::PromptRow`
  - `rows_from(&[NamedPrompt]) -> Vec<PromptRow>` / `rows_to_value(&RwSignal<Vec<PromptRow>>) -> Vec<Value>`
  - 组件 `PromptRows(rows: RwSignal<Vec<PromptRow>>, placeholder: String)`

- [ ] **Step 1: 建编辑行组件 `src/frontend/ai_prompt_editor.rs`**

```rust
use leptos::prelude::*;

use crate::frontend::graphql_client::NamedPrompt;

/// 一行「名称 + 提示词」。两个字段各自独立更新，和 `workspace_main.rs` 里的
/// `DraftLabel` 同一路数：RwSignal 字段让单行可以就地改，不必整表重建。
#[derive(Clone, Copy)]
pub struct PromptRow {
    pub name: RwSignal<String>,
    pub prompt: RwSignal<String>,
}

impl PromptRow {
    pub fn new(name: String, prompt: String) -> Self {
        Self {
            name: RwSignal::new(name),
            prompt: RwSignal::new(prompt),
        }
    }

    /// 名称去空白后为空的行会被丢掉：空行等于用户加了行又没填。
    /// 提示词允许为空——服务端把它当作「这条没配」。
    pub fn to_value(&self) -> Option<serde_json::Value> {
        let name = self.name.get_untracked().trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some(serde_json::json!({
            "name": name,
            "prompt": self.prompt.get_untracked(),
        }))
    }
}

pub fn rows_from(list: &[NamedPrompt]) -> Vec<PromptRow> {
    list.iter()
        .map(|p| PromptRow::new(p.name.clone(), p.prompt.clone()))
        .collect()
}

pub fn rows_to_value(rows: &RwSignal<Vec<PromptRow>>) -> Vec<serde_json::Value> {
    rows.get_untracked()
        .iter()
        .filter_map(PromptRow::to_value)
        .collect()
}

/// 一组可增删的「名称 + 提示词」行，场景与语气共用。
#[component]
pub fn PromptRows(rows: RwSignal<Vec<PromptRow>>, placeholder: String) -> impl IntoView {
    view! {
        <div class="stack">
            {move || {
                rows.get()
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| {
                        view! {
                            <div style="display:flex;gap:8px;align-items:flex-start">
                                <input class="inp" style="width:160px" placeholder=placeholder.clone()
                                    prop:value=move || r.name.get()
                                    on:input=move |ev| r.name.set(event_target_value(&ev)) />
                                <textarea class="inp" rows="2" placeholder="提示词"
                                    prop:value=move || r.prompt.get()
                                    on:input=move |ev| r.prompt.set(event_target_value(&ev))></textarea>
                                <button class="btn sm" type="button"
                                    on:click=move |_| rows.update(|v| { if i < v.len() { v.remove(i); } })>
                                    "删除"
                                </button>
                            </div>
                        }
                    })
                    .collect::<Vec<_>>()
            }}
            <button class="btn sm" type="button" style="align-self:flex-start"
                on:click=move |_| rows.update(|v| v.push(PromptRow::new(String::new(), String::new())))>
                "添加一行"
            </button>
        </div>
    }
}
```

`src/frontend/mod.rs` 的模块列表加 `pub mod ai_prompt_editor;`（放 `pub mod components;` 之前，保持字母序）。

- [ ] **Step 2: 设置页补 import**

`src/frontend/pages/settings.rs`：

```rust
use crate::frontend::ai_prompt_editor::{rows_from, rows_to_value, PromptRow, PromptRows};
use crate::frontend::icons::{..., ic_comment};
```

（`ic_comment` 追进既有的 `use crate::frontend::icons::{...}` 列表。）

`use crate::frontend::graphql_client::{...}` 列表里加 `update_workspace_ai_config, workspace_ai_config`。

- [ ] **Step 3: 加状态与处理函数**

在 `let invite_role = RwSignal::new(String::from("worker"));` 之后加：

```rust
    // ---- AI 总结配置 ----
    // 打开标签页时才拉数据，和「已归档」弹窗一个路数。
    let ai_scenarios = RwSignal::new(Vec::<PromptRow>::new());
    let ai_tones = RwSignal::new(Vec::<PromptRow>::new());
    let ai_loaded = RwSignal::new(false);
    let ai_busy = RwSignal::new(false);
    let ai_msg = RwSignal::new(None::<String>);
    let ai_error = RwSignal::new(None::<String>);
```

在 `let ws_id_of = move || -> Option<String> { ... };` **之后**（该闭包要先于使用它的闭包定义）加：

```rust
    let load_ai = move |ws_id: String| {
        ai_loaded.set(false);
        ai_error.set(None);
        spawn_local(async move {
            match workspace_ai_config(&ws_id).await {
                Ok(cfg) => {
                    ai_scenarios.set(rows_from(&cfg.scenarios));
                    ai_tones.set(rows_from(&cfg.tones));
                    ai_loaded.set(true);
                }
                Err(e) => ai_error.set(Some(e)),
            }
        });
    };

    let save_ai = move |ev: SubmitEvent| {
        ev.prevent_default();
        let Some(ws_id) = ws_id_of() else { return };
        let scenarios = Value::Array(rows_to_value(&ai_scenarios));
        let tones = Value::Array(rows_to_value(&ai_tones));
        ai_busy.set(true);
        ai_error.set(None);
        ai_msg.set(None);
        spawn_local(async move {
            match update_workspace_ai_config(&ws_id, &scenarios, &tones).await {
                Ok(cfg) => {
                    // 回填服务端落库后的结果：空行被丢掉、名称被 trim，界面应与库内一致。
                    ai_scenarios.set(rows_from(&cfg.scenarios));
                    ai_tones.set(rows_from(&cfg.tones));
                    ai_msg.set(Some("已保存".to_string()));
                }
                Err(e) => ai_error.set(Some(e)),
            }
            ai_busy.set(false);
        });
    };
```

- [ ] **Step 4: 加导航项**

`set-nav` 里「标签定义」那一项之后加：

```rust
                    <div class="it" class:on=move || tab.get() == "ai"
                        on:click=move |_| {
                            tab.set("ai".into());
                            if let Some(ws_id) = ws_id_of() {
                                load_ai(ws_id);
                            }
                        }>
                        {ic_comment()}"AI 总结"
                    </div>
```

- [ ] **Step 5: 加标签页正文**

在 `} else if cur_tab == "audit" {` 那个分支**之前**插入：

```rust
                            } else if cur_tab == "ai" {
                                view! {
                                    <h2>"AI 总结"</h2>
                                    <p class="mut">"生成总结时可选「场景」与「语气」；两项都是工作空间内可维护的「名称 + 提示词」。生成总结会把提示词追加到内置模板之后。"</p>
                                    <p class="mut">"服务端的模型与密钥在 config.toml 的 [ai] 段配置，不在本页。"</p>
                                    {move || ai_error.get().map(|e| view! { <p class="error">{e}</p> })}
                                    {move || ai_msg.get().map(|m| view! { <p class="mut">{m}</p> })}
                                    {if can_manage {
                                        view! {
                                            <form class="stack" on:submit=save_ai>
                                                <h3 style="margin-top:18px">"场景"</h3>
                                                <PromptRows rows=ai_scenarios placeholder="场景名称，如「迭代复盘」".to_string() />
                                                <h3 style="margin-top:18px">"语气"</h3>
                                                <PromptRows rows=ai_tones placeholder="语气名称，如「简洁」".to_string() />
                                                <button class="btn pri" type="submit" disabled=move || ai_busy.get()
                                                    style="align-self:flex-start">
                                                    {move || if ai_busy.get() { "保存中…" } else { "保存" }}
                                                </button>
                                            </form>
                                        }.into_any()
                                    } else {
                                        view! {
                                            <div class="stack">
                                                <h3>"场景"</h3>
                                                {move || ai_scenarios.get().into_iter().map(|r| view! {
                                                    <div class="mut">{r.name.get()}</div>
                                                }).collect::<Vec<_>>()}
                                                <h3>"语气"</h3>
                                                {move || ai_tones.get().into_iter().map(|r| view! {
                                                    <div class="mut">{r.name.get()}</div>
                                                }).collect::<Vec<_>>()}
                                                <p class="mut">"仅 Maintainer 及以上可修改"</p>
                                            </div>
                                        }.into_any()
                                    }}
                                    {move || ai_loaded.get().then(|| view! { <div></div> })}
                                }.into_any()
```

注意：`can_manage` 这个局部变量在既有的 `Some(Ok((_ws, role, ...)))` 分支里已经算好了（`src/frontend/pages/settings.rs:386` 附近），本分支直接沿用，不要重新计算。

- [ ] **Step 6: 跑编译门 + 浏览器实测**

Run: `cargo check --lib` → 期望 exit 0
Run: `cargo check --lib --no-default-features --features hydrate --target wasm32-unknown-unknown` → 期望 exit 0

`make dev` 后在浏览器里：

1. 打开 `/工作空间slug/settings`，左侧出现「AI 总结」，点进去（Maintainer 及以上账号）。
2. 加两行场景（名称「复盘」+ 提示词「按迭代周期归纳」）、一行语气，保存 → 显示「已保存」。
3. 刷新页面再进来 → 三行原样回填（确认落库）。
4. 回到工作空间，勾两条 → AI 总结 → 两个下拉里能选到「复盘」，语气里能选到那一行。
5. 用 Reader 账号打开设置页：能看到已有名称，但只有「仅 Maintainer 及以上可修改」的提示，没有保存按钮。

- [ ] **Step 7: 提交**

```bash
git add src/frontend/ai_prompt_editor.rs src/frontend/mod.rs src/frontend/pages/settings.rs
git commit -m "feat(frontend): add AI summary settings tab"
```

---

### Task 10: 真实网关端到端验收

**Files:**
- Modify: `config.toml`（**已被 gitignore，不进仓库**）
- Modify: 本计划文件末尾追加「验收记录」小节

**Interfaces:**
- Consumes: 全部前置任务
- Produces: 一份可复查的验收记录

- [ ] **Step 1: 向用户索取并写入网关配置**

需要用户提供：`base_url`、`api_key`、`model`。写入 `config.toml`（不要写进 `config.example.toml`）：

```toml
[ai]
base_url = "https://<用户的网关>/v1"
api_key = "sk-..."
model = "<模型名>"
timeout_seconds = 60
```

写完先确认它没被 git 看到：

Run: `git status --porcelain config.toml`
期望：无输出（被 gitignore 忽略）。

- [ ] **Step 2: 重启服务并生成一条真实总结**

`make dev`，浏览器里：勾选 ≥ 2 条有正文的条目 → AI 总结 → 选一个场景 → 生成。

期望：
- 新条目被创建，标题来自模型输出里的第一个 `#` 标题行（或首行前 40 字符）；正文是模型返回的 Markdown。
- 弹窗自动关闭，全屏详情打开，显示的就是这条新条目。
- 列表里能看到这条新条目，条数 +1。
- 控制台无 page error。

- [ ] **Step 3: 负路径 1 —— 未配置模型**

把 `config.toml` 的 `model` 改成空串并重启，重复上面的生成操作。

期望：弹窗内报「服务端未配置 AI 模型」，弹窗不关闭，**条目数不变**（没有建出空条目）。改回原值并重启。

- [ ] **Step 4: 负路径 2 —— 选中项含已删除条目**

在浏览器里勾选 2 条，然后另开标签页删掉其中一条，再回到原页面点生成。

期望：报错（`NOT_FOUND`），弹窗不关闭，条目数不变。

- [ ] **Step 5: 弱网慢响应的表现**

把 `timeout_seconds` 临时改成 2 并重启，选一个较大的条目集合生成。

期望：报上游错误（超时），弹窗不关闭，没有半成品条目。改回 60 并重启。

- [ ] **Step 6: 写验收记录**

在本计划文件末尾追加：

```markdown
## 验收记录

- 日期：2026-09-15
- 网关：<base_url> / <model>（api_key 不入库、不记录）
- 主路径：勾选 N 条 → 生成 → 新条目 code=<code>，标题=<title>，正文长度=<n> 字符；全屏自动打开。
- 未配置模型：报「服务端未配置 AI 模型」，条数不变。
- 选中含已删除条目：报 NOT_FOUND，条数不变。
- 超时（timeout_seconds=2）：报上游错误，条数不变。
- 编译门：`cargo check --lib` 与 wasm hydrate check 均 exit 0。
- 已知留白：总结以 Markdown 源码存入 detail，详情编辑器按富文本处理，可能显示为纯文本（见 spec §15）。
```

- [ ] **Step 7: 提交**

```bash
git add docs/superpowers/plans/2026-09-15-ai-entry-summary.md
git commit -m "docs: record AI summary acceptance results"
```

（`config.toml` 不进这个提交。）

---

## 自检

**Spec 覆盖**

| Spec 章节 | 落在哪个任务 |
|---|---|
| §3 服务端 `[ai]` 配置 | Task 1 |
| §4.1 `NamedPrompt` / `WorkspaceAiConfig` | Task 2 |
| §4.2 新列族 `WORKSPACE_AI` + `ALL_CFS` | Task 2 |
| §5 `AiService` / `AiClient` / Chat Completions 请求与解析 | Task 4、Task 5 |
| §6 三个 GraphQL 接口 | Task 6 |
| §6.1 `create_with_detail` | Task 3 |
| §7 提示词组装（基础模板 + 场景 + 语气） | Task 4（`BASE_INSTRUCTIONS` / `pick_prompt` / `render_entry_block`） |
| §8 标题生成三级兜底 | Task 5（`derive_title`） |
| §9.1 批量栏入口 + 弹窗 + 跳全屏 | Task 8 |
| §9.2 设置页「AI 总结」标签页 | Task 9 |
| §10 权限（读=成员 / 改=Maintainer / 生成=Worker） | Task 6 |
| §11 错误处理两个变体与失败不建条目 | Task 1（变体）、Task 6（顺序）、Task 10（验证） |
| §12 reqwest 依赖与 feature | Task 1 |
| §13 测试策略（不写新单测、真实密钥验收） | Global Constraints、Task 10 |
| §14 无破坏性变更 | Task 3 保持 `create` 签名、Task 2 用新列族 |

**类型一致性**

- `NamedPrompt` / `WorkspaceAiConfig` 在 `domain`（Task 2）、服务层（Task 4）、GraphQL（Task 6）、前端（Task 7）四处使用，字段名统一为 `name` / `prompt` / `scenarios` / `tones`。
- `create_with_detail(actor, workspace_id, title, detail)` 在 Task 3 定义、Task 6 调用，参数顺序一致。
- `derive_title(markdown, count)` 在 Task 5 定义、Task 6 调用。
- `PromptRows` / `PromptRow` / `rows_from` / `rows_to_value` 在 Task 9 定义并在同任务内使用，`placeholder` 是 `String`（不是 `&'static str`），调用处传 `.to_string()`。
- `workspace_ai_config` / `update_workspace_ai_config` / `summarize_entries` 在 Task 7 定义、Task 8 与 Task 9 调用；`summarize_entries` 返回 `Entry`（前端类型），Task 8 用到的是 `.code`——该字段在 `src/frontend/graphql_client.rs:133` 确实存在。

**无占位符**：每个代码步骤都是可直接落地的完整代码；唯一的「视情况调整」出现在 Task 1 Step 5（`reqwest` 版本回退到 0.13），这是依赖解析的现实不确定性，且给了明确的判断条件与命令。

---

## 验收记录

- 日期：2026-09-16
- 网关：`https://api.deepseek.com` / `deepseek-flash`（api_key 不入库、不记录）
- 验收环境：隔离实例（`127.0.0.1:3099` + 临时 `data_dir`），未触碰开发库；浏览器由 playwright 无头驱动。
- 主路径（UI 全流程）：勾选 2 条（场景=迭代复盘、语气=简洁）→ 生成
  → 新条目 `code=34PpVkGMfi0g51hL`，标题=`概览`，正文 1868 字符（Markdown）
  → 弹窗关闭、全屏详情自动打开并显示该条目正文；列表 6 → 7；无 page error / console.error。
  9/9 断言通过。
- 未配置模型（UI）：报「服务端未配置 AI 模型」，弹窗不关闭，条目数不变；8/8 断言通过。
- 选中含已删除条目（UI）：报「资源不存在」（NotFound），弹窗不关闭，条目数不变；9/9 断言通过。
- 超时（`timeout_seconds=2`，接口级）：2.0s 后报
  「调用模型失败: error sending request for url (https://api.deepseek.com/chat/completions)」，条目数不变。
- 权限（接口级，Reader 角色）：
  - `workspaceAiConfig` 读 → 通过（读 = 成员）；
  - `summarizeEntries` 生成 → 「无权限执行此操作」，不建条目（生成 = Worker）；
  - `updateWorkspaceAiConfig` 写 → 「无权限执行此操作」（改 = Maintainer）。
- 编译门：`cargo check --lib` exit 0；`cargo check --no-default-features --features hydrate --target wasm32-unknown-unknown` exit 0。
- `config.toml` 未入库：`git status --porcelain config.toml` 无输出。

**观察（非缺陷，记录备查）**

- `derive_title`（`src/service/ai.rs:244`）的一级兜底取**第一个以 `#` 开头的行**并剥掉全部 `#`。
  真实模型这次的输出以 `## 概览`（小节标题）开头而非文档标题，于是标题被抽成「概览」——
  行为忠于 spec §8，但模型不吐文档级标题时会得到一个偏泛化的标题。
- 总结以 Markdown 源码存入 `detail`，详情编辑器按富文本处理，可能显示为纯文本（见 spec §15）。
- 网关的模型名大小写敏感：`DeepSeek-Flash` 被 400 拒绝，须写 `deepseek-flash`。
  这条错误正是由 `AppError::Ai` 承载上游 `message` 透出到弹窗，属设计中的错误路径按预期工作。
