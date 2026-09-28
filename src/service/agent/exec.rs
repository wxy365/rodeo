use std::sync::Arc;

use async_graphql::{Request as GraphQLRequest, ServerError, Variables};
use ulid::Ulid;

use crate::api::graphql::{AppSchema, GraphqlContext};
use crate::domain::agent_events::ToolErrorKind;
use crate::domain::SideEffect;
use crate::error::AppError;
use crate::service::{AuthContext, Services};

pub struct ToolOutcome {
    pub ok: bool,
    pub kind: ToolErrorKind,
    pub result_preview: String,
    pub side_effect: Option<SideEffect>,
}

/// 执行一个 tool call：复用现有 GraphQL `Schema::execute`，同 `GraphqlContext`。
/// AuthContext 与权限路径与前端 mutation 100% 一致——零权限漏洞。
pub async fn execute_tool(
    schema: &AppSchema,
    services: &Arc<Services>,
    auth: &AuthContext,
    workspace_id: Ulid,
    name: &str,
    args: serde_json::Value,
) -> Result<ToolOutcome, AppError> {
    // 1) 在白名单里找模板
    let template = super::templates::TEMPLATES
        .iter()
        .find(|(n, _)| *n == name)
        .ok_or_else(|| AppError::Ai(format!("未知工具: {name}")))?
        .1;

    // 2) 构造 GraphQL 请求：args 本身就是一个变量 map（`{ input: {...}, ... }`），
    // 直接转 `Variables` 后塞进去即可。
    let req = GraphQLRequest::new(template).variables(Variables::from_json(args.clone()));

    // 3) 复用 GraphqlContext：AuthContext 决定一切权限
    let ctx = GraphqlContext {
        services: services.clone(),
        auth: Some(auth.clone()),
    };
    let resp = schema.execute(req.data(ctx)).await;

    // 4) 解析响应
    let data_json = serde_json::to_value(&resp.data)
        .map_err(|e| AppError::Ai(format!("tool 响应序列化失败: {e}")))?;

    if !resp.errors.is_empty() {
        let first = &resp.errors[0];
        let msg = first.message.clone();
        let kind = classify_tool_error(first);
        return Ok(ToolOutcome {
            ok: false,
            kind,
            result_preview: truncate_chars(&msg, 500),
            side_effect: None,
        });
    }

    let preview = truncate_chars(&data_json.to_string(), 500);
    let side_effect = classify_side_effect(name, &args, &data_json);
    // workspace_id 暂未注入 args；保留供将来做 ws 域内校验
    let _ = workspace_id;
    Ok(ToolOutcome {
        ok: true,
        kind: ToolErrorKind::Ok,
        result_preview: preview,
        side_effect,
    })
}

/// 从单条 GraphQL error 推断错误大类。规则顺序很关键：
/// 1) 基础设施类 message（"存储错误:" / "内部错误:"）→ ServerError。这
///    些是 AppError::Storage / Internal / Ai(_, ...) 经 Display 落到
///    response.error.message 上的固定前缀，再调一次也会同样失败。
/// 2) async-graphql 自带的 schema 校验关键字 → BadArgs。LLM 应调整参数。
/// 3) 其他业务消息（资源不存在 / 无权限 / 标签值不合法 等）→ Rejected。
///
/// 为什么不走 extensions.code：async-graphql 7 的 blanket
/// `From<T: Display> for Error` 在转换链上不调 `ErrorExtensions::extend`，
/// AppError 的 code 不会进 extensions（见 error.rs 注释）。要在响应里
/// 拿到 extensions.code，得要么开 `custom-error-conversion` feature 要么
/// 在 resolver 站点手动 `.extend_err(...)`——前者要全量构建开关，后者
/// 每个 resolver 都要包一层，按当前代码面铺开太贵。message 分类在
/// AppError 的 Display 字面量稳定（见 error.rs）的前提下够用。
fn classify_tool_error(err: &ServerError) -> ToolErrorKind {
    let msg = &err.message;

    // 1) 基础设施类：AppError::Storage(_, ...) / Internal(_, ...) / Ai(_, ...)
    //    这几个的 #[error("...")] 前缀就是 "存储错误: " / "内部错误: " /
    //    "{0}"（Ai 是空包装，看不到稳定前缀）。Ai 走 message 没法判——
    //    默认归 Rejected，LLM 看到内容能知道是模型配置问题。
    if msg.starts_with("存储错误:") || msg.starts_with("内部错误:") {
        return ToolErrorKind::ServerError;
    }

    // 2) async-graphql 校验错的中英文前缀——这些 message 在 extensions
    // 里不会有 code。命中即 BadArgs。匹配的是 async-graphql 7 已知的
    // 几种格式，业务消息前缀（如 "资源不存在"）不会被误命中。
    let lower = msg.to_ascii_lowercase();
    let schema_signal = [
        "variable ",
        "unknown argument",
        "unknown field",
        "unknown type",
        "is required but not provided",
        "got invalid value",
        "failed to parse",
        "expected ",
        "must be",
        "cannot represent",
        "invalid value",
        "field \"",
    ];
    if schema_signal.iter().any(|p| lower.contains(p)) {
        return ToolErrorKind::BadArgs;
    }

    // 3) 兜底：业务消息（资源不存在 / 无权限 / 标签值不合法 / 邮箱已存在
    //    等）都是 Rejected。LLM 看到消息后能自行决定要不要重试。
    ToolErrorKind::Rejected
}

/// 把工具副作用映射为前端可订阅的领域事件。
/// 名字必须出现在 templates::TEMPLATES；新加 mutation 时同步追加。
pub fn classify_side_effect(
    name: &str,
    args: &serde_json::Value,
    data: &serde_json::Value,
) -> Option<SideEffect> {
    let get = |path: &[&str]| -> Option<String> {
        let mut cur = data;
        for p in path {
            cur = cur.get(*p)?;
        }
        cur.as_str().map(|s| s.to_string())
    };
    match name {
        "createEntry" => {
            let id = get(&["createEntry", "id"])?;
            Some(SideEffect::Entry { action: crate::domain::EntryAction::Create, id })
        }
        "updateEntry" => {
            let id = args.get("input").and_then(|v| v.get("id")).and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .or_else(|| get(&["updateEntry", "id"]))?;
            Some(SideEffect::Entry { action: crate::domain::EntryAction::Update, id })
        }
        "deleteEntry" => {
            let id = args.get("id").and_then(|v| v.as_str()).map(|s| s.to_string())?;
            Some(SideEffect::Entry { action: crate::domain::EntryAction::Delete, id })
        }
        "createLabeling" | "updateLabeling" | "deleteLabeling" => {
            let code = args
                .get("input")
                .and_then(|v| v.get("entryCode"))
                .or_else(|| args.get("entryCode"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())?;
            Some(SideEffect::Labeling { code })
        }
        "createComment" => {
            let code = args
                .get("input")
                .and_then(|v| v.get("entryCode"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())?;
            Some(SideEffect::Comment { code })
        }
        _ => None,
    }
}

/// 与 `service/ai.rs` 同款；Agent 复用以保证 preview 行为一致。
fn truncate_chars(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}
