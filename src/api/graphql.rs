use std::sync::Arc;

use async_graphql::{
    Context, EmptySubscription, ID, Json, Object, Result as GqlResult, Schema, SimpleObject,
};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::Extension;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::HeaderMap;
use ulid::Ulid;

use crate::domain::{
    Account, AuditLog, Entry, LabelSchema, LabelValueType, Labeling, Query as ViewQuery, SortField,
    SortSpec, View, Workspace, WorkspaceRole,
};
use crate::error::AppError;
use crate::service::entry::PageInput as EntryPageInput;
use crate::service::{AuthContext, Services};

// ---------- GraphQL 类型映射 ----------

#[derive(SimpleObject, Clone)]
pub struct GqlAccount {
    id: ID,
    email: String,
    name: String,
    is_admin: bool,
}

impl From<Account> for GqlAccount {
    fn from(a: Account) -> Self {
        Self {
            id: a.id.to_string().into(),
            email: a.email,
            name: a.name,
            is_admin: a.is_admin,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlWorkspace {
    id: ID,
    name: String,
    slug: String,
    description: String,
}

impl From<Workspace> for GqlWorkspace {
    fn from(w: Workspace) -> Self {
        Self {
            id: w.id.to_string().into(),
            name: w.name,
            slug: w.slug,
            description: w.description,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlWorkspaceWithRole {
    workspace: GqlWorkspace,
    role: String,
}

#[derive(SimpleObject, Clone)]
pub struct GqlLabelSchema {
    name: String,
    title: String,
    value_type: String,
    enum_values: Vec<String>,
}

impl From<LabelSchema> for GqlLabelSchema {
    fn from(s: LabelSchema) -> Self {
        Self {
            name: s.name,
            title: s.title,
            value_type: s.value_type.as_str().to_string(),
            enum_values: s.enum_values,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlLabeling {
    entry_code: String,
    label_name: String,
    value: Json<serde_json::Value>,
    set_by: ID,
    set_at: String,
}

impl From<Labeling> for GqlLabeling {
    fn from(l: Labeling) -> Self {
        Self {
            entry_code: l.entry_code,
            label_name: l.label_name,
            value: Json(l.value.to_json()),
            set_by: l.set_by.to_string().into(),
            set_at: l.set_at.to_rfc3339(),
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlEntry {
    code: String,
    workspace_id: ID,
    title: String,
    detail: String,
    created_by: ID,
    updated_by: ID,
    created_at: String,
    updated_at: String,
    labels: Vec<GqlLabeling>,
}

impl GqlEntry {
    fn new(entry: Entry, labels: Vec<Labeling>) -> Self {
        Self {
            code: entry.code,
            workspace_id: entry.workspace_id.to_string().into(),
            title: entry.title,
            detail: entry.detail,
            created_by: entry.created_by.to_string().into(),
            updated_by: entry.updated_by.to_string().into(),
            created_at: entry.created_at.to_rfc3339(),
            updated_at: entry.updated_at.to_rfc3339(),
            labels: labels.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlAuthResult {
    token: String,
    account: GqlAccount,
}

#[derive(SimpleObject, Clone)]
pub struct GqlAuditLog {
    id: ID,
    action: String,
    actor_id: ID,
    resource_type: String,
    resource_id: String,
    workspace_id: Option<ID>,
    before: Option<String>,
    after: Option<String>,
    at: String,
}

impl From<AuditLog> for GqlAuditLog {
    fn from(l: AuditLog) -> Self {
        Self {
            id: l.id.to_string().into(),
            action: serde_json::to_value(&l.action)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_default(),
            actor_id: l.actor_id.to_string().into(),
            resource_type: l.resource_type,
            resource_id: l.resource_id,
            workspace_id: l.workspace_id.map(|w| w.to_string().into()),
            before: l.before,
            after: l.after,
            at: l.at.to_rfc3339(),
        }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlSortSpec {
    field: String,
    desc: bool,
}

impl From<SortSpec> for GqlSortSpec {
    fn from(s: SortSpec) -> Self {
        Self { field: s.field.as_str().to_string(), desc: s.desc }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlView {
    id: ID,
    name: String,
    query: Json<serde_json::Value>,
    query_expr: String,
    sort: GqlSortSpec,
    columns: Vec<String>,
    is_shared: bool,
    owner_id: ID,
    created_at: String,
    updated_at: String,
}

impl GqlView {
    fn new(v: View) -> Self {
        let query = serde_json::to_value(&v.query).unwrap_or(serde_json::Value::Null);
        let query_expr = v.query.to_expr();
        Self {
            id: v.id.to_string().into(),
            name: v.name,
            query: Json(query),
            query_expr,
            sort: v.sort.into(),
            columns: v.columns,
            is_shared: v.is_shared,
            owner_id: v.owner_id.to_string().into(),
            created_at: v.created_at.to_rfc3339(),
            updated_at: v.updated_at.to_rfc3339(),
        }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlEntryConnection {
    items: Vec<GqlEntry>,
    total: i32,
    page: i32,
    page_size: i32,
}

#[derive(async_graphql::InputObject)]
pub struct SortInput {
    field: Option<String>,
    desc: Option<bool>,
}

impl SortInput {
    fn to_sort(&self) -> GqlResult<SortSpec> {
        let field = match &self.field {
            Some(f) => SortField::from_str(f)
                .ok_or_else(|| AppError::InvalidQuery(format!("未知排序字段: {f}")))?,
            None => SortField::UpdatedAt,
        };
        Ok(SortSpec { field, desc: self.desc.unwrap_or(true) })
    }
}

#[derive(async_graphql::InputObject)]
pub struct PageInput {
    page: Option<i32>,
    page_size: Option<i32>,
}

impl PageInput {
    fn to_page(&self) -> EntryPageInput {
        EntryPageInput {
            page: self.page.unwrap_or(1).max(1) as usize,
            page_size: self.page_size.unwrap_or(20).clamp(1, 100) as usize,
        }
    }
}

fn parse_query_json(value: Option<Json<serde_json::Value>>) -> GqlResult<ViewQuery> {
    match value {
        None => Ok(ViewQuery::all()),
        Some(Json(v)) if v.is_null() => Ok(ViewQuery::all()),
        Some(Json(v)) => serde_json::from_value(v)
            .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")).into()),
    }
}

// ---------- Context ----------

#[derive(Clone)]
pub struct GraphqlContext {
    pub services: Arc<Services>,
    pub auth: Option<AuthContext>,
}

impl GraphqlContext {
    fn require_auth(&self) -> GqlResult<AuthContext> {
        self.auth.ok_or_else(|| AppError::Unauthorized.into())
    }

    fn require_role(&self, ws_id: Ulid, min_role: WorkspaceRole) -> GqlResult<()> {
        let auth = self.require_auth()?;
        let member = self
            .services
            .workspace
            .get_member(ws_id, auth.account_id)?
            .ok_or(AppError::Forbidden)?;
        if member.role < min_role {
            return Err(AppError::Forbidden.into());
        }
        Ok(())
    }

    fn require_member(&self, ws_id: Ulid) -> GqlResult<()> {
        let auth = self.require_auth()?;
        self.services
            .workspace
            .get_member(ws_id, auth.account_id)?
            .ok_or(AppError::Forbidden)?;
        Ok(())
    }
}

// ---------- Query ----------

pub struct Query;

#[Object]
impl Query {
    async fn me(&self, ctx: &Context<'_>) -> GqlResult<Option<GqlAccount>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let Some(auth) = gql.auth else {
            return Ok(None);
        };
        Ok(gql.services.auth.find_by_id(auth.account_id)?.map(Into::into))
    }

    async fn workspaces(&self, ctx: &Context<'_>) -> GqlResult<Vec<GqlWorkspaceWithRole>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let list = gql.services.workspace.list_for(auth.account_id)?;
        Ok(list
            .into_iter()
            .map(|(w, r)| GqlWorkspaceWithRole {
                workspace: w.into(),
                role: r.as_str().to_string(),
            })
            .collect())
    }

    async fn workspace(&self, ctx: &Context<'_>, slug: String) -> GqlResult<Option<GqlWorkspace>> {
        let gql = ctx.data::<GraphqlContext>()?;
        gql.require_auth()?;
        Ok(gql.services.workspace.get_by_slug(&slug)?.map(Into::into))
    }

    async fn label_schemas(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlLabelSchema>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        Ok(gql
            .services
            .label
            .list_schemas(ws_id)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    async fn entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<Option<GqlEntry>> {
        let gql = ctx.data::<GraphqlContext>()?;
        gql.require_auth()?;
        let Some(entry) = gql.services.entry.get(&code)? else {
            return Ok(None);
        };
        gql.require_member(entry.workspace_id)?;
        let labels = gql.services.entry.labelings(&code)?;
        Ok(Some(GqlEntry::new(entry, labels)))
    }

    async fn audit_logs(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        limit: Option<usize>,
    ) -> GqlResult<Vec<GqlAuditLog>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        let list = gql.services.audit.list(ws_id, limit.unwrap_or(100))?;
        Ok(list.into_iter().map(Into::into).collect())
    }

    async fn my_role(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<String> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        let auth = gql.require_auth()?;
        match gql.services.workspace.get_member(ws_id, auth.account_id)? {
            Some(m) => Ok(m.role.as_str().to_string()),
            None => Ok("none".to_string()),
        }
    }

    async fn views(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlView>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        Ok(gql
            .services
            .view
            .list(auth.account_id, ws)?
            .into_iter()
            .map(GqlView::new)
            .collect())
    }

    async fn view(&self, ctx: &Context<'_>, id: ID) -> GqlResult<Option<GqlView>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let view_id = parse_ulid(id.as_str())?;
        let Some(v) = gql.services.view.get(view_id)? else {
            return Ok(None);
        };
        gql.require_member(v.workspace_id)?;
        if !v.is_shared && v.owner_id != auth.account_id {
            return Err(AppError::Forbidden.into());
        }
        Ok(Some(GqlView::new(v)))
    }

    async fn parse_view_query(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        expr: String,
    ) -> GqlResult<Json<serde_json::Value>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let query = ViewQuery::parse(&expr)?;
        query.validate(&gql.services.label.list_schemas(ws)?)?;
        Ok(Json(serde_json::to_value(&query).unwrap_or(serde_json::Value::Null)))
    }

    async fn format_view_query(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        query: Json<serde_json::Value>,
    ) -> GqlResult<String> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let q: ViewQuery = serde_json::from_value(query.0)
            .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")))?;
        Ok(q.to_expr())
    }

    async fn query_entries(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        query: Option<Json<serde_json::Value>>,
        sort: Option<SortInput>,
        page: Option<PageInput>,
    ) -> GqlResult<GqlEntryConnection> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let q = parse_query_json(query)?;
        q.validate(&gql.services.label.list_schemas(ws)?)?;
        let sort = sort.map(|s| s.to_sort()).transpose()?.unwrap_or_default();
        let page = page.map(|p| p.to_page()).unwrap_or_default();
        let result = gql.services.entry.query(ws, &q, &sort, page)?;
        let items = result
            .items
            .into_iter()
            .map(|(e, labels)| GqlEntry::new(e, labels))
            .collect();
        Ok(GqlEntryConnection {
            items,
            total: result.total as i32,
            page: page.page as i32,
            page_size: page.page_size as i32,
        })
    }
}

// ---------- Mutation ----------

pub struct Mutation;

#[Object]
impl Mutation {
    async fn register(
        &self,
        ctx: &Context<'_>,
        email: String,
        name: String,
        password: String,
    ) -> GqlResult<GqlAuthResult> {
        let gql = ctx.data::<GraphqlContext>()?;
        let account = gql.services.auth.register(&email, &name, &password, false)?;
        let token = gql.services.auth.sign_token(account.id)?;
        Ok(GqlAuthResult {
            token,
            account: account.into(),
        })
    }

    async fn login(&self, ctx: &Context<'_>, email: String, password: String) -> GqlResult<GqlAuthResult> {
        let gql = ctx.data::<GraphqlContext>()?;
        let (account, token) = gql.services.auth.login(&email, &password)?;
        Ok(GqlAuthResult {
            token,
            account: account.into(),
        })
    }

    async fn create_workspace(
        &self,
        ctx: &Context<'_>,
        name: String,
        slug: Option<String>,
        description: Option<String>,
    ) -> GqlResult<GqlWorkspace> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = gql.services.workspace.create(
            auth.account_id,
            &name,
            slug.as_deref(),
            description.as_deref().unwrap_or(""),
        )?;
        Ok(ws.into())
    }

    async fn create_entry(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        title: String,
    ) -> GqlResult<GqlEntry> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Worker)?;
        let entry = gql.services.entry.create(auth.account_id, ws_id, &title)?;
        Ok(GqlEntry::new(entry, vec![]))
    }

    async fn set_labeling(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        label_name: String,
        value: Json<serde_json::Value>,
    ) -> GqlResult<GqlLabeling> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        let labeling = gql
            .services
            .entry
            .set_labeling(auth.account_id, &entry_code, &label_name, &value.0)?;
        Ok(labeling.into())
    }

    async fn update_entry(
        &self,
        ctx: &Context<'_>,
        code: String,
        expected_updated_at: String,
        title: String,
        detail: String,
    ) -> GqlResult<GqlEntry> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        // 乐观并发：expectedUpdatedAt 与最新 updated_at 不一致时，服务层返回
        // ConflictDetected，其 GraphQL message 为「内容已被他人修改，请刷新后重试」。
        let updated = gql
            .services
            .entry
            .update(auth.account_id, &code, &expected_updated_at, &title, &detail)?;
        let labels = gql.services.entry.labelings(&code)?;
        Ok(GqlEntry::new(updated, labels))
    }

    async fn delete_entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        gql.services.entry.soft_delete(auth.account_id, &code)?;
        Ok(true)
    }

    async fn create_label_schema(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        title: String,
        value_type: String,
        enum_values: Vec<String>,
    ) -> GqlResult<GqlLabelSchema> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let vt = LabelValueType::from_str(&value_type)
            .ok_or_else(|| AppError::Internal("无效的标签值类型".to_string()))?;
        let schema = gql
            .services
            .label
            .create_schema(auth.account_id, ws_id, &name, &title, vt, enum_values)?;
        Ok(schema.into())
    }

    async fn update_label_schema(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        title: String,
        enum_values: Vec<String>,
    ) -> GqlResult<GqlLabelSchema> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let schema = gql
            .services
            .label
            .update_schema(auth.account_id, ws_id, &name, &title, enum_values)?;
        Ok(schema.into())
    }

    async fn remove_labeling(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        label_name: String,
    ) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        gql.services
            .entry
            .remove_labeling(auth.account_id, &entry_code, &label_name)?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    async fn create_view(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        query: Json<serde_json::Value>,
        sort: Option<SortInput>,
        columns: Vec<String>,
        is_shared: bool,
    ) -> GqlResult<GqlView> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_role(
            ws,
            if is_shared { WorkspaceRole::Maintainer } else { WorkspaceRole::Worker },
        )?;
        let q: ViewQuery = serde_json::from_value(query.0)
            .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")))?;
        let sort = sort.map(|s| s.to_sort()).transpose()?.unwrap_or_default();
        let v = gql
            .services
            .view
            .create(auth.account_id, ws, &name, q, sort, columns, is_shared)?;
        Ok(GqlView::new(v))
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_view(
        &self,
        ctx: &Context<'_>,
        id: ID,
        name: String,
        query: Json<serde_json::Value>,
        sort: Option<SortInput>,
        columns: Vec<String>,
        is_shared: bool,
    ) -> GqlResult<GqlView> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let view_id = parse_ulid(id.as_str())?;
        let existing = gql.services.view.get(view_id)?.ok_or(AppError::NotFound)?;
        gql.require_member(existing.workspace_id)?;
        // 共享视图需 Maintainer；个人视图本人可改，他人需 Maintainer。
        let need = if is_shared || existing.is_shared || existing.owner_id != auth.account_id {
            WorkspaceRole::Maintainer
        } else {
            WorkspaceRole::Worker
        };
        gql.require_role(existing.workspace_id, need)?;
        let q: ViewQuery = serde_json::from_value(query.0)
            .map_err(|e| AppError::InvalidQuery(format!("查询条件格式错误: {e}")))?;
        let sort = sort.map(|s| s.to_sort()).transpose()?.unwrap_or_default();
        let v = gql
            .services
            .view
            .update(auth.account_id, view_id, &name, q, sort, columns, is_shared)?;
        Ok(GqlView::new(v))
    }

    async fn delete_view(&self, ctx: &Context<'_>, id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let view_id = parse_ulid(id.as_str())?;
        let existing = gql.services.view.get(view_id)?.ok_or(AppError::NotFound)?;
        gql.require_member(existing.workspace_id)?;
        let need = if existing.is_shared || existing.owner_id != auth.account_id {
            WorkspaceRole::Maintainer
        } else {
            WorkspaceRole::Worker
        };
        gql.require_role(existing.workspace_id, need)?;
        gql.services.view.delete(auth.account_id, view_id)?;
        Ok(true)
    }
}

// ---------- 请求处理 ----------

pub type AppSchema = Schema<Query, Mutation, EmptySubscription>;

pub fn build_schema() -> AppSchema {
    Schema::build(Query, Mutation, EmptySubscription).finish()
}

/// 全局共享状态：Services + GraphQL schema。
pub struct AppState {
    pub services: Arc<Services>,
    pub schema: AppSchema,
}

pub async fn graphql_handler(
    Extension(state): Extension<Arc<AppState>>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> GraphQLResponse {
    let auth = extract_auth(&state.services, &headers);
    let gql_ctx = GraphqlContext {
        services: state.services.clone(),
        auth,
    };
    state.schema.execute(req.into_inner().data(gql_ctx)).await.into()
}

fn extract_auth(services: &Services, headers: &HeaderMap) -> Option<AuthContext> {
    // 优先 cookie，其次 Authorization: Bearer 头。
    if let Some(token) = cookie_value(headers, "jwt") {
        if let Ok(auth) = services.auth.verify_token(&token) {
            return Some(auth);
        }
    }
    if let Some(token) = bearer_token(headers) {
        if let Ok(auth) = services.auth.verify_token(&token) {
            return Some(auth);
        }
    }
    None
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(COOKIE)?.to_str().ok()?;
    for pair in cookie.split(';') {
        let mut kv = pair.trim().splitn(2, '=');
        if kv.next() == Some(name) {
            return kv.next().map(|s| s.trim().to_string());
        }
    }
    None
}

fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(|s| s.trim().to_string())
}

fn parse_ulid(s: &str) -> GqlResult<Ulid> {
    Ulid::from_string(s).map_err(|e| AppError::Internal(format!("无效 ID: {e}")).into())
}
