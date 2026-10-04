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

    /// 解绑。spec §9「唯一登录方式」守卫：OAuth-only 账号不能解绑唯一的第三方绑定，
    /// 必须先设密码或绑别的 provider，否则账号登不回去。
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

        let account = self
            .store
            .get::<crate::domain::Account>(cf::ACCOUNTS, &account_id.to_bytes())?
            .ok_or(AppError::NotFound)?;
        if !account.has_password() {
            // 「唯一登录方式」守卫：排除正在解绑的那一条，统计该账号其余 binding 数；
            // 0 才拒。brief 用 `find_by_account_and_provider(.., provider)?.is_none()`
            // 会恒为 false（同一 provider 下的当前 binding 必然存在），守卫永不触发——
            // 改用全 CF 扫 + 外部 id 排除。
            let mut other_count: usize = 0;
            for (_k, v) in self.store.scan_prefix(cf::OAUTH_BINDINGS, b"")? {
                let b: IdentityBinding = bincode::deserialize(&v)?;
                if b.account_id == account_id && b.external_id != external_id {
                    other_count += 1;
                }
            }
            if other_count == 0 {
                return Err(AppError::InvalidQuery(
                    "请先设置密码或绑定其他第三方账号再解绑".to_string(),
                ));
            }
        }

        self.store
            .delete(cf::OAUTH_BINDINGS, &Self::key(provider, external_id))?;
        Ok(())
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

    /// 通过 OAuth 创建 Rodeo 账号 + 同时写入绑定关系。
    ///
    /// 原子写：`ACCOUNTS + ACCOUNTS_EMAIL_IDX + OAUTH_BINDINGS` 三张 CF
    /// 在同一个 `write_batch` 里。要么都成、要么都不写——避免
    /// `take_bind` 已消费、但 binding 落库失败时留下孤儿账号。
    ///
    /// - `password.is_none()` 时 password_hash 留空串（OAuth-only 无密码）。
    /// - 与 `AuthService::register` 共用 `allow_registration` 守门规则，但调用方负责先查 config。
    pub fn create_account_and_bind_oauth(
        &self,
        email: &str,
        name: &str,
        password: Option<&str>,
        provider: crate::domain::OAuthProvider,
        external_id: &str,
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
        let now = chrono::Utc::now();
        let binding = crate::domain::IdentityBinding {
            account_id: account.id,
            provider,
            external_id: external_id.to_string(),
            email: None,
            display_name: None,
            bound_at: now,
            last_used_at: now,
        };

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
            BatchOp::put(
                crate::storage::cf::OAUTH_BINDINGS,
                Self::key(provider, external_id),
                &binding,
            )?,
        ])?;
        Ok(account)
    }
}
