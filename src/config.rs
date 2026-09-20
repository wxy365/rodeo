use std::path::Path;

use serde::Deserialize;

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
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_base_url")]
    pub base_url: String,
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
}

#[derive(Debug, Clone, Deserialize)]
pub struct BuiltinAuthConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub allow_registration: bool,
    #[serde(default = "default_admin_email")]
    pub admin_email: String,
    #[serde(default = "default_admin_password")]
    pub admin_password: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    /// 本地根目录。即使文档与附件都改用外部后端，它仍承载 tantivy 全文索引
    /// （`{data_dir}/search`），所以四种组合下都必需。
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

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            base_url: default_base_url(),
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
        }
    }
}

impl Default for BuiltinAuthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_registration: true,
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
        Self { backend: default_doc_backend(), url: String::new() }
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
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    3000
}
fn default_base_url() -> String {
    "http://localhost:3000".to_string()
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
