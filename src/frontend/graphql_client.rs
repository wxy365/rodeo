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
}

#[derive(Clone, serde::Deserialize)]
pub struct WorkspaceItem {
    pub workspace: Workspace,
    pub role: String,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub updated_at: String,
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
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditLog {
    pub id: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub at: String,
}

// ---------- 类型化查询/变更 ----------

pub async fn me() -> Result<Option<User>, String> {
    let data = graphql("query { me { id email name } }", json!({})).await?;
    Ok(data
        .get("me")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
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
        "query { workspaces { workspace { id name slug description } role } }",
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

pub async fn workspace_by_slug(slug: &str) -> Result<Option<Workspace>, String> {
    let data = graphql(
        "query($s: String!) { workspace(slug: $s) { id name slug description } }",
        json!({ "s": slug }),
    )
    .await?;
    Ok(data
        .get("workspace")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
}

pub async fn entries(workspace_id: &str) -> Result<Vec<Entry>, String> {
    let data = graphql(
        "query($id: ID!) { entries(workspaceId: $id) { code title detail updatedAt labels { labelName value } } }",
        json!({ "id": workspace_id }),
    )
    .await?;
    serde_json::from_value(data.get("entries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn create_entry(workspace_id: &str, title: &str) -> Result<Entry, String> {
    let data = graphql(
        "mutation($id: ID!, $t: String!) { createEntry(workspaceId: $id, title: $t) { code title detail updatedAt labels { labelName value } } }",
        json!({ "id": workspace_id, "t": title }),
    )
    .await?;
    serde_json::from_value(data.get("createEntry").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn label_schemas(workspace_id: &str) -> Result<Vec<LabelSchema>, String> {
    let data = graphql(
        "query($id: ID!) { labelSchemas(workspaceId: $id) { name title valueType enumValues } }",
        json!({ "id": workspace_id }),
    )
    .await?;
    serde_json::from_value(data.get("labelSchemas").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn set_labeling(entry_code: &str, label_name: &str, value: &Value) -> Result<Value, String> {
    let data = graphql(
        "mutation($c: String!, $n: String!, $v: JSON!) { setLabeling(entryCode: $c, labelName: $n, value: $v) { labelName value } }",
        json!({ "c": entry_code, "n": label_name, "v": value }),
    )
    .await?;
    Ok(data.get("setLabeling").cloned().unwrap_or(Value::Null))
}

pub async fn entry(code: &str) -> Result<Option<Entry>, String> {
    let data = graphql(
        "query($c: String!) { entry(code: $c) { code title detail updatedAt labels { labelName value } } }",
        json!({ "c": code }),
    )
    .await?;
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
    let data = graphql(
        "mutation($c: String!, $e: String!, $t: String!, $d: String!) { updateEntry(code: $c, expectedUpdatedAt: $e, title: $t, detail: $d) { code title detail updatedAt labels { labelName value } } }",
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
) -> Result<LabelSchema, String> {
    let data = graphql(
        "mutation($id: ID!, $n: String!, $t: String!, $vt: String!, $ev: [String!]!) { createLabelSchema(workspaceId: $id, name: $n, title: $t, valueType: $vt, enumValues: $ev) { name title valueType enumValues } }",
        json!({ "id": workspace_id, "n": name, "t": title, "vt": value_type, "ev": enum_values }),
    )
    .await?;
    serde_json::from_value(data.get("createLabelSchema").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn update_label_schema(
    workspace_id: &str,
    name: &str,
    title: &str,
    enum_values: &[String],
) -> Result<LabelSchema, String> {
    let data = graphql(
        "mutation($id: ID!, $n: String!, $t: String!, $ev: [String!]!) { updateLabelSchema(workspaceId: $id, name: $n, title: $t, enumValues: $ev) { name title valueType enumValues } }",
        json!({ "id": workspace_id, "n": name, "t": title, "ev": enum_values }),
    )
    .await?;
    serde_json::from_value(data.get("updateLabelSchema").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn audit_logs(workspace_id: &str) -> Result<Vec<AuditLog>, String> {
    let data = graphql(
        "query($id: ID!) { auditLogs(workspaceId: $id) { id action resourceType resourceId at } }",
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
