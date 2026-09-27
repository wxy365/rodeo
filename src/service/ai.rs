use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt};
use serde::Serialize;
use ulid::Ulid;

use crate::domain::{
    AgentMessage, AuditAction, AuditLog, Entry, Labeling, NamedPrompt, Role, WorkspaceAiConfig,
};
use crate::error::AppError;
use crate::service::agent::templates::ToolSchema;
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

pub struct AiClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

/// 流式对话消息：role + content，可选附带 tool_call_id（tool 结果回传）
/// 或 tool_calls（assistant 已发起的工具调用，向模型回放历史时需要）。
#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRequest>>,
}

/// 单个完整 tool call：id + name + arguments 原始 JSON 字符串。
/// 流式累加后由调用方自行 `serde_json::from_str` 解析 parameters。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallRequest {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // 固定 "function"
    pub function: ToolCallRequestFn,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallRequestFn {
    pub name: String,
    pub arguments: String, // 字符串，让 OpenAI 自己解析
}

/// 流式响应的单帧：
/// - `Delta`：assistant 文本增量；
/// - `ToolCallsPartial`：单帧只携带了某个 tool_call 的部分字段，调用方应
///   据此更新自己的累积状态，不要立刻视为完整 tool call；
/// - `ToolCalls`：流结束时一次性吐出的完整 tool calls；
/// - `Done`：流结束。
#[derive(Debug, Clone)]
pub enum StreamChunk {
    Delta(String),
    #[allow(dead_code)]
    ToolCallsPartial(Vec<(u32, String, String, String)>),
    ToolCalls(Vec<ToolCallRequest>),
    Done,
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

    /// 流式调用 Chat Completions。
    ///
    /// 把整段响应拆成一个 `StreamChunk` 流：
    /// - 每个 `Delta` 是一次 assistant 文本增量；
    /// - 每个 `ToolCallsPartial` 是某帧 `tool_calls` 数组里某个 index 的部分
    ///   字段——调用方应当用它去更新自己的累积状态（id / name / 累加 arguments）；
    /// - `ToolCalls` 仅在流结束时一次性吐出，把所有已完成的 tool_call 整合为
    ///   `ToolCallRequest`（含完整 arguments 字符串）；
    /// - `Done` 表示流结束。
    ///
    /// 错误一律落在 `Stream::Err(AppError::Ai(_))` 上——带尽量可读的上游信息。
    pub async fn complete_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSchema],
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<StreamChunk, AppError>> + Send>>,
        AppError,
    > {
        use async_stream::try_stream;

        let url = format!("{}/chat/completions", self.base_url);
        // 把工具描述转成 OpenAI 协议的 JSON：每个 tool 一层
        // `{type:"function", function:{name, description, parameters}}`。
        let tools_json: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    }
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "model": self.model,
            "stream": true,
            "messages": messages,
        });
        // 没工具就不发 tools 字段——OpenAI 在空数组上的行为各家网关不一致，
        // 最稳的做法是不带这个字段。
        if !tools_json.is_empty() {
            body["tools"] = serde_json::Value::Array(tools_json);
        }

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
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::Ai(format!(
                "模型返回 {status}: {}",
                upstream_error(&text)
            )));
        }

        let mut stream = resp.bytes_stream();
        let s: Pin<Box<dyn Stream<Item = Result<StreamChunk, AppError>> + Send>> =
            Box::pin(try_stream! {
                use std::collections::HashMap;
                // 已完成字段：(id, name, 累加过的 arguments 字符串)
                let mut pending: HashMap<u32, (String, String, String)> = HashMap::new();
                // SSE 帧之间的未完整数据——按 \n\n 切事件，多出来的尾巴留到下次。
                let mut buffer = String::new();
                while let Some(chunk) = stream.next().await.transpose()
                    .map_err(|e| AppError::Ai(format!("读取流失败: {e}")))? {
                    buffer.push_str(std::str::from_utf8(&chunk)
                        .map_err(|e| AppError::Ai(format!("SSE 不是 UTF-8: {e}")))?);
                    // 按 \n\n 切 SSE 事件：一次可能切到多个，靠 while 循环剥完。
                    while let Some(split) = buffer.find("\n\n") {
                        let event = buffer[..split].to_string();
                        buffer = buffer[split + 2..].to_string();
                        match parse_sse_event(&event)? {
                            Some(StreamChunk::Delta(s)) => yield StreamChunk::Delta(s),
                            Some(StreamChunk::ToolCallsPartial(partials)) => {
                                // OpenAI 流式协议：每个 delta 只携带变化的部分
                                // （id / function.name 各自出现一次，function.arguments
                                //  是 JSON 字符串的逐片片段），所以这里做就地合并。
                                for (index, id, name, args_fragment) in partials {
                                    let entry = pending.entry(index).or_insert_with(|| {
                                        (String::new(), String::new(), String::new())
                                    });
                                    if !id.is_empty() {
                                        entry.0 = id;
                                    }
                                    if !name.is_empty() {
                                        entry.1 = name;
                                    }
                                    entry.2.push_str(&args_fragment);
                                }
                                // 不在这里 yield：等流末统一吐完整 ToolCalls，
                                // 调用方可以一次性拿到所有 arguments 累加结果。
                            }
                            Some(StreamChunk::ToolCalls(_)) | Some(StreamChunk::Done) | None => {}
                        }
                    }
                }
                // 流末：把累积好的 tool_calls 一次性吐出去；空就直接 Done。
                if !pending.is_empty() {
                    let tcs: Vec<ToolCallRequest> = pending
                        .into_iter()
                        .map(|(_, (id, name, args))| ToolCallRequest {
                            id,
                            kind: "function".into(),
                            function: ToolCallRequestFn { name, arguments: args },
                        })
                        .collect();
                    yield StreamChunk::ToolCalls(tcs);
                }
                yield StreamChunk::Done;
            });
        Ok(s)
    }
}

/// 解析 Chat Completions 响应：取 `choices[0].message.content`。
/// 顶层 `error` 非空、`choices` 缺失或为空、内容为 null/空白，都按失败处理——
/// 宁可报错，也不要把空串当总结建出一条空条目。
fn parse_completion(text: &str) -> Result<String, AppError> {
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| AppError::Ai("模型响应不是合法 JSON".to_string()))?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let msg = error_message(err).unwrap_or_else(|| "模型返回错误".to_string());
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

/// 解析单条 SSE 事件（已剥离 `\n\n`）。只关心 `data:` 行；
/// 其他字段（event/id/retry）忽略。
///
/// 返回：
/// - `Ok(None)` —— 这一事件无内容（仅 `[DONE]`、空、或全是元数据）
/// - `Ok(Some(StreamChunk::Delta(s)))` —— 增量文本
/// - `Ok(Some(StreamChunk::ToolCallsPartial(partials)))` —— 工具调用 delta；
///   调用方需就地累积
///
/// OpenAI 流式协议下，单次 chunk 的 `delta.tool_calls` 数组里每个元素只携带
/// 变化的部分（id / function.name / function.arguments 各自出现一次），所以
/// 这里返回「这一帧的变化」，由 `complete_stream` 的外层循环做就地合并。
fn parse_sse_event(event: &str) -> Result<Option<StreamChunk>, AppError> {
    // 多个 `data:` 行属于同一 payload，按 SSE 规范用 `\n` 拼接。
    let mut data_lines = Vec::new();
    for line in event.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }
    if data_lines.is_empty() {
        return Ok(None);
    }
    let payload = data_lines.join("\n");
    // 流结束哨兵：OpenAI 协议用 `[DONE]` 表示流尾。
    if payload == "[DONE]" {
        return Ok(None);
    }
    let v: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|_| AppError::Ai("SSE chunk 不是 JSON".to_string()))?;
    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| AppError::Ai("SSE chunk 缺少 choices".to_string()))?;
    let delta = choice.get("delta");
    if let Some(content) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
        return Ok(Some(StreamChunk::Delta(content.to_string())));
    }
    if let Some(tcs) = delta
        .and_then(|d| d.get("tool_calls"))
        .and_then(|c| c.as_array())
    {
        // 每条 entry 都按 index 索引：同一 tool_call 的多个 delta 共享一个 index。
        // id / function.name 只在首个 delta 出现；function.arguments 是 JSON
        // 字符串的逐片片段，需要按 index 累加。这里只把这一帧的变化透传出去。
        let mut out = Vec::with_capacity(tcs.len());
        for tc in tcs {
            let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let id = tc.get("id").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let name = tc
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let args_fragment = tc
                .get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            out.push((index, id, name, args_fragment));
        }
        return Ok(Some(StreamChunk::ToolCallsPartial(out)));
    }
    Ok(None)
}

fn error_message(err: &serde_json::Value) -> Option<String> {
    err.get("message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .or_else(|| err.as_str().map(str::to_string))
        .filter(|m| !m.trim().is_empty())
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

/// 把领域层 `AgentMessage` 列表转 OpenAI Chat Completions 的 messages 数组。
/// 历史里 assistant 已经发起过的 tool_calls 也必须原样回放给模型——不然它下一次
/// 不知道上一轮自己已经调用过哪些工具。
pub fn to_chat_messages(history: &[AgentMessage]) -> Vec<ChatMessage> {
    history
        .iter()
        .map(|m| {
            let tool_calls = if m.tool_calls.is_empty() {
                None
            } else {
                Some(
                    m.tool_calls
                        .iter()
                        .map(|tc| ToolCallRequest {
                            id: tc.id.clone(),
                            kind: "function".into(),
                            function: ToolCallRequestFn {
                                name: tc.name.clone(),
                                arguments: serde_json::to_string(&tc.args)
                                    .unwrap_or_default(),
                            },
                        })
                        .collect(),
                )
            };
            ChatMessage {
                role: match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                }
                .to_string(),
                content: m.content.clone(),
                tool_call_id: m.tool_call_id.clone(),
                tool_calls,
            }
        })
        .collect()
}

/// 通用 system 提示 + 工作空间附加段。返回单条 system 消息；
/// 调用方自行拼到 messages 数组头部。
pub fn agent_system_prompt(ws_title: &str, ws_id: &str) -> ChatMessage {
    ChatMessage {
        role: "system".to_string(),
        content: format!(
            "你是 Rodeo 工作空间内的对话式 Agent。你可以调用工具查询和修改条目、标签、评论、视图等数据。\
             所有工具调用都在当前用户权限下执行——管理员才能改的，你查不到也改不了。\
             执行多条工具调用时，按依赖顺序串行：先 list 再 update，避免基于过期信息修改。\
             一次工具调用失败就停下来告诉用户，不要反复重试同一参数。\
             回答使用 Markdown，简洁优先。\n\n\
             当前工作空间：{ws_title}\n\
             工作空间 ID：{ws_id}"
        ),
        tool_call_id: None,
        tool_calls: None,
    }
}
