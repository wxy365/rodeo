# 多选条目 AI 总结 设计文档

> 日期：2026-09-14
> 依据：用户 2026-09-14 需求 2
> 状态：设计已与用户确认
> 关联：标签与元数据增强见 `2026-09-14-label-metadata-enhancements-design.md`

## 1. 背景与目标

支持勾选多条 Entry，一键把内容发给大模型，生成 Markdown 总结；可选场景与语气（两者均为工作空间内可维护的「名称 + 提示词」条目）。生成的总结**新建为一条 Entry**（标题自动生成、正文为总结），并自动全屏打开该 Entry。

## 2. 范围与非目标

**范围**

- 服务端 `[ai]` 配置（`base_url` / `api_key` / `model`）与 HTTP 调用（OpenAI Responses 协议）。
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
base_url = "https://api.openai.com/v1"   # 兼容 OpenAI Responses 协议的自建网关同理
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

AI 调用单独放一层异步客户端（`src/service/ai_client.rs` 或同文件）：

```rust
pub struct AiClient { http: reqwest::Client, base_url: String, api_key: String, model: String }
impl AiClient {
    pub async fn complete(&self, prompt: &Prompt) -> Result<String, AppError>;
}
```

`complete` 发 `POST {base_url}/responses`，头 `Authorization: Bearer {api_key}`、`Content-Type: application/json`，体：

```json
{ "model": "<model>", "instructions": "<instructions>", "input": "<input>" }
```

响应解析（OpenAI Responses 协议）：遍历 `output[]` 中 `type == "message"` 的项，取其 `content[]` 里 `type == "output_text"` 的 `text` 依次拼接；顶层 `error` 非空或 `status != "completed"` 时报错。拼接结果为空也报错。

## 6. GraphQL（`src/api/graphql.rs`）

- **查询** `workspaceAiConfig(workspaceId: ID!): GqlWorkspaceAiConfig!`
  - `GqlWorkspaceAiConfig { scenarios: [GqlNamedPrompt!]!, tones: [GqlNamedPrompt!]! }`，`GqlNamedPrompt { name, prompt }`。
- **变更** `updateWorkspaceAiConfig(workspaceId: ID!, scenarios: [NamedPromptInput!]!, tones: [NamedPromptInput!]!): GqlWorkspaceAiConfig!`
- **变更** `summarizeEntries(workspaceId: ID!, codes: [String!]!, scenario: String, tone: String): GqlEntry!`
  - resolver：`AiService::build_prompt`（同步）→ `AiClient::complete`（await）→ 生成标题、建 Entry（`EntryService::create` 路径）、写审计 → 返回 `GqlEntry`（前端据此直接跳全屏）。

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

- 编辑场景 / 语气：Maintainer 及以上（与标签管理一致）。
- 生成总结：Worker 及以上（因为会新建 Entry）。

## 11. 错误处理

`AppError` 新增两个变体并在 `error.rs` 的 code 映射里登记：`AiNotConfigured`（code `AI_NOT_CONFIGURED`，消息「服务端未配置 AI 模型」）与 `Ai(String)`（code `AI_ERROR`，承载上游错误文本）。

- 未配置模型 / 密钥：返回 `AiNotConfigured`，不建 Entry。
- HTTP 失败、超时、响应结构异常、输出为空：返回 `Ai(可读信息)`，不建 Entry。
- 选中的 code 含不属于本工作空间 / 已删除项：拒绝（`NotFound` / `InvalidQuery`），不建 Entry。

## 12. 依赖

服务端新增 `reqwest`（`rustls-tls`、`json`），仅 `ssr` feature 启用（与现有 `axum` / `tokio` 同组，见 `Cargo.toml` 的 `ssr = [...]`）。

## 13. 测试策略

- **后端不写新单测**（用户明确）；以 `cargo build` + wasm `check` 为编译门。
- AI 调用属外部依赖，不做自动化端到端；由用户在浏览器验收（可用真实密钥或自建 OpenAI 兼容网关）。

## 14. 破坏性变更

无。新列族 + 新配置段 + 新增 GraphQL 字段/变更，均为增量。

## 15. 待确认 / 后续留白

- **总结以 Markdown 源码存入 `detail`**：详情编辑器（tiny-editor）按富文本处理，可能以纯文本显示 Markdown 源码。本轮不做 Markdown 渲染；若需要渲染，另开一轮。
- 流式输出、每工作空间独立模型、生成历史的留白。
