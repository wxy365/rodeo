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

// ---------- 最近使用过的颜色（按工作空间隔离，仅 WASM 生效） ----------

/// 留存条数：再多颜色选择器就被撑开了，也不再是「最近」。
pub const RECENT_COLORS_MAX: usize = 6;

#[cfg(target_arch = "wasm32")]
fn recent_colors_key(ws_id: &str) -> String {
    format!("rodeo_recent_colors_{ws_id}")
}

#[cfg(target_arch = "wasm32")]
pub fn get_recent_colors(ws_id: &str) -> Vec<String> {
    use gloo_storage::{LocalStorage, Storage};
    LocalStorage::get::<Vec<String>>(recent_colors_key(ws_id)).unwrap_or_default()
}

/// 记一笔用过的颜色：统一小写、去重、最新在前、超出上限截断，落盘后返回新列表。
///
/// 只做「记下来」这一件事——哪些颜色值得记（比如标准色板里已有的）由调用方判断。
#[cfg(target_arch = "wasm32")]
pub fn push_recent_color(ws_id: &str, color: &str) -> Vec<String> {
    use gloo_storage::{LocalStorage, Storage};
    let mut list = get_recent_colors(ws_id);
    let c = color.trim().to_ascii_lowercase();
    if c.is_empty() {
        return list;
    }
    list.retain(|x| x != &c);
    list.insert(0, c);
    list.truncate(RECENT_COLORS_MAX);
    let _ = LocalStorage::set(recent_colors_key(ws_id), &list);
    list
}

#[cfg(not(target_arch = "wasm32"))]
pub fn get_recent_colors(_ws_id: &str) -> Vec<String> {
    Vec::new()
}
#[cfg(not(target_arch = "wasm32"))]
pub fn push_recent_color(_ws_id: &str, _color: &str) -> Vec<String> {
    Vec::new()
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
    /// 系统管理员标记。`#[serde(default)]` 是必需的：`login` / `register` 的查询里
    /// 若没选 `isAdmin`，字段缺失会整条反序列化失败，而缺省 false 恰好与
    /// 「这个账号不是管理员」在语义上一致。
    #[serde(default)]
    pub is_admin: bool,
    /// 是否已设置本地密码。OAuth-only 账号是 false——它在账号页决定要不要展示
    /// 「设置密码」section。`#[serde(default)]` 让旧查询（`me` / `login` /
    /// `register` 等）不报字段缺失错。
    #[serde(default)]
    pub has_password: bool,
}

/// 账号已绑定的第三方登录方式（`myOAuthBindings` 列表项）。
///
/// `bound_at` 在服务端是 `DateTime<Utc>`，async-graphql 序列化成 RFC3339 字符串
/// 到达前端，所以这边按 String 收；展示时切片取前 10 位（`YYYY-MM-DD`）即可。
/// `#[serde(default)]` 让老查询忘了请求 `email` / `displayName` 时也不炸。
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthBinding {
    pub provider: String,
    pub external_id: String,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    pub bound_at: String,
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
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBrief {
    /// `#[serde(default)]` 让 GraphQL 片段不包含此字段时反序列化落空串——
    /// 给公开页 `GqlAccountPublic { name }` 这种最小投影留口子，避免共享同一类型
    /// 的全字段详情页（id/email 都拉到）因少字段就报错。空串表达式仍按「未渲染」
    /// 处理（上游链路都是 `if account.id.is_empty()` 之类显式判空）。
    #[serde(default)]
    pub id: String,
    #[serde(default)]
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Labeling {
    pub label_name: String,
    pub value: Value,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub entry_code: String,
    pub body: String,
    pub created_by: String,
    pub updated_by: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub created_by_account: Option<AccountBrief>,
    #[serde(default)]
    pub updated_by_account: Option<AccountBrief>,
}

/// 关联语义：服务端内部 tag 形态是 `{"type":"custom","value":"..."}`，
/// 内置形态只有 `{"type":"contains"}` 等。这里反过来只暴露前端用得到的字段：
/// `kind` 用于图标与配色，`display` 用于中文文案，`value` 仅自定义有值。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelationSemantic {
    pub kind: String,
    pub display: String,
    #[serde(default)]
    pub value: Option<String>,
}

/// Entry 分享行（owner 面板用）。**不带 `url`**：前端用 `id` +
/// `window.location.origin` 自拼 `"/share/" + share.id`——避免在
/// GraphQL 层抓 HTTP 上下文。字段名来自服务端 `GqlEntryShare` 的 camelCase
/// 投影。
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryShare {
    pub id: String,
    pub entry_code: String,
    pub workspace_id: String,
    pub created_at: String,
    pub created_by: String,
    pub revoked: bool,
}

/// 公开分享页拿到的 Entry 子集（`GqlSharedEntry` 投影）。复用现有
/// `AccountBrief` / `Labeling` 类型以减少重复定义；详情页与公开页
/// 的标签结构是同一份事实。
///
/// 同时派生 `Serialize` 是让 `leptos::Resource::new` 满足 `JsonSerdeCodec`
/// 内部 `Encoder<T>` / `Decoder<T>` 约束——SSR 把 Resource 序列化到
/// hydration script 里需要 `Serialize`，hydrate 后还要 `Deserialize`。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedEntry {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub created_by_account: Option<AccountBrief>,
    pub labels: Vec<Labeling>,
}

/// `/share/{token}` 全屏只读视图的返回类型——与 `SharedEntry` 的字段全部
/// `entry.*` 嵌套，额外补 workspace / comments / attachments / label_schemas /
/// members。所有 PII 字段（author email / id 等）在 GraphQL 端就已经裁掉。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryFullPublic {
    pub entry: SharedEntry,
    pub workspace_name: String,
    pub workspace_slug: String,
    pub comments: Vec<SharedComment>,
    pub attachments: Vec<SharedAttachment>,
    pub label_schemas: Vec<LabelSchema>,
    pub members: Vec<MemberPublic>,
}

/// 公开分享评论投影：仅作者名 + 时间，id 暴露是为了让前端在 hooks /
/// reactive 流里有稳定 key（实际是「键」而非「暴露」）。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedComment {
    pub id: String,
    pub body: String,
    pub author_name: String,
    pub created_at: String,
    pub updated_at: String,
    pub edited: bool,
}

/// 公开分享附件投影：与 `Attachment` 等价但不带 `created_by` 系列字段——前者
/// 是说「有人上传了某文件」，后者是「谁上传的」，后者与匿名读者无关。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedAttachment {
    pub id: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    pub url: String,
    pub created_at: String,
}

/// 公开分享成员投影：仅 id + name。`role` / `email` 不暴露。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberPublic {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Relation {
    pub id: String,
    pub workspace_id: String,
    pub from_code: String,
    pub to_code: String,
    pub semantic: RelationSemantic,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    /// 关联对端条目标题（不是当前 entry 的那一头）。后端查不到时为空串，
    /// 列表里退化成显示 entry code。
    #[serde(default)]
    pub other_title: String,
    /// 服务端按查询条目是 from / to 给出方向："outgoing" / "incoming" / null。
    #[serde(default)]
    pub direction: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GqlMessage {
    pub id: String,
    pub workspace_id: String,
    pub workspace_name: String,
    /// 工作空间 slug（URL 段），点击消息跳转 Entry 全屏时用。
    /// 服务端在工作空间已被硬删时回空串，前端据此跳过跳转、退化为只标已读。
    pub workspace_slug: String,
    pub entry_code: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub actor_id: String,
    pub actor_name: String,
    pub preview: String,
    pub read: bool,
    pub created_at: String,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub id: String,
    pub entry_code: String,
    pub filename: String,
    pub content_type: String,
    pub size: i64,
    /// 服务端拼好的下载路径。客户端不要自己拼。
    pub url: String,
    pub created_at: String,
    pub created_by: String,
    #[serde(default)]
    pub created_by_account: Option<AccountBrief>,
}

const ATTACHMENT_FIELDS: &str =
    "id entryCode filename contentType size url createdAt createdBy createdByAccount { id email name }";

#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
    /// 继承/覆盖关系：`[{ kind, other, otherValue, ownValue }]`，`kind` 为 inherit|override。
    #[serde(default)]
    pub links: Value,
    /// 是否可作为视图排序键。`true` 时 EntryIndexService 启动期灌库标签索引。
    #[serde(default)]
    pub sortable: bool,
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

const COMMENT_FIELDS: &str = "id entryCode body createdAt updatedAt createdBy updatedBy \
     createdByAccount { id name email } updatedByAccount { id name email }";

const RELATION_FIELDS: &str =
    "id workspaceId fromCode toCode direction otherTitle createdBy createdAt updatedAt \
     semantic { kind display value }";

const LABEL_SCHEMA_FIELDS: &str =
    "name title valueType enumValues color valueColors multi format currencySymbol unit defaultValue links sortable";

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
    links: &Value,
    sortable: bool,
) -> Value {
    json!({
        "multi": multi,
        "format": format,
        "currencySymbol": currency_symbol,
        "unit": unit,
        "defaultValue": default_value,
        "links": links,
        "sortable": sortable,
    })
}

// ---------- 类型化查询/变更 ----------

pub async fn me() -> Result<Option<User>, String> {
    let data = graphql(
        "query { me { id email name isAdmin hasPassword } }",
        json!({}),
    )
    .await?;
    Ok(data
        .get("me")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
}

/// 服务端吊销当前令牌，再清理本地存储。吊销请求失败（离线等）也要清本地，
/// 否则用户会卡在「退不出去」的状态——代价是那张令牌要到过期才失效。
pub async fn logout() {
    let _ = graphql("mutation { logout }", json!({})).await;
    clear_token();
}

/// 账号管理列表里的一个账号。
///
/// `rename_all` 必需：请求的是 `isAdmin` / `createdAt`，少了它 `is_admin` 会因
/// 「字段缺失」整条反序列化失败——与 `CreatedAccount` 上踩过的是同一个坑。
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAccount {
    pub id: String,
    pub email: String,
    pub name: String,
    pub is_admin: bool,
    /// `"active"` / `"frozen"` / `"deactivated"`。
    pub status: String,
    pub created_at: String,
    /// 配置里 `auth.builtin.admin_email` 指定的那个账号。它的「冻结 / 注销」按钮
    /// 要禁用——服务端也会拒，但摆一个点了必然失败的按钮只会让人白点。
    pub is_builtin: bool,
}

pub async fn accounts() -> Result<Vec<AdminAccount>, String> {
    let data = graphql(
        "query { accounts { id email name isAdmin status createdAt isBuiltin } }",
        json!({}),
    )
    .await?;
    serde_json::from_value(data.get("accounts").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 冻结 / 解冻 / 注销。`status` 取 `"active"` / `"frozen"` / `"deactivated"`。
pub async fn set_account_status(account_id: &str, status: &str) -> Result<AdminAccount, String> {
    let data = graphql(
        "mutation($id: ID!, $s: String!) { \
         setAccountStatus(accountId: $id, status: $s) { id email name isAdmin status createdAt isBuiltin } }",
        json!({ "id": account_id, "s": status }),
    )
    .await?;
    serde_json::from_value(data.get("setAccountStatus").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 改账号的姓名与管理员标记。状态变更走 [`set_account_status`]——那一条还要吊销令牌、
/// 注销时释放邮箱索引，和这里的字段覆盖不是一回事。
pub async fn update_account(
    account_id: &str,
    name: &str,
    is_admin: bool,
) -> Result<AdminAccount, String> {
    let data = graphql(
        "mutation($id: ID!, $n: String!, $a: Boolean!) { \
         updateAccount(accountId: $id, name: $n, isAdmin: $a) { id email name isAdmin status createdAt isBuiltin } }",
        json!({ "id": account_id, "n": name, "a": is_admin }),
    )
    .await?;
    serde_json::from_value(data.get("updateAccount").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn login(email: &str, password: &str) -> Result<(String, User), String> {
    let data = graphql(
        "mutation($e: String!, $p: String!) { login(email: $e, password: $p) { token account { id email name isAdmin hasPassword } } }",
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

/// 改密。服务端在成功后会吊销旧令牌并回一张新的，所以这里必须**立刻**把本地那张
/// 换成响应里的新令牌——晚一步，下一个请求就带着已吊销的旧令牌 401。
/// 返回换新后的账号信息（供调用方刷新 `auth.user`）。
pub async fn change_password(old_password: &str, new_password: &str) -> Result<User, String> {
    let data = graphql(
        "mutation($o: String!, $n: String!) { changePassword(oldPassword: $o, newPassword: $n) { token account { id email name isAdmin hasPassword } } }",
        json!({ "o": old_password, "n": new_password }),
    )
    .await?;
    let r = data.get("changePassword").ok_or("改密响应缺失")?;
    let token = r
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or("token 缺失")?
        .to_string();
    // 先解析再落盘：解析失败就报错退出，别把本地令牌换成一张解析不出账号的。
    let account: User = serde_json::from_value(r.get("account").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())?;
    set_token(&token);
    Ok(account)
}

/// 管理员建号的结果。`initial_password` 只在这一次响应里出现，服务端只留 Argon2 哈希。
///
/// `rename_all` 必需：查询里请求的是 `initialPassword`，少了它 `initial_password`
/// 会因「字段缺失」整条反序列化失败，建号成功却报错。
#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedAccount {
    pub account: User,
    pub initial_password: String,
}

/// 管理员建号。`password` 传 `None` 则由服务端生成初始密码。
pub async fn create_account(
    email: &str,
    name: &str,
    is_admin: bool,
    password: Option<&str>,
) -> Result<CreatedAccount, String> {
    let data = graphql(
        "mutation($e: String!, $n: String!, $a: Boolean!, $p: String) { \
         createAccount(email: $e, name: $n, isAdmin: $a, password: $p) { \
         account { id email name isAdmin } initialPassword } }",
        json!({ "e": email, "n": name, "a": is_admin, "p": password }),
    )
    .await?;
    serde_json::from_value(data.get("createAccount").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
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
    Ok(data
        .get("transferOwner")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 软删除工作空间（Owner）。数据保留，可恢复。
pub async fn delete_workspace(workspace_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { deleteWorkspace(workspaceId: $id) }",
        json!({ "id": workspace_id }),
    )
    .await?;
    Ok(data
        .get("deleteWorkspace")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn restore_workspace(workspace_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { restoreWorkspace(workspaceId: $id) }",
        json!({ "id": workspace_id }),
    )
    .await?;
    Ok(data
        .get("restoreWorkspace")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
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

/// 新建条目。`detail` 为 `Some(_)` 时服务端用 `create_with_detail` 一次性落标题与正文，
/// 为 `None` 或空串时走原 `create`（向后兼容）。配合 `setLabelings` 可在 UI 里
/// 实现「标题 + 正文 + 标签」一次提交。
pub async fn create_entry(
    workspace_id: &str,
    title: &str,
    detail: Option<&str>,
) -> Result<Entry, String> {
    // 服务端签名是 `detail: String`（可空）：传 None 时给空串，触发原 create 路径。
    let d = detail.unwrap_or("");
    let q = format!(
        "mutation($id: ID!, $t: String!, $d: String) {{ createEntry(workspaceId: $id, title: $t, detail: $d) {{ {ENTRY_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id, "t": title, "d": d })).await?;
    serde_json::from_value(data.get("createEntry").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn label_schemas(workspace_id: &str) -> Result<Vec<LabelSchema>, String> {
    let q =
        format!("query($id: ID!) {{ labelSchemas(workspaceId: $id) {{ {LABEL_SCHEMA_FIELDS} }} }}");
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
    serde_json::from_value(
        data.get("workspaceAiConfig")
            .cloned()
            .unwrap_or(Value::Null),
    )
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
    serde_json::from_value(
        data.get("updateWorkspaceAiConfig")
            .cloned()
            .unwrap_or(Value::Null),
    )
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

pub async fn set_labeling(
    entry_code: &str,
    label_name: &str,
    value: &Value,
) -> Result<Value, String> {
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
    Ok(data
        .get("setLabelings")
        .and_then(|v| v.as_i64())
        .unwrap_or(0))
}

pub async fn entry(code: &str) -> Result<Option<Entry>, String> {
    let q = format!("query($c: String!) {{ entry(code: $c) {{ {ENTRY_FIELDS} }} }}");
    let data = graphql(&q, json!({ "c": code })).await?;
    Ok(data
        .get("entry")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok()))
}

pub async fn comments(entry_code: &str) -> Result<Vec<Comment>, String> {
    let q = format!("query($c: String!) {{ comments(entryCode: $c) {{ {COMMENT_FIELDS} }} }}");
    let data = graphql(&q, json!({ "c": entry_code })).await?;
    serde_json::from_value(data.get("comments").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 取一批条目的评论条数。服务端会跳过越权或不存在的条目。
pub async fn comment_counts(entry_codes: &[String]) -> Result<Vec<(String, i32)>, String> {
    let data = graphql(
        "query($c: [String!]!) { commentCounts(entryCodes: $c) { entryCode count } }",
        json!({ "c": entry_codes }),
    )
    .await?;
    let list = data
        .get("commentCounts")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    Ok(list
        .into_iter()
        .filter_map(|v| {
            let code = v.get("entryCode")?.as_str()?.to_string();
            let count = v.get("count")?.as_i64()? as i32;
            Some((code, count))
        })
        .collect())
}

pub async fn create_comment(entry_code: &str, body: &str) -> Result<Comment, String> {
    let q = format!(
        "mutation($c: String!, $b: String!) {{ createComment(entryCode: $c, body: $b) {{ {COMMENT_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "c": entry_code, "b": body })).await?;
    serde_json::from_value(data.get("createComment").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn update_comment(entry_code: &str, id: &str, body: &str) -> Result<Comment, String> {
    let q = format!(
        "mutation($c: String!, $i: ID!, $b: String!) {{ updateComment(entryCode: $c, id: $i, body: $b) {{ {COMMENT_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "c": entry_code, "i": id, "b": body })).await?;
    serde_json::from_value(data.get("updateComment").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// `id` 与 `entryCode` 必须成对给出：评论主键是 (entry_code, comment_id)。
pub async fn delete_comment(entry_code: &str, id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($c: String!, $i: ID!) { deleteComment(entryCode: $c, id: $i) }",
        json!({ "c": entry_code, "i": id }),
    )
    .await?;
    Ok(data
        .get("deleteComment")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 拉某个条目的全部关联（from + to 两侧合并）。
pub async fn entry_relations(entry_code: &str) -> Result<Vec<Relation>, String> {
    let q =
        format!("query($c: String!) {{ entryRelations(entryCode: $c) {{ {RELATION_FIELDS} }} }}");
    let data = graphql(&q, json!({ "c": entry_code })).await?;
    serde_json::from_value(data.get("entryRelations").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 新建条目关联。`semantic_kind` 是 "contains" / "derives" / ... / "relates_to"
/// 或 "custom"；后者需额外给 `custom_value`。幂等：同 from + to + semantic
/// 已存在时返回已有那条，不会重复建。
#[allow(clippy::too_many_arguments)]
pub async fn add_entry_relation(
    from_code: &str,
    to_code: &str,
    semantic_kind: &str,
    custom_value: Option<&str>,
) -> Result<Relation, String> {
    let q = format!(
        "mutation($f: String!, $t: String!, $s: GqlRelationSemanticInput!) {{ \
         addEntryRelation(fromCode: $f, toCode: $t, semantic: $s) {{ {RELATION_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "f": from_code,
            "t": to_code,
            "s": { "kind": semantic_kind, "customValue": custom_value },
        }),
    )
    .await?;
    serde_json::from_value(data.get("addEntryRelation").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 改关联语义。方向 / 端点锁死，只能换语义。
pub async fn update_entry_relation(
    workspace_id: &str,
    relation_id: &str,
    semantic_kind: &str,
    custom_value: Option<&str>,
) -> Result<Relation, String> {
    let q = format!(
        "mutation($w: ID!, $r: ID!, $s: GqlRelationSemanticInput!) {{ \
         updateEntryRelation(workspaceId: $w, relationId: $r, semantic: $s) \
         {{ {RELATION_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "w": workspace_id,
            "r": relation_id,
            "s": { "kind": semantic_kind, "customValue": custom_value },
        }),
    )
    .await?;
    serde_json::from_value(
        data.get("updateEntryRelation")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|e| e.to_string())
}

pub async fn delete_entry_relation(workspace_id: &str, relation_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($w: ID!, $r: ID!) { deleteEntryRelation(workspaceId: $w, relationId: $r) }",
        json!({ "w": workspace_id, "r": relation_id }),
    )
    .await?;
    Ok(data
        .get("deleteEntryRelation")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

const MESSAGE_FIELDS: &str = "id workspaceId workspaceName workspaceSlug entryCode sourceType sourceId actorId actorName preview read createdAt";

pub async fn unread_message_count() -> Result<i32, String> {
    let q = "query { unreadMessageCount }";
    let data = graphql(q, serde_json::json!({})).await?;
    let n = data
        .get("unreadMessageCount")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Ok(n as i32)
}

pub async fn messages(limit: Option<i32>) -> Result<Vec<GqlMessage>, String> {
    let q = format!("query($l: Int) {{ messages(limit: $l) {{ {MESSAGE_FIELDS} }} }}");
    let data = graphql(&q, serde_json::json!({ "l": limit })).await?;
    serde_json::from_value(data.get("messages").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn mark_message_read(id: &str) -> Result<bool, String> {
    let q = "mutation($i: ID!) { markMessageRead(id: $i) }";
    let data = graphql(q, serde_json::json!({ "i": id })).await?;
    Ok(data
        .get("markMessageRead")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn mark_all_messages_read() -> Result<bool, String> {
    let q = "mutation { markAllMessagesRead }";
    let data = graphql(q, serde_json::json!({})).await?;
    Ok(data
        .get("markAllMessagesRead")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn attachments(entry_code: &str) -> Result<Vec<Attachment>, String> {
    let q =
        format!("query($c: String!) {{ attachments(entryCode: $c) {{ {ATTACHMENT_FIELDS} }} }}");
    let data = graphql(&q, json!({ "c": entry_code })).await?;
    serde_json::from_value(data.get("attachments").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn delete_attachment(id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($i: ID!) { deleteAttachment(id: $i) }",
        json!({ "i": id }),
    )
    .await?;
    Ok(data
        .get("deleteAttachment")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 走 GraphQL multipart 上传。字段名 `operations` / `map` / `map` 里映射的键三者必须自洽
/// （async-graphql 按 graphql-multipart-request-spec 解析）。
/// `agent_session_id` 不为空时：跳过 entry 校验，附件落到 agent 会话下的
/// `_agent` sentinel（见 `attachment::save_for_session`）。
/// 只在 wasm 下存在：`web_sys::File` 在非 wasm 目标不可用，调用点也是 wasm-only。
#[cfg(target_arch = "wasm32")]
pub async fn upload_attachment(
    entry_code: &str,
    file: &web_sys::File,
    inline: bool,
    agent_session_id: Option<&str>,
) -> Result<Attachment, String> {
    let operations = json!({
        "query": format!(
            "mutation($c: String!, $f: Upload!, $in: Boolean!, $sid: ID) {{ uploadAttachment(entryCode: $c, file: $f, inline: $in, agentSessionId: $sid) {{ {ATTACHMENT_FIELDS} }} }}"
        ),
        "variables": {
            "c": entry_code,
            "f": null,
            "in": inline,
            "sid": agent_session_id,
        },
    });
    let map = json!({ "0": ["variables.f"] });

    let fd = web_sys::FormData::new().map_err(|e| format!("{e:?}"))?;
    fd.append_with_str("operations", &operations.to_string())
        .map_err(|e| format!("{e:?}"))?;
    fd.append_with_str("map", &map.to_string())
        .map_err(|e| format!("{e:?}"))?;
    fd.append_with_blob_and_filename("0", AsRef::<web_sys::Blob>::as_ref(file), &file.name())
        .map_err(|e| format!("{e:?}"))?;

    // 不手写 Content-Type：浏览器要自己补 multipart 的 boundary，手写会让服务端解析失败。
    let mut req = gloo_net::http::Request::post("/api/graphql");
    if let Some(token) = get_token() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let resp = req
        .body(fd)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json_val: Value = resp.json().await.map_err(|e| e.to_string())?;
    if let Some(msg) = json_val
        .get("errors")
        .and_then(|e| e.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Err(msg.to_string());
    }
    serde_json::from_value(
        json_val
            .pointer("/data/uploadAttachment")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|e| e.to_string())
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

/// 归档条目：移出基础视图，数据保留。返回服务端确认。
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
    let q = format!("query($id: ID!) {{ archivedEntries(workspaceId: $id) {{ {ENTRY_FIELDS} }} }}");
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
    serde_json::from_value(
        data.get("createLabelSchema")
            .cloned()
            .unwrap_or(Value::Null),
    )
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
    serde_json::from_value(
        data.get("updateLabelSchema")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|e| e.to_string())
}

/// 删一条标签定义（级联清理所有打标与索引）。返回服务端是否真的命中（不存在返回 false）。
pub async fn delete_label_schema(workspace_id: &str, name: &str) -> Result<bool, String> {
    let q = "mutation($id: ID!, $n: String!) { deleteLabelSchema(workspaceId: $id, name: $n) }";
    let data = graphql(&q, json!({ "id": workspace_id, "n": name })).await?;
    Ok(data
        .get("deleteLabelSchema")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
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

#[derive(Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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

const INVITE_FIELDS: &str =
    "workspaceId workspaceName workspaceSlug accountId email name role invitedBy createdAt";

pub async fn invite_member(workspace_id: &str, email: &str, role: &str) -> Result<Invite, String> {
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
    let q =
        format!("mutation($id: ID!) {{ acceptInvite(workspaceId: $id) {{ {MEMBER_FIELDS} }} }}");
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
    Ok(data
        .get("declineInvite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn revoke_invite(workspace_id: &str, account_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!, $a: ID!) { revokeInvite(workspaceId: $id, accountId: $a) }",
        json!({ "id": workspace_id, "a": account_id }),
    )
    .await?;
    Ok(data
        .get("revokeInvite")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
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
    let data = graphql(
        &q,
        json!({ "id": workspace_id, "a": account_id, "r": role }),
    )
    .await?;
    serde_json::from_value(data.get("updateMemberRole").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn remove_member(workspace_id: &str, account_id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!, $a: ID!) { removeMember(workspaceId: $id, accountId: $a) }",
        json!({ "id": workspace_id, "a": account_id }),
    )
    .await?;
    Ok(data
        .get("removeMember")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

// ---------- 视图（View） ----------

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSort {
    pub field: String,
    pub desc: bool,
}

/// 视图的时间轴配置。`person` 为 `None` 表示不展示相关人。
#[derive(Clone, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ViewTimeline {
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub person: Option<String>,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: String,
    pub name: String,
    pub query: Value,
    pub query_expr: String,
    /// 排序键链，按优先级从高到低。空数组等价于默认排序（更新时间降序）。
    pub sorts: Vec<ViewSort>,
    pub columns: Vec<String>,
    pub is_shared: bool,
    pub owner_id: String,
    #[serde(default)]
    pub title_colors: Value,
    pub entry_count: i64,
    /// 基础视图：始终存在、不可删除，列表中置顶。
    #[serde(default)]
    pub is_default: bool,
    /// 时间轴配置；`None` 表示未配置，此时界面不显示「普通 / 时间轴」开关。
    #[serde(default)]
    pub timeline: Option<ViewTimeline>,
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
    /// 服务端走 cursor 路径且后面还有匹配时返回。
    #[serde(default)]
    pub next_cursor: Option<String>,
}

const VIEW_FIELDS: &str = "id name query queryExpr sorts { field desc } columns isShared ownerId \
     titleColors entryCount isDefault timeline { start end person }";

/// 排序键链 → GraphQL 变量值。空数组会被服务端解释为「用默认排序」。
fn sorts_var(sorts: &[ViewSort]) -> Value {
    Value::Array(
        sorts
            .iter()
            .map(|s| json!({ "field": s.field, "desc": s.desc }))
            .collect(),
    )
}

pub async fn views(workspace_id: &str) -> Result<Vec<View>, String> {
    let q = format!("query($id: ID!) {{ views(workspaceId: $id) {{ {VIEW_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("views").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn query_entries(
    workspace_id: &str,
    query: &Value,
    sorts: &[ViewSort],
    page: i64,
    page_size: i64,
    cursor: Option<&str>,
    limit: Option<i64>,
) -> Result<EntryPage, String> {
    let q = format!(
        "query($id: ID!, $q: JSON, $s: [SortInput!], $p: PageInput) {{ \
         queryEntries(workspaceId: $id, query: $q, sorts: $s, page: $p) {{ \
         items {{ {ENTRY_FIELDS} }} total page pageSize labelNames nextCursor }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id,
            "q": query,
            "s": sorts_var(sorts),
            "p": {
                "page": page,
                "pageSize": page_size,
                "cursor": cursor,
                "limit": limit,
            },
        }),
    )
    .await?;
    serde_json::from_value(data.get("queryEntries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 全文检索（用作 Entry 选择器等）。keyword 由 Tantivy ngram 解析，
/// 跨 title / content / labels / code / comments 五字段。返回结构与
/// `query_entries` 同形态（`EntryPage`），picker 只用 `items`，剩下的字段
/// 留着方便未来在别处复用。
pub async fn search_entries(
    workspace_id: &str,
    keyword: &str,
    limit: i64,
) -> Result<EntryPage, String> {
    let q = format!(
        "query($id: ID!, $k: String!, $l: Int) {{ \
         searchEntries(workspaceId: $id, keyword: $k, limit: $l) {{ \
         items {{ {ENTRY_FIELDS} }} total page pageSize labelNames nextCursor }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id,
            "k": keyword,
            "l": limit,
        }),
    )
    .await?;
    serde_json::from_value(data.get("searchEntries").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 时间轴模式一次取回的条目上限。时间轴要按全量画（铺在轴上才有意义），
/// 而服务端 `pageSize` 上限是 100，故逐页翻。500 条覆盖了绝大多数视图；
/// 真超了也只是少画几条，界面上有提示。
pub const TIMELINE_MAX_ENTRIES: usize = 500;

/// 按视图条件取回**全部**命中条目（时间轴模式用）。
/// 返回值与 `query_entries` 同一个 `EntryPage` 形状，调用方两条分支可以走同一段代码。
pub async fn query_all_entries(
    workspace_id: &str,
    query: &Value,
    sorts: &[ViewSort],
) -> Result<EntryPage, String> {
    const PAGE_SIZE: i64 = 100;
    let pages = (TIMELINE_MAX_ENTRIES as i64 + PAGE_SIZE - 1) / PAGE_SIZE;
    let mut items = Vec::new();
    let mut names: Vec<String> = Vec::new();
    // 不预置初值：循环体在读取 `total`（下文的取满判断）之前必定先赋值，
    // 给个 `0i64` 初值只会在编译期被判为从未读取，招来 unused_assignments 告警。
    let mut total;
    for page in 1..=pages {
        let ep = query_entries(workspace_id, query, sorts, page, PAGE_SIZE, None, None).await?;
        total = ep.total;
        for n in ep.label_names {
            if !names.contains(&n) {
                names.push(n);
            }
        }
        items.extend(ep.items);
        // 取满了（`total` 是本次查询命中总数，与分页无关）就停，不再空翻。
        if items.len() as i64 >= total {
            break;
        }
    }
    Ok(EntryPage {
        total: items.len() as i64,
        page: 1,
        page_size: items.len() as i64,
        label_names: names,
        next_cursor: None,
        items,
    })
}

pub async fn create_view(
    workspace_id: &str,
    name: &str,
    query: &Value,
    sorts: &[ViewSort],
    columns: &[String],
    is_shared: bool,
    title_colors: &Value,
) -> Result<View, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $q: JSON!, $s: [SortInput!], $c: [String!]!, $sh: Boolean!, $tc: JSON!) {{ \
         createView(workspaceId: $id, name: $n, query: $q, sorts: $s, columns: $c, isShared: $sh, titleColors: $tc) {{ {VIEW_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": workspace_id, "n": name, "q": query,
            "s": sorts_var(sorts), "c": columns, "sh": is_shared,
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
    sorts: &[ViewSort],
    columns: &[String],
    is_shared: bool,
    title_colors: &Value,
) -> Result<View, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $q: JSON!, $s: [SortInput!], $c: [String!]!, $sh: Boolean!, $tc: JSON!) {{ \
         updateView(id: $id, name: $n, query: $q, sorts: $s, columns: $c, isShared: $sh, titleColors: $tc) {{ {VIEW_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({
            "id": id, "n": name, "q": query,
            "s": sorts_var(sorts), "c": columns, "sh": is_shared,
            "tc": title_colors,
        }),
    )
    .await?;
    serde_json::from_value(data.get("updateView").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 设置 / 清除视图的时间轴配置。`start` 或 `end` 空串即清除，返回 `None`。
pub async fn set_view_timeline(
    id: &str,
    start: &str,
    end: &str,
    person: &str,
) -> Result<Option<ViewTimeline>, String> {
    let data = graphql(
        "mutation($id: ID!, $s: String!, $e: String!, $p: String!) { \
         setViewTimeline(id: $id, start: $s, end: $e, person: $p) { start end person } }",
        json!({ "id": id, "s": start, "e": end, "p": person }),
    )
    .await?;
    let v = data.get("setViewTimeline").cloned().unwrap_or(Value::Null);
    if v.is_null() {
        return Ok(None);
    }
    serde_json::from_value(v)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub async fn delete_view(id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { deleteView(id: $id) }",
        json!({ "id": id }),
    )
    .await?;
    Ok(data
        .get("deleteView")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
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

// ---------- 自动化规则（AutomationRule） ----------

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleWrite {
    pub label_name: String,
    pub op: String,
    pub value_kind: String,
    pub value: Value,
}

#[derive(Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomationRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger_expr: String,
    pub target_event_source: bool,
    pub target_expr: String,
    pub writes: Vec<RuleWrite>,
    pub created_at: String,
    pub updated_at: String,
}

const RULE_FIELDS: &str = "id name enabled triggerExpr targetEventSource targetExpr \
     writes { labelName op valueKind value } createdAt updatedAt";

pub async fn automation_rules(workspace_id: &str) -> Result<Vec<AutomationRule>, String> {
    let q = format!("query($id: ID!) {{ automationRules(workspaceId: $id) {{ {RULE_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("automationRules").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn parse_rule_trigger(workspace_id: &str, expr: &str) -> Result<Value, String> {
    let data = graphql(
        "query($id: ID!, $e: String!) { parseRuleTrigger(workspaceId: $id, expr: $e) }",
        json!({ "id": workspace_id, "e": expr }),
    )
    .await?;
    Ok(data.get("parseRuleTrigger").cloned().unwrap_or(Value::Null))
}

/// `writes` 是 `[{labelName, op, valueKind, value}]`。
pub async fn create_automation_rule(
    workspace_id: &str,
    name: &str,
    enabled: bool,
    trigger_expr: &str,
    target_event_source: bool,
    target_expr: Option<&str>,
    writes: &Value,
) -> Result<AutomationRule, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $e: Boolean!, $t: String!, $tes: Boolean!, $te: String, $w: [RuleWriteInput!]!) {{ \
         createAutomationRule(workspaceId: $id, name: $n, enabled: $e, triggerExpr: $t, \
         targetEventSource: $tes, targetExpr: $te, writes: $w) {{ {RULE_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({ "id": workspace_id, "n": name, "e": enabled, "t": trigger_expr,
                "tes": target_event_source, "te": target_expr, "w": writes }),
    )
    .await?;
    serde_json::from_value(
        data.get("createAutomationRule")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|e| e.to_string())
}

pub async fn update_automation_rule(
    id: &str,
    name: &str,
    enabled: bool,
    trigger_expr: &str,
    target_event_source: bool,
    target_expr: Option<&str>,
    writes: &Value,
) -> Result<AutomationRule, String> {
    let q = format!(
        "mutation($id: ID!, $n: String!, $e: Boolean!, $t: String!, $tes: Boolean!, $te: String, $w: [RuleWriteInput!]!) {{ \
         updateAutomationRule(id: $id, name: $n, enabled: $e, triggerExpr: $t, \
         targetEventSource: $tes, targetExpr: $te, writes: $w) {{ {RULE_FIELDS} }} }}"
    );
    let data = graphql(
        &q,
        json!({ "id": id, "n": name, "e": enabled, "t": trigger_expr,
                "tes": target_event_source, "te": target_expr, "w": writes }),
    )
    .await?;
    serde_json::from_value(
        data.get("updateAutomationRule")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|e| e.to_string())
}

pub async fn delete_automation_rule(id: &str) -> Result<bool, String> {
    let data = graphql(
        "mutation($id: ID!) { deleteAutomationRule(id: $id) }",
        json!({ "id": id }),
    )
    .await?;
    Ok(data
        .get("deleteAutomationRule")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

// ---------- Agent 会话 ----------

#[derive(Clone, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    /// 服务端 `Option<DateTime<Utc>>`：未发过消息的会话里字段为 None；
    /// `#[serde(default)]` 防止老查询忘了请求该字段时整条反序列化失败。
    #[serde(default)]
    pub last_message_at: Option<String>,
}

const AGENT_SESSION_FIELDS: &str = "id workspaceId title createdAt updatedAt lastMessageAt";

#[derive(Clone, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolCallRecord {
    pub id: String,
    pub name: String,
    /// 服务端 `Json<serde_json::Value>` scalar，原样到达。
    pub args: serde_json::Value,
    pub result_preview: String,
    pub ok: bool,
}

#[derive(Clone, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessage {
    pub id: String,
    pub session_id: String,
    /// 服务端 `AgentRole` enum，async-graphql 默认以字符串（`"USER"` 等）序列化。
    pub role: String,
    pub content: String,
    /// 仅前端累积的推理链：流式期间由 Reasoning SSE 事件累加,
    /// 服务端不持久化——重开会话不重现推理链,但保证同一会话内
    /// 「思考 + 回答」样式区分。
    #[serde(default)]
    pub reasoning: String,
    pub tool_calls: Vec<AgentToolCallRecord>,
    /// 仅 role=Tool 时有值。`#[serde(default)]` 同上，防止字段缺失炸反序列化。
    #[serde(default)]
    pub tool_call_id: Option<String>,
    pub created_at: String,
}

const AGENT_MESSAGE_FIELDS: &str =
    "id sessionId role content toolCalls { id name args resultPreview ok } \
     toolCallId createdAt";

#[derive(Clone, serde::Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AgentTurn {
    pub id: String,
    pub session_id: String,
    pub started_at: String,
}

const AGENT_TURN_FIELDS: &str = "id sessionId startedAt";

pub async fn list_agent_sessions(workspace_id: &str) -> Result<Vec<AgentSession>, String> {
    let q = format!(
        "query($id: ID!) {{ agentSessions(workspaceId: $id) {{ {AGENT_SESSION_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(data.get("agentSessions").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn list_agent_messages(session_id: &str) -> Result<Vec<AgentMessage>, String> {
    let q =
        format!("query($id: ID!) {{ agentMessages(sessionId: $id) {{ {AGENT_MESSAGE_FIELDS} }} }}");
    let data = graphql(&q, json!({ "id": session_id })).await?;
    serde_json::from_value(data.get("agentMessages").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn create_agent_session(workspace_id: &str) -> Result<AgentSession, String> {
    let q = format!(
        "mutation($id: ID!) {{ createAgentSession(workspaceId: $id) {{ {AGENT_SESSION_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "id": workspace_id })).await?;
    serde_json::from_value(
        data.get("createAgentSession")
            .cloned()
            .unwrap_or(Value::Null),
    )
    .map_err(|e| e.to_string())
}

pub async fn delete_agent_session(id: &str) -> Result<bool, String> {
    let q = "mutation($id: ID!) { deleteAgentSession(id: $id) }";
    let data = graphql(q, json!({ "id": id })).await?;
    Ok(data
        .get("deleteAgentSession")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

pub async fn send_agent_message(
    session_id: &str,
    content: &str,
    context: Option<AgentMessageContext>,
    attachment_ids: Option<Vec<String>>,
) -> Result<AgentTurn, String> {
    // content / context / attachmentIds 走 GraphQL input 变量（项目约定）：
    // 转义、引号、换行都交给服务端变量解析。
    let q = format!(
        "mutation($input: SendAgentMessageInput!) {{ sendAgentMessage(input: $input) {{ {AGENT_TURN_FIELDS} }} }}"
    );
    let input = json!({
        "sessionId": session_id,
        "content": content,
        "context": context,
        "attachmentIds": attachment_ids,
    });
    let data = graphql(&q, json!({ "input": input })).await?;
    serde_json::from_value(data.get("sendAgentMessage").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 显式取消一个正在跑的 turn。`Ok(true)` 表示刚被取消、`Ok(false)` 表示
/// 已经没有活跃 turn（重复点击、turn 已结束都是 false）。前端不关心这个
/// bool 本身——只是为了把服务端 `was_active` 的语义直接透出来；UI 拿到
/// 任何 Ok 都立即把 streaming 标志关掉。
pub async fn cancel_agent_turn(turn_id: &str) -> Result<bool, String> {
    let q = "mutation($id: ID!) { cancelAgentTurn(turnId: $id) }";
    let data = graphql(q, json!({ "id": turn_id })).await?;
    Ok(data
        .get("cancelAgentTurn")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

#[derive(Clone, serde::Deserialize, serde::Serialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageContext {
    pub view_id: Option<String>,
    pub selected_entry_codes: Vec<String>,
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
        assert_eq!(
            deleted.deleted_at.as_deref(),
            Some("2026-01-01T00:00:00+00:00")
        );

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

/// 当前启用的 OAuth provider 列表（决定登录页按钮渲染）。
pub async fn oauth_providers() -> Result<Vec<String>, String> {
    let data = graphql("query { oauthProviders }", json!({})).await?;
    serde_json::from_value(data.get("oauthProviders").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

pub async fn bind_oauth_to_existing(
    bind_token: &str,
    email: &str,
    password: &str,
) -> Result<(String, User), String> {
    let data = graphql(
        "mutation($t: String!, $e: String!, $p: String!) { \
         bindOAuthToExisting(bindToken: $t, email: $e, password: $p) \
         { token account { id email name isAdmin hasPassword } } }",
        json!({ "t": bind_token, "e": email, "p": password }),
    )
    .await?;
    let r = data.get("bindOAuthToExisting").ok_or("绑定响应缺失")?;
    let token = r
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or("token 缺失")?
        .to_string();
    let account: User = serde_json::from_value(r.get("account").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())?;
    Ok((token, account))
}

/// 当前账号已绑定的第三方登录方式（账号页展示 / 解除用）。
pub async fn my_oauth_bindings() -> Result<Vec<OAuthBinding>, String> {
    let data = graphql(
        "query { myOAuthBindings { provider externalId email displayName boundAt } }",
        json!({}),
    )
    .await?;
    serde_json::from_value(data.get("myOAuthBindings").cloned().unwrap_or(Value::Null))
        .map_err(|e| format!("decode myOAuthBindings: {e}"))
}

/// 解绑一个第三方登录方式。`account_id` 必须等于登录态里的账号 id——resolver 端会
/// 再校验一次，前端这里传本机 `auth.user.id`。
pub async fn unbind_oauth_binding(
    account_id: &str,
    provider: &str,
    external_id: &str,
) -> Result<(), String> {
    // 不读返回的 `bool`：错误已被 `graphql()` 抽到 Err 分支里了，这里只关心成功。
    let _ = graphql(
        "mutation($a: ID!, $p: String!, $e: String!) { \
         unbindOauthBinding(accountId: $a, provider: $p, externalId: $e) }",
        json!({ "a": account_id, "p": provider, "e": external_id }),
    )
    .await?;
    Ok(())
}

/// OAuth-only 账号后补本地密码。服务端会重签一张令牌，但与 `change_password`
/// 不同——`set_password` 不调 `revoke_tokens`，所以其它设备不会被踢出；新令牌
/// 仍要立刻落到本地，否则下一个请求就带着旧令牌 401。
/// 返回换新后的账号（供调用方刷新 `auth.user`，里面已经带上 `hasPassword=true`）。
pub async fn set_password(new_password: &str) -> Result<(String, User), String> {
    let data = graphql(
        "mutation($p: String!) { \
         setPassword(newPassword: $p) { token account { id email name isAdmin hasPassword } } }",
        json!({ "p": new_password }),
    )
    .await?;
    let r = data.get("setPassword").ok_or("设置密码响应缺失")?;
    let token = r
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or("token 缺失")?
        .to_string();
    let account: User = serde_json::from_value(r.get("account").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())?;
    Ok((token, account))
}

// ---------- Entry 分享：公开解析 + Owner 端的 create/list/revoke ----------

const ENTRY_SHARE_FIELDS: &str = "id entryCode workspaceId createdAt createdBy revoked";

const SHARED_ENTRY_FIELDS: &str = "code title detail createdAt updatedAt \
     createdByAccount { name } labels { labelName value }";

/// 公开分享全量读视图的 GraphQL 字段集合。字段顺序对应服务端 `GqlEntryFullPublic`
/// 嵌套结构；前端 `EntryFullPublic` 一次性 deserialize——任何字段不匹配都会
/// 让 `serde_json::from_value` 直接报错（`#[serde(default)]` 用在
/// optional 字段，缺这一层的字段不会静默回退）。
const ENTRY_FULL_PUBLIC_FIELDS: &str = "entry { code title detail createdAt updatedAt \
     createdByAccount { name } labels { labelName value } } \
     workspaceName workspaceSlug \
     comments { id body authorName createdAt updatedAt edited } \
     attachments { id filename contentType size url createdAt } \
     labelSchemas { name title valueType enumValues color valueColors multi format currencySymbol unit defaultValue links } \
     members { id name }";

/// 公开解析分享 token。`Ok(None)`：token 无效 / 已撤销 / Entry 删了，
/// UI 走「该分享链接无效或已撤销」即可——与登录路径「无命中」同源安全策略。
pub async fn entry_by_share_token(token: &str) -> Result<Option<SharedEntry>, String> {
    let q = format!(
        "query($t: String!) {{ entryByShareToken(token: $t) {{ {SHARED_ENTRY_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "t": token })).await?;
    let raw = data
        .get("entryByShareToken")
        .cloned()
        .unwrap_or(Value::Null);
    if raw.is_null() {
        return Ok(None);
    }
    let entry: SharedEntry = serde_json::from_value(raw).map_err(|e| e.to_string())?;
    Ok(Some(entry))
}

/// 公开拉取分享页全量只读视图：`/share/{token}` 一次性拿到 entry / 评论 /
/// 附件 / 标签定义 / 成员名，与 `entry_by_share_token` 同款安全策略——失败
/// 形态统一 `None`，不分流。匿名访问、不传 Authorization。
pub async fn entry_full_by_share_token(token: &str) -> Result<Option<EntryFullPublic>, String> {
    let q = format!(
        "query($t: String!) {{ entryFullByShareToken(token: $t) {{ {ENTRY_FULL_PUBLIC_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "t": token })).await?;
    let raw = data
        .get("entryFullByShareToken")
        .cloned()
        .unwrap_or(Value::Null);
    if raw.is_null() {
        return Ok(None);
    }
    let full: EntryFullPublic = serde_json::from_value(raw).map_err(|e| e.to_string())?;
    Ok(Some(full))
}

/// 列出某条目下的全部分享（含已撤销）。Owner 面板用，鉴权调用。
pub async fn entry_shares(workspace_id: &str, entry_code: &str) -> Result<Vec<EntryShare>, String> {
    let q = format!(
        "query($w: ID!, $c: String!) {{ entryShares(workspaceId: $w, entryCode: $c) {{ {ENTRY_SHARE_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "w": workspace_id, "c": entry_code })).await?;
    serde_json::from_value(data.get("entryShares").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 新建一个分享链接，返回带 `id` 的 share 行；前端用 `id` + 当前 origin 自拼 URL。
pub async fn create_entry_share(
    workspace_id: &str,
    entry_code: &str,
) -> Result<EntryShare, String> {
    let q = format!(
        "mutation($w: ID!, $c: String!) {{ createEntryShare(workspaceId: $w, entryCode: $c) {{ {ENTRY_SHARE_FIELDS} }} }}"
    );
    let data = graphql(&q, json!({ "w": workspace_id, "c": entry_code })).await?;
    serde_json::from_value(data.get("createEntryShare").cloned().unwrap_or(Value::Null))
        .map_err(|e| e.to_string())
}

/// 软撤销分享（幂等）。
pub async fn revoke_entry_share(workspace_id: &str, share_id: &str) -> Result<bool, String> {
    let q = "mutation($w: ID!, $s: ID!) { revokeEntryShare(workspaceId: $w, shareId: $s) }";
    let data = graphql(q, json!({ "w": workspace_id, "s": share_id })).await?;
    Ok(data
        .get("revokeEntryShare")
        .and_then(|v| v.as_bool())
        .unwrap_or(false))
}

/// 前端拼分享 URL：anonymous reader 看到的「完整地址」文本。
/// SSR 下 origin 取空串，前端组件此时也不挂载（页面永远 hydrate 后才用）。
/// web_sys 只在 wasm 编译；native 编译走空 origin。
#[cfg(target_arch = "wasm32")]
pub fn build_share_url(share_id: &str) -> String {
    let origin = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_default();
    format!("{}/share/{}", origin.trim_end_matches('/'), share_id)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn build_share_url(share_id: &str) -> String {
    format!("/share/{}", share_id)
}
