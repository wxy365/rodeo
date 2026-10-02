//! 写读 `cf::OAUTH_BINDINGS` 的薄封装。
//! 主键 `(provider_byte, external_id)`；按账号查用全表扫后过滤，账号量级下足够。

use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{IdentityBinding, OAuthProvider};
use crate::error::AppError;
use crate::storage::{cf, DocStore};

pub struct OAuthBindingsService {
    pub store: Arc<DocStore>,
}

impl OAuthBindingsService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    fn key(provider: OAuthProvider, external_id: &str) -> Vec<u8> {
        let mut k = Vec::with_capacity(2 + external_id.len());
        k.push(provider.as_byte());
        k.push(b'/');
        k.extend_from_slice(external_id.as_bytes());
        k
    }

    pub fn find(
        &self,
        provider: OAuthProvider,
        external_id: &str,
    ) -> Result<Option<IdentityBinding>, AppError> {
        self.store
            .get(cf::OAUTH_BINDINGS, &Self::key(provider, external_id))
    }

    /// 找「该账号已绑此 provider」的记录。O(N) —— 扫一遍全 CF，过滤后取第一条匹配。
    /// 用法：bind_to_existing 校验「同 provider 不同 external_id」冲突。
    pub fn find_by_account_and_provider(
        &self,
        account_id: Ulid,
        provider: OAuthProvider,
    ) -> Result<Option<IdentityBinding>, AppError> {
        let prefix = vec![provider.as_byte(), b'/'];
        for (_k, v) in self.store.scan_prefix(cf::OAUTH_BINDINGS, &prefix)? {
            let b: IdentityBinding = bincode::deserialize(&v)?;
            if b.account_id == account_id {
                return Ok(Some(b));
            }
        }
        Ok(None)
    }

    /// 写或更新绑定（已有同 `(provider, external_id)` 时只刷 `last_used_at`）。
    pub fn upsert(
        &self,
        account_id: Ulid,
        provider: OAuthProvider,
        external_id: &str,
        email: Option<String>,
        display_name: Option<String>,
    ) -> Result<(), AppError> {
        let now = Utc::now();
        let binding = match self.find(provider, external_id)? {
            Some(mut b) => {
                b.last_used_at = now;
                b
            }
            None => IdentityBinding {
                account_id,
                provider,
                external_id: external_id.to_string(),
                email,
                display_name,
                bound_at: now,
                last_used_at: now,
            },
        };
        self.store.put(
            cf::OAUTH_BINDINGS,
            &Self::key(provider, external_id),
            &binding,
        )?;
        Ok(())
    }

    /// 通过 OAuth 创建 Rodeo 账号。
    /// - `password.is_none()` 时 password_hash 留空串（OAuth-only 无密码）。
    /// - 与 `AuthService::register` 共用 `allow_registration` 守门规则，但调用方负责先查 config。
    /// - 原子写：ACCOUNTS + ACCOUNTS_EMAIL_IDX。binding 由 GraphQL mutation 在外层写。
    pub fn create_account_via_oauth(
        &self,
        email: &str,
        name: &str,
        password: Option<&str>,
    ) -> Result<crate::domain::Account, AppError> {
        use crate::domain::Account;
        use crate::service::auth::{hash_password, normalize_email, validate_password};
        use crate::storage::BatchOp;

        let email = normalize_email(email)?;
        let password_hash = match password {
            Some(p) => {
                validate_password(p)?;
                hash_password(p)?
            }
            None => String::new(),
        };
        let account = Account::new(email.clone(), name.trim().to_string(), password_hash, false);

        self.store.write_batch(vec![
            BatchOp::put(
                crate::storage::cf::ACCOUNTS,
                account.id.to_bytes().to_vec(),
                &account,
            )?,
            BatchOp::put_raw(
                crate::storage::cf::ACCOUNTS_EMAIL_IDX,
                email.as_bytes().to_vec(),
                account.id.to_bytes().to_vec(),
            ),
        ])?;
        Ok(account)
    }
}
