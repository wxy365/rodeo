//! AI 配置（场景/语气 + 一次性总结）和 Rig 客户端薄壳。
//!
//! 多轮循环、tool 执行、SSE 解析全部交给 Rig；本模块只
//! 1) 提供 `AiService::build_prompt` 之类的配置层逻辑（graphql.rs 还在用，task 9 拆走）
//! 2) 提供 `RigClient` —— 从 `AiConfig` 拼一个 `rig::providers::openai::CompletionsClient`，
//!    强制走 Chat Completions 协议，让 MiniMax / DeepSeek / 通义等 OpenAI 兼容服务都
//!    走标准 Chat 路径。

use std::sync::Arc;

use rig::client::CompletionClient;
use rig::completion::{AssistantContent, CompletionModel as CoreCompletionModel};
use rig::providers::openai::{self, CompletionModel};
use ulid::Ulid;

use crate::config::AiConfig;
use crate::domain::{
    AuditAction, AuditLog, Entry, LabelSchema, Labeling, NamedPrompt, Query as ViewQuery,
    WorkspaceAiConfig,
};
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
        ops.push(BatchOp::put(
            cf::WORKSPACE_AI,
            ws.to_bytes().to_vec(),
            &cfg,
        )?);
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

/// 自然语言查询 → 标签表达式：system 提示里钉死语法与当前工作空间可用的
/// 标签 / 成员，user 消息只放用户原话。标签 key、枚举值与成员名必须由
/// 服务端给全，前端不在每轮请求里重复上传这些 schema。
pub fn build_query_translation_prompt(
    schemas: &[LabelSchema],
    members: &[(String, String)],
    text: &str,
) -> Prompt {
    Prompt {
        instructions: query_translation_instructions(schemas, members),
        input: text.to_string(),
    }
}

/// 第一次输出过不了 `parse` / `validate` 时的重试提示：把上一版与错误一起
/// 回给模型。只重试一次——连续两次不合法说明模型没理解 schema，继续烧 token
/// 不如把错误抛给用户。
pub fn build_query_translation_retry_prompt(
    schemas: &[LabelSchema],
    members: &[(String, String)],
    text: &str,
    previous: &str,
    error: &str,
) -> Prompt {
    Prompt {
        instructions: format!(
            "{}\n\n## 上一次输出\n{previous}\n\n## 上一次输出的问题\n{error}\n\n\
             请修正上述问题，重新输出一条合法表达式。仍然只输出表达式本身。",
            query_translation_instructions(schemas, members),
            previous = previous.trim(),
            error = error.trim(),
        ),
        input: text.to_string(),
    }
}

fn query_translation_instructions(schemas: &[LabelSchema], members: &[(String, String)]) -> String {
    let mut out = String::from(
        "你是任务跟踪系统 rodeo 的查询翻译器。把用户的自然语言筛选需求翻译成一条「标签表达式」。\n\n\
         ## 语法\n\
         - 布尔运算：AND / OR / NOT，可用括号分组。\n\
         - 标签存在：直接写标签 key（如 Task）；不存在写 !Task 或 NOT Task。\n\
         - 比较：<标签> <op> <值>。op 可用 = != > >= < <= ~ !~ in not in has is。\n\
         - 单值标签用 = / !=；多值标签包含某元素用 has，集合等于用 is。\n\
         - in 的右值写成 (\"a\", \"b\")；枚举的 in 候选值必须来自枚举值列表。\n\
         - 字符串值用双引号；数字 / 布尔不加引号。\n\
         - 内置字段：Code、Title、Detail、CreatedBy、UpdatedBy、CreatedAt、UpdatedAt；\
           全文检索写 text ~ \"关键词\"。\n\
         - 时间值写成 \"YYYY-MM-DD\" 或 \"YYYY-MM-DD HH:MM:SS\"。\n\n\
         ## 可用标签\n",
    );
    if schemas.is_empty() {
        out.push_str("（本工作空间还没有定义任何标签；只能使用上面的内置字段。）\n");
    }
    for s in schemas {
        out.push_str("- ");
        out.push_str(&describe_schema(s));
        out.push('\n');
    }
    if !members.is_empty() {
        out.push_str("\n## 工作空间成员（account 型标签的值可用姓名或邮箱）\n");
        for (name, email) in members {
            let name = name.trim();
            if name.is_empty() {
                out.push_str(&format!("- {email}\n"));
            } else {
                out.push_str(&format!("- {name} <{email}>\n"));
            }
        }
    }
    out.push_str(
        "\n## 输出要求\n\
         只输出一条标签表达式，不要 Markdown 代码块，不要解释、前后缀或句号。\n\
         标签必须使用上面列出的 key 原文，不要使用标题、拼音或自造名称。\n\
         无法翻译成标签条件时，退化为全文检索 text ~ \"用户的原话\"。\n\n\
         示例：\n\
         用户：王晓雁未完成的任务\n\
         输出：Assignee has \"王晓雁\" AND !Done AND Task\n\
         用户：标题里带登录的 bug\n\
         输出：Bug AND Title ~ \"登录\"",
    );
    out
}

fn describe_schema(s: &LabelSchema) -> String {
    let mut out = format!("`{}`", s.name);
    let title = s.title.trim();
    if !title.is_empty() && title != s.name {
        out.push_str(&format!("（标题：{title}）"));
    }
    out.push_str(&format!("，类型：{}", s.value_type.as_str()));
    if s.multi {
        out.push_str("，多值");
    }
    match s.value_type {
        crate::domain::LabelValueType::Null => {
            out.push_str("，无值标签：直接写 key 表示已打上，!key 表示未打上");
        }
        crate::domain::LabelValueType::Enum if !s.enum_values.is_empty() => {
            out.push_str(&format!("，可选值：{}", s.enum_values.join("、")));
        }
        crate::domain::LabelValueType::Account => {
            out.push_str("，值用成员姓名或邮箱，多值时用 has");
        }
        crate::domain::LabelValueType::Date
        | crate::domain::LabelValueType::Time
        | crate::domain::LabelValueType::DateTime => {
            out.push_str("，值写成时间字符串");
        }
        _ => {}
    }
    out
}

/// 从模型输出里抽出表达式并做语法 + schema 校验。
///
/// 模型偶尔会裹 ```` ```...``` ```` 代码块、加「表达式：」前缀，或在表达式后
/// 补一句解释。先清掉最外层的代码围栏与前缀，整段不合法时再逐行尝试——
/// 这样「第一行解释、第二行表达式」也能直接成功，不必多花一次重试请求。
pub fn parse_translation(raw: &str, schemas: &[LabelSchema]) -> Result<ViewQuery, AppError> {
    let cleaned = clean_model_output(raw);
    if cleaned.is_empty() {
        return Err(AppError::Ai("模型没有返回可用的查询表达式".into()));
    }
    let first_err = match ViewQuery::parse(&cleaned).and_then(|q| {
        q.validate(schemas)?;
        Ok(q)
    }) {
        Ok(q) => return Ok(q),
        Err(e) => e,
    };
    for line in cleaned.lines() {
        let line = line.trim().trim_matches('`').trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(q) = ViewQuery::parse(line) {
            if q.validate(schemas).is_ok() {
                return Ok(q);
            }
        }
    }
    Err(first_err)
}

fn clean_model_output(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("```") {
        // 去掉 ```lang 那一行与结尾的围栏。
        s = match rest.find('\n') {
            Some(i) => &rest[i + 1..],
            None => rest,
        };
        if let Some(end) = s.rfind("```") {
            s = &s[..end];
        }
    }
    let s = s.trim().trim_start_matches('`').trim();
    let s = s
        .strip_prefix("表达式：")
        .or_else(|| s.strip_prefix("表达式:"))
        .unwrap_or(s)
        .trim();
    s.trim_end_matches('`')
        .trim()
        .trim_end_matches('。')
        .trim()
        .to_string()
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

    /// 一次性 chat completion：发一段 prompt，拿回模型的纯文本响应。
    ///
    /// rig agent rewrite (task 10) 只接管了多轮 + tool 调用、SSE streaming
    /// 那条路径；graphql `summarize_entries` 这种「不需要 tool、不需要 streaming」
    /// 的单次补全 path 上，调用方拿到的还是 `client.complete(&prompt).await?`
    /// ——所以这里补一个最薄的 rig 包装。出错统一映射为 `AppError::Ai`，
    /// 调用方拿到的就是文本摘要字符串，调用站点不需要改。
    ///
    /// `Prompt::instructions` 走 preamble（system message），`Prompt::input`
    /// 走 user message —— 与旧 RigClient 的 system+user 注入语义保持一致。
    pub async fn complete(&self, prompt: &Prompt) -> Result<String, AppError> {
        let model = self.completion_model();
        let request = model
            .completion_request(rig::completion::Message::user(&prompt.input))
            .preamble(prompt.instructions.clone())
            .build();
        let response = model
            .completion(request)
            .await
            .map_err(|e| AppError::Ai(format!("rig completion 失败: {e}")))?;
        // 抽取文本。summarize 不期望 tool call / reasoning / image —— 如果
        // 模型真返回这些就忽略，保留兼容旧行为的「只取 Text」语义。
        let mut text = String::new();
        for item in response.choice {
            if let AssistantContent::Text(t) = item {
                text.push_str(t.text());
            }
        }
        if text.is_empty() {
            return Err(AppError::Ai("rig completion 返回空响应".into()));
        }
        Ok(text)
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
        let title = rest
            .trim_start_matches('#')
            .trim()
            .trim_end_matches('#')
            .trim();
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
