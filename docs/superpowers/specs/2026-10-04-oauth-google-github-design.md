# Google / GitHub OAuth + 第三方账号管理 设计文档

> 日期：2026-10-04
> 状态：设计已与用户确认
> 范围：Spec 2/2 —— 复用 Spec 1（[2026-10-02-oauth-wechat-design.md](2026-10-02-oauth-wechat-design.md)）搭好的 `OAuthProvider` trait / `OAuthRegistry` / `OAuthStateStore` / `cf::OAUTH_BINDINGS` 基础设施，新增 Google + GitHub 两个 provider、邮箱命中静默登录、`/account` 第三方面板 + 解绑、OAuth-only 账号后补密码。

## 1. 背景与目标

Spec 1 落地了微信扫码登录与 OAuth provider 抽象。本轮：

1. **Google + GitHub provider 实现**：复用 Spec 1 的 `OAuthProvider` trait，每个 provider 一个文件，端点差异（authorize / token / userinfo）封在文件内。
2. **邮箱命中静默登录**：Google/GitHub 的 userinfo 带 email，OAuth 返回的 email 若在系统里已有账号 → 直接登录那个账号（不弹 bind 面板）。这是「用 Google 登录」按钮的预期 UX。微信路径不变（始终弹 bind 面板，因为微信不返回 email）。
3. **`/account` 第三方面板**：列出当前账号绑定的所有第三方账号，提供解绑入口。
4. **后补密码**：OAuth-only 账号后来想用邮箱+密码登录时，可通过 `/account` 设置密码；不踢自己下线。
5. **解绑守卫**：不能解绑唯一的登录方式（OAuth-only 且该 binding 是该 provider 的唯一记录），避免用户被困在门外。

## 2. 范围与非目标

**本轮范围**

- Google provider：`src/service/oauth/google.rs`，OpenID Connect 流程，scope `openid email profile`，`sub` 作 external_id
- GitHub provider：`src/service/oauth/github.rs`，OAuth2 + 单独 userinfo，scope `read:user user:email`，`id` 作 external_id，必要时调 `/user/emails` 找主邮箱
- `OAuthConfig` 解开 Spec 1 的注释占位：`pub google: Option<GoogleOAuthConfig>` / `pub github: Option<GithubOAuthConfig>`
- 公共回调路径 `oauth_callback_common`：抽出三个 provider 共用的「拿 token → 查 binding → 没 binding 视 email 走静默登录 / bind 面板」流程
- GraphQL `Mutation::unbind_oauth_binding(account_id, provider, external_id)` + `Query::my_oauth_bindings`
- GraphQL `Mutation::set_password(new_password)`：`AuthService::set_password` 服务方法，不踢自己下线
- `Query::me` 投影 `has_password: Boolean!`（避免把 `password_hash` 漏给前端）
- 前端 `/account` 新增「第三方账号」区段（列出 bindings + 解除绑定按钮）和「设置密码」表单（仅 OAuth-only 可见）
- `BindEntry` 已经在 Spec 1 预留 `email` / `display_name` 字段；bind 面板在 `email` Some 时预填

**非本轮（本轮不做）**

- 已有 WeChat binding 的数据迁移——`IdentityBinding.email: Option<String>` 已存在，微信填的 `None` 反序列化兼容，无需 repair
- 微信 unionid（继续不取；微信本身仍未返回邮箱）
- 第三方账号的合并（同一 Rodeo 账号被两个 WeChat 绑）——unbind 后重绑即可
- 多账号邮箱切换 / 邮箱变更——保持简单，邮箱走 builtin `register` 流程
- 失败回调（用户在 Google/GitHub 侧拒绝授权）的专门页面——沿用 Spec 1 的 `/login?oauth_error=...` 横幅
- 重新触发 OAuth 时是否提示「该 provider 已绑定其它账号」——bind 阶段已报
- token 过期后 userinfo 续拉——`access_token` 落 `BindEntry` 仅供本流程使用，登录后即作废

## 3. 关键决策

| 决策 | 选择 | 理由 |
| --- | --- | --- |
| Provider 文件位置 | `src/service/oauth/{google,github}.rs`，与 `wechat.rs` 同级 | 每个文件一个 provider；公共代码留在 `mod.rs` |
| `ExternalToken` 扩展 | **零改动** | Spec 1 已有 `email: Option<String>` / `display_name: Option<String>`，Google/GitHub 填 Some，微信填 None |
| 回调路径 | 抽出 `oauth_callback_common(provider_name, code, state)`；`wechat_callback/google_callback/github_callback` 都是薄壳 | 三家行为一致（查 binding → email 命中 → bind 面板），避免复制粘贴；provider 之间的差异只在 `exchange_code` 内部 |
| 静默登录策略 | 仅对返回 email 的 provider（Google / GitHub）生效；命中 email = 直接登录。命中条件 = email match 找到唯一账号 | 用户点「用 Google 登录」时预期就是该行为。微信不返回 email，强制走 bind 面板的现有策略不变 |
| 解绑守卫 | 「不能解绑唯一的登录方式」：若账号 `!has_password()` 且该 provider 是该账号的唯一 binding → 拒绝 | 否则 OAuth-only 用户 unbind 之后只能找回密码 / 再扫一次码，跟当前一样；目前没有自服务找回密码通道 |
| 后补密码服务方法 | 新增 `AuthService::set_password(account_id, new_password)`：`!has_password()` 才允许；**不**调 `revoke_tokens` | `revoke_tokens` 会让用户在唯一的登录方式被踢后无法自服务恢复——这是写后即死的恢复路径 |
| 已有密码的「改密」 | 沿用 Spec 1 的 `AuthService::change_password`，要求旧密码 + 吊销其他设备令牌 | 两个 mutation 各管一个语义：初始密码 vs 改密 |
| `me()` 是否暴露 `password_hash` | **否**；新增 `has_password: Boolean!` 投影 | 把 hash 漏到 GraphQL 层是数据泄漏。UI 只关心「能不能密码登录」 |
| `/account` 路由 | 复用现有 `Account` 组件，新增区段 | 不另起路由；同一账号的所有设置在一页内可见 |
| 回调 redirect URL 校验 | 沿用 Spec 1 的 `is_safe_return_to`，三个 callback 共用 | 配置面已经在 Spec 1 加好；新 provider 不需要重复 |

## 4. 数据模型

### 4.1 `Account` 不动

沿用 Spec 1 的 `password_hash: String`（空串 = 无密码）+ `has_password()` 方法。`set_password` 服务方法在 bincode 字段上做的是「从空串变非空」，序列化层零变化。

### 4.2 `IdentityBinding` 不动

Spec 1 已经有 `email: Option<String>` / `display_name: Option<String>`。Google/GitHub 写 Some；微信写 None。已有微信 binding 反序列化兼容。

### 4.3 `BindEntry` 不动

Spec 1 已有 `email` / `display_name`。静默登录命中后用不上 `BindEntry`（直接 upsert binding）；bind 面板分支用 `BindEntry.email` 预填表单。

### 4.4 新 GraphQL 类型

```rust
#[derive(SimpleObject)]
pub struct GqlOAuthBinding {
    pub provider: String,        // "wechat" | "google" | "github"
    pub external_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub bound_at: chrono::DateTime<chrono::Utc>,
}
```

不需要单独的 ID：解绑用 `(provider, external_id)` 复合键定位，主键就是列族 key。

## 5. 配置

### 5.1 `src/config.rs`

解开 Spec 1 的注释占位：

```rust
pub struct OAuthConfig {
    #[serde(default)]
    pub wechat: Option<WeChatOAuthConfig>,
    #[serde(default)]
    pub google: Option<GoogleOAuthConfig>,
    #[serde(default)]
    pub github: Option<GithubOAuthConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    /// 申请 Google API console 时填的 authorized redirect URI；
    /// 本服务用它构造 callback URL，并校验 Google 回跳的 redirect_uri 与之一致。
    /// `client_id.trim().is_empty()` 即视为未启用。
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}
```

`AuthConfig::default()` 把三个 provider 都设为 `None`（都没填 = 都不启用）。

### 5.2 `config.example.toml`

```toml
[auth.oauth.google]
# 在 https://console.cloud.google.com/apis/credentials 建 OAuth 2.0 Client ID（应用类型：Web application）。
# Authorized redirect URI 填 ${redirect_uri}。
client_id = ""
client_secret = ""
redirect_uri = "https://your-rodeo.example.com/api/auth/google/callback"

[auth.oauth.github]
# 在 https://github.com/settings/developers 建 OAuth App。
# Authorization callback URL 填 ${redirect_uri}。
client_id = ""
client_secret = ""
redirect_uri = "https://your-rodeo.example.com/api/auth/github/callback"
```

注释：`client_id` / `client_secret` 留空即视作该 provider 未启用，与微信一致。

### 5.3 启动时的注册表

`src/service/oauth/mod.rs::OAuthRegistry::from_config` 当前只注册微信。扩展为：

```rust
pub fn from_config(config: &Config) -> Result<Self, AppError> {
    let mut providers: Vec<Arc<dyn OAuthProvider>> = Vec::new();
    if let Some(cfg) = &config.auth.oauth.wechat {
        if !cfg.app_id.trim().is_empty() {
            providers.push(Arc::new(WeChatProvider::from_config(cfg)?));
        }
    }
    if let Some(cfg) = &config.auth.oauth.google {
        if !cfg.client_id.trim().is_empty() {
            providers.push(Arc::new(GoogleProvider::from_config(cfg)?));
        }
    }
    if let Some(cfg) = &config.auth.oauth.github {
        if !cfg.client_id.trim().is_empty() {
            providers.push(Arc::new(GithubProvider::from_config(cfg)?));
        }
    }
    Ok(Self { providers })
}
```

`from_config` 返回 `Result`，错误来自 `WeChatProvider::from_config` 的 `NotConfigured`——其他 provider 实现该 trait 时同样返回该错误，但 `from_config` 已经先做了 `client_id.trim().is_empty()` 守卫，理论上不会触发；保留错误返回保持 trait 对齐。

## 6. Provider 实现

### 6.1 Google

`src/service/oauth/google.rs`：

```rust
pub struct GoogleProvider {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

impl GoogleProvider {
    pub fn from_config(cfg: &GoogleOAuthConfig) -> Result<Option<Self>, AppError> {
        if cfg.client_id.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
            redirect_uri: cfg.redirect_uri.clone(),
        }))
    }
}

#[async_trait]
impl OAuthProvider for GoogleProvider {
    fn name(&self) -> &'static str { "google" }
    fn enabled(&self) -> bool { true }

    fn authorization_url(&self, state: &str, redirect_uri: &str) -> String {
        format!(
            "https://accounts.google.com/o/oauth2/v2/auth\
             ?client_id={}&redirect_uri={}&response_type=code\
             &scope=openid+email+profile&state={}",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(state),
        )
    }

    async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<ExternalToken, AppError> {
        // 1. POST https://oauth2.googleapis.com/token
        //    form: code={code}&client_id={...}&client_secret={...}&redirect_uri={...}&grant_type=authorization_code
        //    → { access_token, id_token, ... }
        // 2. GET https://openidconnect.googleapis.com/v1/userinfo
        //    Authorization: Bearer {access_token}
        //    → { sub, email, name, picture }
        // ExternalToken { external_id: sub, email, display_name: Some(name), access_token }
    }
}
```

`urlencoding::encode` 已经是依赖（Spec 1 已加），无需 Cargo.toml 改动。

### 6.2 GitHub

`src/service/oauth/github.rs`：

```rust
pub struct GithubProvider {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

#[async_trait]
impl OAuthProvider for GithubProvider {
    fn name(&self) -> &'static str { "github" }
    fn enabled(&self) -> bool { true }

    fn authorization_url(&self, state: &str, redirect_uri: &str) -> String {
        format!(
            "https://github.com/login/oauth/authorize\
             ?client_id={}&redirect_uri={}&scope=read:user+user:email&state={}",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(state),
        )
    }

    async fn exchange_code(&self, code: &str, redirect_uri: &str) -> Result<ExternalToken, AppError> {
        // 1. POST https://github.com/login/oauth/access_token
        //    Headers: Accept: application/json
        //    Body: { client_id, client_secret, code, redirect_uri }
        //    → { access_token, scope, token_type }
        // 2. GET https://api.github.com/user
        //    Authorization: Bearer {access_token}
        //    → { id, login, name, email (may be null), avatar_url }
        // 3. 若 email 为 null：GET https://api.github.com/user/emails
        //    → [{ email, primary, verified, visibility }, ...]
        //    取首个 primary && verified 的 email
        // ExternalToken { external_id: id (as string), email, display_name: name.or(Some(login)), access_token }
    }
}
```

## 7. 公共回调路径

`src/api/oauth.rs` 新增私有函数：

```rust
async fn oauth_callback_common(
    state: Arc<AppState>,
    provider_name: &str,
    code: String,
    state_token: String,
    return_to: String,
) -> Result<Redirect, ...> {
    let return_to = if is_safe_return_to(&return_to) { return_to } else { "/workspaces".into() };

    let csrf = state.services.oauth_state
        .take_csrf(&state_token)
        .ok_or(AppError::OAuthCallback("登录已过期，请重试".into()))?;

    // csrf 必须与本次请求的 provider 对应，防止拿微信 csrf 走 Google 回调。
    if csrf.provider != provider_name {
        return Err(AppError::OAuthCallback("登录状态不匹配".into()).into());
    }

    let provider_kind = OAuthProvider::from_byte(/* parse provider_name */)?;
    let external = state.services.oauth_registry
        .exchange(provider_name, &code, &csrf.redirect_uri).await?;

    // 1. 已绑过 → 直接签 token
    if let Some(b) = state.services.oauth_bindings.find(provider_kind, &external.external_id)? {
        return Ok(redirect_with_token(b.account_id, &return_to, state).await);
    }

    // 2. 邮箱命中 → 静默登录
    if let Some(email) = &external.email {
        if let Some(account) = state.services.auth.find_by_email(email)? {
            if !matches!(state.services.auth.status(account.id)?, AccountStatus::Active) {
                // 静默登录也守 AccountStatus——冻结/注销账号不能借此绕开。
                return Err(AppError::OAuthWechat("此账号已被冻结或注销".into()).into());
            }
            state.services.oauth_bindings.upsert(
                account.id, provider_kind, &external.external_id,
                external.email.clone(), external.display_name.clone(),
            )?;
            return Ok(redirect_with_token(account.id, &return_to, state).await);
        }
    }

    // 3. 走 bind 面板
    let bind_token = new_bind_token();
    state.services.oauth_state.insert_bind(BindEntry {
        provider: provider_kind,
        external_id: external.external_id,
        email: external.email,
        display_name: external.display_name,
        // access_token 暂存 5 分钟，供 bind 后写 IdentityBinding.email/display_name 用——
        // 见 BindEntry 现有字段，Spec 1 已留 access_token: String。
        access_token: external.access_token,
        created_at: Utc::now(),
    });
    Ok(Redirect::to(&format!(
        "/oauth/callback?bind={bind_token}&provider={provider_name}&return_to={}",
        urlencoding::encode(&return_to)
    )))
}
```

`wechat_callback` / `google_callback` / `github_callback` 各自解析 `OAuthProvider::from_*` + 调 `oauth_callback_common`。

注意：`BindEntry` Spec 1 已有 `access_token: String` 字段，5 分钟过期后被懒清理扫掉，期间仅供 bind 阶段使用。bind 完成后写 `IdentityBinding` 即用 `BindEntry.email/display_name`，不必复用 `access_token`。

## 8. 邮箱静默登录

详见第 7 节公共回调路径中的步骤 2。**仅 Google / GitHub 触发**（两者 `external.email: Some`）。微信 `external.email: None`，永远走步骤 3（bind 面板）。

**账号状态守门**：静默登录命中后，仍校验 `AccountStatus`，与正常登录路径一致——冻结账号不能借第三方登录绕开。

**边界条件**：
- 同一 email 在系统里有多个账号（理论上不会——邮箱索引唯一）—— 不处理，按 index 命中第一个返回
- 命中账号的 `password_hash` 是 OAuth-only（空串）—— 静默登录不读 `password_hash`，OK
- 命中账号被冻结 / 注销 —— 拒绝，与正常路径文案一致

## 9. Unbind mutation

### 9.1 GraphQL

```graphql
mutation unbindOauthBinding(accountId: ID!, provider: String!, externalId: String!): Boolean!
```

注：`accountId` 看似冗余（调用者已登录）—— 实际是 GraphQL resolver 用它走 `gql.require_owner(account_id)`，避免被借用未鉴权的 `account_id` 参数绕过。

### 9.2 服务

`OAuthBindingsService::delete(account_id, provider, external_id) -> Result<(), AppError>`：

```rust
pub fn delete(
    &self,
    account_id: Ulid,
    provider: OAuthProvider,
    external_id: &str,
) -> Result<(), AppError> {
    let binding = self.find(provider, external_id)?
        .ok_or(AppError::NotFound)?;
    if binding.account_id != account_id {
        return Err(AppError::InvalidQuery("无权解绑此账号".into()));
    }

    // 唯一登录方式守卫
    let account = self.store.get::<Account>(cf::ACCOUNTS, &account_id.to_bytes())?
        .ok_or(AppError::NotFound)?;
    if !account.has_password() {
        // 该 provider 是不是该账号唯一的 binding？
        let other = self.find_by_account_and_provider(account_id, provider)?;
        if other.is_none() {
            return Err(AppError::InvalidQuery(
                "请先设置密码或绑定其他第三方账号再解绑".into(),
            ));
        }
    }

    self.store.delete(cf::OAUTH_BINDINGS, &Self::key(provider, external_id))?;
    Ok(())
}
```

### 9.3 查询

```graphql
query myOAuthBindings: [GqlOAuthBinding!]!
```

服务：`OAuthBindingsService::find_all_by_account(account_id)`——全表扫后过滤；账号量级下 O(N) 可接受（与现有 `find_by_account_and_provider` 同样的扫法）。

## 10. 后补密码

### 10.1 服务

`AuthService::set_password(account_id, new_password)`：

```rust
pub fn set_password(&self, account_id: Ulid, new_password: &str) -> Result<Account, AppError> {
    let mut account = self.find_by_id(account_id)?.ok_or(AppError::NotFound)?;
    if account.has_password() {
        return Err(AppError::InvalidQuery(
            "此账号已有密码，请使用 changePassword 修改".into(),
        ));
    }
    validate_password(new_password)?;
    account.password_hash = hash_password(new_password)?;
    self.store.put(cf::ACCOUNTS, &account.id.to_bytes(), &account)?;
    Ok(account)
}
```

**不**调 `revoke_tokens`：这是用户给 OAuth-only 账号加备用登录方式，加完后没必要踢自己下线。`change_password` 路径仍然 `revoke_tokens`（其他设备必须重新登录）。

### 10.2 GraphQL

```graphql
mutation setPassword(newPassword: String!): AuthResult!
```

Resolver：从 `gql.require_auth()` 拿 `account_id`，调 `auth.set_password(...)`，签新 token 返回。

## 11. `/account` 面板

复用现有 `src/frontend/pages/account.rs`。新增两个区段：

### 11.1 第三方账号

```
第三方账号
─────────────────────
[微信]  ox8Kz...          绑定于 2026-10-04      [解除绑定]
[Google] [email]         绑定于 2026-10-04      [解除绑定]
[GitHub] [login]         绑定于 2026-10-04      [解除绑定]
```

无 binding 时整段不显示。

数据来源：`Query::myOAuthBindings`（登录态自动带 `account.id` 过滤）。

「解除绑定」点击 → `Mutation::unbindOauthBinding(account_id, provider, external_id)` → 成功后重拉 query。

### 11.2 设置密码（仅 OAuth-only 可见）

```
设置 Rodeo 密码
─────────────────────
新密码：(input, password)         至少 8 位，含大小写和数字
[保存]   ← 调 setPassword
```

`me().has_password == false` 时显示。

成功保存后 → 重拉 `me()` 把 `has_password` 切到 true → 该区段隐藏，「第三方账号」区段中 OAuth-only 唯一 binding 行上的「解除绑定」按钮也变为可点（之前会显示 422「请先设置密码」）。

## 12. `me()` 投影

`GqlAccount` 加字段：

```rust
pub struct GqlAccount {
    // ... 既有字段 ...
    pub has_password: bool,
}
```

Resolver：`GqlAccount::from(account)` 时 `has_password: account.has_password()`。前端拿到的就是布尔，不会见到 hash。

## 13. 测试与验证

按项目 memory `feedback-no-backend-unit-tests.md`，**不补 Rust 单测**。门：

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

两项均须 0 errors。

浏览器冒烟（spec §13 11-case + 本轮新增 case）：
- 微信 / Google / GitHub 三个 start → callback → `/oauth/callback#token=...` → `/workspaces`（已绑）
- Google 首次登录 + email 命中已有账号 → 静默登录成功
- GitHub 首次登录 + email 命中 + 命中账号被冻结 → 拒绝（与正常登录一致）
- Google / GitHub 首次登录 + email 未命中 → 进 bind 面板 + email 预填
- `/account` 列出三个绑定（若有）
- `/account` 解绑 OAuth-only 唯一 binding → 拒绝（提示先设密码）
- `/account` 解绑有密码账号的 OAuth binding → 成功
- `/account` 给 OAuth-only 账号设置密码 → 成功（不踢自己下线）
- 设置密码后再次尝试解绑 → 成功

冒烟由用户跑，本环境无法执行 `make dev`（`project-make-check-memory-pressure.md`）。

## 14. `CsrfEntry` 扩展（provider 字段）

`src/service/oauth_state.rs` 现有 `CsrfEntry` 只含 `return_to` / `created_at`，不带 provider。本轮必加：

```rust
pub struct CsrfEntry {
    pub provider: String,        // 新增："wechat" | "google" | "github"
    pub return_to: String,
    pub created_at: DateTime<Utc>,
}
```

公共回调 `oauth_callback_common` 取出 csrf 后，**先**校验 `csrf.provider == provider_name`，否则 `OAuthCallback("登录状态不匹配".into())`——防拿微信 csrf 走 Google 回调。

`/api/auth/wechat/start` / `/api/auth/google/start` / `/api/auth/github/start` 三个 handler 在写 csrf 时各自填 `provider: "wechat"` 等。`pub use` 已经在 Spec 1 加好，复用即可。

由于 `CsrfEntry` 是进程内 `Mutex<HashMap<String, _>>`，**不持久化**，无迁移成本——重启即清空，无存量数据要兼容。

## 15. 任务分解（占位，待 writing-plans 落到 plan 文档）

预计 14 个 task：
1. `CsrfEntry` 加 `provider` 字段 + `wechat_start` 填字段（其余两个 start 还没写）
2. Config 扩展（`OAuthConfig` 加 google/github，example.toml）
3. `GoogleProvider` 实现（含 token + userinfo 两步 HTTP 调用）
4. `GithubProvider` 实现（含 `/user` + 必要时 `/user/emails`）
5. `OAuthRegistry::from_config` 扩展注册三个 provider
6. 公共回调路径 `oauth_callback_common` 抽取 + wechat/google/github_callback 薄壳化（含 csrf provider 校验）
7. 邮箱静默登录分支（命中即 upsert binding + 签 token + AccountStatus 守门）
8. `OAuthBindingsService::delete` + 「唯一登录方式」守卫
9. `OAuthBindingsService::find_all_by_account`（全表扫过滤）
10. GraphQL：`Query::my_oauth_bindings` + `Mutation::unbind_oauth_binding`
11. `AuthService::set_password` 服务方法
12. GraphQL：`Mutation::set_password` + `GqlAccount::has_password` 投影
13. HTTP：`/api/auth/{google,github}/start` + `/callback` 路由
14. 前端：`/account` 第三方账号区段 + unbind 按钮 + 设置密码表单（仅 `!has_password` 可见）

## 16. 风险

- **GitHub email 为 null + 用户未公开 primary email**：`/user/emails` 仍能取主邮箱（需 `user:email` scope）；若该用户连 verified primary email 都没有，走 bind 面板（email 字段空，用户手填）。这与「邮箱匹配静默登录」的边界一致——只在 email 真实存在且能验证时静默。
- **Google `id_token` 未校验签名**：Spec 1 WeChat 不走 id_token，本轮也不验。userinfo 端点是 https，且 access_token 是服务侧拿到再调，没暴露给前端。可在 Spec 3 加 id_token 验签，本轮不做。
- **`wechat_start` 兼容改动**：`CsrfEntry` 加 `provider` 字段后，Spec 1 的 `wechat_start` 写 csrf 时必须填 `"wechat"`，否则 `oauth_callback_common` 校验时挂在「provider 不匹配」。这块属于任务 1 的隐性契约。
