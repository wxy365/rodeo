use std::sync::Arc;

use async_graphql::{Request as GraphQLRequest, Variables};
use ulid::Ulid;

use crate::api::graphql::{AppSchema, GraphqlContext};
use crate::domain::SideEffect;
use crate::error::AppError;
use crate::service::{AuthContext, Services};

pub struct ToolOutcome {
    pub ok: bool,
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
    let errors = resp.errors.len();

    if errors > 0 {
        let msg = resp
            .errors
            .first()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "工具返回错误".to_string());
        return Ok(ToolOutcome {
            ok: false,
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
        result_preview: preview,
        side_effect,
    })
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
