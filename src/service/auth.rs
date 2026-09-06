use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::OsRng;
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::config::Config;
use crate::domain::Account;
use crate::error::AppError;
use crate::storage::{cf, DocStore};

#[derive(Debug, Clone, Copy)]
pub struct AuthContext {
    pub account_id: Ulid,
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
    iat: usize,
}

pub struct AuthService {
    store: Arc<DocStore>,
    config: Arc<Config>,
}

impl AuthService {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Self {
        Self { store, config }
    }

    pub fn register(
        &self,
        email: &str,
        name: &str,
        password: &str,
        is_admin: bool,
    ) -> Result<Account, AppError> {
        let email = normalize_email(email)?;
        validate_password(password)?;
        if self
            .store
            .exists(cf::ACCOUNTS_EMAIL_IDX, email.as_bytes())?
        {
            return Err(AppError::EmailExists);
        }
        let password_hash = hash_password(password)?;
        let account = Account::new(email.clone(), name.trim().to_string(), password_hash, is_admin);

        let id_key = account.id.to_bytes();
        self.store.put(cf::ACCOUNTS, &id_key, &account)?;
        self.store
            .put_raw(cf::ACCOUNTS_EMAIL_IDX, email.as_bytes(), &id_key)?;
        Ok(account)
    }

    pub fn login(&self, email: &str, password: &str) -> Result<(Account, String), AppError> {
        let email = normalize_email(email)?;
        let account = self
            .find_by_email(&email)?
            .ok_or(AppError::InvalidCredentials)?;
        if !verify_password(password, &account.password_hash)? {
            return Err(AppError::InvalidCredentials);
        }
        let token = self.sign_token(account.id)?;
        Ok((account, token))
    }

    pub fn find_by_email(&self, email: &str) -> Result<Option<Account>, AppError> {
        let Some(raw) = self.store.get_raw(cf::ACCOUNTS_EMAIL_IDX, email.as_bytes())? else {
            return Ok(None);
        };
        let id_bytes: [u8; 16] = raw
            .try_into()
            .map_err(|_| AppError::Internal("邮箱索引损坏".to_string()))?;
        self.store.get(cf::ACCOUNTS, &id_bytes)
    }

    pub fn find_by_id(&self, id: Ulid) -> Result<Option<Account>, AppError> {
        self.store.get(cf::ACCOUNTS, &id.to_bytes())
    }

    pub fn sign_token(&self, account_id: Ulid) -> Result<String, AppError> {
        let now = Utc::now();
        let exp = now + Duration::hours(self.config.auth.session_ttl_hours as i64);
        let claims = Claims {
            sub: account_id.to_string(),
            exp: exp.timestamp() as usize,
            iat: now.timestamp() as usize,
        };
        let token = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(self.config.auth.jwt_secret.as_bytes()),
        )?;
        Ok(token)
    }

    pub fn verify_token(&self, token: &str) -> Result<AuthContext, AppError> {
        let data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(self.config.auth.jwt_secret.as_bytes()),
            &Validation::default(),
        )?;
        let account_id = Ulid::from_string(&data.claims.sub)
            .map_err(|e| AppError::Internal(format!("JWT 中的账号 ID 无效: {e}")))?;
        Ok(AuthContext { account_id })
    }

    /// 首次启动时若管理员邮箱不存在则创建管理员账号。
    pub fn bootstrap_admin(&self) -> Result<(), AppError> {
        let email = self.config.auth.builtin.admin_email.clone();
        if self.find_by_email(&email)?.is_some() {
            return Ok(());
        }
        self.register(
            &email,
            "Administrator",
            &self.config.auth.builtin.admin_password,
            true,
        )?;
        Ok(())
    }
}

fn normalize_email(email: &str) -> Result<String, AppError> {
    let email = email.trim().to_lowercase();
    if !email.contains('@') || email.len() < 3 {
        return Err(AppError::Internal("邮箱格式无效".to_string()));
    }
    Ok(email)
}

fn validate_password(password: &str) -> Result<(), AppError> {
    let has_upper = password.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = password.chars().any(|c| c.is_ascii_lowercase());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    if password.len() < 8 || !has_upper || !has_lower || !has_digit {
        return Err(AppError::WeakPassword);
    }
    Ok(())
}

fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string();
    Ok(hash)
}

fn verify_password(password: &str, hash: &str) -> Result<bool, AppError> {
    let parsed = PasswordHash::new(hash)?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_strength_rules() {
        assert!(validate_password("Abcd1234").is_ok());
        assert!(validate_password("short1A").is_err()); // too short
        assert!(validate_password("alllowercase1").is_err()); // no upper
        assert!(validate_password("ALLUPPERCASE1").is_err()); // no lower
        assert!(validate_password("NoDigitsHere").is_err()); // no digit
    }

    #[test]
    fn hash_and_verify_roundtrip() {
        let hash = hash_password("Abcd1234").unwrap();
        assert!(verify_password("Abcd1234", &hash).unwrap());
        assert!(!verify_password("WrongPass1", &hash).unwrap());
    }
}
