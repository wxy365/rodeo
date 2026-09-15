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
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
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
fn default_ai_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}
fn default_ai_timeout() -> u64 {
    60
}
