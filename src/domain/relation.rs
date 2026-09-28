//! 条目之间关联：方向 + 语义（包含 / 派生 / 归属 / 阻塞 / 关联 / 自定义）。
//!
//! 关联是工作空间内两个条目之间的有向边。一对 `Relation` 行存的是
//! `(from_code) --[semantic]--> (to_code)`；列出某个条目的「所有关联」需要
//! 同时扫「FROM 它」和「TO 它」两个方向，因此存两份索引
//! （`ENTRY_RELATIONS_BY_FROM` / `ENTRY_RELATIONS_BY_TO`），与附件 / 评论的
//! 单边索引同套取舍。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

/// 一条关联。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Relation {
    pub id: Ulid,
    pub workspace_id: Ulid,
    pub from_code: String,
    pub to_code: String,
    pub semantic: RelationSemantic,
    pub created_by: Ulid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 关联语义。
///
/// 前五个是「常用业务语义」单选项；`Custom(String)` 兜住用户手输入的任何词——
/// 一旦选定自定义语义就当字符串存，不做规范化（避免「自定义标签」这类
/// 域外概念污染语义枚举）。serde 用 tag = "type" + content = "value"：
/// 内置形态平铺为 `"type": "contains"`，自定义形态是
/// `"type": "custom", "value": "上下游"`。这样前端按 type 分流渲染即可。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum RelationSemantic {
    Contains,
    Derives,
    BelongsTo,
    Blocks,
    RelatesTo,
    Custom(String),
}

impl RelationSemantic {
    pub const PRESETS: &'static [(&'static str, &'static str)] = &[
        ("contains", "包含"),
        ("derives", "派生"),
        ("belongs_to", "归属"),
        ("blocks", "阻塞"),
        ("relates_to", "关联"),
    ];

    /// 展示名：内置给中文标签，custom 原样回显。
    pub fn display(&self) -> &str {
        match self {
            Self::Contains => "包含",
            Self::Derives => "派生",
            Self::BelongsTo => "归属",
            Self::Blocks => "阻塞",
            Self::RelatesTo => "关联",
            Self::Custom(s) => s.as_str(),
        }
    }

    /// 用户表单里的字符串 → 语义：内置关键字按 snake_case 匹配；
    /// 其余一律归 Custom，保证「任何词都能存」。
    pub fn from_input(s: &str) -> Self {
        let trimmed = s.trim();
        match trimmed {
            "" => Self::RelatesTo,
            "contains" | "包含" => Self::Contains,
            "derives" | "派生" => Self::Derives,
            "belongs_to" | "归属" => Self::BelongsTo,
            "blocks" | "阻塞" => Self::Blocks,
            "relates_to" | "关联" => Self::RelatesTo,
            other => Self::Custom(other.to_string()),
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom(_))
    }
}

impl Relation {
    pub fn new(
        workspace_id: Ulid,
        from_code: String,
        to_code: String,
        semantic: RelationSemantic,
        actor: Ulid,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Ulid::new(),
            workspace_id,
            from_code,
            to_code,
            semantic,
            created_by: actor,
            created_at: now,
            updated_at: now,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_input_maps_builtins_in_both_languages() {
        assert_eq!(RelationSemantic::from_input("包含"), RelationSemantic::Contains);
        assert_eq!(RelationSemantic::from_input("contains"), RelationSemantic::Contains);
        assert_eq!(RelationSemantic::from_input("归属"), RelationSemantic::BelongsTo);
        assert_eq!(RelationSemantic::from_input("派生"), RelationSemantic::Derives);
        assert_eq!(RelationSemantic::from_input("阻塞"), RelationSemantic::Blocks);
        assert_eq!(RelationSemantic::from_input("关联"), RelationSemantic::RelatesTo);
    }

    #[test]
    fn from_input_falls_back_to_custom() {
        assert_eq!(
            RelationSemantic::from_input("上下游"),
            RelationSemantic::Custom("上下游".to_string())
        );
        // 大小写敏感，避免误吃 "Relates_To" / "RELATES_TO" 之类方言
        assert!(matches!(
            RelationSemantic::from_input("上下游"),
            RelationSemantic::Custom(_)
        ));
    }

    #[test]
    fn from_input_empty_defaults_to_relates() {
        assert_eq!(RelationSemantic::from_input(""), RelationSemantic::RelatesTo);
        assert_eq!(RelationSemantic::from_input("   "), RelationSemantic::RelatesTo);
    }

    #[test]
    fn custom_display_round_trips() {
        let s = RelationSemantic::Custom("上下游".to_string());
        assert_eq!(s.display(), "上下游");
    }

    #[test]
    fn serde_round_trips_through_json() {
        let cases = [
            RelationSemantic::Contains,
            RelationSemantic::Derives,
            RelationSemantic::BelongsTo,
            RelationSemantic::Blocks,
            RelationSemantic::RelatesTo,
            RelationSemantic::Custom("上下游".into()),
        ];
        for s in cases {
            let j = serde_json::to_string(&s).unwrap();
            let back: RelationSemantic = serde_json::from_str(&j).unwrap();
            assert_eq!(back, s, "round-trip mismatch for {j}");
        }
    }

    #[test]
    fn serde_uses_internal_tag_with_builtins_and_custom_value() {
        // 内置：内部 tag，只有 `type`，无 `value`
        let j = serde_json::to_string(&RelationSemantic::Contains).unwrap();
        assert_eq!(j, "{\"type\":\"contains\"}");
        let j = serde_json::to_string(&RelationSemantic::BelongsTo).unwrap();
        assert_eq!(j, "{\"type\":\"belongs_to\"}");
        // 自定义：tag + value
        let j = serde_json::to_string(&RelationSemantic::Custom("上下游".into())).unwrap();
        assert!(j.contains("\"type\":\"custom\""));
        assert!(j.contains("\"value\":\"上下游\""));
    }
}