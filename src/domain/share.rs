use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// 一次性匿名只读分享链接：share.id 自身即 token（见 plan/决策 A），
/// 是公开 URL `/share/{id}` 的 capability。撤销走软删除 `revoked = true`，
/// 与 `ACCOUNT_STATUS` 同款「单值排除无意义状态」取舍；公开解析
/// 时 `revoked = true` 一律返回 `NotFound`，免得成为探测口（与登录路径
/// `auth.rs` 的多命中歧义同源安全策略一致）。
///
/// bincode 是位置编码：本结构体加新字段必须只能追加在末尾，否则存量记录
/// 反序列化会读到 EOF。撤销字段（`revoked` / `revoked_by` / `revoked_at`）
/// 已经在「撤销」这个状态被构造出来之前就预留好位，新装零迁移。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EntryShare {
    /// 即 token；与 `cf::ENTRY_SHARES` 的键同源——这就是公开 URL 的能力。
    pub id: Ulid,
    /// 冗余存：撤销时不再回查 Entry。Entry 软删会让 share 指向不存在的
    /// 条目，所以 resolve 时再校验一次。
    pub workspace_id: Ulid,
    /// 冗余存：同上。
    pub entry_code: String,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
    /// 单值软撤销标志：`true` 即被撤销，对外一律 `NotFound`。
    pub revoked: bool,
    pub revoked_by: Option<Ulid>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl EntryShare {
    pub fn new(workspace_id: Ulid, entry_code: String, created_by: Ulid) -> Self {
        Self {
            id: Ulid::new(),
            workspace_id,
            entry_code,
            created_by,
            created_at: Utc::now(),
            revoked: false,
            revoked_by: None,
            revoked_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_share_is_unrevoked_and_fresh() {
        let ws = Ulid::new();
        let by = Ulid::new();
        let s = EntryShare::new(ws, "CODE0000000000001".to_string(), by);
        assert!(!s.id.is_nil());
        assert!(!s.revoked);
        assert!(s.revoked_by.is_none());
        assert!(s.revoked_at.is_none());
        assert_eq!(s.workspace_id, ws);
        assert_eq!(s.entry_code, "CODE0000000000001");
        assert_eq!(s.created_by, by);
    }

    #[test]
    fn share_roundtrips_through_bincode() {
        let s = EntryShare::new(Ulid::new(), "CODE0000000000002".to_string(), Ulid::new());
        let bytes = bincode::serialize(&s).unwrap();
        let back: EntryShare = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, s);
    }
}
