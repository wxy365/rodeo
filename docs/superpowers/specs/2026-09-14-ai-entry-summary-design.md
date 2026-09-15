# 多选条目 AI 总结 设计文档

> 日期：2026-09-14
> 依据：用户 2026-09-14 需求 2
> 状态：设计已与用户确认
> 关联：标签与元数据增强见 `2026-09-14-label-metadata-enhancements-design.md`
> 修订：2026-09-15 —— 见文末「修订记录」。协议由 OpenAI Responses 改为 Chat Completions；
> 补充 `EntryService` 需要能一次写入正文的建条目路径。

## 1. 背景与目标

支持勾选多条 Entry，一键把内容发给大模型，生成 Markdown 总结；可选场景与语气（两者均为工作空间内可维护的「名称 + 提示词」条目）。生成的总结**新建为一条 Entry**（标题自动生成、正文为总结），并自动全屏打开该 Entry。

## 2. 范围与非目标

**范围**

- 服务端 `[ai]` 配置（`base_url` / `api_key` / `model`）与 HTTP 调用（OpenAI Chat Completions 协议）。
- 工作空间级「场景 / 语气」配置的存储、GraphQL 与设置页。
- `summarizeEntries` 变更：组装提示词 → 调模型 → 建 Entry → 返回新 Entry。
- 批量操作栏入口 + 生成弹窗 + 成功后跳全屏详情。

**非目标（本轮不做）**

- 流式输出（一次性返回）。
- 每个工作空间单独指定模型 / 密钥（只有服务端一份配置）。
- token 计费展示、提示词历史、总结的 Markdown 富文本渲染。
- 把总结写入已有 Entry 的详情（只新建）。

## 3. 服务端配置（`src/config.rs`）

新增配置段：

```toml
[ai]
base_url = "https://api.openai.com/v1"   # 兼容 Chat Completions 协议的自建网关同理
api_key  = ""                            # 留空则 AI 功能关闭
model    = ""                            # 留空则 AI 功能关闭
timeout_seconds = 60
```

`base_url` 缺省 `https://api.openai.com/v1`，`timeout_seconds` 缺省 60。`api_key` 或 `model` 为空即视为**未启用**：`summarizeEntries` 返回明确错误（「服务端未配置 AI 模型」），前端据此引导。

## 4. 数据模型

### 4.1 工作空间级配置（`src/domain/ai.rs`）

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NamedPrompt { pub name: String, pub prompt: String }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceAiConfig {
    pub scenarios: Vec<NamedPrompt>, // 场景
    pub tones: Vec<NamedPrompt>,     // 语气
}
```

### 4.2 存储：新列族（不改进 `Workspace`）

新列族 `cf::WORKSPACE_AI`（键 = workspace id 的 16 字节，值 = `bincode(WorkspaceAiConfig)`）。沿用本仓库既有约定——用**独立列族**而非给 bincode 结构加字段（参见 `ACCOUNT_TOKEN_VERSION` / `WORKSPACES_DELETED` / `ENTRIES_ARCHIVED` 的注释），避免存量 `Workspace` 读不出来。需在 `ALL_CFS` 注册。

## 5. 服务（`src/service/ai.rs`）

分为同步的数据层与异步的调用层：

```rust
pub struct AiService { store: Arc<DocStore> }

impl AiService {
    pub fn get_config(&self, ws: Ulid) -> Result<WorkspaceAiConfig, AppError>;
    pub fn update_config(&self, actor: Ulid, ws: Ulid,
                         scenarios: Vec<NamedPrompt>, tones: Vec<NamedPrompt>)
        -> Result<WorkspaceAiConfig, AppError>; // 写审计

    /// 同步：把选中的 Entry 组装成待发送的提示词
    pub fn build_prompt(&self, ws: Ulid, codes: &[String],
                        scenario: Option<&str>, tone: Option<&str>)
        -> Result<Prompt, AppError>;
}

pub struct Prompt { pub instructions: String, pub input: String }
```

`build_prompt`：校验 codes 非空、条目都属于该 workspace 且未删除/未归档；取 `title` + 标签 + 详情纯文本（复用 `search::strip_rich_text`）；按 §7 拼 `instructions` 与 `input`。

AI 调用单独放一层异步客户端，与数据层同文件（`src/service/ai.rs`）——两者都窄，
拆两个文件反而要来回跳；整个子系统一个文件符合本仓库 `service/<子系统>.rs` 的既有分法：

```rust
pub struct AiClient { http: reqwest::Client, base_url: String, api_key: String, model: String }
impl AiClient {
    pub async fn complete(&self, prompt: &Prompt) -> Result<String, AppError>;
}
```

`complete` 发 `POST {base_url}/chat/completions`，头 `Authorization: Bearer {api_key}`、`Content-Type: application/json`，体：

```json
{
  "model": "<model>",
  "messages": [
    { "role": "system", "content": "<instructions>" },
    { "role": "user",   "content": "<input>" }
  ]
}
```

响应解析（Chat Completions 协议，兼容自建网关 / vLLM / one-api / DeepSeek / 通义）：

- 取 `choices[0].message.content` 作为正文。该值为 `null` 或空串时报错。
- 顶层 `error` 非空时报错，取其 `message` 字段作为可读信息。
- HTTP 非 2xx 时报错，尽量带上响应体里的 `error.message`。
- `choices` 缺失或为空数组时报错（结构异常）。

`instructions` 与 `input` 分别作为 system / user 两条消息——不加额外的 `messages` 角色，网关兼容性最好。

## 6. GraphQL（`src/api/graphql.rs`）

- **查询** `workspaceAiConfig(workspaceId: ID!): GqlWorkspaceAiConfig!`
  - `GqlWorkspaceAiConfig { scenarios: [GqlNamedPrompt!]!, tones: [GqlNamedPrompt!]! }`，`GqlNamedPrompt { name, prompt }`。
- **变更** `updateWorkspaceAiConfig(workspaceId: ID!, scenarios: [NamedPromptInput!]!, tones: [NamedPromptInput!]!): GqlWorkspaceAiConfig!`
- **变更** `summarizeEntries(workspaceId: ID!, codes: [String!]!, scenario: String, tone: String): GqlEntry!`
  - resolver：`AiService::build_prompt`（同步）→ `AiClient::complete`（await）→ 生成标题 → `EntryService::create_with_detail` 建条目（正文 = 模型输出）→ 返回 `GqlEntry`（前端据此直接跳全屏）。
  - 审计在 `create_with_detail` 内部写一条 `EntryCreated`（after 快照含正文），**不新增 `AuditAction` 变体**——该枚举按 bincode 变体序号编码，新变体只能追加在末尾（见 `src/domain/audit.rs` 的注释）。
  - 失败路径（未配置 / HTTP 失败 / 结构异常 / 输出为空 / 选中项越权或已删）一律在**建条目之前**返回，不留半成品。

### 6.1 配套的服务层改动（`src/service/entry.rs`）

现有 `EntryService::create(actor, workspace_id, title)` 只收标题，`Entry::new` 把 `detail` 置为空串；
而 `update` 走乐观并发（要求 `expected_updated_at`）。总结正文必须与标题**一次写入**，
否则要么正文进不去，要么 create 完再 update——两条审计 + 一次多余的并发校验。

因此新增：

```rust
pub fn create_with_detail(
    &self,
    actor: Ulid,
    workspace_id: Ulid,
    title: &str,
    detail: &str,
) -> Result<Entry, AppError>;
```

`create` 保持现签名，改为 `create_with_detail(.., "")` 的薄封装，既有调用点不受影响。

## 7. 提示词组装

`instructions` 由三段拼接：

1. **基础模板**（内置常量）：说明输入是若干条任务/问题条目，要求输出结构化 Markdown 总结（建议含概览、要点、风险/待办）；只输出 Markdown 正文。
2. **场景提示词**：所选场景的 `prompt`（未选则跳过）。
3. **语气提示词**：所选语气的 `prompt`（未选则跳过）。

`input` 由每条 Entry 的内容拼成，含：编码、标题、标签（名=值）、详情纯文本；条与条之间用明确分隔。

## 8. 标题生成

按以下顺序取第一个非空结果：

1. 输出里第一个 `#` 开头的标题行（去掉 `#` 与前导空白）；
2. 首个非空行的前 40 个字符；
3. 兜底 `AI 总结（N 条）`（N 为选中条数）。

## 9. 前端

### 9.1 入口与弹窗（`pages/workspace_main.rs`）

- 批量操作栏（现有 `.batchbar`）新增「AI 总结」按钮，选中 ≥ 1 条时可用。
- 点击打开弹窗（复用 `.dmodal` / `.dmbox`）：场景下拉、语气下拉（数据来自 `workspaceAiConfig`）、生成按钮、进行中与错误态。
- 下拉项来自配置；**均可留空**（不选则不加对应提示词）。配置为空时提示「可在工作空间设置中配置场景与语气」。
- 成功后：用返回的 Entry 把 `selected` 置为新 code 并打开全屏详情（复用现有全屏浮层机制），同时触发列表重查。

### 9.2 设置页（`pages/settings.rs`）

新增「AI 总结」标签页（Maintainer+ 可编辑）：两组可增删的「名称 + 提示词」行（场景、语气），保存走 `updateWorkspaceAiConfig`。

## 10. 权限

- 读取场景 / 语气：成员即可（与 `labelSchemas` 一致）。
- 编辑场景 / 语气：Maintainer 及以上（与标签管理一致）。
- 生成总结：Worker 及以上（因为会新建 Entry）。

## 11. 错误处理

`AppError` 新增两个变体并在 `error.rs` 的 code 映射里登记：`AiNotConfigured`（code `AI_NOT_CONFIGURED`，消息「服务端未配置 AI 模型」）与 `Ai(String)`（code `AI_ERROR`，承载上游错误文本）。

- 未配置模型 / 密钥：返回 `AiNotConfigured`，不建 Entry。
- HTTP 失败、超时、响应结构异常、输出为空：返回 `Ai(可读信息)`，不建 Entry。
- 选中的 code 含不属于本工作空间 / 已删除项：拒绝（`NotFound` / `InvalidQuery`），不建 Entry。

## 12. 依赖

服务端新增 `reqwest`，仅 `ssr` feature 启用（与现有 `axum` / `tokio` 同组，见 `Cargo.toml` 的 `ssr = [...]`）：

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"], optional = true }
```

`default-features = false` 是为了不引入 openssl（`rustls` 走纯 Rust 栈），与现有依赖风格一致。

## 13. 测试策略

- **后端不写新单测**（用户明确）；以 `cargo build` + wasm `check` 为编译门。
- AI 调用属外部依赖，不做自动化端到端。验收由用户提供真实 `base_url` / `api_key` / `model`
  （写入已被 gitignore 的 `config.toml`；`config.example.toml` 只放空占位），在浏览器实测
  「勾选 → 生成 → 建条目 → 跳全屏」，并另测两条负路径：
  - 未配置模型 / 密钥：报「服务端未配置 AI 模型」，且**不建条目**；
  - 选中项含已删除条目：报错且不建条目。
  浏览器控制台无 page error。

## 14. 破坏性变更

无。新列族 + 新配置段 + 新增 GraphQL 字段/变更，均为增量。

## 15. 待确认 / 后续留白

- **总结以 Markdown 源码存入 `detail`**：详情编辑器（tiny-editor）按富文本处理，可能以纯文本显示 Markdown 源码。本轮不做 Markdown 渲染；若需要渲染，另开一轮。
- 流式输出、每工作空间独立模型、生成历史的留白。

## 16. 修订记录

### 2026-09-15

1. **协议改为 Chat Completions**（原为 OpenAI Responses）。
   依据：用户实际要接的网关走 `/chat/completions`——自建网关 / vLLM / one-api / DeepSeek / 通义
   基本都是这一套，Responses 是 OpenAI 官方新版协议、在这些网关上不可用。
   影响：§5 的请求体与响应解析全部改写。
2. **补充 §6.1**：`EntryService` 需新增 `create_with_detail`。
   依据：现有 `create` 只收标题、`update` 走乐观并发，而总结正文必须与标题一次写入——
   否则要么正文进不去，要么产生两条审计与一次多余的并发校验。
3. **§12 明确 reqwest 的 feature 组合**（`default-features = false` + `json` + `rustls-tls`），
   避免引入 openssl。
4. **§13 验收方式落定**：由用户提供真实 `base_url` / `api_key` / `model`，在浏览器实测；
   不做自动化端到端，不写后端单测。
