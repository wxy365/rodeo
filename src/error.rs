use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Clone, Serialize, Deserialize)]
pub enum AppError {
    #[error("未授权")]
    Unauthorized,
    #[error("无权限执行此操作")]
    Forbidden,
    #[error("邮箱或密码错误")]
    InvalidCredentials,
    #[error("资源不存在")]
    NotFound,
    #[error("该邮箱已被注册")]
    EmailExists,
    #[error("密码强度不足：最少 8 位，需包含大小写字母和数字")]
    WeakPassword,
    #[error("标签值不合法")]
    InvalidLabelValue,
    #[error("存储错误: {0}")]
    Storage(String),
    #[error("内部错误: {0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Unauthorized => "UNAUTHORIZED",
            AppError::Forbidden => "FORBIDDEN",
            AppError::InvalidCredentials => "INVALID_CREDENTIALS",
            AppError::NotFound => "NOT_FOUND",
            AppError::EmailExists => "EMAIL_EXISTS",
            AppError::WeakPassword => "WEAK_PASSWORD",
            AppError::InvalidLabelValue => "INVALID_LABEL_VALUE",
            AppError::Storage(_) => "STORAGE",
            AppError::Internal(_) => "INTERNAL",
        }
    }
}

impl From<rocksdb::Error> for AppError {
    fn from(e: rocksdb::Error) -> Self {
        AppError::Storage(e.to_string())
    }
}

impl From<bincode::Error> for AppError {
    fn from(e: bincode::Error) -> Self {
        AppError::Internal(format!("序列化错误: {e}"))
    }
}

impl From<argon2::password_hash::Error> for AppError {
    fn from(e: argon2::password_hash::Error) -> Self {
        AppError::Internal(format!("密码哈希错误: {e}"))
    }
}

impl From<jsonwebtoken::errors::Error> for AppError {
    fn from(e: jsonwebtoken::errors::Error) -> Self {
        AppError::Internal(format!("JWT 错误: {e}"))
    }
}
