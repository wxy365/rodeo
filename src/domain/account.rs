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
