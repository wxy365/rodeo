use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: Ulid,
    pub email: String,
    pub name: String,
    pub password_hash: String,
    pub is_admin: bool,
    pub created_at: DateTime<Utc>,
}

impl Account {
    pub fn new(email: String, name: String, password_hash: String, is_admin: bool) -> Self {
        Self {
            id: Ulid::new(),
            email,
            name,
            password_hash,
            is_admin,
            created_at: Utc::now(),
        }
    }
}

/// 账号状态。**不放进 `Account`** —— 它以 bincode 持久化，加字段会让存量账号记录
/// 解码失败；状态另存 `cf::ACCOUNT_STATUS`，键缺席即 [`AccountStatus::Active`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountStatus {
    Active,
    Frozen,
    Deactivated,
}

impl AccountStatus {
    /// 落库用的字节。`Active` 返回 `None`，调用方据此删键而不是写一个「正常」值
    /// —— 缺席即正常，这样存量账号和新建账号走的是同一条读路径。
    pub fn to_byte(self) -> Option<u8> {
        match self {
            AccountStatus::Active => None,
            AccountStatus::Frozen => Some(1),
            AccountStatus::Deactivated => Some(2),
        }
    }

    /// 读不懂的字节按 `Active` 处理：库里出现意外值时，「能登录」比
    /// 「把所有账号锁在门外」安全。
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => AccountStatus::Frozen,
            2 => AccountStatus::Deactivated,
            _ => AccountStatus::Active,
        }
    }

    /// GraphQL 对外的那套字符串。
    pub fn as_str(self) -> &'static str {
        match self {
            AccountStatus::Active => "active",
            AccountStatus::Frozen => "frozen",
            AccountStatus::Deactivated => "deactivated",
        }
    }

    /// 解析 GraphQL 传进来的状态串。不叫 `from_str` 是刻意的：那个名字会让人以为
    /// 它实现了 `std::str::FromStr`（也该触发 clippy 的 `should_implement_trait`）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "active" => Some(AccountStatus::Active),
            "frozen" => Some(AccountStatus::Frozen),
            "deactivated" => Some(AccountStatus::Deactivated),
            _ => None,
        }
    }
}
