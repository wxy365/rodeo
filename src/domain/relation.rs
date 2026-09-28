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

/// 内置语义的稳定 slug。`Custom` 不带数据，词面走 `RelationSemantic.value`。
///
/// 拆 enum + struct 而不是把 `Custom(String)` 做成 enum 变体：bincode 无法
/// 编码「internally tagged enum + data variant」，会抛
/// `Bincode does not support Deserializer::deserialize_identifier`。结构体形式
/// 两个字段固定可预测，bincode / postcard / 后端存储都干净。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SemanticKind {
    Contains,
    Derives,
    BelongsTo,
    Blocks,
    RelatesTo,
    Custom,
}

/// 关联语义：`kind` 是稳定 slug，`value` 仅自定义语义带值。
///
/// JSON 形态故意做成平铺：
/// - 内置：`{"kind":"contains","display":"包含","value":null}`
/// - 自定义：`{"kind":"custom","display":"上下游","value":"上下游"}`
///
/// **不要** 给 `value` 加 `skip_serializing_if = "Option::is_none"`——bincode
/// 写时跳过 None，读时却依然按字段顺序读一个 varint tag，builtins 反序列化
/// 会以 `Io(Kind(UnexpectedEof))` 失败（写 4 字节、读 5 字节，对不上）。
/// 留着 None 走满 5 字节是稳的；bincode 里冗余 tag 比漏写更安全。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationSemantic {
    pub kind: SemanticKind,
    #[serde(default)]
    pub value: Option<String>,
}

impl RelationSemantic {
    pub const PRESETS: &'static [(&'static str, &'static str)] = &[
        ("contains", "包含"),
        ("derives", "派生"),
        ("belongs_to", "归属"),
        ("blocks", "阻塞"),
        ("relates_to", "关联"),
    ];

    /// 展示名：内置给中文标签，custom 原样回显 `value`。
    pub fn display(&self) -> &str {
        match (&self.kind, self.value.as_deref()) {
            (SemanticKind::Contains, _) => "包含",
            (SemanticKind::Derives, _) => "派生",
            (SemanticKind::BelongsTo, _) => "归属",
            (SemanticKind::Blocks, _) => "阻塞",
            (SemanticKind::RelatesTo, _) => "关联",
            (SemanticKind::Custom, Some(v)) => v,
            (SemanticKind::Custom, None) => "",
        }
    }

    /// 用户表单里的字符串 → 语义：内置关键字按 snake_case 匹配；
    /// 其余一律归 Custom，保证「任何词都能存」。
    pub fn from_input(s: &str) -> Self {
        let trimmed = s.trim();
        match trimmed {
            "" => Self::builtin(SemanticKind::RelatesTo),
            "contains" | "包含" => Self::builtin(SemanticKind::Contains),
            "derives" | "派生" => Self::builtin(SemanticKind::Derives),
            "belongs_to" | "归属" => Self::builtin(SemanticKind::BelongsTo),
            "blocks" | "阻塞" => Self::builtin(SemanticKind::Blocks),
            "relates_to" | "关联" => Self::builtin(SemanticKind::RelatesTo),
            other => Self {
                kind: SemanticKind::Custom,
                value: Some(other.to_string()),
            },
        }
    }

    pub(crate) fn builtin(kind: SemanticKind) -> Self {
        Self { kind, value: None }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self.kind, SemanticKind::Custom)
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

    fn contains() -> RelationSemantic {
        RelationSemantic::builtin(SemanticKind::Contains)
    }
    fn custom(s: &str) -> RelationSemantic {
        RelationSemantic {
            kind: SemanticKind::Custom,
            value: Some(s.to_string()),
        }
    }

    #[test]
    fn from_input_maps_builtins_in_both_languages() {
        assert_eq!(RelationSemantic::from_input("包含"), contains());
        assert_eq!(RelationSemantic::from_input("contains"), contains());
        assert_eq!(
            RelationSemantic::from_input("归属"),
            RelationSemantic::builtin(SemanticKind::BelongsTo)
        );
        assert_eq!(
            RelationSemantic::from_input("派生"),
            RelationSemantic::builtin(SemanticKind::Derives)
        );
        assert_eq!(
            RelationSemantic::from_input("阻塞"),
            RelationSemantic::builtin(SemanticKind::Blocks)
        );
        assert_eq!(
            RelationSemantic::from_input("关联"),
            RelationSemantic::builtin(SemanticKind::RelatesTo)
        );
    }

    #[test]
    fn from_input_falls_back_to_custom() {
        assert_eq!(RelationSemantic::from_input("上下游"), custom("上下游"));
        // 大小写敏感，避免误吃 "Relates_To" / "RELATES_TO" 之类方言
        assert!(RelationSemantic::from_input("上下游").is_custom());
    }

    #[test]
    fn from_input_empty_defaults_to_relates() {
        assert_eq!(
            RelationSemantic::from_input(""),
            RelationSemantic::builtin(SemanticKind::RelatesTo)
        );
        assert_eq!(
            RelationSemantic::from_input("   "),
            RelationSemantic::builtin(SemanticKind::RelatesTo)
        );
    }

    #[test]
    fn custom_display_round_trips() {
        let s = custom("上下游");
        assert_eq!(s.display(), "上下游");
    }

    #[test]
    fn builtin_display_chinese() {
        assert_eq!(contains().display(), "包含");
        assert_eq!(
            RelationSemantic::builtin(SemanticKind::BelongsTo).display(),
            "归属"
        );
    }

    #[test]
    fn serde_round_trips_through_json() {
        let cases = [
            contains(),
            RelationSemantic::builtin(SemanticKind::Derives),
            RelationSemantic::builtin(SemanticKind::BelongsTo),
            RelationSemantic::builtin(SemanticKind::Blocks),
            RelationSemantic::builtin(SemanticKind::RelatesTo),
            custom("上下游"),
        ];
        for s in cases {
            let j = serde_json::to_string(&s).unwrap();
            let back: RelationSemantic = serde_json::from_str(&j).unwrap();
            assert_eq!(back, s, "round-trip mismatch for {j}");
        }
    }

    #[test]
    fn serde_keeps_value_field_for_builtins() {
        // 内置语义 value 是 None，但 bincode 不能 skip_serializing_if（读端仍按
        // 字段顺序读 varint tag，skip 后写少读多会 Io(UnexpectedEof)）。JSON
        // 也保持 value 字段，避免双编码器走两条路——稳定性优先。
        let j = serde_json::to_string(&contains()).unwrap();
        assert!(j.contains("\"kind\":\"contains\""), "{j}");
        assert!(j.contains("\"value\":null"), "{j}");
    }

    #[test]
    fn serde_keeps_value_field_for_custom() {
        let j = serde_json::to_string(&custom("上下游")).unwrap();
        assert!(j.contains("\"kind\":\"custom\""), "{j}");
        assert!(j.contains("\"value\":\"上下游\""), "{j}");
    }

    #[test]
    fn bincode_round_trips_through_relation_record() {
        // 直接走 Relation 整条记录的 bincode 编解码——list_for_entry 是这条路，
        // 不应该出现任何 InvalidTagEncoding。
        let rel = Relation {
            id: Ulid::new(),
            workspace_id: Ulid::new(),
            from_code: "AAA".into(),
            to_code: "BBB".into(),
            semantic: RelationSemantic::from_input("包含"),
            created_by: Ulid::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let bytes = bincode::serialize(&rel).unwrap();
        let back: Relation = bincode::deserialize(&bytes).unwrap();
        assert_eq!(back.from_code, "AAA");
        assert_eq!(back.semantic.kind, SemanticKind::Contains);

        let rel2 = Relation {
            semantic: custom("上下游"),
            ..rel.clone()
        };
        let bytes2 = bincode::serialize(&rel2).unwrap();
        let back2: Relation = bincode::deserialize(&bytes2).unwrap();
        assert_eq!(back2.semantic.kind, SemanticKind::Custom);
        assert_eq!(back2.semantic.value.as_deref(), Some("上下游"));
    }
}