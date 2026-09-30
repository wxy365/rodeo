//! AI 配置（场景/语气 + 一次性总结）和 Rig 客户端薄壳。
//!
//! 多轮循环、tool 执行、SSE 解析全部交给 Rig；本模块只
//! 1) 提供 `AiService::build_prompt` 之类的配置层逻辑（graphql.rs 还在用，task 9 拆走）
//! 2) 提供 `RigClient` —— 从 `AiConfig` 拼一个 `rig::providers::openai::CompletionsClient`，
//!    强制走 Chat Completions 协议，让 MiniMax / DeepSeek / 通义等 OpenAI 兼容服务都
//!    走标准 Chat 路径。

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::providers::openai::{self, CompletionModel};
use ulid::Ulid;

use crate::config::AiConfig;
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
/// `AiClient::complete` 在 task 9 被移除后整个 `Prompt` 也会被一起拆走，
/// 现在暂时保留，因为 graphql.rs 还在用 `build_prompt`。
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

/// Rig 客户端薄壳：把 `AiConfig` 喂给 rig-core 的 OpenAI provider，固化 Chat
/// Completions 协议，让任何 OpenAI 兼容服务（MiniMax / DeepSeek / 通义 / OpenAI
/// 自身）都走标准 Chat 路径。
///
/// runner 不直接消费 `RigClient`：它拿 `completion_model()` 拿到的 `CompletionModel`
/// 调 rig-core 的 streaming API（task 10 接入）。这里只负责 provider 装配 + model
/// 标识保留，跨请求复用底层 reqwest 连接池。
#[derive(Clone)]
pub struct RigClient {
    provider: Arc<openai::CompletionsClient>,
    model: String,
}

impl RigClient {
    /// 未配置密钥或模型时返回 `None`：调用方据此报 `AiNotConfigured`，
    /// 而不是发一个注定 401 的请求。
    pub fn from_config(cfg: &AiConfig) -> Result<Option<Self>, AppError> {
        if !cfg.enabled() {
            return Ok(None);
        }
        // 去掉末尾斜杠，免得拼出 `//chat/completions`——不少网关对此直接 404。
        let base_url = cfg.base_url.trim_end_matches('/');
        let provider = openai::CompletionsClient::builder()
            .api_key(&cfg.api_key)
            .base_url(base_url)
            .build()
            .map_err(|e| AppError::Ai(format!("rig client 初始化失败: {e}")))?;
        Ok(Some(Self {
            provider: Arc::new(provider),
            model: cfg.model.clone(),
        }))
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// 拿到一个 Chat Completions 协议的 completion model。
    /// 走 `CompletionsClient` 而不是默认的 Responses client，保证 MiniMax /
    /// DeepSeek / 通义等 OpenAI 兼容服务走标准 Chat 路径。
    pub fn completion_model(&self) -> CompletionModel {
        self.provider.completion_model(&self.model)
    }
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