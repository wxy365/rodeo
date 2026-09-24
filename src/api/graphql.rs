use std::sync::Arc;

use async_graphql::{
    Context, EmptySubscription, ID, Json, Object, Result as GqlResult, Schema, SimpleObject, Upload,
};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::extract::Extension;
use axum::http::header::{AUTHORIZATION, COOKIE};
use axum::http::HeaderMap;
use ulid::Ulid;

use crate::domain::{
    Account, AccountStatus, ActionTarget, Attachment, AuditLog, AutomationRule, Comment, Entry,
    Invite,
    LabelSchema, LabelValueType, LinkKind,
    LabelWrite, Labeling, NamedPrompt, Query as ViewQuery, SortField, SortKey, SortSpec, TitleColorRule,
    ValueColor, ValueSource, View, ViewTimeline, Workspace, WorkspaceAiConfig, WorkspaceMember,
    WorkspaceRole,
    WriteOp, ATTACHMENT_URL_PREFIX,
};
use crate::error::AppError;
use crate::service::ai::derive_title;
use crate::service::entry::PageInput as EntryPageInput;
use crate::service::{AuthContext, Services};
use crate::service::auth::generate_initial_password;

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

/// 管理员建号的结果。初始密码只存在于这一次响应里——服务端只存 Argon2 哈希，
/// 没有明文可再取——所以前端必须当场展示并可复制。
#[derive(SimpleObject, Clone)]
pub struct GqlCreatedAccount {
    account: GqlAccount,
    initial_password: String,
}

/// 账号管理列表里的一个账号。
///
/// 单独一个类型，而不是给 `GqlAccount` 加字段：`GqlAccount` 的 `From<Account>`
/// 是不会失败的转换，却拿不到状态（状态在另一个列族里，只有服务层知道）；
/// 硬塞进去就得把这个 `From` 改成带状态参数的构造函数，`me` / `login` /
/// `register` / `createAccount` 四处都会跟着变，还会把「冻结/注销」泄漏进
/// 登录响应。这个投影只在 `require_admin` 后面出现。
#[derive(SimpleObject, Clone)]
pub struct GqlAdminAccount {
    id: ID,
    email: String,
    name: String,
    is_admin: bool,
    /// `"active"` / `"frozen"` / `"deactivated"`。
    status: String,
    /// RFC3339。
    created_at: String,
    /// 是否配置里 `auth.builtin.admin_email` 指定的那个账号。前端据此把它的
    /// 「冻结 / 注销」按钮禁掉并说明原因——否则那两个按钮点了必然失败，
    /// 摆在那里只会让人一次次去点。这个投影只在 `require_admin` 后面出现，
    /// 所以它不构成新的信息暴露。
    is_builtin: bool,
}

impl GqlAdminAccount {
    fn new(a: Account, status: AccountStatus, builtin_email: &str) -> Self {
        let is_builtin = a.email == builtin_email;
        Self {
            id: a.id.to_string().into(),
            email: a.email,
            name: a.name,
            is_admin: a.is_admin,
            status: status.as_str().to_string(),
            created_at: a.created_at.to_rfc3339(),
            is_builtin,
        }
    }
}

/// 配置里指定的内置管理员邮箱，小写去空格——与 `AuthService` 里 `normalize_email`
/// 的落地形态一致，这样才比得中库里存的那个账号。
fn builtin_admin_email(gql: &GraphqlContext) -> String {
    gql.services.config.auth.builtin.admin_email.trim().to_lowercase()
}

/// 免登录可见的服务端开关，供登录页决定是否展示注册入口。
#[derive(SimpleObject, Clone)]
pub struct GqlServerConfig {
    allow_registration: bool,
}

#[derive(SimpleObject, Clone)]
pub struct GqlWorkspace {
    id: ID,
    name: String,
    slug: String,
    description: String,
    /// 软删除时间；null 表示正常。删除标记存在单独的 CF，不进 Workspace 文档。
    deleted_at: Option<String>,
}

impl GqlWorkspace {
    fn with_state(w: Workspace, deleted_at: Option<String>) -> Self {
        Self {
            id: w.id.to_string().into(),
            name: w.name,
            slug: w.slug,
            description: w.description,
            deleted_at,
        }
    }
}

impl From<Workspace> for GqlWorkspace {
    fn from(w: Workspace) -> Self {
        Self::with_state(w, None)
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
    color: Option<String>,
    value_colors: Json<serde_json::Value>,
    multi: bool,
    format: Option<String>,
    currency_symbol: Option<String>,
    unit: Option<String>,
    default_value: Json<serde_json::Value>,
    /// 继承/覆盖关系，形如 `[{"kind":"inherit","other":"L4","otherValue":"V1"}]`。
    links: Json<serde_json::Value>,
}

impl From<LabelSchema> for GqlLabelSchema {
    fn from(s: LabelSchema) -> Self {
        let value_colors =
            serde_json::to_value(&s.value_colors).unwrap_or(serde_json::Value::Null);
        let default_value = s
            .default_value
            .as_ref()
            .map(|v| v.to_json())
            .unwrap_or(serde_json::Value::Null);
        // 手写而非 serde 直出：LabelValue 的 serde 形态是外部标记枚举（`{"string":"V1"}`），
        // 前端只需朴素 JSON 值，写出去还得能原样传回来。
        let links = serde_json::Value::Array(
            s.links
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "kind": match l.kind {
                            LinkKind::Inherit => "inherit",
                            LinkKind::Override => "override",
                        },
                        "other": l.other,
                        "otherValue": l.other_value.as_ref().map(|v| v.to_json()),
                        "ownValue": l.own_value.as_ref().map(|v| v.to_json()),
                    })
                })
                .collect(),
        );
        Self {
            name: s.name,
            title: s.title,
            value_type: s.value_type.as_str().to_string(),
            enum_values: s.enum_values,
            color: s.color,
            value_colors: Json(value_colors),
            multi: s.multi,
            format: s.format,
            currency_symbol: s.currency_symbol,
            unit: s.unit,
            default_value: Json(default_value),
            links: Json(links),
        }
    }
}

/// 标签类型的附加属性，随 create/update 一起提交；缺省时保持默认（非多值、无格式）。
#[derive(async_graphql::InputObject)]
#[graphql(rename_fields = "camelCase")]
pub struct LabelSchemaAttrsInput {
    multi: Option<bool>,
    format: Option<String>,
    currency_symbol: Option<String>,
    unit: Option<String>,
    /// 默认值：缺省或 `null` 表示没有默认值。
    default_value: Option<Json<serde_json::Value>>,
    /// 继承/覆盖关系；缺省表示清空。
    links: Option<Json<serde_json::Value>>,
}

impl LabelSchemaAttrsInput {
    fn to_service(
        self,
        name: String,
        title: String,
        value_type: LabelValueType,
        enum_values: Vec<String>,
        color: Option<String>,
        value_colors: Vec<ValueColor>,
    ) -> GqlResult<crate::service::label::LabelSchemaInput> {
        let links = parse_json_list(self.links, "标签关系")?;
        Ok(crate::service::label::LabelSchemaInput {
            name,
            title,
            value_type,
            enum_values,
            multi: self.multi.unwrap_or(false),
            format: self.format,
            currency_symbol: self.currency_symbol,
            unit: self.unit,
            color,
            value_colors,
            default_value: self.default_value.map(|j| j.0),
            links,
        })
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
pub struct GqlNamedPrompt {
    name: String,
    prompt: String,
}

impl From<NamedPrompt> for GqlNamedPrompt {
    fn from(p: NamedPrompt) -> Self {
        Self {
            name: p.name,
            prompt: p.prompt,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlWorkspaceAiConfig {
    scenarios: Vec<GqlNamedPrompt>,
    tones: Vec<GqlNamedPrompt>,
}

impl From<WorkspaceAiConfig> for GqlWorkspaceAiConfig {
    fn from(c: WorkspaceAiConfig) -> Self {
        Self {
            scenarios: c.scenarios.into_iter().map(Into::into).collect(),
            tones: c.tones.into_iter().map(Into::into).collect(),
        }
    }
}

/// 「名称 + 提示词」输入行。整体替换语义：提交什么就是什么。
#[derive(async_graphql::InputObject)]
pub struct NamedPromptInput {
    name: String,
    prompt: String,
}

impl NamedPromptInput {
    fn into_named(self) -> NamedPrompt {
        NamedPrompt {
            name: self.name,
            prompt: self.prompt,
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
    /// 归档时间（RFC3339）；未归档为 null。前端据此决定详情面板显示「归档」还是「取消归档」。
    archived_at: Option<String>,
    /// 创建人 / 更新人账号；账号已删除则为 null。不改上面的 createdBy / updatedBy（ID）。
    created_by_account: Option<GqlAccount>,
    updated_by_account: Option<GqlAccount>,
    labels: Vec<GqlLabeling>,
}

impl GqlEntry {
    fn new(
        entry: Entry,
        labels: Vec<Labeling>,
        archived_at: Option<String>,
        created_by_account: Option<GqlAccount>,
        updated_by_account: Option<GqlAccount>,
    ) -> Self {
        Self {
            code: entry.code,
            workspace_id: entry.workspace_id.to_string().into(),
            title: entry.title,
            detail: entry.detail,
            created_by: entry.created_by.to_string().into(),
            updated_by: entry.updated_by.to_string().into(),
            created_at: entry.created_at.to_rfc3339(),
            updated_at: entry.updated_at.to_rfc3339(),
            archived_at,
            created_by_account,
            updated_by_account,
            labels: labels.into_iter().map(Into::into).collect(),
        }
    }
}

/// 组装 GqlEntry，顺带补上归档标记与创建/更新人账号——调用点不必各自去查 CF。
fn gql_entry(
    gql: &GraphqlContext,
    entry: Entry,
    labels: Vec<Labeling>,
) -> GqlResult<GqlEntry> {
    let archived_at = gql.services.entry.archived_at(&entry.code)?;
    let created_by_account = gql.services.auth.find_by_id(entry.created_by)?.map(Into::into);
    let updated_by_account = gql.services.auth.find_by_id(entry.updated_by)?.map(Into::into);
    Ok(GqlEntry::new(
        entry,
        labels,
        archived_at,
        created_by_account,
        updated_by_account,
    ))
}

#[derive(SimpleObject, Clone)]
pub struct GqlComment {
    id: ID,
    entry_code: String,
    body: String,
    created_by: ID,
    updated_by: ID,
    created_at: String,
    updated_at: String,
    /// 作者 / 最后修改人账号；账号已删除则为 null。与 GqlEntry 同一套回填方式。
    created_by_account: Option<GqlAccount>,
    updated_by_account: Option<GqlAccount>,
}

#[derive(SimpleObject, Clone)]
pub struct GqlCommentCount {
    entry_code: String,
    count: i32,
}

/// 组装 GqlComment，顺带补上作者与最后修改人的账号。
fn gql_comment(gql: &GraphqlContext, c: Comment) -> GqlResult<GqlComment> {
    let created_by_account = gql.services.auth.find_by_id(c.created_by)?.map(Into::into);
    let updated_by_account = gql.services.auth.find_by_id(c.updated_by)?.map(Into::into);
    Ok(GqlComment {
        id: c.id.to_string().into(),
        entry_code: c.entry_code,
        body: c.body,
        created_by: c.created_by.to_string().into(),
        updated_by: c.updated_by.to_string().into(),
        created_at: c.created_at.to_rfc3339(),
        updated_at: c.updated_at.to_rfc3339(),
        created_by_account,
        updated_by_account,
    })
}

#[derive(SimpleObject, Clone)]
pub struct GqlAttachment {
    id: ID,
    entry_code: String,
    filename: String,
    content_type: String,
    size: i32,
    /// 由服务端拼死：下载路径只有一处定义，客户端不自己拼。
    url: String,
    created_at: String,
    created_by: ID,
    created_by_account: Option<GqlAccount>,
}

/// 组装 GqlAttachment，顺带补上上传者账号。
fn gql_attachment(gql: &GraphqlContext, a: Attachment) -> GqlResult<GqlAttachment> {
    let created_by_account = gql.services.auth.find_by_id(a.created_by)?.map(Into::into);
    Ok(GqlAttachment {
        id: a.id.to_string().into(),
        entry_code: a.entry_code,
        filename: a.filename,
        content_type: a.content_type,
        // GraphQL Int 是 32 位；上限 50MB 远在范围内。
        size: a.size as i32,
        url: format!("{ATTACHMENT_URL_PREFIX}{}", a.id),
        created_at: a.created_at.to_rfc3339(),
        created_by: a.created_by.to_string().into(),
        created_by_account,
    })
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
pub struct GqlMember {
    account_id: ID,
    email: String,
    name: String,
    role: String,
    joined_at: String,
}

impl GqlMember {
    fn new(m: WorkspaceMember, a: Account) -> Self {
        Self {
            account_id: m.account_id.to_string().into(),
            email: a.email,
            name: a.name,
            role: m.role.as_str().to_string(),
            joined_at: m.joined_at.to_rfc3339(),
        }
    }
}

/// 待接受的邀请。同一个类型服务两个方向：工作空间侧（谁被邀请了）与账号侧（我收到了什么）。
#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlInvite {
    workspace_id: ID,
    workspace_name: String,
    workspace_slug: String,
    account_id: ID,
    email: String,
    name: String,
    role: String,
    invited_by: ID,
    created_at: String,
}

impl GqlInvite {
    fn new(inv: Invite, ws: &Workspace, a: &Account) -> Self {
        Self {
            workspace_id: inv.workspace_id.to_string().into(),
            workspace_name: ws.name.clone(),
            workspace_slug: ws.slug.clone(),
            account_id: inv.account_id.to_string().into(),
            email: a.email.clone(),
            name: a.name.clone(),
            role: inv.role.as_str().to_string(),
            invited_by: inv.invited_by.to_string().into(),
            created_at: inv.created_at.to_rfc3339(),
        }
    }
}

/// 标题颜色规则的**线上**形态：`query` 是嵌套对象，与 `GqlView.query` 同一形状。
/// 领域侧的 `TitleColorRule` 给 `query` 套了 `query_json`（bincode 要字符串），
/// 直接复用它收发会让写侧索要字符串、读侧发出字符串，两头都错。
/// 这里不带适配器——serde_json 支持 `deserialize_any`，`Query` 自身的 serde 够用。
#[derive(serde::Deserialize)]
struct TitleColorRuleWire {
    query: ViewQuery,
    color: String,
}

/// 一个排序键的线上形态。`field` 是内置字段名或标签名。
#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlSortKey {
    field: String,
    desc: bool,
}

impl From<SortKey> for GqlSortKey {
    fn from(k: SortKey) -> Self {
        Self { field: k.field.as_str().to_string(), desc: k.desc }
    }
}

#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlView {
    id: ID,
    name: String,
    query: Json<serde_json::Value>,
    query_expr: String,
    sorts: Vec<GqlSortKey>,
    columns: Vec<String>,
    is_shared: bool,
    owner_id: ID,
    created_at: String,
    updated_at: String,
    title_colors: Json<serde_json::Value>,
    entry_count: i32,
    /// 是否为该工作空间的基础视图（不可删除、始终存在）。
    is_default: bool,
    /// 时间轴配置；未配置为 `null`，此时前端不显示「普通 / 时间轴」开关。
    timeline: Option<GqlViewTimeline>,
}

/// 视图的时间轴配置。必须是**对象类型**而不是 JSON 标量：客户端按
/// `timeline { start end person }` 取字段，JSON 标量不允许带子选择，整条 views 查询会被拒。
#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlViewTimeline {
    start: String,
    end: String,
    person: Option<String>,
}

impl From<ViewTimeline> for GqlViewTimeline {
    fn from(t: ViewTimeline) -> Self {
        Self {
            start: t.start,
            end: t.end,
            person: t.person,
        }
    }
}

impl GqlView {
    fn new(v: View, entry_count: i32, is_default: bool, timeline: Option<ViewTimeline>) -> Self {
        let query = serde_json::to_value(&v.query).unwrap_or(serde_json::Value::Null);
        let query_expr = v.query.to_expr();
        // 手写投影：整结构序列化会把 TitleColorRule.query 变成转义过的 JSON 字符串。
        let title_colors = serde_json::Value::Array(
            v.title_colors
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "query": serde_json::to_value(&r.query)
                            .unwrap_or(serde_json::Value::Null),
                        "color": r.color,
                    })
                })
                .collect(),
        );
        let sorts: Vec<GqlSortKey> = v.sort.keys.into_iter().map(GqlSortKey::from).collect();
        let timeline = timeline.map(GqlViewTimeline::from);
        Self {
            id: v.id.to_string().into(),
            name: v.name,
            query: Json(query),
            query_expr,
            sorts,
            columns: v.columns,
            is_shared: v.is_shared,
            owner_id: v.owner_id.to_string().into(),
            created_at: v.created_at.to_rfc3339(),
            updated_at: v.updated_at.to_rfc3339(),
            title_colors: Json(title_colors),
            entry_count,
            is_default,
            timeline,
        }
    }
}

#[derive(SimpleObject, Clone)]
pub struct GqlRuleWrite {
    label_name: String,
    op: String,
    /// literal / now / new / old
    value_kind: String,
    value: Json<serde_json::Value>,
}

#[derive(SimpleObject, Clone)]
pub struct GqlAutomationRule {
    id: ID,
    name: String,
    enabled: bool,
    trigger_expr: String,
    target_event_source: bool,
    /// 目标为「事件源条目」时为空串。
    target_expr: String,
    writes: Vec<GqlRuleWrite>,
    created_at: String,
    updated_at: String,
}

// 逐字段手工映射：`trigger` / `ActionTarget::Query` / `ValueSource::Literal` 在领域侧套了
// query_json / raw_json，整结构序列化会把它们变成转义后的 JSON 字符串而非对象。
impl From<AutomationRule> for GqlAutomationRule {
    fn from(r: AutomationRule) -> Self {
        let trigger_expr = r.trigger.to_expr();
        let (target_event_source, target_expr) = match &r.action.target {
            ActionTarget::EventSource => (true, String::new()),
            ActionTarget::Query(q) => (false, q.to_expr()),
        };
        let writes = r
            .action
            .writes
            .iter()
            .map(|w| {
                let (kind, value) = match &w.value {
                    Some(ValueSource::Literal(v)) => ("literal", v.clone()),
                    Some(ValueSource::Now) => ("now", serde_json::Value::Null),
                    Some(ValueSource::New) => ("new", serde_json::Value::Null),
                    Some(ValueSource::Old) => ("old", serde_json::Value::Null),
                    None => ("", serde_json::Value::Null),
                };
                GqlRuleWrite {
                    label_name: w.label_name.clone(),
                    op: match w.op {
                        WriteOp::Set => "set".to_string(),
                        WriteOp::Remove => "remove".to_string(),
                    },
                    value_kind: kind.to_string(),
                    value: Json(value),
                }
            })
            .collect();
        Self {
            id: r.id.to_string().into(),
            name: r.name,
            enabled: r.enabled,
            trigger_expr,
            target_event_source,
            target_expr,
            writes,
            created_at: r.created_at.to_rfc3339(),
            updated_at: r.updated_at.to_rfc3339(),
        }
    }
}

#[derive(async_graphql::InputObject)]
pub struct RuleWriteInput {
    label_name: String,
    op: String,
    /// literal / now / new / old
    value_kind: String,
    value: Option<Json<serde_json::Value>>,
}

/// 入参 → 领域写入。op / valueKind 的字符串在这里收敛成枚举，非法值给可读错误。
fn to_rule_writes(inputs: Vec<RuleWriteInput>) -> GqlResult<Vec<LabelWrite>> {
    let mut out = Vec::with_capacity(inputs.len());
    for i in inputs {
        let op = match i.op.as_str() {
            "set" => WriteOp::Set,
            "remove" => WriteOp::Remove,
            other => {
                return Err(AppError::InvalidQuery(format!("未知的标签操作: {other}")).into())
            }
        };
        let value = match op {
            WriteOp::Remove => None,
            WriteOp::Set => {
                let kind = match i.value_kind.as_str() {
                    "literal" => {
                        ValueSource::Literal(i.value.map(|j| j.0).unwrap_or(serde_json::Value::Null))
                    }
                    "now" => ValueSource::Now,
                    "new" => ValueSource::New,
                    "old" => ValueSource::Old,
                    // 缺省即字面量：前端下拉未选时不该报错，值本身仍会被校验。
                    "" => {
                        ValueSource::Literal(i.value.map(|j| j.0).unwrap_or(serde_json::Value::Null))
                    }
                    other => {
                        return Err(
                            AppError::InvalidQuery(format!("未知的值来源: {other}")).into()
                        )
                    }
                };
                Some(kind)
            }
        };
        out.push(LabelWrite {
            label_name: i.label_name,
            op,
            value,
        });
    }
    Ok(out)
}

#[derive(SimpleObject, Clone)]
#[graphql(rename_fields = "camelCase")]
pub struct GqlEntryConnection {
    items: Vec<GqlEntry>,
    total: i32,
    page: i32,
    page_size: i32,
    /// 命中条目实际带有的标签名（跨分页去重），前端用它提示可用标签。
    label_names: Vec<String>,
}

#[derive(async_graphql::InputObject)]
pub struct SortInput {
    field: Option<String>,
    desc: Option<bool>,
}

impl SortInput {
    fn to_key(&self) -> SortKey {
        let field = match &self.field {
            // 内置三值优先：标签恰好叫 `title` 时仍按内置「标题」解释。
            Some(f) => SortField::from_str(f).unwrap_or_else(|| SortField::Label(f.clone())),
            None => SortField::UpdatedAt,
        };
        SortKey { field, desc: self.desc.unwrap_or(true) }
    }
}

/// 入参缺省（`None` / 空数组）落回默认排序，与改动前一致。
fn to_sort(sorts: Option<Vec<SortInput>>) -> SortSpec {
    let keys: Vec<SortKey> = sorts.unwrap_or_default().iter().map(|s| s.to_key()).collect();
    if keys.is_empty() {
        SortSpec::default()
    } else {
        SortSpec { keys }
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

/// 批量设置标签时的一条待写值。无值标签（Task/Bug 这类）的 value 传 null 或缺省。
#[derive(async_graphql::InputObject)]
pub struct LabelingInput {
    name: String,
    value: Option<Json<serde_json::Value>>,
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

    /// 系统管理员专属。判据是账号上的 `is_admin` 标记，而不是拿邮箱去比
    /// `Config.auth.builtin.admin_email`——邮箱是可变配置，标记才是身份。
    /// 该标记只有两条来路：启动期的 `bootstrap_admin`（读 `admin_email`）与
    /// 管理员自己调 `createAccount(isAdmin: true)`。两条都源自配置里那个账号，
    /// 所以 `is_admin == true` 仍然等价于 spec 说的「配置文件中指定的管理员账号」。
    fn require_admin(&self) -> GqlResult<AuthContext> {
        let auth = self.require_auth()?;
        let account = self
            .services
            .auth
            .find_by_id(auth.account_id)?
            .ok_or(AppError::Unauthorized)?;
        if !account.is_admin {
            return Err(AppError::Forbidden.into());
        }
        Ok(auth)
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

    /// 账号管理列表，仅系统管理员可见。
    async fn accounts(&self, ctx: &Context<'_>) -> GqlResult<Vec<GqlAdminAccount>> {
        let gql = ctx.data::<GraphqlContext>()?;
        gql.require_admin()?;
        let builtin = builtin_admin_email(gql);
        Ok(gql
            .services
            .auth
            .list_all()?
            .into_iter()
            .map(|(a, s)| GqlAdminAccount::new(a, s, &builtin))
            .collect())
    }

    /// 免登录：登录页据此渲染注册入口。
    async fn server_config(&self, ctx: &Context<'_>) -> GqlResult<GqlServerConfig> {
        let gql = ctx.data::<GraphqlContext>()?;
        Ok(GqlServerConfig {
            allow_registration: gql.services.config.auth.builtin.allow_registration,
        })
    }

    async fn workspaces(&self, ctx: &Context<'_>) -> GqlResult<Vec<GqlWorkspaceWithRole>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let list = gql.services.workspace.list_for(auth.account_id)?;
        Ok(list
            .into_iter()
            .map(|(w, r, deleted_at)| GqlWorkspaceWithRole {
                workspace: GqlWorkspace::with_state(w, deleted_at),
                role: r.as_str().to_string(),
            })
            .collect())
    }

    async fn workspace(&self, ctx: &Context<'_>, slug: String) -> GqlResult<Option<GqlWorkspace>> {
        let gql = ctx.data::<GraphqlContext>()?;
        gql.require_auth()?;
        let Some(ws) = gql.services.workspace.get_by_slug(&slug)? else {
            return Ok(None);
        };
        let deleted_at = gql.services.workspace.deleted_at(ws.id)?;
        Ok(Some(GqlWorkspace::with_state(ws, deleted_at)))
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

    /// 场景 / 语气配置。成员即可读（与 `labelSchemas` 一致）——生成弹窗要用它渲染下拉框。
    async fn workspace_ai_config(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
    ) -> GqlResult<GqlWorkspaceAiConfig> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        Ok(gql.services.ai.get_config(ws_id)?.into())
    }

    async fn entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<Option<GqlEntry>> {
        let gql = ctx.data::<GraphqlContext>()?;
        gql.require_auth()?;
        let Some(entry) = gql.services.entry.get(&code)? else {
            return Ok(None);
        };
        gql.require_member(entry.workspace_id)?;
        let labels = gql.services.entry.labelings(&code)?;
        Ok(Some(gql_entry(gql, entry, labels)?))
    }

    /// 某条目的全部评论，按发表时间升序。成员即可读（与 labelSchemas 一致）。
    async fn comments(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
    ) -> GqlResult<Vec<GqlComment>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_member(entry.workspace_id)?;
        gql.services
            .comment
            .list(&entry_code)?
            .into_iter()
            .map(|c| gql_comment(gql, c))
            .collect()
    }

    /// 某条目的全部附件，按上传时间升序。成员即可读（与 `comments` 一致）。
    async fn attachments(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
    ) -> GqlResult<Vec<GqlAttachment>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_member(entry.workspace_id)?;
        gql.services
            .attachment
            .list(&entry_code)?
            .into_iter()
            .map(|a| gql_attachment(gql, a))
            .collect()
    }

    /// 视图表格当前页的评论计数。越权或已删除的条目直接跳过——不泄露其存在性。
    async fn comment_counts(
        &self,
        ctx: &Context<'_>,
        entry_codes: Vec<String>,
    ) -> GqlResult<Vec<GqlCommentCount>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let mut out = Vec::with_capacity(entry_codes.len());
        for code in entry_codes {
            let Some(entry) = gql.services.entry.get(&code)? else {
                continue;
            };
            if gql
                .services
                .workspace
                .get_member(entry.workspace_id, auth.account_id)?
                .is_none()
            {
                continue;
            }
            out.push(GqlCommentCount {
                entry_code: code.clone(),
                count: gql.services.comment.count(&code)? as i32,
            });
        }
        Ok(out)
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

    async fn members(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlMember>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        Ok(gql
            .services
            .workspace
            .list_members(ws)?
            .into_iter()
            .map(|(m, a)| GqlMember::new(m, a))
            .collect())
    }

    /// 某工作空间待接受的邀请。需要是该工作空间的成员。
    async fn invites(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlInvite>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws_id)?;
        let ws = gql.services.workspace.get_by_id(ws_id)?.ok_or(AppError::NotFound)?;
        Ok(gql
            .services
            .workspace
            .list_invites(ws_id)?
            .into_iter()
            .map(|(inv, a)| GqlInvite::new(inv, &ws, &a))
            .collect())
    }

    /// 我收到的、尚未接受的邀请。仅需登录——被邀请的人此时还不是成员。
    async fn my_invites(&self, ctx: &Context<'_>) -> GqlResult<Vec<GqlInvite>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let account = gql
            .services
            .auth
            .find_by_id(auth.account_id)?
            .ok_or(AppError::NotFound)?;
        Ok(gql
            .services
            .workspace
            .list_invites_for(auth.account_id)?
            .into_iter()
            .map(|(inv, ws)| GqlInvite::new(inv, &ws, &account))
            .collect())
    }

    async fn views(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<Vec<GqlView>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        // 基础视图可能还没建立（老工作空间），首次拉取时补上。
        let def = gql.services.view.ensure_default(auth.account_id, ws)?.id;
        let mut out = Vec::new();
        for v in gql.services.view.list(auth.account_id, ws)? {
            let count = gql.services.entry.count(ws, &v.query)? as i32;
            let is_default = v.id == def;
            let timeline = gql.services.view.timeline_of(v.id)?;
            out.push(GqlView::new(v, count, is_default, timeline));
        }
        Ok(out)
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
        let count = gql.services.entry.count(v.workspace_id, &v.query)? as i32;
        let is_default = gql.services.view.default_view_id(v.workspace_id)? == Some(v.id);
        let timeline = gql.services.view.timeline_of(v.id)?;
        Ok(Some(GqlView::new(v, count, is_default, timeline)))
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

    /// 规则列表：成员即可读。返回顺序即求值顺序（创建顺序）。
    async fn automation_rules(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
    ) -> GqlResult<Vec<GqlAutomationRule>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        Ok(gql
            .services
            .rule
            .list(ws)?
            .into_iter()
            .map(Into::into)
            .collect())
    }

    /// 解析并按规则校验触发条件，供编辑器实时校验。
    async fn parse_rule_trigger(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        expr: String,
    ) -> GqlResult<Json<serde_json::Value>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let q = gql.services.rule.parse_trigger(ws, &expr)?;
        Ok(Json(serde_json::to_value(&q).unwrap_or(serde_json::Value::Null)))
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
        sorts: Option<Vec<SortInput>>,
        page: Option<PageInput>,
    ) -> GqlResult<GqlEntryConnection> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        let q = parse_query_json(query)?;
        q.validate(&gql.services.label.list_schemas(ws)?)?;
        let sort = to_sort(sorts);
        let page = page.map(|p| p.to_page()).unwrap_or_default();
        let result = gql.services.entry.query(ws, &q, &sort, page)?;
        let items = result
            .items
            .into_iter()
            .map(|(e, labels)| gql_entry(gql, e, labels))
            .collect::<GqlResult<Vec<_>>>()?;
        Ok(GqlEntryConnection {
            items,
            total: result.total as i32,
            page: page.page as i32,
            page_size: page.page_size as i32,
            label_names: result.label_names,
        })
    }

    /// 已归档条目（Reader+），按归档时间倒序。
    async fn archived_entries(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
    ) -> GqlResult<Vec<GqlEntry>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_member(ws)?;
        gql.services
            .entry
            .list_archived(ws)?
            .into_iter()
            .map(|e| {
                let labels = gql.services.entry.labelings(&e.code)?;
                gql_entry(gql, e, labels)
            })
            .collect()
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
        let account = gql.services.auth.register(&email, &name, &password)?;
        let token = gql.services.auth.sign_token(account.id)?;
        Ok(GqlAuthResult {
            token,
            account: account.into(),
        })
    }

    /// 系统管理员建号，返回初始密码用于分发。
    /// `password` 留空则由服务端生成一个必然满足强度规则的随机密码。
    async fn create_account(
        &self,
        ctx: &Context<'_>,
        email: String,
        name: String,
        #[graphql(default)] is_admin: bool,
        #[graphql(default)] password: Option<String>,
    ) -> GqlResult<GqlCreatedAccount> {
        let gql = ctx.data::<GraphqlContext>()?;
        gql.require_admin()?;
        let initial_password = match password {
            Some(p) if !p.trim().is_empty() => p,
            _ => generate_initial_password(),
        };
        // 走 create_by_admin：自助注册开关不该拦住管理员建号。
        let account =
            gql.services
                .auth
                .create_by_admin(&email, &name, &initial_password, is_admin)?;
        Ok(GqlCreatedAccount {
            account: account.into(),
            initial_password,
        })
    }

    /// 冻结 / 解冻 / 注销账号，仅系统管理员。一个 mutation 覆盖三个动作：它们本质是
    /// 同一个写操作（改状态 + 推进令牌版本 + 注销时释放邮箱），拆成三个会有三份重复
    /// 的守卫，还容易在某条路径上漏掉 `revoke_tokens`——那正是「冻结了却没踢下线」
    /// 这类 bug 的来源。
    async fn set_account_status(
        &self,
        ctx: &Context<'_>,
        account_id: ID,
        status: String,
    ) -> GqlResult<GqlAdminAccount> {
        let gql = ctx.data::<GraphqlContext>()?;
        let admin = gql.require_admin()?;
        let Some(status) = AccountStatus::parse(&status) else {
            return Err(AppError::InvalidQuery("未知的账号状态".to_string()).into());
        };
        let target_id = Ulid::from_string(&account_id.0)
            .map_err(|_| AppError::InvalidQuery("账号 ID 格式非法".to_string()))?;

        // 保护一：不能对自己操作。否则管理员一点就把自己锁在门外。
        if target_id == admin.account_id {
            return Err(AppError::InvalidQuery("不能对自己的账号执行该操作".to_string()).into());
        }
        // 保护二：内置管理员（配置里 auth.builtin.admin_email 指定的那个账号）不可冻结、
        // 不可注销。注销会释放邮箱索引，而启动引导 `bootstrap_admin` 只按邮箱找账号——
        // 找不到就建，于是下次重启会照配置再建一个同邮箱的新账号，密码就是配置文件里
        // 那个。冻结的风险是另一个：它若是唯一的管理员，就再没人能解冻它。
        // 这道守卫要拿配置跟目标账号的邮箱比对，而配置不在服务层，故放在这里。
        let builtin = builtin_admin_email(gql);
        let target = gql.services.auth.find_by_id(target_id)?.ok_or(AppError::NotFound)?;
        if status != AccountStatus::Active && target.email == builtin {
            return Err(
                AppError::InvalidQuery("内置管理员账号不能冻结或注销".to_string()).into(),
            );
        }

        let account = gql.services.auth.set_status(target_id, status)?;
        let updated_status = gql.services.auth.status(target_id)?;
        Ok(GqlAdminAccount::new(account, updated_status, &builtin))
    }

    /// 改账号的姓名与管理员标记，仅系统管理员。状态与密码各有各的 mutation——那两条
    /// 路径分别附带吊销令牌 / 验旧密码，跟这里纯粹的字段覆盖不是一回事。
    async fn update_account(
        &self,
        ctx: &Context<'_>,
        account_id: ID,
        name: String,
        is_admin: bool,
    ) -> GqlResult<GqlAdminAccount> {
        let gql = ctx.data::<GraphqlContext>()?;
        let admin = gql.require_admin()?;
        let target_id = Ulid::from_string(&account_id.0)
            .map_err(|_| AppError::InvalidQuery("账号 ID 格式非法".to_string()))?;

        let builtin = builtin_admin_email(gql);
        let target = gql.services.auth.find_by_id(target_id)?.ok_or(AppError::NotFound)?;

        // 两道守卫只管「撤销管理员」这一个方向：提权是安全的，把本来就是管理员的账号
        // 再存一次管理员也不该被拒——只认 `!is_admin` 会把这种原样保存也一并拦下。
        let demoting = target.is_admin && !is_admin;
        // 保护一：不能撤销自己的管理员权限。一点就把自己关在门外，而这与冻结自己不同
        // ——冻结了自己还有别人能解冻，这个没人能改回来。
        if demoting && target_id == admin.account_id {
            return Err(AppError::InvalidQuery("不能撤销自己的管理员权限".to_string()).into());
        }
        // 保护二：内置管理员的管理员权限不可撤销。启动引导 `bootstrap_admin` 只按邮箱
        // 找账号、找到就原样返回——它**不会**把降了权的账号重新提上来，所以一旦撤销，
        // 下次重启也补不回来，系统可能就此再无管理员。
        if demoting && target.email == builtin {
            return Err(
                AppError::InvalidQuery("内置管理员账号的管理员权限不可撤销".to_string()).into(),
            );
        }

        let account = gql.services.auth.update_profile(target_id, &name, is_admin)?;
        let updated_status = gql.services.auth.status(target_id)?;
        Ok(GqlAdminAccount::new(account, updated_status, &builtin))
    }

    async fn login(&self, ctx: &Context<'_>, email: String, password: String) -> GqlResult<GqlAuthResult> {
        let gql = ctx.data::<GraphqlContext>()?;
        let (account, token) = gql.services.auth.login(&email, &password)?;
        Ok(GqlAuthResult {
            token,
            account: account.into(),
        })
    }

    /// 改密。成功后吊销该账号**所有**已签发令牌，再为当前设备重签一张：
    /// 其他设备被迫重新登录（密码换过，旧会话不该续命），本设备拿着新令牌无感续用。
    async fn change_password(
        &self,
        ctx: &Context<'_>,
        old_password: String,
        new_password: String,
    ) -> GqlResult<GqlAuthResult> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let account = gql
            .services
            .auth
            .change_password(auth.account_id, &old_password, &new_password)?;
        gql.services.auth.revoke_tokens(auth.account_id)?;
        let token = gql.services.auth.sign_token(auth.account_id)?;
        Ok(GqlAuthResult {
            token,
            account: account.into(),
        })
    }

    /// 登出：吊销该账号所有已签发令牌。无有效令牌时也返回成功，保证幂等，
    /// 这样「令牌已过期/已吊销」的前端仍能干净地清掉本地状态。
    async fn logout(&self, ctx: &Context<'_>) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        if let Some(auth) = gql.auth {
            gql.services.auth.revoke_tokens(auth.account_id)?;
        }
        Ok(true)
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

    /// 改名称/描述需 Maintainer；slug 是 URL 身份，独立变更需 Owner。
    async fn update_workspace(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        description: String,
        slug: Option<String>,
    ) -> GqlResult<GqlWorkspace> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        let min_role = if slug.is_some() {
            WorkspaceRole::Owner
        } else {
            WorkspaceRole::Maintainer
        };
        gql.require_role(ws_id, min_role)?;
        let ws = gql.services.workspace.update(
            auth.account_id,
            ws_id,
            &name,
            &description,
            slug.as_deref(),
        )?;
        Ok(ws.into())
    }

    /// 一步转让所有权：对方升为 Owner，自己降为 Maintainer。仅 Owner。
    async fn transfer_owner(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        account_id: ID,
    ) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        let target = parse_ulid(account_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Owner)?;
        gql.services.workspace.transfer_owner(auth.account_id, ws_id, target)?;
        Ok(true)
    }

    /// 软删除工作空间，仅 Owner。数据保留，可恢复。
    async fn delete_workspace(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Owner)?;
        gql.services.workspace.delete(auth.account_id, ws_id)?;
        Ok(true)
    }

    /// 取消软删除，仅 Owner。
    async fn restore_workspace(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Owner)?;
        gql.services.workspace.restore(auth.account_id, ws_id)?;
        Ok(true)
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
        gql_entry(gql, entry, vec![])
    }

    /// 整体替换场景与语气：Maintainer 及以上（与标签管理一致）。
    async fn update_workspace_ai_config(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        scenarios: Vec<NamedPromptInput>,
        tones: Vec<NamedPromptInput>,
    ) -> GqlResult<GqlWorkspaceAiConfig> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let cfg = gql.services.ai.update_config(
            auth.account_id,
            ws_id,
            scenarios.into_iter().map(NamedPromptInput::into_named).collect(),
            tones.into_iter().map(NamedPromptInput::into_named).collect(),
        )?;
        Ok(cfg.into())
    }

    /// 生成总结并新建条目（会写 Entry，故要 Worker 及以上）。
    ///
    /// 顺序是有意的：先确认模型配置存在（最快、最可操作的错误），再组装提示词
    /// （这一步会校验选中条目的归属与状态），最后才发请求、建条目。
    /// 任何失败都发生在 `create_with_detail` 之前，不会留下半成品条目。
    async fn summarize_entries(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        codes: Vec<String>,
        scenario: Option<String>,
        tone: Option<String>,
    ) -> GqlResult<GqlEntry> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Worker)?;

        let client = gql
            .services
            .ai_client
            .as_ref()
            .ok_or(AppError::AiNotConfigured)?;
        let prompt = gql
            .services
            .ai
            .build_prompt(ws_id, &codes, scenario.as_deref(), tone.as_deref())?;
        let summary = client.complete(&prompt).await?;

        let title = derive_title(&summary, codes.len());
        let entry =
            gql.services
                .entry
                .create_with_detail(auth.account_id, ws_id, &title, &summary)?;
        gql_entry(gql, entry, vec![])
    }

    async fn set_labeling(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        label_name: String,
        // 无值标签（Null 类型，如内置 Task/Bug）的值就是 JSON null；async-graphql 的
        // JSON! 标量会拒绝 null，故此处用可空参数：省略或传 null 都落到 serde_json::Null。
        value: Option<Json<serde_json::Value>>,
    ) -> GqlResult<GqlLabeling> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        let value = value.map(|j| j.0).unwrap_or(serde_json::Value::Null);
        let labeling = gql
            .services
            .entry
            .set_labeling(auth.account_id, &entry_code, &label_name, &value)?;
        Ok(labeling.into())
    }

    /// 批量给多个条目写多个标签值（Worker+）。所有条目须同属一个工作空间，
    /// 整批原子写入。返回写入的 Labeling 条数。
    async fn set_labelings(
        &self,
        ctx: &Context<'_>,
        entry_codes: Vec<String>,
        labelings: Vec<LabelingInput>,
    ) -> GqlResult<i32> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        // 权限按所选条目所属工作空间校验：先确认它们同属一个工作空间，
        // 避免「用 A 空间的 Worker 身份改 B 空间的条目」。
        let mut workspace_id = None;
        for code in &entry_codes {
            let entry = gql
                .services
                .entry
                .get(code)?
                .ok_or(AppError::NotFound)?;
            match workspace_id {
                None => workspace_id = Some(entry.workspace_id),
                Some(id) if id != entry.workspace_id => {
                    return Err(AppError::InvalidQuery(
                        "选中的条目不属于同一个工作空间".to_string(),
                    )
                    .into())
                }
                Some(_) => {}
            }
        }
        let Some(ws_id) = workspace_id else {
            return Err(AppError::InvalidQuery("未选择任何条目".to_string()).into());
        };
        gql.require_role(ws_id, WorkspaceRole::Worker)?;
        let pairs: Vec<(String, serde_json::Value)> = labelings
            .into_iter()
            .map(|l| (l.name, l.value.map(|j| j.0).unwrap_or(serde_json::Value::Null)))
            .collect();
        let written = gql
            .services
            .entry
            .set_labelings(auth.account_id, &entry_codes, &pairs)?;
        Ok(written as i32)
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
        // 记下旧正文：正文里被删掉的内联图片，文件也要跟着走。
        let before_detail = entry.detail.clone();
        // 乐观并发：expectedUpdatedAt 与最新 updated_at 不一致时，服务层返回
        // ConflictDetected，其 GraphQL message 为「内容已被他人修改，请刷新后重试」。
        // 取一次用户名给消息预览/通知用，避免在 EntryService 里反向依赖 AuthService。
        let name = gql
            .services
            .auth
            .find_by_id(auth.account_id)?
            .map(|a| a.name)
            .unwrap_or_default();
        let updated = gql
            .services
            .entry
            .update(auth.account_id, &name, &code, &expected_updated_at, &title, &detail)?;
        // 回收放在更新成功之后：冲突或条目不存在时不该动任何附件。
        gql.services
            .attachment
            .purge_unreferenced(auth.account_id, &code, &before_detail, &updated.detail)
            .await?;
        let labels = gql.services.entry.labelings(&code)?;
        gql_entry(gql, updated, labels)
    }

    async fn delete_entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        gql.services.entry.soft_delete(auth.account_id, &code)?;
        Ok(true)
    }

    /// 发表评论（Worker+）。同时推进条目的 updated_at。
    async fn create_comment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        body: String,
    ) -> GqlResult<GqlComment> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        let name = gql
            .services
            .auth
            .find_by_id(auth.account_id)?
            .map(|a| a.name)
            .unwrap_or_default();
        let c = gql
            .services
            .comment
            .create(auth.account_id, &name, &entry_code, &body)?;
        gql_comment(gql, c)
    }

    /// 编辑评论（Worker+ 且作者本人）。
    async fn update_comment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        id: ID,
        body: String,
    ) -> GqlResult<GqlComment> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        let id = parse_ulid(id.as_str())?;
        let name = gql
            .services
            .auth
            .find_by_id(auth.account_id)?
            .map(|a| a.name)
            .unwrap_or_default();
        // 评论正文里的内联图片同样按「旧有今无」回收。
        let before_body = gql
            .services
            .comment
            .get(&entry_code, id)?
            .map(|c| c.body)
            .unwrap_or_default();
        let c = gql
            .services
            .comment
            .update(auth.account_id, &name, &entry_code, id, &body)?;
        gql.services
            .attachment
            .purge_unreferenced(auth.account_id, &entry_code, &before_body, &c.body)
            .await?;
        gql_comment(gql, c)
    }

    /// 删除评论（作者本人，或 Maintainer+）。
    async fn delete_comment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        id: ID,
    ) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_member(entry.workspace_id)?;
        // 先看是不是 Maintainer+；不是也不立刻拒绝——作者本人仍可撤回自己的评论。
        let can_moderate = gql
            .require_role(entry.workspace_id, WorkspaceRole::Maintainer)
            .is_ok();
        let id = parse_ulid(id.as_str())?;
        // 评论被删，它引用的内联图片也不该留在存储里。
        let before_body = gql
            .services
            .comment
            .get(&entry_code, id)?
            .map(|c| c.body)
            .unwrap_or_default();
        gql.services
            .comment
            .delete(auth.account_id, &entry_code, id, can_moderate)?;
        gql.services
            .attachment
            .purge_unreferenced(auth.account_id, &entry_code, &before_body, "")
            .await?;
        Ok(true)
    }

    /// 上传附件（Worker+）。multipart 由 async-graphql 的 `Upload` scalar 承载。
    async fn upload_attachment(
        &self,
        ctx: &Context<'_>,
        entry_code: String,
        file: Upload,
        // 编辑器内联图片传 true：不进附件列表。省略即 false，等于一条独立附件。
        inline: Option<bool>,
    ) -> GqlResult<GqlAttachment> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql
            .services
            .entry
            .get(&entry_code)?
            .ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        // tempfile 特性开启时 content 是临时文件句柄，try_clone 取一份自有的，
        // 这样 filename / content_type 还能从原值上读。
        let value = file
            .value(ctx)?
            .try_clone()
            .map_err(|e| AppError::Storage(e.to_string()))?;
        let filename = value.filename.clone();
        let content_type = value
            .content_type
            .clone()
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let a = gql
            .services
            .attachment
            .save(
                auth.account_id,
                &entry_code,
                &filename,
                &content_type,
                value.content,
                inline.unwrap_or(false),
            )
            .await?;
        gql_attachment(gql, a)
    }

    /// 删除附件（上传者本人，或 Maintainer+）。
    async fn delete_attachment(&self, ctx: &Context<'_>, id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let id = parse_ulid(id.as_str())?;
        // 附件只带 workspace_id，先取出来才知道该问哪个工作空间的权限。
        let attachment = gql
            .services
            .attachment
            .get(id)?
            .ok_or(AppError::NotFound)?;
        gql.require_member(attachment.workspace_id)?;
        // 先看是不是 Maintainer+；不是也不立刻拒绝——上传者本人仍可撤回自己的附件。
        let can_moderate = gql
            .require_role(attachment.workspace_id, WorkspaceRole::Maintainer)
            .is_ok();
        gql.services
            .attachment
            .delete(auth.account_id, id, can_moderate)
            .await?;
        Ok(true)
    }

    /// 归档条目（Worker+）：移出基础视图与全文检索，数据保留，可取消归档。
    async fn archive_entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        gql.services.entry.archive(auth.account_id, &code)?;
        Ok(true)
    }

    /// 取消归档（Worker+）。
    async fn unarchive_entry(&self, ctx: &Context<'_>, code: String) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let entry = gql.services.entry.get(&code)?.ok_or(AppError::NotFound)?;
        gql.require_role(entry.workspace_id, WorkspaceRole::Worker)?;
        gql.services.entry.unarchive(auth.account_id, &code)?;
        Ok(true)
    }

    async fn create_label_schema(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        title: String,
        value_type: String,
        attrs: LabelSchemaAttrsInput,
        enum_values: Vec<String>,
        color: Option<String>,
        value_colors: Option<Json<serde_json::Value>>,
    ) -> GqlResult<GqlLabelSchema> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let vt = LabelValueType::from_str(&value_type)
            .ok_or_else(|| AppError::Internal("无效的标签值类型".to_string()))?;
        let value_colors: Vec<ValueColor> = parse_json_list(value_colors, "值颜色配置")?;
        let input = attrs.to_service(name, title, vt, enum_values, color, value_colors)?;
        let schema = gql.services.label.create_schema(auth.account_id, ws_id, input)?;
        Ok(schema.into())
    }

    async fn update_label_schema(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        title: String,
        attrs: LabelSchemaAttrsInput,
        enum_values: Vec<String>,
        color: Option<String>,
        value_colors: Option<Json<serde_json::Value>>,
    ) -> GqlResult<GqlLabelSchema> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws_id = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws_id, WorkspaceRole::Maintainer)?;
        let value_colors: Vec<ValueColor> = parse_json_list(value_colors, "值颜色配置")?;
        // 类型不入参：服务层会保留库中已有类型。
        let input = attrs.to_service(
            name,
            title,
            LabelValueType::Null,
            enum_values,
            color,
            value_colors,
        )?;
        let schema = gql.services.label.update_schema(auth.account_id, ws_id, input)?;
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
        sorts: Option<Vec<SortInput>>,
        columns: Vec<String>,
        is_shared: bool,
        title_colors: Option<Json<serde_json::Value>>,
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
        let sort = to_sort(sorts);
        let title_colors = parse_title_colors(title_colors)?;
        let v = gql.services.view.create(
            auth.account_id,
            ws,
            &name,
            q,
            sort,
            columns,
            is_shared,
            title_colors,
        )?;
        let count = gql.services.entry.count(ws, &v.query)? as i32;
        Ok(GqlView::new(v, count, false, None))
    }

    #[allow(clippy::too_many_arguments)]
    async fn update_view(
        &self,
        ctx: &Context<'_>,
        id: ID,
        name: String,
        query: Json<serde_json::Value>,
        sorts: Option<Vec<SortInput>>,
        columns: Vec<String>,
        is_shared: bool,
        title_colors: Option<Json<serde_json::Value>>,
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
        let sort = to_sort(sorts);
        let title_colors = parse_title_colors(title_colors)?;
        let v = gql.services.view.update(
            auth.account_id,
            view_id,
            &name,
            q,
            sort,
            columns,
            is_shared,
            title_colors,
        )?;
        let count = gql.services.entry.count(existing.workspace_id, &v.query)? as i32;
        let is_default = gql.services.view.default_view_id(existing.workspace_id)? == Some(v.id);
        let timeline = gql.services.view.timeline_of(v.id)?;
        Ok(GqlView::new(v, count, is_default, timeline))
    }

    /// 设置 / 清除视图的时间轴配置。`start` 或 `end` 传空串即清除。
    ///
    /// 与 `updateView` 分成两条 mutation：配置在独立列族，改它不该顺带重写视图的
    /// 查询 / 排序 / 列——那些字段各有自己的并发语义（排序已改为自动串行落库），
    /// 混在一起会互相覆盖。
    async fn set_view_timeline(
        &self,
        ctx: &Context<'_>,
        id: ID,
        start: String,
        end: String,
        person: String,
    ) -> GqlResult<Option<GqlViewTimeline>> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let view_id = parse_ulid(id.as_str())?;
        let existing = gql.services.view.get(view_id)?.ok_or(AppError::NotFound)?;
        gql.require_member(existing.workspace_id)?;
        // 与 update_view 同一套权限：改共享视图、或改别人的视图，都要 Maintainer。
        let need = if existing.is_shared || existing.owner_id != auth.account_id {
            WorkspaceRole::Maintainer
        } else {
            WorkspaceRole::Worker
        };
        gql.require_role(existing.workspace_id, need)?;
        let start = start.trim().to_string();
        let end = end.trim().to_string();
        let person = person.trim().to_string();
        let cfg = (!start.is_empty() && !end.is_empty()).then(|| ViewTimeline {
            start,
            end,
            person: (!person.is_empty()).then_some(person),
        });
        let saved = gql.services.view.set_timeline(auth.account_id, view_id, cfg)?;
        Ok(saved.map(GqlViewTimeline::from))
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

    /// 新建规则（Maintainer+）。返回落库后的规则，前端据此刷新列表。
    #[allow(clippy::too_many_arguments)]
    async fn create_automation_rule(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        name: String,
        enabled: bool,
        trigger_expr: String,
        target_event_source: bool,
        target_expr: Option<String>,
        writes: Vec<RuleWriteInput>,
    ) -> GqlResult<GqlAutomationRule> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.require_role(ws, WorkspaceRole::Maintainer)?;
        let rule = gql.services.rule.create(
            auth.account_id,
            ws,
            &name,
            enabled,
            &trigger_expr,
            target_event_source,
            target_expr.as_deref(),
            to_rule_writes(writes)?,
        )?;
        Ok(rule.into())
    }

    /// 改名 / 启用 / 改条件须对规则所属工作空间有 Maintainer——不能凭一个 id 越界改别人的规则。
    #[allow(clippy::too_many_arguments)]
    async fn update_automation_rule(
        &self,
        ctx: &Context<'_>,
        id: ID,
        name: String,
        enabled: bool,
        trigger_expr: String,
        target_event_source: bool,
        target_expr: Option<String>,
        writes: Vec<RuleWriteInput>,
    ) -> GqlResult<GqlAutomationRule> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let rule_id = parse_ulid(id.as_str())?;
        let existing = gql.services.rule.get(rule_id)?.ok_or(AppError::NotFound)?;
        gql.require_role(existing.workspace_id, WorkspaceRole::Maintainer)?;
        let rule = gql.services.rule.update(
            auth.account_id,
            rule_id,
            &name,
            enabled,
            &trigger_expr,
            target_event_source,
            target_expr.as_deref(),
            to_rule_writes(writes)?,
        )?;
        Ok(rule.into())
    }

    async fn delete_automation_rule(&self, ctx: &Context<'_>, id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let rule_id = parse_ulid(id.as_str())?;
        let existing = gql.services.rule.get(rule_id)?.ok_or(AppError::NotFound)?;
        gql.require_role(existing.workspace_id, WorkspaceRole::Maintainer)?;
        gql.services.rule.delete(auth.account_id, rule_id)?;
        Ok(true)
    }

    /// 邀请已有账号（按邮箱）加入工作空间。写入的是「待接受邀请」——对方接受后才成为成员。
    /// 需要 Maintainer 及以上；授予 Owner 需要本人也是 Owner。
    async fn invite_member(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        email: String,
        role: String,
    ) -> GqlResult<GqlInvite> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        let role = parse_role(&role)?;
        // 授予 Owner 需要本人也是 Owner；其余角色 Maintainer 即可。
        let need = if role == WorkspaceRole::Owner {
            WorkspaceRole::Owner
        } else {
            WorkspaceRole::Maintainer
        };
        gql.require_role(ws, need)?;
        let invite = gql.services.workspace.invite(auth.account_id, ws, &email, role)?;
        let account = gql
            .services
            .auth
            .find_by_id(invite.account_id)?
            .ok_or(AppError::NotFound)?;
        let ws = gql.services.workspace.get_by_id(ws)?.ok_or(AppError::NotFound)?;
        Ok(GqlInvite::new(invite, &ws, &account))
    }

    /// 接受别人发来的邀请，成为工作空间成员。仅需登录。
    async fn accept_invite(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<GqlMember> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        let member = gql.services.workspace.accept_invite(auth.account_id, ws)?;
        let account = gql
            .services
            .auth
            .find_by_id(member.account_id)?
            .ok_or(AppError::NotFound)?;
        Ok(GqlMember::new(member, account))
    }

    /// 拒绝别人发来的邀请。仅需登录。
    async fn decline_invite(&self, ctx: &Context<'_>, workspace_id: ID) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        gql.services.workspace.decline_invite(auth.account_id, ws)?;
        Ok(true)
    }

    /// 撤销尚未被接受的邀请。需要 Maintainer 及以上；撤销 Owner 角色的邀请需要 Owner。
    async fn revoke_invite(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        account_id: ID,
    ) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        let target = parse_ulid(account_id.as_str())?;
        let invite = gql
            .services
            .workspace
            .get_invite(ws, target)?
            .ok_or(AppError::NotFound)?;
        let need = if invite.role == WorkspaceRole::Owner {
            WorkspaceRole::Owner
        } else {
            WorkspaceRole::Maintainer
        };
        gql.require_role(ws, need)?;
        gql.services.workspace.revoke_invite(auth.account_id, ws, target)?;
        Ok(true)
    }

    /// 变更成员角色。涉及 Owner 的调整需要 Owner 权限。
    async fn update_member_role(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        account_id: ID,
        role: String,
    ) -> GqlResult<GqlMember> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        let target = parse_ulid(account_id.as_str())?;
        let role = parse_role(&role)?;
        let existing = gql.services.workspace.get_member(ws, target)?.ok_or(AppError::NotFound)?;
        let need = if existing.role == WorkspaceRole::Owner || role == WorkspaceRole::Owner {
            WorkspaceRole::Owner
        } else {
            WorkspaceRole::Maintainer
        };
        gql.require_role(ws, need)?;
        let member = gql.services.workspace.update_role(auth.account_id, ws, target, role)?;
        let account = gql
            .services
            .auth
            .find_by_id(member.account_id)?
            .ok_or(AppError::NotFound)?;
        Ok(GqlMember::new(member, account))
    }

    /// 移除成员。移除 Owner 需要 Owner 权限，且不能移除最后一名 Owner。
    async fn remove_member(
        &self,
        ctx: &Context<'_>,
        workspace_id: ID,
        account_id: ID,
    ) -> GqlResult<bool> {
        let gql = ctx.data::<GraphqlContext>()?;
        let auth = gql.require_auth()?;
        let ws = parse_ulid(workspace_id.as_str())?;
        let target = parse_ulid(account_id.as_str())?;
        let existing = gql.services.workspace.get_member(ws, target)?.ok_or(AppError::NotFound)?;
        let need = if existing.role == WorkspaceRole::Owner {
            WorkspaceRole::Owner
        } else {
            WorkspaceRole::Maintainer
        };
        gql.require_role(ws, need)?;
        gql.services.workspace.remove_member(auth.account_id, ws, target)?;
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

/// GraphQL 传入的角色字符串 → WorkspaceRole。
fn parse_role(s: &str) -> GqlResult<WorkspaceRole> {
    WorkspaceRole::from_str(s)
        .ok_or_else(|| AppError::InvalidQuery(format!("未知角色: {s}")).into())
}

/// 将可选的 GraphQL JSON 入参反序列化为 `Vec<T>`；缺省时返回空列表。
/// 解析失败映射为 `InvalidQuery`，`what` 用于错误前缀（如「值颜色配置」）。
fn parse_json_list<T: serde::de::DeserializeOwned>(
    value: Option<Json<serde_json::Value>>,
    what: &str,
) -> GqlResult<Vec<T>> {
    match value {
        None => Ok(Vec::new()),
        Some(Json(v)) => serde_json::from_value(v)
            .map_err(|e| AppError::InvalidQuery(format!("{what}格式错误: {e}")).into()),
    }
}

/// 解析标题颜色规则入参。与 `parse_json_list` 的区别只有一处：
/// `Query` 手写投影而非走 `TitleColorRule` 的 `query_json` 适配器。
/// `null` 与 `None` 都等价于空列表。
fn parse_title_colors(value: Option<Json<serde_json::Value>>) -> GqlResult<Vec<TitleColorRule>> {
    let Some(Json(v)) = value else { return Ok(Vec::new()) };
    if v.is_null() {
        return Ok(Vec::new());
    }
    let items: Vec<TitleColorRuleWire> = serde_json::from_value(v)
        .map_err(|e| AppError::InvalidQuery(format!("标题颜色规则格式错误: {e}")))?;
    Ok(items
        .into_iter()
        .map(|w| TitleColorRule { query: w.query, color: w.color })
        .collect())
}
