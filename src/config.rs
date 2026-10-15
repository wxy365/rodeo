use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub ai: AiConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub query: QueryConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub tls: Option<TlsConfig>,
}

/// 存在即意味着以 HTTPS 启动；缺省则维持明文 HTTP。
#[derive(Debug, Clone, Deserialize)]
pub struct TlsConfig {
    pub cert_path: String,
    pub key_path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_jwt_secret")]
    pub jwt_secret: String,
    #[serde(default = "default_session_ttl")]
    pub session_ttl_hours: u32,
    #[serde(default)]
    pub builtin: BuiltinAuthConfig,
    #[serde(default)]
    pub oauth: OAuthConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuiltinAuthConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_admin_email")]
    pub admin_email: String,
    #[serde(default = "default_admin_password")]
    pub admin_password: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OAuthConfig {
    #[serde(default)]
    pub wechat: Option<WeChatOAuthConfig>,
    #[serde(default)]
    pub google: Option<GoogleOAuthConfig>,
    #[serde(default)]
    pub github: Option<GithubOAuthConfig>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct WeChatOAuthConfig {
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub redirect_uri: String,
}

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

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    /// 本地根目录。承载 tantivy 全文索引（`{data_dir}/search`）与本地附件目录
    ///（`{data_dir}/attachments`）。切到 `search = "meilisearch"` 后端时仍可保留——
    /// 它不再被索引占用，但附件的 local 后端还会用它。
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
    #[serde(default)]
    pub doc: DocConfig,
    #[serde(default)]
    pub blob: BlobConfig,
}

/// 文档与索引元数据的后端。`rocksdb` 是轻量默认（单机、零外部依赖）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocBackend {
    Rocksdb,
    Postgres,
}

/// 附件文件本体的后端。`local` 落 `{data_dir}/attachments`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BlobBackend {
    Local,
    Rustfs,
}

/// 全文检索后端。`tantivy` 是单机嵌入式默认；`meilisearch` 是外部服务，
/// 适合多实例部署共享同一索引。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchBackend {
    Tantivy,
    Meilisearch,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DocConfig {
    /// 用枚举承载而不是字符串：后端名拼错时在解析配置阶段就报错，
    /// 而不是启动到一半、拿到一个空后端才炸。
    #[serde(default = "default_doc_backend")]
    pub backend: DocBackend,
    /// `backend = "postgres"` 时必填，形如 `postgres://user:pass@host:5432/rodeo`。
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BlobConfig {
    #[serde(default = "default_blob_backend")]
    pub backend: BlobBackend,
    /// S3 兼容端点，形如 `http://127.0.0.1:9000`（RustFS / MinIO / 任何 S3 实现）。
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub bucket: String,
    #[serde(default)]
    pub access_key: String,
    #[serde(default)]
    pub secret_key: String,
    #[serde(default = "default_region")]
    pub region: String,
    /// 允许 `http://` 明文端点。内网自建 RustFS 通常没有证书，公网端点应保持 false。
    #[serde(default)]
    pub allow_http: bool,
}

/// 服务端日志配置。同 stdout + 文件双写，文件按天滚动。
/// `[logging]` 整段缺失走默认（dir="logs", prefix="rodeo", keep_days=7）。
#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    /// 日志目录，相对当前工作目录解析。目录不存在会自动创建。
    /// 部署在 systemd 等 CWD 不明确的环境时，建议改成绝对路径
    ///（例如 `/var/log/rodeo`）或在 unit 里设 `WorkingDirectory=`。
    #[serde(default = "default_log_dir")]
    pub dir: String,
    /// 日志文件名前缀，文件名最终为 `{prefix}.YYYY-MM-DD.log`。
    #[serde(default = "default_log_prefix")]
    pub prefix: String,
    /// 历史日志保留天数。启动时删除早于此天数的 `{prefix}.*.log` 文件；
    /// 0 表示不自动清理，由外部 logrotate / cron 处理。
    #[serde(default = "default_log_keep_days")]
    pub keep_days: u32,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            dir: default_log_dir(),
            prefix: default_log_prefix(),
            keep_days: default_log_keep_days(),
        }
    }
}

/// 视图查询相关开关（plan 2026-10-15）。
#[derive(Debug, Clone, Deserialize)]
pub struct QueryConfig {
    /// 强制走 Legacy 路径（load-all + in-memory filter）。cursor 索引出问题时
    /// 临时回滚用；走此模式 20M workspace 下会慢。
    #[serde(default)]
    pub force_legacy_path: bool,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            force_legacy_path: false,
        }
    }
}

fn default_log_dir() -> String {
    "logs".to_string()
}
fn default_log_prefix() -> String {
    "rodeo".to_string()
}
fn default_log_keep_days() -> u32 {
    7
}

/// Tantivy 后端的可调参数。`[search] backend = "tantivy"` 时生效。
#[derive(Debug, Clone, Deserialize)]
pub struct TantivySearchConfig {
    /// 索引目录。留空走默认 `{storage.data_dir}/search`。
    #[serde(default)]
    pub dir: Option<String>,
}

/// Meilisearch 后端的连接参数。`[search] backend = "meilisearch"` 时生效。
#[derive(Debug, Clone, Deserialize)]
pub struct MeilisearchSearchConfig {
    #[serde(default = "default_meili_url")]
    pub url: String,
    /// master key；明文部署（如内网）可留空。
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_meili_index")]
    pub index_name: String,
    /// true：rodeo 侧用 `NgramTokenizer::new(1, 2)` 预分词后空格连接再写入；
    /// CJK 命中行为与 Tantivy 完全一致。false：直接写原文，依赖 Meilisearch 内置分词
    ///（仅适合纯空格分词语言）。默认 true。
    #[serde(default = "default_true")]
    pub pre_segment: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchConfig {
    #[serde(default = "default_search_backend")]
    pub backend: SearchBackend,
    #[serde(default)]
    pub tantivy: TantivySearchConfig,
    #[serde(default)]
    pub meilisearch: MeilisearchSearchConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AiConfig {
    #[serde(default = "default_ai_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub model: String,
    #[serde(default = "default_ai_timeout")]
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AgentConfig {
    pub enabled: bool,
    pub max_turns: usize,
    pub concurrency: usize,
    pub turn_timeout_seconds: u64,
    pub context_truncate_tokens: usize,
    pub max_history_turns: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_turns: 10,
            concurrency: 4,
            turn_timeout_seconds: 60,
            context_truncate_tokens: 16384,
            max_history_turns: 40,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            tls: None,
        }
    }
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: default_jwt_secret(),
            session_ttl_hours: default_session_ttl(),
            builtin: BuiltinAuthConfig::default(),
            oauth: OAuthConfig::default(),
        }
    }
}

impl Default for BuiltinAuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            admin_email: default_admin_email(),
            admin_password: default_admin_password(),
        }
    }
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            doc: DocConfig::default(),
            blob: BlobConfig::default(),
        }
    }
}

impl Default for DocConfig {
    fn default() -> Self {
        Self {
            backend: default_doc_backend(),
            url: String::new(),
        }
    }
}

impl Default for BlobConfig {
    fn default() -> Self {
        Self {
            backend: default_blob_backend(),
            endpoint: String::new(),
            bucket: String::new(),
            access_key: String::new(),
            secret_key: String::new(),
            region: default_region(),
            allow_http: false,
        }
    }
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            base_url: default_ai_base_url(),
            api_key: String::new(),
            model: String::new(),
            timeout_seconds: default_ai_timeout(),
        }
    }
}

impl Default for TantivySearchConfig {
    fn default() -> Self {
        Self { dir: None }
    }
}

impl Default for MeilisearchSearchConfig {
    fn default() -> Self {
        Self {
            url: default_meili_url(),
            api_key: String::new(),
            index_name: default_meili_index(),
            pre_segment: true,
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            backend: default_search_backend(),
            tantivy: TantivySearchConfig::default(),
            meilisearch: MeilisearchSearchConfig::default(),
        }
    }
}

impl AiConfig {
    /// 密钥与模型缺一都发不出可用请求，故两者任一为空即视为「未启用」。
    /// 前端据此收到明确的 `AI_NOT_CONFIGURED`，而不是一个费解的 HTTP 错误。
    pub fn enabled(&self) -> bool {
        !self.api_key.trim().is_empty() && !self.model.trim().is_empty()
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            auth: AuthConfig::default(),
            storage: StorageConfig::default(),
            ai: AiConfig::default(),
            agent: AgentConfig::default(),
            search: SearchConfig::default(),
            logging: LoggingConfig::default(),
            query: QueryConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &str) -> Result<Self, String> {
        let p = Path::new(path);
        if !p.exists() {
            return Ok(Config::default());
        }
        let content = std::fs::read_to_string(p).map_err(|e| format!("读取配置失败: {e}"))?;
        toml::from_str(&content).map_err(|e| format!("解析配置失败: {e}"))
    }

    pub fn data_dir(&self) -> String {
        self.storage.data_dir.clone()
    }

    pub fn tls(&self) -> Option<&TlsConfig> {
        self.server.tls.as_ref()
    }

    /// `[server] host` + `port` 拼出的监听地址。host 必须是字面 IP：SocketAddr 不接受
    /// 主机名，写 `localhost` 会在这里报错告终，而不是静默绑到别处去。
    pub fn bind_addr(&self) -> Result<SocketAddr, String> {
        let raw = format!("{}:{}", self.server.host, self.server.port);
        raw.parse()
            .map_err(|e| format!("[server] host/port 非法（{raw}）: {e}"))
    }
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    3000
}
fn default_jwt_secret() -> String {
    "dev-only-insecure-secret-change-me".to_string()
}
fn default_session_ttl() -> u32 {
    72
}
fn default_true() -> bool {
    true
}
// 自助注册入口已删除（私有化部署要求所有账号由系统管理员创建），配置
// `[auth.builtin] allow_registration` 字段随之移除——历史 config.toml 里若
// 残留此行，TOML 允许未知字段不报错。
fn default_admin_email() -> String {
    "admin@local".to_string()
}
fn default_admin_password() -> String {
    "Admin12345".to_string()
}
fn default_data_dir() -> String {
    "./data".to_string()
}
/// 缺省组合即 spec 里的「rocksdb + 本地文件系统」：老配置一个字不改也能跑。
fn default_doc_backend() -> DocBackend {
    DocBackend::Rocksdb
}
fn default_blob_backend() -> BlobBackend {
    BlobBackend::Local
}
fn default_region() -> String {
    "us-east-1".to_string()
}
fn default_ai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_ai_timeout() -> u64 {
    60
}
fn default_search_backend() -> SearchBackend {
    SearchBackend::Tantivy
}
fn default_meili_url() -> String {
    "http://127.0.0.1:7700".to_string()
}
fn default_meili_index() -> String {
    "entries".to_string()
}
