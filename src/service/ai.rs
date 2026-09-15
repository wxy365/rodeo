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
