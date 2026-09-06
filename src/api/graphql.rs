use std::sync::Arc;

use async_graphql::{
    Context, EmptySubscription, ID, Json, Object, Result as GqlResult, Schema, SimpleObject,
};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::Extension;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::HeaderMap;
use ulid::Ulid;

use crate::domain::{Account, Entry, LabelSchema, Labeling, Workspace, WorkspaceRole};
use crate::error::AppError;
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

    async fn entries(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlEntry>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        let entries = gql.services.entry.list(ws_id)?;
        let mut out = Vec::new();
        for e in entries {
            let labels = gql.services.entry.labelings(&e.code)?;
            out.push(GqlEntry::new(e, labels));
        }
        Ok(out)
    }

    async fn label_schemas(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlLabelSchema>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        Ok(gql
            .services
            .workspace
            .label_schemas(ws_id)?
            .into_iter()
            .map(Into::into)
            .collect())
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
