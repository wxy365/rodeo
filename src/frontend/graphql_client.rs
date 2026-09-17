use serde_json::{json, Value};

pub const TOKEN_KEY: &str = "rodeo_jwt";

// ---------- Token 存储（仅 WASM 可用） ----------

#[cfg(target_arch = "wasm32")]
pub fn set_token(token: &str) {
    use gloo_storage::{LocalStorage, Storage};
    let _ = LocalStorage::set(TOKEN_KEY, token);
}

#[cfg(target_arch = "wasm32")]
pub fn clear_token() {
    use gloo_storage::{LocalStorage, Storage};
    LocalStorage::delete(TOKEN_KEY);
}

#[cfg(target_arch = "wasm32")]
pub fn get_token() -> Option<String> {
    use gloo_storage::{LocalStorage, Storage};
    LocalStorage::get::<String>(TOKEN_KEY).ok()
}

#[cfg(not(target_arch = "wasm32"))]
pub fn set_token(_token: &str) {}
#[cfg(not(target_arch = "wasm32"))]
pub fn clear_token() {}
#[cfg(not(target_arch = "wasm32"))]
pub fn get_token() -> Option<String> {
    None
}

// ---------- 布局偏好存储（侧栏收缩状态，仅 WASM 生效） ----------

pub const SIDEBAR_KEY: &str = "rodeo_sidebar_collapsed";

#[cfg(target_arch = "wasm32")]
pub fn get_sidebar_collapsed() -> bool {
    use gloo_storage::{LocalStorage, Storage};
    LocalStorage::get::<bool>(SIDEBAR_KEY).unwrap_or(false)
}

#[cfg(target_arch = "wasm32")]
pub fn set_sidebar_collapsed(collapsed: bool) {
    use gloo_storage::{LocalStorage, Storage};
    let _ = LocalStorage::set(SIDEBAR_KEY, collapsed);
}

#[cfg(not(target_arch = "wasm32"))]
pub fn get_sidebar_collapsed() -> bool {
    false
}
#[cfg(not(target_arch = "wasm32"))]
pub fn set_sidebar_collapsed(_collapsed: bool) {}

// ---------- 底层 GraphQL 请求 ----------

#[cfg(target_arch = "wasm32")]
pub async fn graphql(query: &str, variables: Value) -> Result<Value, String> {
    use gloo_net::http::Request;

    let body = json!({ "query": query, "variables": variables });
    let mut req = Request::post("/api/graphql").header("Content-Type", "application/json");
    if let Some(token) = get_token() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let resp = req
        .body(serde_json::to_string(&body).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json_val: Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(errors) = json_val.get("errors").and_then(|e| e.as_array()) {
        if let Some(first) = errors.first() {
            let msg = first
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("GraphQL 错误");
            return Err(msg.to_string());
        }
    }
    Ok(json_val.get("data").cloned().unwrap_or(Value::Null))
}

#[cfg(not(target_arch = "wasm32"))]
pub async fn graphql(_query: &str, _variables: Value) -> Result<Value, String> {
    Err("GraphQL 客户端仅在浏览器可用".to_string())
}

// ---------- 响应类型 ----------

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: String,
}

#[derive(Clone, serde::Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub description: String,
    /// 软删除时间；None = 正常。查询里没请求该字段时为 None，与「未删除」不可区分——
    /// 需要区分的地方（设置页、工作空间列表）都要显式请求 `deletedAt`。
    /// 本结构体没有 rename_all，多词字段必须逐个 rename，否则会被当成未知字段静默丢弃。
    #[serde(rename = "deletedAt", default)]
    pub deleted_at: Option<String>,
}

#[derive(Clone, serde::Deserialize)]
pub struct WorkspaceItem {
    pub workspace: Workspace,
    pub role: String,
}

/// 创建人 / 更新人账号的精简投影。账号已删除时服务端返回 None。
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBrief {
    pub id: String,
    pub email: String,
    pub name: String,
}

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

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub created_at: String,
    pub updated_at: String,
    /// 创建人 / 更新人账号 id。展示与账号筛选用下面的 `*_account`。
    pub created_by: String,
    pub updated_by: String,
    #[serde(default)]
    pub created_by_account: Option<AccountBrief>,
    #[serde(default)]
    pub updated_by_account: Option<AccountBrief>,
    /// 归档时间；未归档为 None。决定了详情面板显示「归档」还是「取消归档」。
    #[serde(default)]
    pub archived_at: Option<String>,
    pub labels: Vec<Labeling>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Labeling {
    pub label_name: String,
    pub value: Value,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelSchema {
    pub name: String,
    pub title: String,
    pub value_type: String,
    pub enum_values: Vec<String>,
    pub color: Option<String>,
    #[serde(default)]
    pub value_colors: Value,
    /// 值类型之外附加的属性。`multi` 允许多值（数组）。
    #[serde(default)]
    pub multi: bool,
    /// 时间 / 日期型标签的展示格式（常规表示法，如 `YYYY-MM-DD HH:mm:ss`）。
    #[serde(default)]
    pub format: Option<String>,
    /// 打这个标签时预填的值；JSON `null` 表示没有默认值。
    #[serde(default)]
    pub default_value: Value,
    /// 金额型标签的货币符号。
    #[serde(default)]
    pub currency_symbol: Option<String>,
    /// 数值型标签的单位后缀。
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLog {
    pub id: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub at: String,
    /// 变更前的资源快照（JSON 字符串），无则 None。
    pub before: Option<String>,
    /// 变更后的资源快照（JSON 字符串），删除类操作为 None。
    pub after: Option<String>,
}

// ---------- 共享字段列表 ----------
// 所有请求 Entry / LabelSchema 的查询都从这里取字段，避免新增字段时六处手改漏一处。

const ENTRY_FIELDS: &str = "code title detail createdAt updatedAt createdBy updatedBy \
     createdByAccount { id name email } updatedByAccount { id name email } archivedAt \
     labels { labelName value }";

const LABEL_SCHEMA_FIELDS: &str =
    "name title valueType enumValues color valueColors multi format currencySymbol unit defaultValue";

/// 组装 `LabelSchemaAttrsInput`（camelCase）。
///
/// 服务端 update 是「整体替换」，所以调用方必须始终传齐所有键——
/// 这里用 Option → null 也保留键位，不会把未改的属性抹掉。
pub fn label_attrs(
    multi: bool,
    format: Option<&str>,
    currency_symbol: Option<&str>,
    unit: Option<&str>,
    default_value: &Value,
) -> Value {
    json!({
        "multi": multi,
        "format": format,
        "currencySymbol": currency_symbol,
        "unit": unit,
        "defaultValue": default_value,
    })
}

// ---------- 类型化查询/变更 ----------

pub async fn me() -> Result<Option<User>, String> {
    let data = graphql("query { me { id email name } }", json!({})).await?;
    Ok(data
        .get("me")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
}

/// 免登录：登录页据此决定是否展示注册入口。取不到时按「关闭」处理（保守，宁可少显示）。
pub async fn allow_registration() -> bool {
    graphql("query { serverConfig { allowRegistration } }", json!({}))
        .await
        .ok()
        .and_then(|d| d.get("serverConfig").cloned())
        .and_then(|v| v.get("allowRegistration").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// 服务端吊销当前令牌，再清理本地存储。吊销请求失败（离线等）也要清本地，
/// 否则用户会卡在「退不出去」的状态——代价是那张令牌要到过期才失效。
pub async fn logout() {
    let _ = graphql("mutation { logout }", json!({})).await;
    clear_token();
}

pub async fn login(email: &str, password: &str) -> Result<(String, User), String> {
    let data = graphql(
        "mutation($e: String!, $p: String!) { login(email: $e, password: $p) { token account { id email name } } }",
        json!({ "e": email, "p": password }),
    )
    .await?;
    let r = data.get("login").ok_or("登录响应缺失")?;
    let token = r
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or("token 缺失")?
        .to_string();
    let account: User = serde_json::from_value(r.get("account").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())?;
    Ok((token, account))
}

pub async fn register(email: &str, name: &str, password: &str) -> Result<(String, User), String> {
    let data = graphql(
        "mutation($e: String!, $n: String!, $p: String!) { register(email: $e, name: $n, password: $p) { token account { id email name } } }",
        json!({ "e": email, "n": name, "p": password }),
    )
    .await?;
    let r = data.get("register").ok_or("注册响应缺失")?;
    let token = r
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or("token 缺失")?
        .to_string();
    let account: User = serde_json::from_value(r.get("account").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())?;
    Ok((token, account))
}

pub async fn workspaces() -> Result<Vec<WorkspaceItem>, String> {
    let data = graphql(
        "query { workspaces { workspace { id name slug description deletedAt } role } }",
        json!({}),
    )
    .await?;
    serde_json::from_value(data.get("workspaces").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn create_workspace(name: &str, description: &str) -> Result<Workspace, String> {
    let data = graphql(
        "mutation($n: String!, $d: String) { createWorkspace(name: $n, description: $d) { id name slug description } }",
        json!({ "n": name, "d": description }),
    )
    .await?;
    serde_json::from_value(data.get("createWorkspace").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 改名称/描述（Maintainer+）。`slug` 传 Some 才会改地址，且服务端要求 Owner——
/// 因此非 Owner 保存名称时必须传 None，否则会被 Owner 校验拦下。
pub async fn update_workspace(
    workspace_id: &str,
    name: &str,
    description: &str,
    slug: Option<&str>,
) -> Result<Workspace, String> {
    let data = graphql(
        "mutation($id: ID!, $n: String!, $d: String!, $s: String) { \
         updateWorkspace(workspaceId: $id, name: $n, description: $d, slug: $s) { id name slug description } }",
        json!({ "id": workspace_id, "n": name, "d": description, "s": slug }),
    )
    .await?;
    serde_json::from_value(data.get("updateWorkspace").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 一步转让所有权（Owner）：对方升为 Owner，自己降为 Maintainer。
pub async fn transfer_owner(workspace_id: &str, account_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!, $a: ID!) { transferOwner(workspaceId: $id, accountId: $a) }",
        json!({ "id": workspace_id, "a": account_id }),
    )
    .await?;
    Ok(data.get("transferOwner").and_then(|v| v.as_bool()).unwrap_or(false))
}

/// 软删除工作空间（Owner）。数据保留，可恢复。
pub async fn delete_workspace(workspace_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { deleteWorkspace(workspaceId: $id) }",
        json!({ "id": workspace_id }),
    )
    .await?;
    Ok(data.get("deleteWorkspace").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn restore_workspace(workspace_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { restoreWorkspace(workspaceId: $id) }",
        json!({ "id": workspace_id }),
    )
    .await?;
    Ok(data.get("restoreWorkspace").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn workspace_by_slug(slug: &str) -> Result<Option<Workspace>, String> {
    let data = graphql(
        "query($s: String!) { workspace(slug: $s) { id name slug description deletedAt } }",
        json!({ "s": slug }),
    )
    .await?;
    Ok(data
        .get("workspace")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
}

pub async fn entries(workspace_id: &str) -> Result<Vec<Entry>, String> {
    let q = format!("query($id: ID!) {{ entries(workspaceId: $id) {{ {ENTRY_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("entries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn create_entry(workspace_id: &str, title: &str) -> Result<Entry, String> {
    let q = format!(
        "mutation($id: ID!, $t: String!) {{ createEntry(workspaceId: $id, title: $t) {{ {ENTRY_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id, "t": title })).await?;
    serde_json::from_value(data.get("createEntry").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn label_schemas(workspace_id: &str) -> Result<Vec<LabelSchema>, String> {
    let q = format!(
        "query($id: ID!) {{ labelSchemas(workspaceId: $id) {{ {LABEL_SCHEMA_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("labelSchemas").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 场景 / 语气行只请求 name prompt——显式列出字段，前端类型才不会跟着服务端结构悄悄漂移。
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

pub async fn set_labeling(entry_code: &str, label_name: &str, value: &Value) -> Result<Value, String> {
    // $v 可空：无值标签的值是 JSON null，非空标量 JSON! 会被服务端拒绝。
    let data = graphql(
        "mutation($c: String!, $n: String!, $v: JSON) { setLabeling(entryCode: $c, labelName: $n, value: $v) { labelName value } }",
        json!({ "c": entry_code, "n": label_name, "v": value }),
    )
    .await?;
    Ok(data.get("setLabeling").cloned().unwrap_or(Value::Null))
}

/// 批量写标签：`labelings` 是 `[{name, value}]`，一次性原子写入所有选中条目。
/// 返回写入的 Labeling 条数。服务端任一取值非法则整批失败。
pub async fn set_labelings(entry_codes: &[String], labelings: &Value) -> Result<i64, String> {
    let data = graphql(
        "mutation($c: [String!]!, $l: [LabelingInput!]!) { setLabelings(entryCodes: $c, labelings: $l) }",
        json!({ "c": entry_codes, "l": labelings }),
    )
    .await?;
    Ok(data.get("setLabelings").and_then(|v| v.as_i64()).unwrap_or(0))
}

pub async fn entry(code: &str) -> Result<Option<Entry>, String> {
    let q = format!("query($c: String!) {{ entry(code: $c) {{ {ENTRY_FIELDS} }} }}");
    let data = graphql(&q, json!({ "c": code })).await?;
    Ok(data
        .get("entry")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
}

pub async fn update_entry(
    code: &str,
    expected_updated_at: &str,
    title: &str,
    detail: &str,
) -> Result<Entry, String> {
    let q = format!(
        "mutation($c: String!, $e: String!, $t: String!, $d: String!) {{ \
         updateEntry(code: $c, expectedUpdatedAt: $e, title: $t, detail: $d) {{ {ENTRY_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({ "c": code, "e": expected_updated_at, "t": title, "d": detail }),
    )
    .await?;
    serde_json::from_value(data.get("updateEntry").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn delete_entry(code: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!) { deleteEntry(code: $c) }",
        json!({ "c": code }),
    )
    .await?;
    Ok(data
        .get("deleteEntry")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 归档条目：移出默认视图，数据保留。返回服务端确认。
pub async fn archive_entry(code: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!) { archiveEntry(code: $c) }",
        json!({ "c": code }),
    )
    .await?;
    Ok(data
        .get("archiveEntry")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 取消归档。
pub async fn unarchive_entry(code: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!) { unarchiveEntry(code: $c) }",
        json!({ "c": code }),
    )
    .await?;
    Ok(data
        .get("unarchiveEntry")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 已归档条目，按归档时间倒序。
pub async fn archived_entries(workspace_id: &str) -> Result<Vec<Entry>, String> {
    let q = format!(
        "query($id: ID!) {{ archivedEntries(workspaceId: $id) {{ {ENTRY_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("archivedEntries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn remove_labeling(entry_code: &str, label_name: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!, $n: String!) { removeLabeling(entryCode: $c, labelName: $n) }",
        json!({ "c": entry_code, "n": label_name }),
    )
    .await?;
    Ok(data
        .get("removeLabeling")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn create_label_schema(
    workspace_id: &str,
    name: &str,
    title: &str,
    value_type: &str,
    enum_values: &[String],
    attrs: &Value,
    color: Option<&str>,
    value_colors: &Value,
) -> Result<LabelSchema, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $t: String!, $vt: String!, $ev: [String!]!, \
         $attrs: LabelSchemaAttrsInput!, $color: String, $vc: JSON!) {{ \
         createLabelSchema(workspaceId: $id, name: $n, title: $t, valueType: $vt, \
         enumValues: $ev, attrs: $attrs, color: $color, valueColors: $vc) \
         {{ {LABEL_SCHEMA_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id, "n": name, "t": title, "vt": value_type,
            "ev": enum_values, "attrs": attrs, "color": color, "vc": value_colors,
        }),
    )
    .await?;
    serde_json::from_value(data.get("createLabelSchema").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 改标签（不改类型）。attrs 必须传齐四个键：服务端整体替换，漏传会清空已有属性。
pub async fn update_label_schema(
    workspace_id: &str,
    name: &str,
    title: &str,
    enum_values: &[String],
    attrs: &Value,
    color: Option<&str>,
    value_colors: &Value,
) -> Result<LabelSchema, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $t: String!, $ev: [String!]!, \
         $attrs: LabelSchemaAttrsInput!, $color: String, $vc: JSON!) {{ \
         updateLabelSchema(workspaceId: $id, name: $n, title: $t, \
         enumValues: $ev, attrs: $attrs, color: $color, valueColors: $vc) \
         {{ {LABEL_SCHEMA_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id, "n": name, "t": title, "ev": enum_values,
            "attrs": attrs, "color": color, "vc": value_colors,
        }),
    )
    .await?;
    serde_json::from_value(data.get("updateLabelSchema").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn audit_logs(workspace_id: &str) -> Result<Vec<AuditLog>, String> {
    let data = graphql(
        "query($id: ID!) { auditLogs(workspaceId: $id) { id action resourceType resourceId at before after } }",
        json!({ "id": workspace_id }),
    )
    .await?;
    serde_json::from_value(data.get("auditLogs").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn my_role(workspace_id: &str) -> Result<String, String> {
    let data = graphql(
        "query($id: ID!) { myRole(workspaceId: $id) }",
        json!({ "id": workspace_id }),
    )
    .await?;
    Ok(data
        .get("myRole")
        .and_then(|v| v.as_str())
        .unwrap_or("none")
        .to_string())
}

// ---------- 成员（Member） ----------

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Member {
    pub account_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub joined_at: String,
}

const MEMBER_FIELDS: &str = "accountId email name role joinedAt";

pub async fn members(workspace_id: &str) -> Result<Vec<Member>, String> {
    let q = format!("query($id: ID!) {{ members(workspaceId: $id) {{ {MEMBER_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("members").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

// ---------- 邀请（Invite） ----------

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invite {
    pub workspace_id: String,
    pub workspace_name: String,
    pub workspace_slug: String,
    pub account_id: String,
    pub email: String,
    pub name: String,
    pub role: String,
    pub invited_by: String,
    pub created_at: String,
}

const INVITE_FIELDS: &str = "workspaceId workspaceName workspaceSlug accountId email name role invitedBy createdAt";

pub async fn invite_member(
    workspace_id: &str,
    email: &str,
    role: &str,
) -> Result<Invite, String> {
    let q = format!(
        "mutation($id: ID!, $e: String!, $r: String!) {{ \
         inviteMember(workspaceId: $id, email: $e, role: $r) {{ {INVITE_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id, "e": email, "r": role })).await?;
    serde_json::from_value(data.get("inviteMember").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 某工作空间待接受的邀请（管理成员页用）。
pub async fn invites(workspace_id: &str) -> Result<Vec<Invite>, String> {
    let q = format!("query($id: ID!) {{ invites(workspaceId: $id) {{ {INVITE_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("invites").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 我收到的、尚未接受的邀请（收件箱用）。
pub async fn my_invites() -> Result<Vec<Invite>, String> {
    let q = format!("query {{ myInvites {{ {INVITE_FIELDS} }} }}");
    let data = graphql(&q, json!({})).await?;
    serde_json::from_value(data.get("myInvites").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn accept_invite(workspace_id: &str) -> Result<Member, String> {
    let q = format!(
        "mutation($id: ID!) {{ acceptInvite(workspaceId: $id) {{ {MEMBER_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("acceptInvite").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn decline_invite(workspace_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { declineInvite(workspaceId: $id) }",
        json!({ "id": workspace_id }),
    )
    .await?;
    Ok(data.get("declineInvite").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn revoke_invite(workspace_id: &str, account_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!, $a: ID!) { revokeInvite(workspaceId: $id, accountId: $a) }",
        json!({ "id": workspace_id, "a": account_id }),
    )
    .await?;
    Ok(data.get("revokeInvite").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn update_member_role(
    workspace_id: &str,
    account_id: &str,
    role: &str,
) -> Result<Member, String> {
    let q = format!(
        "mutation($id: ID!, $a: ID!, $r: String!) {{ \
         updateMemberRole(workspaceId: $id, accountId: $a, role: $r) {{ {MEMBER_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id, "a": account_id, "r": role })).await?;
    serde_json::from_value(data.get("updateMemberRole").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn remove_member(workspace_id: &str, account_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!, $a: ID!) { removeMember(workspaceId: $id, accountId: $a) }",
        json!({ "id": workspace_id, "a": account_id }),
    )
    .await?;
    Ok(data.get("removeMember").and_then(|v| v.as_bool()).unwrap_or(false))
}

// ---------- 视图（View） ----------

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSort {
    pub field: String,
    pub desc: bool,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: String,
    pub name: String,
    pub query: Value,
    pub query_expr: String,
    pub sort: ViewSort,
    pub columns: Vec<String>,
    pub is_shared: bool,
    pub owner_id: String,
    #[serde(default)]
    pub title_colors: Value,
    pub entry_count: i64,
    /// 默认视图：始终存在、不可删除，列表中置顶。
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPage {
    pub items: Vec<Entry>,
    pub total: i64,
    pub page: i64,
    pub page_size: i64,
    #[serde(default)]
    pub label_names: Vec<String>,
}

const VIEW_FIELDS: &str = "id name query queryExpr sort { field desc } columns isShared ownerId \
     titleColors entryCount isDefault";

pub async fn views(workspace_id: &str) -> Result<Vec<View>, String> {
    let q = format!("query($id: ID!) {{ views(workspaceId: $id) {{ {VIEW_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("views").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn query_entries(
    workspace_id: &str,
    query: &Value,
    sort_field: &str,
    desc: bool,
    page: i64,
    page_size: i64,
) -> Result<EntryPage, String> {
    let q = format!(
        "query($id: ID!, $q: JSON, $s: SortInput, $p: PageInput) {{ \
         queryEntries(workspaceId: $id, query: $q, sort: $s, page: $p) {{ \
         items {{ {ENTRY_FIELDS} }} total page pageSize labelNames }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id,
            "q": query,
            "s": { "field": sort_field, "desc": desc },
            "p": { "page": page, "pageSize": page_size },
        }),
    )
    .await?;
    serde_json::from_value(data.get("queryEntries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn create_view(
    workspace_id: &str,
    name: &str,
    query: &Value,
    sort_field: &str,
    desc: bool,
    columns: &[String],
    is_shared: bool,
    title_colors: &Value,
) -> Result<View, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $q: JSON!, $s: SortInput, $c: [String!]!, $sh: Boolean!, $tc: JSON!) {{ \
         createView(workspaceId: $id, name: $n, query: $q, sort: $s, columns: $c, isShared: $sh, titleColors: $tc) {{ {VIEW_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id, "n": name, "q": query,
            "s": { "field": sort_field, "desc": desc }, "c": columns, "sh": is_shared,
            "tc": title_colors,
        }),
    )
    .await?;
    serde_json::from_value(data.get("createView").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn update_view(
    id: &str,
    name: &str,
    query: &Value,
    sort_field: &str,
    desc: bool,
    columns: &[String],
    is_shared: bool,
    title_colors: &Value,
) -> Result<View, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $q: JSON!, $s: SortInput, $c: [String!]!, $sh: Boolean!, $tc: JSON!) {{ \
         updateView(id: $id, name: $n, query: $q, sort: $s, columns: $c, isShared: $sh, titleColors: $tc) {{ {VIEW_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": id, "n": name, "q": query,
            "s": { "field": sort_field, "desc": desc }, "c": columns, "sh": is_shared,
            "tc": title_colors,
        }),
    )
    .await?;
    serde_json::from_value(data.get("updateView").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn delete_view(id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { deleteView(id: $id) }",
        json!({ "id": id }),
    )
    .await?;
    Ok(data.get("deleteView").and_then(|v| v.as_bool()).unwrap_or(false))
}

pub async fn parse_view_query(workspace_id: &str, expr: &str) -> Result<Value, String> {
    let data = graphql(
        "query($id: ID!, $e: String!) { parseViewQuery(workspaceId: $id, expr: $e) }",
        json!({ "id": workspace_id, "e": expr }),
    )
    .await?;
    Ok(data.get("parseViewQuery").cloned().unwrap_or(Value::Null))
}

pub async fn format_view_query(workspace_id: &str, query: &Value) -> Result<String, String> {
    let data = graphql(
        "query($id: ID!, $q: JSON!) { formatViewQuery(workspaceId: $id, query: $q) }",
        json!({ "id": workspace_id, "q": query }),
    )
    .await?;
    Ok(data
        .get("formatViewQuery")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::Workspace;

    #[test]
    fn workspace_maps_deleted_at_from_camel_case_json() {
        // 结构体没有 rename_all，这里盯住那个坑：多词字段必须显式 rename，
        // 否则 GraphQL 的 deletedAt 会被当成未知字段丢掉，删除态在 UI 上永远不出现。
        let deleted: Workspace = serde_json::from_value(serde_json::json!({
            "id": "1", "name": "n", "slug": "s", "description": "d",
            "deletedAt": "2026-01-01T00:00:00+00:00"
        }))
        .unwrap();
        assert_eq!(deleted.deleted_at.as_deref(), Some("2026-01-01T00:00:00+00:00"));

        let live: Workspace = serde_json::from_value(serde_json::json!({
            "id": "1", "name": "n", "slug": "s", "description": "d", "deletedAt": null
        }))
        .unwrap();
        assert!(live.deleted_at.is_none());

        // 老查询没请求 deletedAt 时也不能报错。
        let bare: Workspace = serde_json::from_value(serde_json::json!({
            "id": "1", "name": "n", "slug": "s", "description": "d"
        }))
        .unwrap();
        assert!(bare.deleted_at.is_none());
    }
}
