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

/// 时间轴配置：把**同族**的两个时间型标签当作 Entry 的起止时间，可选一个账号型标签作相关人。
/// 落在独立列族 `VIEW_TIMELINE` 而不是 `View` 的字段上，理由见 `storage::doc::cf::VIEW_TIMELINE`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewTimeline {
    /// 起始时间标签名（`LabelSchema.name`，与 `View::columns` 同一套标识）。
    pub start: String,
    /// 结束时间标签名；与 `start` 同族（都含日期，或都是纯时刻）。
    pub end: String,
    /// 相关人标签名（Account 型）；`None` 表示不展示相关人。
    pub person: Option<String>,
}

/// 一个排序键。`SortSpec` 按顺序依次比较，前一个不定的才看后一个。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortKey {
    pub field: SortField,
    pub desc: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortSpec {
    pub keys: Vec<SortKey>,
}

impl Default for SortSpec {
    fn default() -> Self {
        Self { keys: vec![SortKey { field: SortField::UpdatedAt, desc: true }] }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortField {
    UpdatedAt,
    CreatedAt,
    Title,
    /// 创建人账号（按创建人显示名排序；账号已删除时落到 id 上，保证次序稳定）。
    CreatedBy,
    /// 更新人账号（按更新人显示名排序；同上）。
    UpdatedBy,
    /// 标签名排序。载荷是标签名（`LabelSchema.name`），与 `View::columns` 同一套标识。
    /// 因此 `SortField` 不再是 `Copy`。
    Label(String),
}

impl SortField {
    pub fn as_str(&self) -> &str {
        match self {
            SortField::UpdatedAt => "updatedAt",
            SortField::CreatedAt => "createdAt",
            SortField::Title => "title",
            SortField::CreatedBy => "createdBy",
            SortField::UpdatedBy => "updatedBy",
            SortField::Label(name) => name,
        }
    }

    /// 只认内置值。标签名由 GraphQL 层在解析入参时单独构造 `Label(..)`——
    /// 这样 `title` / `createdBy` / `updatedBy` 恒按内置解释，不会被同名标签抢走。
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "updatedAt" => Some(SortField::UpdatedAt),
            "createdAt" => Some(SortField::CreatedAt),
            "title" => Some(SortField::Title),
            "createdBy" => Some(SortField::CreatedBy),
            "updatedBy" => Some(SortField::UpdatedBy),
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
        assert_eq!(s.keys.len(), 1);
        assert_eq!(s.keys[0].field, SortField::UpdatedAt);
        assert!(s.keys[0].desc);
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
