use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::domain::Query;

/// 视图以 bincode 落库，而条件里的 `serde_json::Value` 反序列化需要 `deserialize_any`，
/// bincode 不支持——于是「带值的条件」写得进去、读不出来。
/// 这里把内联的查询 AST 先编码成 JSON 字符串再交给 bincode：字符串两边都支持。
pub(crate) mod query_json {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T, S>(value: &T, ser: S) -> Result<S::Ok, S::Error>
    where
        T: Serialize,
        S: Serializer,
    {
        let s = serde_json::to_string(value).map_err(serde::ser::Error::custom)?;
        ser.serialize_str(&s)
    }

    pub fn deserialize<'de, T, D>(de: D) -> Result<T, D::Error>
    where
        T: serde::de::DeserializeOwned,
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        serde_json::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub name: String,
    #[serde(with = "query_json")]
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
    #[serde(with = "query_json")]
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
            name: "全部内容".into(),
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

    #[test]
    fn view_with_conditions_roundtrips_through_bincode() {
        use crate::domain::{Condition, Field, Op};
        // present（无值）与带值条件都要能过 bincode：后者曾是线上保存视图的报错来源。
        let cases: [(&str, Query); 3] = [
            ("present/no-value", Query::Cond(Condition {
                field: Field::Label("Task".into()),
                op: Op::Present,
                value: None,
            })),
            ("label/eq-value", Query::Cond(Condition {
                field: Field::Label("Task".into()),
                op: Op::Eq,
                value: Some(serde_json::json!("Open")),
            })),
            ("text/contains", Query::Cond(Condition {
                field: Field::Text,
                op: Op::Contains,
                value: Some(serde_json::json!("检索词")),
            })),
        ];
        for (label, query) in cases {
            let v = View {
                id: Ulid::new(),
                workspace_id: Ulid::new(),
                name: "视图".into(),
                query: query.clone(),
                sort: SortSpec::default(),
                columns: vec![],
                is_shared: false,
                owner_id: Ulid::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
                title_colors: vec![],
            };
            let bytes = bincode::serialize(&v).unwrap();
            let back: View = bincode::deserialize(&bytes).expect(label);
            assert_eq!(back.query, query, "{label}");
        }
    }
}
