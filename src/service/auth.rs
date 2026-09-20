use std::sync::Arc;

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::{OsRng, RngCore};
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
    /// 签发时的令牌版本。旧令牌没有此字段，default 0 与「CF 中无记录」一致，
    /// 因此升级前签发的令牌在首次登出前仍然有效。
    #[serde(default)]
    ver: u64,
}

pub struct AuthService {
    store: Arc<DocStore>,
    config: Arc<Config>,
}

impl AuthService {
    pub fn new(store: Arc<DocStore>, config: Arc<Config>) -> Self {
        Self { store, config }
    }

    /// 自助注册。受 `allow_registration` 限制——这个开关管的是「陌生人能否自己注册」，
    /// 与「管理员能否建号」是两件事，后者见 [`Self::create_by_admin`]。
    pub fn register(&self, email: &str, name: &str, password: &str) -> Result<Account, AppError> {
        if !self.config.auth.builtin.allow_registration {
            return Err(AppError::InvalidQuery(
                "已关闭开放注册，请联系系统管理员创建账号".to_string(),
            ));
        }
        self.create(email, name, password, false)
    }

    /// 管理员建号：**不受** `allow_registration` 限制。调用方负责鉴权
    /// （GraphQL 侧走 `require_admin`；`bootstrap_admin` 是启动期的引导路径）。
    ///
    /// 旧签名让这两种调用者共用一个 `is_admin` 参数，结果关闭自助注册后管理员连
    /// 普通账号都建不出来。开关与建号权限从此各管各的。
    pub fn create_by_admin(
        &self,
        email: &str,
        name: &str,
        password: &str,
        is_admin: bool,
    ) -> Result<Account, AppError> {
        self.create(email, name, password, is_admin)
    }

    fn create(
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
            ver: self.token_version(account_id)?,
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
        // 版本不符 = 该令牌已被登出吊销。
        if data.claims.ver != self.token_version(account_id)? {
            return Err(AppError::Unauthorized);
        }
        Ok(AuthContext { account_id })
    }

    /// 当前令牌版本；CF 中无记录视为 0。
    fn token_version(&self, account_id: Ulid) -> Result<u64, AppError> {
        Ok(self
            .store
            .get_raw(cf::ACCOUNT_TOKEN_VERSION, &account_id.to_bytes())?
            .and_then(|raw| raw.try_into().ok().map(u64::from_be_bytes))
            .unwrap_or(0))
    }

    /// 吊销该账号所有已签发的令牌（登出即全部设备失效），并推进版本号。
    pub fn revoke_tokens(&self, account_id: Ulid) -> Result<(), AppError> {
        let next = self.token_version(account_id)?.wrapping_add(1);
        self.store.put_raw(
            cf::ACCOUNT_TOKEN_VERSION,
            &account_id.to_bytes(),
            &next.to_be_bytes(),
        )
    }

    /// 首次启动时若管理员邮箱不存在则创建管理员账号。
    pub fn bootstrap_admin(&self) -> Result<(), AppError> {
        let email = self.config.auth.builtin.admin_email.clone();
        if self.find_by_email(&email)?.is_some() {
            return Ok(());
        }
        self.create_by_admin(
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

/// 初始密码的字母表。刻意去掉 `I l O 0 1` 这些易混字符——它是要被管理员抄写或口述
/// 分发出去的，不是给程序读的。
const PW_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
const PW_UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
const PW_LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
const PW_DIGIT: &[u8] = b"23456789";

/// 生成初始密码：16 位，且**构造上**保证含大写、小写、数字各至少一个
/// （先按字母表取满 16 位，再让前三位分别取自三个类别，最后把整体打乱）。
///
/// 为什么不是「随机取满再拿 `validate_password` 校验、不合格就重摇」：那样有极小概率
/// 摇出一个被自己拒绝的密码，管理员会收到一条莫名其妙的「密码强度不足」。让它在构造上
/// 不可能发生，比摇完再补救干净。
///
/// 熵：13 位取自 57 字符表 + 3 位受类别约束，约 88 bit。够用。
pub fn generate_initial_password() -> String {
    let mut rng = OsRng;
    let mut buf = [0u8; 16];
    for b in buf.iter_mut() {
        *b = PW_ALPHABET[pick(&mut rng, PW_ALPHABET.len())];
    }
    buf[0] = PW_UPPER[pick(&mut rng, PW_UPPER.len())];
    buf[1] = PW_LOWER[pick(&mut rng, PW_LOWER.len())];
    buf[2] = PW_DIGIT[pick(&mut rng, PW_DIGIT.len())];
    // 打散上面那三个固定位置，免得「第 1 位永远是大写」成为可预判的结构。
    for i in (1..buf.len()).rev() {
        let j = pick(&mut rng, i + 1);
        buf.swap(i, j);
    }
    String::from_utf8(buf.to_vec()).expect("字母表全是 ASCII")
}

/// `OsRng` 取一个 `[0, n)` 的下标。`n` 很小（≤ 57），取模偏差在 2^32 量级下可忽略。
fn pick(rng: &mut impl RngCore, n: usize) -> usize {
    (rng.next_u32() as usize) % n
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

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-auth-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn registration_respects_allow_registration_flag() {
        let dir = temp_dir("closed");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let mut cfg = Config::default();
        cfg.auth.builtin.allow_registration = false;
        let auth = AuthService::new(store.clone(), Arc::new(cfg));

        assert!(auth.register("user@x.io", "User", "Passw0rd!").is_err());
        // 管理员建号不受开关限制，否则关闭注册后连管理员都建不出来。
        assert!(auth.create_by_admin("admin@x.io", "Admin", "Passw0rd!", true).is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn registration_open_by_default() {
        let dir = temp_dir("open");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(Config::default()));

        assert!(auth.register("user@x.io", "User", "Passw0rd!").is_ok());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn logout_revokes_issued_tokens_and_new_login_works() {
        let dir = temp_dir("logout");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store, Arc::new(Config::default()));
        auth.register("u@x.io", "U", "Passw0rd!").unwrap();
        let id = auth.find_by_email("u@x.io").unwrap().unwrap().id;

        let (_, token) = auth.login("u@x.io", "Passw0rd!").unwrap();
        assert!(auth.verify_token(&token).is_ok());

        // Bump 版本 = 登出所有设备：旧令牌立刻失效。
        auth.revoke_tokens(id).unwrap();
        assert!(auth.verify_token(&token).is_err());

        // 重新登录签发的令牌带上新版本，继续可用。
        let (_, fresh) = auth.login("u@x.io", "Passw0rd!").unwrap();
        assert!(auth.verify_token(&fresh).is_ok());
        assert!(auth.verify_token(&token).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

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
