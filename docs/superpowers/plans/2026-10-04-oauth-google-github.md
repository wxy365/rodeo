# Spec 2 — Google/GitHub OAuth + 第三方账号管理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Spec 1 微信 OAuth 基础上扩展 Google + GitHub 两个 provider；邮箱命中静默登录；`/account` 第三方面板 + unbind；OAuth-only 账号后补密码。

**Architecture:** 三个 provider 共用 `OAuthProvider` trait + 抽出的 `oauth_callback_common`；`OAuthStateStore` 加 csrf provider 字段防跨 provider 重放；`OAuthBindingsService` 扩展 delete + find_all_by_account；`AuthService` 加 `set_password` 不踢自己下线；前端 `/account` 加两个新区段。

**Tech Stack:** 沿用 Spec 1 的栈（Rust / Leptos 0.8 / async-graphql / reqwest / `urlencoding`）。

**Spec:** [`docs/superpowers/specs/2026-10-04-oauth-google-github-design.md`](../specs/2026-10-04-oauth-google-github-design.md) —— 计划以 spec 为权威来源，spec 静默处按 spec 显式约定走。

## Global Constraints

这些来自项目 memory 和 CLAUDE.md，**每条 task 都隐式适用**，无需重复声明：

- **不补 Rust 单测**。`feedback-no-backend-unit-tests.md`。验证门仅 `cargo check -p rodeo` + `cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate`，两项 0 errors 即通过（除 `proc-macro-error2` future-incompat warning，与本分支无关）。
- **每任务后自动 commit**。`make check` / `make build` 在 Intel MBA 上 OOM（`project-make-check-memory-pressure.md`），跳过。
- **bincode 按位置编码**，新枚举变体只能追加；新增 CF 名追加到 `cf::*` 常量列表末尾；`IdentityBinding` / `Account` 字段顺序不动。
- **`OAuthProvider as _` import 模式**已删（Spec 1 fix round 1），`dyn` 派发不需要 trait 在 scope。
- **`cfg(target_arch = "wasm32")` vs `cfg!(target_arch = "wasm32")`**：`#[cfg(...)]` 用于 wasm/ssr 函数定义；`cfg!(...)` 用于运行时 Effect 早 return。
- **GraphQL 错误透传**：`?` 已触发 `From<AppError> for async_graphql::Error` blanket impl；单行 `return Err(AppError::Variant(...).into())` 可省 `.into()`，`ok_or_else`/`if` 多分支路径按惯例加 `.into()`。
- **`OAuthProvider` enum/trait 重名**已用 `as ProviderKind` 在 `service/oauth/{wechat,google,github}.rs` 内区分。`api/oauth.rs` 用 `crate::domain::OAuthProvider`（enum）。
- **`OAuthRegistry` 是 `Arc<dyn OAuthProvider>` Vec**，新增 provider 注册到 `from_config` 即可，无需改 handler。
- **HTTP handler 用 `Extension(Arc<AppState>)`**（不是 `State`），沿用 `attachments.rs:33` / `agent_sse.rs:62` 模式。
- **handler 错误返回 `Result<impl IntoResponse, AppError>`**，`AppError: Display + Send + Sync + 'static` 自动转 axum 响应。
- **返回 `Redirect` 用 `axum::response::Redirect::to(&url)`**。

## Review Focus

以下五项来自 spec 但没有显式单测覆盖（项目不补单测），每个 task 实现时人工走一遍：

1. **csrf 跨 provider 重放**：拿微信 csrf 走 Google callback 应被拒绝。`oauth_callback_common` 取出 csrf 后必校验 `csrf.provider == provider_name`，否则 `OAuthCallback("登录状态不匹配".into())`。Task 6 落 `take_csrf` 之后、`exchange_code` 之前。
2. **邮箱静默登录撞上冻结账号**：命中 email 后仍要走 `AccountStatus` 守门，冻结/注销拒绝。Task 7 命中即签 token 之前必查 `auth.status(account.id)`。
3. **OAuth-only 唯一 binding 的 unbind 拒绝**：账号 `!has_password()` 且该 provider 是唯一 binding，`delete` 返回 `InvalidQuery("请先设置密码或绑定其他第三方账号再解绑")`。Task 8 落「唯一登录方式」守卫。
4. **`set_password` 不调 `revoke_tokens`**：OAuth-only 账号加备用登录方式，不应把自己踢下线。Task 11 实现里显式不调。
5. **`me().has_password` 是布尔而非 hash**：`GqlAccount::has_password` 取 `account.has_password()`，绝不暴露 `password_hash` 字段。Task 12 投影时显式不引用 `password_hash`。

---

### Task 1: CsrfEntry 加 `provider` 字段 + wechat_start 同步填字段

**Files:**
- Modify: `src/service/oauth_state.rs:19-22`（`CsrfEntry` 加 `provider: String` 字段）
- Modify: `src/api/oauth.rs`（`wechat_start` 写 csrf 时填 `provider: "wechat"`；新增的 google/github start 同理留待 Task 13）

**Interfaces:**
- Consumes: 无新依赖
- Produces: `CsrfEntry { provider: String, return_to: String, created_at: DateTime<Utc> }`；`take_csrf` 返回的 entry 多一个 `provider` 字段供 Task 6 校验

- [ ] **Step 1: 改 `CsrfEntry` 结构**

`src/service/oauth_state.rs:19-22`，改为：

```rust
pub struct CsrfEntry {
    /// 防跨 provider 重放：拿微信 csrf 走 Google callback 必须被拒。
    pub provider: String,
    pub return_to: String,
    pub created_at: DateTime<Utc>,
}
```

注释贴上方说明一句用途（spec §14）。

- [ ] **Step 2: 跑 native cargo check 看是否过**

```bash
cargo check -p rodeo
```

预期：`take_csrf` 返回类型自带 `provider` 字段，没人会立刻报错；但**所有写 csrf 的地方现在必须填这个字段**，否则类型不匹配 → 编译失败。这是预期的：Task 1 同时强制修了 `wechat_start`（Step 3）。

- [ ] **Step 3: 在 `wechat_start` 处填 provider**

定位 `src/api/oauth.rs` 里 `wechat_start` 调 `oauth_state.put_csrf(state, CsrfEntry { ... })` 处（Spec 1 实现）。补 `provider: "wechat".into()` 到字面量里。

提示：grep `put_csrf` 找到调用点；通常形如：

```rust
services.oauth_state.put_csrf(
    state.clone(),
    CsrfEntry {
        provider: "wechat".into(),
        return_to,
        created_at: chrono::Utc::now(),
    },
);
```

- [ ] **Step 4: cargo check + wasm check 全绿**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：都 0 errors。

- [ ] **Step 5: Commit**

```bash
git add src/service/oauth_state.rs src/api/oauth.rs
git commit -m "fix(oauth): CsrfEntry 加 provider 字段，防跨 provider csrf 重放"
```

---

### Task 2: Config 扩展（OAuthConfig 加 google/github + example.toml）

**Files:**
- Modify: `src/config.rs`（解开 Spec 1 注释占位；新增 `GoogleOAuthConfig` / `GithubOAuthConfig`）
- Modify: `config.example.toml`（加 `[auth.oauth.google]` / `[auth.oauth.github]` 段）

**Interfaces:**
- Consumes: 无
- Produces: `OAuthConfig { wechat: Option<WeChatOAuthConfig>, google: Option<GoogleOAuthConfig>, github: Option<GithubOAuthConfig> }`；每个子结构都是 `{ client_id, client_secret, redirect_uri: String }`（Google/GitHub 没有 app_id，名称对齐 OAuth2 标准字段）

- [ ] **Step 1: 加 `GoogleOAuthConfig` / `GithubOAuthConfig` 结构**

在 `src/config.rs` `WeChatOAuthConfig` 附近加：

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GoogleOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubOAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}
```

- [ ] **Step 2: 解开 `OAuthConfig` 的注释占位**

定位 `src/config.rs` 现有 `pub struct OAuthConfig { wechat: Option<WeChatOAuthConfig>, /* google, github */ }`，把两行注释替换为：

```rust
    #[serde(default)]
    pub google: Option<GoogleOAuthConfig>,
    #[serde(default)]
    pub github: Option<GithubOAuthConfig>,
```

- [ ] **Step 3: `config.example.toml` 加两段**

定位现有 `[auth.oauth.wechat]` 段，**后面**追加：

```toml
[auth.oauth.google]
# Google Cloud Console → APIs & Services → Credentials → Create OAuth 2.0 Client ID（应用类型：Web application）。
# Authorized redirect URI 填 ${redirect_uri}。
client_id = ""
client_secret = ""
redirect_uri = "https://your-rodeo.example.com/api/auth/google/callback"

[auth.oauth.github]
# GitHub → Settings → Developer settings → OAuth Apps → New OAuth App。
# Authorization callback URL 填 ${redirect_uri}。
client_id = ""
client_secret = ""
redirect_uri = "https://your-rodeo.example.com/api/auth/github/callback"
```

- [ ] **Step 4: cargo check**

```bash
cargo check -p rodeo
```

预期：0 errors。`AuthConfig::default()` 派生 `OAuthConfig::default()`，新字段是 `Option<..>`，默认 `None` 自动覆盖。

- [ ] **Step 5: Commit**

```bash
git add src/config.rs config.example.toml
git commit -m "feat(oauth): 配置加 google/github OAuth 子结构"
```

---

### Task 3: GoogleProvider 实现

**Files:**
- Modify: `Cargo.toml`（如果 `reqwest` 的 features 没覆盖 JSON，加 `json`）
- Create: `src/service/oauth/google.rs`
- Modify: `src/service/oauth/mod.rs`（加 `pub mod google;` + re-export）

**Interfaces:**
- Consumes: `GoogleOAuthConfig { client_id, client_secret, redirect_uri }`（Task 2）
- Produces: `GoogleProvider` 实现 `OAuthProvider` trait；`from_config(&GoogleOAuthConfig) -> Result<Option<Self>, AppError>`（Spec 1 已有 `WeChatProvider::from_config` 同形签名，参考之）

注：Spec 1 `OAuthProvider` trait 已经包含 `name() / enabled() / authorization_url() / async exchange_code()`。本 task 不改 trait。

- [ ] **Step 1: 确认 `reqwest` features 包含 json**

```bash
/usr/bin/grep -A3 "reqwest" /Users/wangxiaoyan/Works/git/rodeo/Cargo.toml
```

如果 features 已有 `["json", "rustls-tls"]`（或类似），跳过。如果只有 `rustls-tls`，加 `json`。

- [ ] **Step 2: 写 `src/service/oauth/google.rs`**

```rust
//! Google OAuth2 + OpenID Connect。
//! authorize:  https://accounts.google.com/o/oauth2/v2/auth
//! token:      https://oauth2.googleapis.com/token
//! userinfo:   https://openidconnect.googleapis.com/v1/userinfo
//! external_id = `sub`（OIDC 标准的稳定账号 ID）。

use serde::Deserialize;

use crate::config::GoogleOAuthConfig;
use crate::domain::OAuthProvider;
use crate::error::AppError;
use crate::service::oauth::{ExternalToken, OAuthProvider as OAuthProviderTrait};

pub struct GoogleProvider {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

impl GoogleProvider {
    pub fn from_config(cfg: &GoogleOAuthConfig) -> Result<Option<Self>, AppError> {
        if cfg.client_id.trim().is_empty() || cfg.client_secret.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
            redirect_uri: cfg.redirect_uri.clone(),
        }))
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct Userinfo {
    sub: String,
    email: Option<String>,
    name: Option<String>,
}

#[async_trait::async_trait]
impl OAuthProviderTrait for GoogleProvider {
    fn name(&self) -> &'static str {
        "google"
    }

    fn enabled(&self) -> bool {
        true
    }

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

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> Result<ExternalToken, AppError> {
        let client = reqwest::Client::new();

        // 1. code → access_token
        let token: TokenResponse = client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("code", code),
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("redirect_uri", redirect_uri),
                ("grant_type", "authorization_code"),
            ])
            .send()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("Google token request failed: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::OAuthWechat(format!("Google token status: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("Google token decode: {e}")))?;

        // 2. access_token → userinfo
        let info: Userinfo = client
            .get("https://openidconnect.googleapis.com/v1/userinfo")
            .bearer_auth(&token.access_token)
            .send()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("Google userinfo failed: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::OAuthWechat(format!("Google userinfo status: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("Google userinfo decode: {e}")))?;

        Ok(ExternalToken {
            provider: OAuthProvider::Google,
            external_id: info.sub,
            email: info.email,
            display_name: info.name,
            access_token: token.access_token,
        })
    }
}
```

- [ ] **Step 3: 在 `src/service/oauth/mod.rs` 注册**

```rust
pub mod google;
```

（与已有 `pub mod wechat;` 同级，按字母序插在 wechat 之后或之间——项目现有顺序是 `mod.rs / url_guard.rs / wechat.rs`，google/github 跟在 wechat 后面就行。）

- [ ] **Step 4: 确认 `ExternalToken` 字段名**

```bash
/usr/bin/grep -A8 "pub struct ExternalToken" /Users/wangxiaoyan/Works/git/rodeo/.claude/worktrees/feat-oauth-google-github/src/service/oauth/mod.rs
```

如果字段名是 `provider / external_id / email / display_name / access_token`（spec §6 假定形态），匹配。如果不是，按实际名字调 Step 2 里的字面量。

- [ ] **Step 5: cargo check**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：0 errors。如果有 `OAuthProvider` trait 的方法签名不一致（async_trait / 命名），按编译错误信息调 Step 2。

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml src/service/oauth/google.rs src/service/oauth/mod.rs
git commit -m "feat(oauth): GoogleProvider (OpenID Connect flow)"
```

---

### Task 4: GithubProvider 实现

**Files:**
- Create: `src/service/oauth/github.rs`
- Modify: `src/service/oauth/mod.rs`（加 `pub mod github;`）

**Interfaces:**
- Consumes: `GithubOAuthConfig { client_id, client_secret, redirect_uri }`（Task 2）
- Produces: `GithubProvider` 实现 `OAuthProvider` trait；`from_config(&GithubOAuthConfig) -> Result<Option<Self>, AppError>`

外部 ID 用 GitHub `/user` 的 `id`（数字，转 String）；email 优先取 `/user.email`，null 时查 `/user/emails` 找 primary verified。

- [ ] **Step 1: 写 `src/service/oauth/github.rs`**

```rust
//! GitHub OAuth2（非 OpenID Connect，无 id_token）。
//! authorize:  https://github.com/login/oauth/authorize
//! token:      https://github.com/login/oauth/access_token
//! userinfo:   https://api.github.com/user
//! 邮箱备用:   https://api.github.com/user/emails
//! external_id = `id`（数字，转字符串）。

use serde::Deserialize;

use crate::config::GithubOAuthConfig;
use crate::domain::OAuthProvider;
use crate::error::AppError;
use crate::service::oauth::{ExternalToken, OAuthProvider as OAuthProviderTrait};

pub struct GithubProvider {
    client_id: String,
    client_secret: String,
    redirect_uri: String,
}

impl GithubProvider {
    pub fn from_config(cfg: &GithubOAuthConfig) -> Result<Option<Self>, AppError> {
        if cfg.client_id.trim().is_empty() || cfg.client_secret.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            client_id: cfg.client_id.clone(),
            client_secret: cfg.client_secret.clone(),
            redirect_uri: cfg.redirect_uri.clone(),
        }))
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    id: serde_json::Value,        // number；serde_json 拿到后转字符串
    login: String,
    name: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

#[async_trait::async_trait]
impl OAuthProviderTrait for GithubProvider {
    fn name(&self) -> &'static str {
        "github"
    }

    fn enabled(&self) -> bool {
        true
    }

    fn authorization_url(&self, state: &str, redirect_uri: &str) -> String {
        format!(
            "https://github.com/login/oauth/authorize\
             ?client_id={}&redirect_uri={}&scope=read:user+user:email&state={}",
            urlencoding::encode(&self.client_id),
            urlencoding::encode(redirect_uri),
            urlencoding::encode(state),
        )
    }

    async fn exchange_code(
        &self,
        code: &str,
        redirect_uri: &str,
    ) -> Result<ExternalToken, AppError> {
        let client = reqwest::Client::new();

        // 1. code → access_token
        let token: TokenResponse = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", &self.client_id),
                ("client_secret", &self.client_secret),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("GitHub token request failed: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::OAuthWechat(format!("GitHub token status: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("GitHub token decode: {e}")))?;

        // 2. access_token → /user
        let user: GithubUser = client
            .get("https://api.github.com/user")
            .bearer_auth(&token.access_token)
            .header("User-Agent", "rodeo")
            .send()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("GitHub user failed: {e}")))?
            .error_for_status()
            .map_err(|e| AppError::OAuthWechat(format!("GitHub user status: {e}")))?
            .json()
            .await
            .map_err(|e| AppError::OAuthWechat(format!("GitHub user decode: {e}")))?;

        // external_id：GitHub 的 id 是数字，serde_json::Value → String。
        let external_id = match &user.id {
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::String(s) => s.clone(),
            _ => {
                return Err(AppError::OAuthWechat(
                    "GitHub user.id unexpected type".into(),
                ))
            }
        };

        // 3. email：user.email 为 null 时查 /user/emails 找 primary verified。
        let email = if let Some(e) = user.email.filter(|e| !e.is_empty()) {
            Some(e)
        } else {
            let emails: Vec<GithubEmail> = client
                .get("https://api.github.com/user/emails")
                .bearer_auth(&token.access_token)
                .header("User-Agent", "rodeo")
                .send()
                .await
                .map_err(|e| AppError::OAuthWechat(format!("GitHub emails failed: {e}")))?
                .error_for_status()
                .map_err(|e| AppError::OAuthWechat(format!("GitHub emails status: {e}")))?
                .json()
                .await
                .map_err(|e| AppError::OAuthWechat(format!("GitHub emails decode: {e}")))?;
            emails
                .into_iter()
                .find(|e| e.primary && e.verified)
                .map(|e| e.email)
        };

        Ok(ExternalToken {
            provider: OAuthProvider::GitHub,
            external_id,
            email,
            display_name: user.name.or(Some(user.login)),
            access_token: token.access_token,
        })
    }
}
```

- [ ] **Step 2: 在 `src/service/oauth/mod.rs` 注册**

```rust
pub mod github;
```

放在 google 之后（字母序）。

- [ ] **Step 3: cargo check**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：0 errors。

- [ ] **Step 4: Commit**

```bash
git add src/service/oauth/github.rs src/service/oauth/mod.rs
git commit -m "feat(oauth): GithubProvider (OAuth2 + /user + /user/emails fallback)"
```

---

### Task 5: OAuthRegistry::from_config 扩展注册三个 provider

**Files:**
- Modify: `src/service/oauth/mod.rs`（`OAuthRegistry::from_config` 加 google/github 注册分支）

**Interfaces:**
- Consumes: `GoogleProvider`（Task 3）、`GithubProvider`（Task 4）、`&Config`
- Produces: `OAuthRegistry { providers: Vec<Arc<dyn OAuthProvider>> }` 含三个 provider（每个按配置存在与否决定）

- [ ] **Step 1: 看现有 `OAuthRegistry::from_config`**

```bash
/usr/bin/grep -n "from_config\|OAuthRegistry" /Users/wangxiaoyan/Works/git/rodeo/.claude/worktrees/feat-oauth-google-github/src/service/oauth/mod.rs
```

确认现有形如：

```rust
pub fn from_config(config: &Config) -> Result<Self, AppError> {
    let mut providers = Vec::new();
    if let Some(cfg) = &config.auth.oauth.wechat {
        if !cfg.app_id.trim().is_empty() {
            providers.push(Arc::new(WeChatProvider::from_config(cfg)?));
        }
    }
    // ↓↓↓ 在这里追加 google + github 分支 ↓↓↓
    Ok(Self { providers })
}
```

- [ ] **Step 2: 加 google/github 分支**

在 `providers.push(Arc::new(WeChatProvider::from_config(cfg)?));` 之后加：

```rust
    if let Some(cfg) = &config.auth.oauth.google {
        if !cfg.client_id.trim().is_empty() {
            if let Some(p) = GoogleProvider::from_config(cfg)? {
                providers.push(Arc::new(p));
            }
        }
    }
    if let Some(cfg) = &config.auth.oauth.github {
        if !cfg.client_id.trim().is_empty() {
            if let Some(p) = GithubProvider::from_config(cfg)? {
                providers.push(Arc::new(p));
            }
        }
    }
```

注：`from_config` 返回 `Result<Option<Self>, AppError>`，第一层 `?` 解 `Result`，第二层 `if let Some(p)` 解 `Option`。与 WeChat 分支的写法（`providers.push(Arc::new(WeChatProvider::from_config(cfg)?));`）不同——前者用 `?` 让 `None` 直接落空、后者展开后类型明确。这里选展开形式是为了类型推断（`Arc<dyn OAuthProvider>` 从 trait object 推导）。

- [ ] **Step 3: cargo check**

```bash
cargo check -p rodeo
```

预期：0 errors。如果报「`GoogleProvider`/`GithubProvider` 没有 `OAuthProvider` trait 实现」，回 Task 3/4 检查 `OAuthProviderTrait` 的方法签名。

- [ ] **Step 4: Commit**

```bash
git add src/service/oauth/mod.rs
git commit -m "feat(oauth): 注册 Google/GitHub provider 到 OAuthRegistry"
```

---

### Task 6: 公共回调路径 oauth_callback_common（含 csrf provider 校验）

**Files:**
- Modify: `src/api/oauth.rs`（抽出 `oauth_callback_common`；`wechat_callback` 改用公共路径）

**Interfaces:**
- Consumes: `OAuthRegistry`（Task 5）、`OAuthStateStore`（Task 1 加了 `provider` 字段）、`OAuthBindingsService`、`AuthService`
- Produces: 私有 `async fn oauth_callback_common(state, provider_name, code, state_token, return_to) -> Result<Redirect, AppError>`；三步算法见 spec §7

- [ ] **Step 1: 看现有 `wechat_callback` 结构**

```bash
/usr/bin/grep -n "wechat_callback\|exchange_code\|take_csrf\|take_bind\|put_bind\|redirect_with_token" /Users/wangxiaoyan/Works/git/rodeo/.claude/worktrees/feat-oauth-google-github/src/api/oauth.rs
```

确认形如：

```
async fn wechat_callback(...) -> Result<Redirect, AppError> {
    let csrf = services.oauth_state.take_csrf(&state)?;
    // ... is_safe_return_to ...
    let external = services.oauth_registry.exchange("wechat", &code, ...).await?;
    // existing-binding 路径
    // 没 binding 路径：写 BindEntry，redirect to /oauth/callback?bind=...
}
```

- [ ] **Step 2: 抽出 `oauth_callback_common`**

在 `wechat_callback` 之前加：

```rust
/// 三个 provider 回调的公共路径：
/// 1. csrf 校验 + provider 比对（防跨 provider 重放）
/// 2. 查 binding，命中即签 token
/// 3. email 命中已有账号 → 静默登录（仅 Google/GitHub 路径，WeChat email 为 None 不进）
/// 4. 否则写 BindEntry，redirect to /oauth/callback?bind=...&provider=...
async fn oauth_callback_common(
    state: Arc<AppState>,
    provider_name: &str,
    code: String,
    state_token: String,
    return_to: String,
) -> Result<axum::response::Redirect, AppError> {
    let return_to = if crate::service::oauth::url_guard::is_safe_return_to(&return_to) {
        return_to
    } else {
        "/workspaces".to_string()
    };

    let csrf = state
        .services
        .oauth_state
        .take_csrf(&state_token)
        .ok_or_else(|| AppError::OAuthCallback("登录已过期，请重试".to_string()))?;

    // csrf 必须与本次请求的 provider 对应
    if csrf.provider != provider_name {
        return Err(AppError::OAuthCallback("登录状态不匹配".to_string()));
    }

    let provider_kind = crate::service::oauth::provider_kind_from_str(provider_name)
        .ok_or_else(|| AppError::OAuthCallback(format!("未知的 provider: {provider_name}")))?;

    let external = state
        .services
        .oauth_registry
        .exchange(provider_name, &code, &csrf.redirect_uri())
        .await?;

    // 1. 已绑过 → 直接签 token
    if let Some(b) = state
        .services
        .oauth_bindings
        .find(provider_kind, &external.external_id)?
    {
        return crate::api::oauth::redirect_with_token(
            state.clone(),
            b.account_id,
            &return_to,
        )
        .await;
    }

    // 2. 邮箱命中 → 静默登录（仅 Google/GitHub 触发；WeChat external.email 为 None）
    if let Some(email) = &external.email {
        if let Some(account) = state.services.auth.find_by_email(email)? {
            use crate::domain::AccountStatus;
            match state.services.auth.status(account.id)? {
                AccountStatus::Active => {}
                AccountStatus::Frozen => {
                    return Err(AppError::OAuthWechat(
                        "账号已被冻结，请联系系统管理员".to_string(),
                    ));
                }
                AccountStatus::Deactivated => {
                    return Err(AppError::OAuthCallback(
                        "此账号已注销".to_string(),
                    ));
                }
            }
            state.services.oauth_bindings.upsert(
                account.id,
                provider_kind,
                &external.external_id,
                external.email.clone(),
                external.display_name.clone(),
            )?;
            return crate::api::oauth::redirect_with_token(
                state.clone(),
                account.id,
                &return_to,
            )
            .await;
        }
    }

    // 3. 没 binding、没 email 命中 → bind 面板
    let bind_token = crate::service::oauth_state::OAuthStateStore::new_token();
    state.services.oauth_state.put_bind(
        bind_token.clone(),
        crate::service::oauth_state::BindEntry {
            provider: provider_kind,
            external_id: external.external_id,
            access_token: external.access_token,
            return_to: return_to.clone(),
            created_at: chrono::Utc::now(),
        },
    );
    Ok(axum::response::Redirect::to(&format!(
        "/oauth/callback?bind={bind_token}&provider={provider_name}&return_to={}",
        urlencoding::encode(&return_to)
    )))
}
```

注意：
- `provider_kind_from_str` 是个新加的小函数（Step 3）；不在 trait 上是为了避免循环依赖。
- `csrf.redirect_uri()` —— `CsrfEntry` 当前没 `redirect_uri` 字段。Spec 1 的 start 路径把 `redirect_uri` 放在 handler 闭包里，不在 csrf 里。改 `CsrfEntry` 加这个字段（Step 2.5）。

- [ ] **Step 2.5: 给 `CsrfEntry` 加 `redirect_uri` 字段**

更新 `src/service/oauth_state.rs`：

```rust
pub struct CsrfEntry {
    pub provider: String,
    pub redirect_uri: String,
    pub return_to: String,
    pub created_at: DateTime<Utc>,
}
```

同步在 `wechat_start` 写 csrf 处填 `redirect_uri`（handler 自己拼好了，传进 csrf）。

- [ ] **Step 3: 加 `provider_kind_from_str` 辅助函数**

在 `src/service/oauth/mod.rs` 加：

```rust
use crate::domain::OAuthProvider;

pub fn provider_kind_from_str(s: &str) -> Option<OAuthProvider> {
    match s {
        "wechat" => Some(OAuthProvider::WeChat),
        "google" => Some(OAuthProvider::Google),
        "github" => Some(OAuthProvider::GitHub),
        _ => None,
    }
}
```

- [ ] **Step 4: 重构 `wechat_callback`**

把现有 `wechat_callback` 的「拿 csrf → 校验 → exchange → 已绑 / bind 面板」整段替换为：

```rust
async fn wechat_callback(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_token = params.get("state").cloned().unwrap_or_default();
    let return_to = params.get("return_to").cloned().unwrap_or_else(|| "/workspaces".into());
    oauth_callback_common(state, "wechat", code, state_token, return_to).await
}
```

注：保留现有签名（Query 解析、Redirect 返回）—— 这是 Spec 1 已在用的形态。

- [ ] **Step 5: cargo check**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：0 errors。

- [ ] **Step 6: Commit**

```bash
git add src/service/oauth.rs src/service/oauth/mod.rs src/api/oauth.rs
git commit -m "feat(oauth): 抽出 oauth_callback_common 公共路径（含 csrf provider 校验）"
```

---

### Task 7: 邮箱静默登录分支已合入 Task 6，验证之

> 这条 task 与 Task 6 是同一段代码，仅作 review 关注点列出。

**Files:** 同 Task 6

- [ ] **Step 1: 走一遍 review focus §2**

代码里搜 `find_by_email`（在 `oauth_callback_common` 里）。验证：
- 找到账号后，**先**调 `auth.status(account.id)?`，**再**决定走 Active / Frozen / Deactivated。
- Deactivated 路径走 `OAuthCallback` 错误（与 spec §8 守门规则一致）。
- Frozen 路径走 `OAuthWechat`（与 Spec 1 `login` 路径文案一致）。
- 不调用 `verify_password`（静默登录不要求密码）。

如果 Task 6 Step 2 的代码已经满足这些，不需要再改。否则按 review focus 调。

- [ ] **Step 2: 不单独 commit**

此 task 与 Task 6 一并处理；review focus 在 reviewer 端验证。

---

### Task 8: OAuthBindingsService::delete + 「唯一登录方式」守卫

**Files:**
- Modify: `src/service/oauth_bindings.rs`（新增 `delete` 方法）

**Interfaces:**
- Consumes: `OAuthBindingsService` 已有 `find / find_by_account_and_provider / upsert`
- Produces: `pub fn delete(&self, account_id: Ulid, provider: OAuthProvider, external_id: &str) -> Result<(), AppError>`

守卫：
- `find(provider, external_id)` 必须存在且 `binding.account_id == account_id`（否则 `InvalidQuery("无权解绑此账号")`）
- 若 `account.has_password() == false` 且该 provider 是该账号的唯一 binding → `InvalidQuery("请先设置密码或绑定其他第三方账号再解绑")`
- 否则删 CF key

- [ ] **Step 1: 写 `delete` 方法**

在 `src/service/oauth_bindings.rs` `find_by_account_and_provider` 之后加：

```rust
pub fn delete(
    &self,
    account_id: Ulid,
    provider: OAuthProvider,
    external_id: &str,
) -> Result<(), AppError> {
    let binding = self
        .find(provider, external_id)?
        .ok_or(AppError::NotFound)?;
    if binding.account_id != account_id {
        return Err(AppError::InvalidQuery("无权解绑此账号".to_string()));
    }

    // 「唯一登录方式」守卫：OAuth-only 且该 provider 是该账号唯一 binding 时拒解绑。
    let account = self
        .store
        .get::<crate::domain::Account>(crate::storage::cf::ACCOUNTS, &account_id.to_bytes())?
        .ok_or(AppError::NotFound)?;
    if !account.has_password() {
        let other = self.find_by_account_and_provider(account_id, provider)?;
        if other.is_none() {
            return Err(AppError::InvalidQuery(
                "请先设置密码或绑定其他第三方账号再解绑".to_string(),
            ));
        }
    }

    self.store.delete(crate::storage::cf::OAUTH_BINDINGS, &Self::key(provider, external_id))?;
    Ok(())
}
```

- [ ] **Step 2: 验证 `store.get::<T>` / `store.delete` API**

```bash
/usr/bin/grep -n "pub fn get<\|pub fn delete" /Users/wangxiaoyan/Works/git/rodeo/.claude/worktrees/feat-oauth-google-github/src/storage/doc.rs | head -10
```

确认 `get<T>` 返回 `Result<Option<T>, AppError>`，`delete(cf, key)` 返回 `Result<(), AppError>`。如果名字或签名对不上，按实际改 Step 1。

- [ ] **Step 3: cargo check**

```bash
cargo check -p rodeo
```

预期：0 errors。

- [ ] **Step 4: Commit**

```bash
git add src/service/oauth_bindings.rs
git commit -m "feat(oauth): OAuthBindingsService::delete + 唯一登录方式守卫"
```

---

### Task 9: OAuthBindingsService::find_all_by_account

**Files:**
- Modify: `src/service/oauth_bindings.rs`（新增 `find_all_by_account` 方法）

**Interfaces:**
- Produces: `pub fn find_all_by_account(&self, account_id: Ulid) -> Result<Vec<IdentityBinding>, AppError>`

全表扫 + 过滤（账号量级 O(N) 可接受，与现有 `find_by_account_and_provider` 同思路）。

- [ ] **Step 1: 写方法**

```rust
pub fn find_all_by_account(
    &self,
    account_id: Ulid,
) -> Result<Vec<crate::domain::IdentityBinding>, AppError> {
    let mut out = Vec::new();
    for (_k, v) in self.store.scan_prefix(crate::storage::cf::OAUTH_BINDINGS, b"")? {
        let b: crate::domain::IdentityBinding = bincode::deserialize(&v)?;
        if b.account_id == account_id {
            out.push(b);
        }
    }
    Ok(out)
}
```

- [ ] **Step 2: cargo check**

```bash
cargo check -p rodeo
```

预期：0 errors。

- [ ] **Step 3: Commit**

```bash
git add src/service/oauth_bindings.rs
git commit -m "feat(oauth): OAuthBindingsService::find_all_by_account"
```

---

### Task 10: GraphQL myOAuthBindings 查询 + unbindOauthBinding mutation

**Files:**
- Modify: `src/api/graphql.rs`（Query 加 `my_oauth_bindings`；Mutation 加 `unbind_oauth_binding`）

**Interfaces:**
- Consumes: `OAuthBindingsService::find_all_by_account`（Task 9）、`OAuthBindingsService::delete`（Task 8）
- Produces: `Query::my_oauth_bindings -> Vec<GqlOAuthBinding>`（已登录态，按 `gql.require_auth()?.account_id` 过滤）；`Mutation::unbind_oauth_binding(account_id, provider, external_id) -> bool`

鉴权：
- `my_oauth_bindings` 要求登录态，从 `require_auth()` 拿 `account_id`，不接外部 `account_id` 参数（避免越权）。
- `unbind_oauth_binding` 接 `account_id` 参数，但 resolver 里要校验 `account_id == require_auth()?.account_id`（防借用未鉴权参数绕过）。

- [ ] **Step 1: 加 `GqlOAuthBinding` 类型**

在 `src/api/graphql.rs` 已有 GQL 类型附近（如 `GqlAdminAccount` 旁）加：

```rust
#[derive(SimpleObject)]
pub struct GqlOAuthBinding {
    pub provider: String,
    pub external_id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub bound_at: chrono::DateTime<chrono::Utc>,
}

impl From<crate::domain::IdentityBinding> for GqlOAuthBinding {
    fn from(b: crate::domain::IdentityBinding) -> Self {
        Self {
            provider: b.provider.as_str().to_string(),
            external_id: b.external_id,
            email: b.email,
            display_name: b.display_name,
            bound_at: b.bound_at,
        }
    }
}
```

- [ ] **Step 2: 加 `Query::my_oauth_bindings`**

定位 `Query` impl 块内已有 `me()` 等的位置，加：

```rust
async fn my_oauth_bindings(&self, ctx: &Context<'_>) -> GqlResult<Vec<GqlOAuthBinding>> {
    let gql = ctx.data::<GraphqlContext>()?;
    let auth = gql.require_auth()?;
    let bindings = gql.services.oauth_bindings.find_all_by_account(auth.account_id)?;
    Ok(bindings.into_iter().map(Into::into).collect())
}
```

- [ ] **Step 3: 加 `Mutation::unbind_oauth_binding`**

定位 `Mutation` impl 块内已有 `set_account_status` 等位置，加：

```rust
async fn unbind_oauth_binding(
    &self,
    ctx: &Context<'_>,
    account_id: ID,
    provider: String,
    external_id: String,
) -> GqlResult<bool> {
    let gql = ctx.data::<GraphqlContext>()?;
    let auth = gql.require_auth()?;
    let target_id = parse_ulid(account_id.as_str())?;

    // 防借用未鉴权参数绕过：调用者只能解绑自己的 binding。
    if target_id != auth.account_id {
        return Err(AppError::InvalidQuery("无权解绑此账号".to_string()).into());
    }

    let provider_kind = crate::service::oauth::provider_kind_from_str(&provider)
        .ok_or_else(|| AppError::InvalidQuery(format!("未知的 provider: {provider}")))?;

    gql.services
        .oauth_bindings
        .delete(target_id, provider_kind, &external_id)?;
    Ok(true)
}
```

- [ ] **Step 4: cargo check**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：0 errors。

- [ ] **Step 5: Commit**

```bash
git add src/api/graphql.rs
git commit -m "feat(oauth): GraphQL myOAuthBindings 查询 + unbindOauthBinding mutation"
```

---

### Task 11: AuthService::set_password 服务方法

**Files:**
- Modify: `src/service/auth.rs`（新增 `set_password` 方法）

**Interfaces:**
- Produces: `pub fn set_password(&self, account_id: Ulid, new_password: &str) -> Result<Account, AppError>`

约束：
- `find_by_id` 拿账号 → 不存在则 `NotFound`
- 若 `account.has_password()` → `InvalidQuery("此账号已有密码，请使用 changePassword 修改")`
- `validate_password(new_password)` → `WeakPassword`
- `hash_password(new_password)` → `Account.password_hash` 覆盖
- 写 ACCOUNTS
- **不**调 `revoke_tokens`（OAuth-only 账号加备用登录方式，不踢自己下线）

- [ ] **Step 1: 在 `change_password` 之后加 `set_password`**

`src/service/auth.rs`，定位 `change_password` 方法末尾，加：

```rust
/// 后补密码：仅 OAuth-only 账号（`!has_password()`）允许。
///
/// 与 `change_password` 的区别：这里不需要旧密码（用户根本没设过），
/// 且**不**调 `revoke_tokens`——这是 OAuth-only 账号给自身加备用登录方式，
/// 吊销会把用户在唯一登录方式踢下线。
pub fn set_password(&self, account_id: Ulid, new_password: &str) -> Result<Account, AppError> {
    let mut account = self.find_by_id(account_id)?.ok_or(AppError::NotFound)?;
    if account.has_password() {
        return Err(AppError::InvalidQuery(
            "此账号已有密码，请使用 changePassword 修改".to_string(),
        ));
    }
    validate_password(new_password)?;
    account.password_hash = hash_password(new_password)?;
    self.store
        .put(cf::ACCOUNTS, &account.id.to_bytes(), &account)?;
    Ok(account)
}
```

- [ ] **Step 2: cargo check**

```bash
cargo check -p rodeo
```

预期：0 errors。

- [ ] **Step 3: Commit**

```bash
git add src/service/auth.rs
git commit -m "feat(auth): set_password 服务方法（OAuth-only 账号后补密码，不踢自己下线）"
```

---

### Task 12: GraphQL setPassword mutation + GqlAccount::has_password 投影

**Files:**
- Modify: `src/api/graphql.rs`（Mutation 加 `set_password`；`GqlAccount` 加 `has_password` 字段）

**Interfaces:**
- Consumes: `AuthService::set_password`（Task 11）
- Produces: `Mutation::set_password(new_password) -> GqlAuthResult`；`GqlAccount.has_password: bool`

- [ ] **Step 1: 找 `GqlAccount` 定义和它的 `From<Account>`**

```bash
/usr/bin/grep -n "struct GqlAccount\|impl From<Account> for GqlAccount\|impl From<.*Account> for GqlAccount" /Users/wangxiaoyan/Works/git/rodeo/.claude/worktrees/feat-oauth-google-github/src/api/graphql.rs
```

确认结构形态。

- [ ] **Step 2: 给 `GqlAccount` 加 `has_password`**

在 `GqlAccount` 字段列表里加 `pub has_password: bool`。在 `From<Account>` 实现里加 `has_password: account.has_password()`。

**绝对不要**加 `password_hash` 字段——那是泄漏。

- [ ] **Step 3: 加 `Mutation::set_password`**

在 `change_password` 之后加：

```rust
async fn set_password(
    &self,
    ctx: &Context<'_>,
    new_password: String,
) -> GqlResult<GqlAuthResult> {
    let gql = ctx.data::<GraphqlContext>()?;
    let auth = gql.require_auth()?;
    let account = gql
        .services
        .auth
        .set_password(auth.account_id, &new_password)?;
    let token = gql.services.auth.sign_token(auth.account_id)?;
    Ok(GqlAuthResult {
        token,
        account: account.into(),
    })
}
```

- [ ] **Step 4: cargo check**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：0 errors。如果 `GqlAccount` 已有 `From<Account>` 实现但路径不一致，按实际调 Step 2。

- [ ] **Step 5: Commit**

```bash
git add src/api/graphql.rs
git commit -m "feat(oauth): GraphQL setPassword mutation + GqlAccount.has_password 投影"
```

---

### Task 13: HTTP /api/auth/{google,github}/start + /callback 路由

**Files:**
- Modify: `src/api/oauth.rs`（新增 `google_start` / `google_callback` / `github_start` / `github_callback` 四个 handler）
- Modify: `src/api/mod.rs`（新增 `pub use oauth::{github_callback, github_start, google_callback, google_start};`）
- Modify: `src/main.rs`（注册 4 个新 route）

**Interfaces:**
- Consumes: `OAuthRegistry`（Task 5）、`oauth_callback_common`（Task 6）
- Produces: 4 个 `async fn ... -> Result<Redirect, AppError>`，各自薄壳；main.rs 多 4 行 `.route(...)`

- [ ] **Step 1: 看 `wechat_start` 的现有实现**

```bash
/usr/bin/grep -n "fn wechat_start\|wechat_start\b" /Users/wangxiaoyan/Works/git/rodeo/.claude/worktrees/feat-oauth-google-github/src/api/oauth.rs
```

抄它的形态（读 query、is_safe_return_to、生成 csrf、put_csrf、构造 redirect_url、调 `provider.authorization_url`）。

- [ ] **Step 2: 加 `google_start` / `github_start`**

```rust
pub async fn google_start(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    start_for_provider(state, "google", params).await
}

pub async fn github_start(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    start_for_provider(state, "github", params).await
}

async fn start_for_provider(
    state: Arc<AppState>,
    provider_name: &'static str,
    params: HashMap<String, String>,
) -> Result<Redirect, AppError> {
    let raw_return_to = params
        .get("return_to")
        .cloned()
        .unwrap_or_else(|| "/workspaces".to_string());
    let return_to = if crate::service::oauth::url_guard::is_safe_return_to(&raw_return_to) {
        raw_return_to
    } else {
        "/workspaces".to_string()
    };

    let provider = state
        .services
        .oauth_registry
        .get(provider_name)
        .ok_or(AppError::NotConfigured(format!("{provider_name} 未启用")))?;

    let csrf_token = crate::service::oauth_state::OAuthStateStore::new_token();
    let redirect_uri = format!("https://example.invalid/{}", provider_name); // 见 Step 2.5
    state.services.oauth_state.put_csrf(
        csrf_token.clone(),
        crate::service::oauth_state::CsrfEntry {
            provider: provider_name.into(),
            redirect_uri,
            return_to: return_to.clone(),
            created_at: chrono::Utc::now(),
        },
    );

    let auth_url = provider.authorization_url(&csrf_token, /* 见 Step 2.5 */ "");
    Ok(Redirect::to(&auth_url))
}
```

- [ ] **Step 2.5: redirect_uri 的来源**

`redirect_uri` 必须从配置读，**不能**用 `https://example.invalid/...` 占位。

但 `OAuthRegistry::get(name)` 返回 `Arc<dyn OAuthProvider>`，不直接暴露 `redirect_uri` 字段。

**两个选择**：

- (a) 给 `OAuthProvider` trait 加 `fn redirect_uri(&self) -> &str;` 默认实现返回空串，子结构覆盖。`GoogleProvider::redirect_uri()` 返回 `&self.redirect_uri`。
- (b) 在 `start_for_provider` 里直接读 `state.services.config.auth.oauth.{google,github}`，按 `provider_name` match。

选 (a) 更干净（与 trait 方法对齐），但要改三个 provider impl。选 (b) 更省事（不碰 trait）。

**决定用 (b)**——Task 13 在 `start_for_provider` 内部 `match provider_name`：

```rust
let redirect_uri = match provider_name {
    "google" => state
        .services
        .config
        .auth
        .oauth
        .google
        .as_ref()
        .map(|c| c.redirect_uri.as_str())
        .unwrap_or(""),
    "github" => state
        .services
        .config
        .auth
        .oauth
        .github
        .as_ref()
        .map(|c| c.redirect_uri.as_str())
        .unwrap_or(""),
    _ => "",
};
```

并把 `provider.authorization_url(&csrf_token, redirect_uri)` 的第二参用这个 `redirect_uri`。

- [ ] **Step 3: 加 `google_callback` / `github_callback`**

```rust
pub async fn google_callback(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_token = params.get("state").cloned().unwrap_or_default();
    let return_to = params.get("return_to").cloned().unwrap_or_else(|| "/workspaces".into());
    crate::api::oauth::oauth_callback_common(state, "google", code, state_token, return_to).await
}

pub async fn github_callback(
    Extension(state): Extension<Arc<AppState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    let code = params.get("code").cloned().unwrap_or_default();
    let state_token = params.get("state").cloned().unwrap_or_default();
    let return_to = params.get("return_to").cloned().unwrap_or_else(|| "/workspaces".into());
    crate::api::oauth::oauth_callback_common(state, "github", code, state_token, return_to).await
}
```

- [ ] **Step 4: `pub use` 出口**

`src/api/mod.rs` 加：

```rust
pub use oauth::{github_callback, github_start, google_callback, google_start};
```

- [ ] **Step 5: 注册路由**

`src/main.rs` 在已有 `/api/auth/wechat/...` 路由附近加：

```rust
.route("/api/auth/google/start", get(google_start))
.route("/api/auth/google/callback", get(google_callback))
.route("/api/auth/github/start", get(github_start))
.route("/api/auth/github/callback", get(github_callback))
```

确认 import：`use crate::api::{google_callback, google_start, github_callback, github_start, ...};`（或按 main.rs 现有写法调）。

- [ ] **Step 6: cargo check**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：0 errors。

- [ ] **Step 7: Commit**

```bash
git add src/api/oauth.rs src/api/mod.rs src/main.rs
git commit -m "feat(oauth): Google/GitHub start + callback 路由"
```

---

### Task 14: 前端 /account 第三方账号区段 + unbind 按钮 + 设置密码表单

**Files:**
- Modify: `src/frontend/pages/account.rs`（新增第三方账号区段 + 设置密码表单）
- Modify: `src/frontend/graphql_client.rs`（新增 `my_oauth_bindings` / `unbind_oauth_binding` / `set_password` helper）

**Interfaces:**
- Consumes: GraphQL `myOAuthBindings` / `unbindOauthBinding` / `setPassword`（Tasks 10/12）
- Produces: 前端 helpers（fn signatures 镜像已有 `bind_oauth_to_existing` 形态）；`<Account>` 组件新增两个 section

- [ ] **Step 1: 看现有 `Account` 组件结构**

```bash
/usr/bin/grep -n "fn Account\b\|view!\|<form\|<section" /Users/wangxiaoyan/Works/git/rodeo/.claude/worktrees/feat-oauth-google-github/src/frontend/pages/account.rs | head -30
```

确定现有区段切分。

- [ ] **Step 2: 加 graphql_client helpers**

在 `src/frontend/graphql_client.rs` 末尾加：

```rust
pub async fn my_oauth_bindings() -> Result<Vec<OAuthBinding>, String> {
    let body = r#"{"query":"query { myOAuthBindings { provider externalId email displayName boundAt } }"}"#;
    let resp = send_request(body).await?;
    let data = resp.data.ok_or_else(|| "no data".to_string())?;
    serde_json::from_value(data["myOAuthBindings"].clone())
        .map_err(|e| format!("decode myOAuthBindings: {e}"))
}

pub async fn unbind_oauth_binding(
    account_id: &str,
    provider: &str,
    external_id: &str,
) -> Result<(), String> {
    let body = format!(
        r#"{{"query":"mutation {{ unbindOauthBinding(accountId: \"{}\", provider: \"{}\", externalId: \"{}\") }}"}}"#,
        account_id, provider, external_id
    );
    let resp = send_request(&body).await?;
    if !resp.errors.as_ref().map(|e| e.is_empty()).unwrap_or(true) {
        return Err(resp.errors.unwrap().into_iter().map(|e| e.message).collect::<Vec<_>>().join("; "));
    }
    Ok(())
}

pub async fn set_password(new_password: &str) -> Result<(String, User), String> {
    let body = format!(
        r#"{{"query":"mutation {{ setPassword(newPassword: \"{}\") {{ token account {{ id email name isAdmin hasPassword }} }} }}"#,
        new_password.replace('"', "\\\"")
    );
    let resp = send_request(&body).await?;
    let data = resp.data.ok_or_else(|| "no data".to_string())?;
    let auth: AuthResult = serde_json::from_value(data["setPassword"].clone())
        .map_err(|e| format!("decode setPassword: {e}"))?;
    Ok((auth.token, auth.user))
}
```

类型 `OAuthBinding` / `User` / `AuthResult` 按现有 helper 文件已有形态 mirror（grep `struct User` / `struct AuthResult` / `struct OAuthBinding` 之类的；如果是 serde_json::Value 中转就不需要结构体）。

- [ ] **Step 3: 加 `Account` 组件的「第三方账号」section**

在 `<Account>` 组件内部，`me()` signal 已有后，加：

```rust
let bindings = RwSignal::new(Vec::<OAuthBinding>::new());
let bind_error = RwSignal::new(None::<String>);

Effect::new_sync(move |_| {
    if !cfg!(target_arch = "wasm32") || auth.user.get().is_none() {
        return;
    }
    spawn_local(async move {
        if let Ok(b) = my_oauth_bindings().await {
            bindings.set(b);
        }
    });
});
```

并在 `view!` 里加 section（位置：放在原有「基本信息」section 之后，「修改密码」section 之前）：

```rust
{move || (!bindings.get().is_empty()).then(|| view! {
    <section class="panel">
        <h3>"第三方账号"</h3>
        {move || bindings.get().into_iter().map(|b| view! {
            <div class="binding-row" style="display:flex;justify-content:space-between;padding:8px 0;border-bottom:1px solid var(--bd)">
                <div>
                    <b>{provider_label(&b.provider)}</b>
                    <span class="mut" style="margin-left:8px">{b.email.unwrap_or_else(|| b.display_name.unwrap_or_else(|| b.external_id.clone()))}</span>
                    <span class="mut" style="margin-left:8px;font-size:12px">{"绑定于 "}{format_dt(b.bound_at)}</span>
                </div>
                <button class="btn" on:click=move |_| {
                    let account_id = auth.user.get().as_ref().map(|u| u.id.clone()).unwrap_or_default();
                    let provider = b.provider.clone();
                    let external_id = b.external_id.clone();
                    spawn_local(async move {
                        match unbind_oauth_binding(&account_id, &provider, &external_id).await {
                            Ok(()) => {
                                bindings.update(|v| v.retain(|x| !(x.provider == provider && x.external_id == external_id)));
                                bind_error.set(None);
                            }
                            Err(e) => bind_error.set(Some(e)),
                        }
                    });
                }>"解除绑定"</button>
            </div>
        }).collect::<Vec<_>>()}
        {move || bind_error.get().map(|e| view! { <p class="error">{e}</p> })}
    </section>
})}
```

辅助函数（模块顶层）：

```rust
fn provider_label(p: &str) -> &'static str {
    match p {
        "wechat" => "微信",
        "google" => "Google",
        "github" => "GitHub",
        _ => "其他",
    }
}

fn format_dt(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d").to_string()
}
```

- [ ] **Step 4: 加 `Account` 组件的「设置密码」section**

仅 `!has_password` 时显示：

```rust
let new_pw = RwSignal::new(String::new());
let pw_error = RwSignal::new(None::<String>);
let pw_busy = RwSignal::new(false);

{move || {
    let show = auth.user.get().as_ref().map(|u| !u.has_password).unwrap_or(false);
    show.then(|| view! {
        <section class="panel">
            <h3>"设置 Rodeo 密码"</h3>
            <p class="mut">"此账号未设密码，设置后即可用邮箱+密码登录。"</p>
            <input class="inp" type="password" placeholder="新密码（至少 8 位，含大小写和数字）"
                prop:value=new_pw on:input=move |ev| new_pw.set(event_target_value(&ev)) />
            {move || pw_error.get().map(|e| view! { <p class="error">{e}</p> })}
            <button class="btn pri" disabled=move || pw_busy.get() on:click=move |_| {
                let p = new_pw.get();
                pw_busy.set(true);
                pw_error.set(None);
                spawn_local(async move {
                    match set_password(&p).await {
                        Ok((token, user)) => {
                            set_token(&token);
                            auth.user.set(Some(user));
                            pw_busy.set(false);
                        }
                        Err(e) => {
                            pw_error.set(Some(e));
                            pw_busy.set(false);
                        }
                    }
                });
            }}>"保存密码"</button>
        </section>
    })
}}
```

- [ ] **Step 5: cargo check**

```bash
cargo check -p rodeo
cargo check --target wasm32-unknown-unknown --no-default-features --features hydrate
```

预期：0 errors。

- [ ] **Step 6: Commit**

```bash
git add src/frontend/graphql_client.rs src/frontend/pages/account.rs
git commit -m "feat(oauth): /account 第三方账号区段 + 设置密码表单"
```

---

## 自审

### 1. Spec coverage

| Spec 章节 | 对应 task |
| --- | --- |
| §4 数据模型（不动 Account / IdentityBinding / BindEntry + 新 GqlOAuthBinding） | T10 Step 1 |
| §5 配置（OAuthConfig + GoogleOAuthConfig + GithubOAuthConfig + example.toml） | T2 |
| §6 GoogleProvider | T3 |
| §6 GithubProvider | T4 |
| §7 oauth_callback_common | T6 |
| §8 邮箱静默登录分支 | T6 Step 2 + T7 review focus |
| §9 Unbind（GraphQL + 服务） | T8（服务）+ T10（GraphQL） |
| §10 后补密码 | T11（服务）+ T12（GraphQL） |
| §11 /account 面板 | T14 |
| §12 has_password 投影 | T12 Step 2 |
| §13 验证（cargo check） | 每个 task Step 4/5/6 都跑 |
| §14 CsrfEntry 加 provider 字段 | T1 + T6 Step 2.5（redirect_uri） |

**全部 spec 章节都有 task。**

### 2. Placeholder scan

```
$ /usr/bin/grep -nE "TBD|TODO|FIXME|XXX|fill in|appropriate|implement later" plan.md
(0 hits)
```

无占位符。

### 3. Type consistency

| 类型 | 定义处 | 使用处 | 一致性 |
| --- | --- | --- | --- |
| `GoogleProvider::from_config(&GoogleOAuthConfig) -> Result<Option<Self>, AppError>` | T3 Step 2 | T5 Step 2 | ✓ |
| `GithubProvider::from_config(&GithubOAuthConfig) -> Result<Option<Self>, AppError>` | T4 Step 1 | T5 Step 2 | ✓ |
| `OAuthRegistry::from_config(&Config) -> Result<Self, AppError>` | Spec 1 已有，T5 扩展 | T5 内部调 `provider_kind_from_str` 等 | ✓ |
| `CsrfEntry { provider, redirect_uri, return_to, created_at }` | T1 Step 1 + T6 Step 2.5 | T1 Step 3（wechat_start）+ T13 Step 2（google/github_start）+ T6 Step 2（oauth_callback_common 校验 provider + 读 redirect_uri） | ✓ |
| `OAuthBindingsService::delete(account_id, provider, external_id) -> Result<(), AppError>` | T8 Step 1 | T10 Step 3（resolver） | ✓ |
| `OAuthBindingsService::find_all_by_account(account_id) -> Result<Vec<IdentityBinding>, AppError>` | T9 Step 1 | T10 Step 2（resolver） | ✓ |
| `AuthService::set_password(account_id, new_password) -> Result<Account, AppError>` | T11 Step 1 | T12 Step 3（resolver） | ✓ |
| `Mutation::unbind_oauth_binding(account_id, provider, external_id) -> bool` | T10 Step 3 | T14 Step 2（前端 helper） | ✓ |
| `Mutation::set_password(new_password) -> AuthResult` | T12 Step 3 | T14 Step 2（前端 helper） | ✓ |
| `Query::my_oauth_bindings -> [GqlOAuthBinding!]!` | T10 Step 2 | T14 Step 2（前端 helper） | ✓ |
| `GqlOAuthBinding { provider, external_id, email, display_name, bound_at }` | T10 Step 1 | T14 Step 2（前端 helper 解析） | ✓ |
| `GqlAccount.has_password: bool` | T12 Step 2 | T14 Step 4（前端用） | ✓ |

无类型不一致。

### 4. Review Focus

5 项 review focus 都有 task 显式覆盖：

1. csrf 跨 provider 重放 → T6 Step 2 + T1 Step 3（wechat_start 同步填字段）
2. 邮箱静默登录撞上冻结账号 → T6 Step 2（Active/Frozen/Deactivated match）
3. OAuth-only 唯一 binding 的 unbind 拒绝 → T8 Step 1
4. `set_password` 不调 `revoke_tokens` → T11 Step 1（注释显式说明）
5. `me().has_password` 是布尔而非 hash → T12 Step 2（注释明确禁止 password_hash 暴露）

## Execution Handoff

Plan 已写完，存到 `docs/superpowers/plans/2026-10-04-oauth-google-github.md`，commit hash 在 worktree `worktree-feat-oauth-google-github` 上独立维护。

按项目 memory「每任务后自动 commit」和「Subagent-driven」，建议用 `superpowers:subagent-driven-development` 执行——每 task 一个 implementer subagent、一个 task reviewer、最后整支审查。

是否进入 SDD 执行？如果用，直接说「SDD 执行」或「开干」；如果还要先调哪条 task，告诉我改哪里。
