use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::domain::Query;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub name: String,
    pub query: Query,
    pub sort: SortSpec,
    pub columns: Vec<String>,
    pub is_shared: bool,
    pub owner_id: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 标题颜色规则；按顺序命中即用。
    #[serde(default)]
    pub title_colors: Vec<TitleColorRule>,
}

/// 标题着色规则：条件命中即用对应颜色。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TitleColorRule {
    pub query: Query,
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortSpec {
    pub field: SortField,
    pub desc: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self { field: SortField::UpdatedAt, desc: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortField {
    UpdatedAt,
    CreatedAt,
    Title,
}

impl SortField {
    pub fn as_str(&self) -> &'static str {
        match self {
            SortField::UpdatedAt => "updatedAt",
            SortField::CreatedAt => "createdAt",
            SortField::Title => "title",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "updatedAt" => Some(SortField::UpdatedAt),
            "createdAt" => Some(SortField::CreatedAt),
            "title" => Some(SortField::Title),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use ulid::Ulid;

    #[test]
    fn sort_spec_default_is_updated_desc() {
        let s = SortSpec::default();
        assert_eq!(s.field, SortField::UpdatedAt);
        assert!(s.desc);
    }

    #[test]
    fn sort_field_str_roundtrip() {
        for f in [SortField::UpdatedAt, SortField::CreatedAt, SortField::Title] {
            assert_eq!(SortField::from_str(f.as_str()), Some(f));
        }
        assert_eq!(SortField::from_str("nope"), None);
    }

    #[test]
    fn view_roundtrips_through_bincode() {
        let v = View {
            id: Ulid::new(),
            workspace_id: Ulid::new(),
            name: "全部任务".into(),
            query: Query::all(),
            sort: SortSpec::default(),
            columns: vec!["Task".into(), "Priority".into()],
            is_shared: false,
            owner_id: Ulid::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            title_colors: vec![],
        };
        let bytes = bincode::serialize(&v).unwrap();
        let back: View = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back, v);
    }
}
