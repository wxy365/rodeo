use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ulid::Ulid;

use crate::error::AppError;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum LabelValueType {
    Null,
    Boolean,
    Integer,
    Float,
    String,
    Enum,
    // 新增：只能追加在末尾（bincode 位置编码）
    Date,
    Time,
    DateTime,
    Currency,
    Email,
    // 同上，只能追加
    Account,
}

impl LabelValueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            LabelValueType::Null => "null",
            LabelValueType::Boolean => "boolean",
            LabelValueType::Integer => "integer",
            LabelValueType::Float => "float",
            LabelValueType::String => "string",
            LabelValueType::Enum => "enum",
            LabelValueType::Date => "date",
            LabelValueType::Time => "time",
            LabelValueType::DateTime => "datetime",
            LabelValueType::Currency => "currency",
            LabelValueType::Email => "email",
            LabelValueType::Account => "account",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "null" => Some(LabelValueType::Null),
            "boolean" => Some(LabelValueType::Boolean),
            "integer" => Some(LabelValueType::Integer),
            "float" => Some(LabelValueType::Float),
            "string" => Some(LabelValueType::String),
            "enum" => Some(LabelValueType::Enum),
            "date" => Some(LabelValueType::Date),
            "time" => Some(LabelValueType::Time),
            "datetime" => Some(LabelValueType::DateTime),
            "currency" => Some(LabelValueType::Currency),
            "email" => Some(LabelValueType::Email),
            "account" => Some(LabelValueType::Account),
            _ => None,
        }
    }
}

/// 标签值，对应 LabelSchema.value_type。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LabelValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Enum(String),
    // 追加，勿插入到上方
    EnumList(Vec<String>),
    Date(String),
    Time(String),
    DateTime(String),
    Currency(f64),
    Email(String),
    // 追加，勿插入到上方
    Account(String),
    // 追加，勿插入到上方：多选账号（schema.multi && Account）。
    AccountList(Vec<String>),
}

impl LabelValue {
    /// 将 GraphQL 传入的 JSON 值按 schema 的 value_type 校验并转换为 LabelValue。
    pub fn from_json(value: &serde_json::Value, schema: &LabelSchema) -> Result<Self, AppError> {
        match schema.value_type {
            LabelValueType::Null => Ok(LabelValue::Null),
            LabelValueType::Boolean => value
                .as_bool()
                .map(LabelValue::Bool)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::Integer => value
                .as_i64()
                .map(LabelValue::Int)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::Float => value
                .as_f64()
                .map(LabelValue::Float)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::String => {
                // 多值场景：JSON 数组序列化为字符串存储。
                if let Some(arr) = value.as_array() {
                    let s = serde_json::to_string(arr)
                        .map_err(|e| AppError::Internal(e.to_string()))?;
                    Ok(LabelValue::String(s))
                } else {
                    value
                        .as_str()
                        .map(|s| LabelValue::String(s.to_string()))
                        .ok_or(AppError::InvalidLabelValue)
                }
            }
            LabelValueType::Enum => {
                if schema.multi {
                    let vals = match value {
                        serde_json::Value::Array(a) => a
                            .iter()
                            .map(|x| x.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                            .ok_or(AppError::InvalidLabelValue)?,
                        serde_json::Value::String(s) => vec![s.clone()],
                        _ => return Err(AppError::InvalidLabelValue),
                    };
                    if vals
                        .iter()
                        .any(|v| !schema.enum_values.iter().any(|e| e == v))
                    {
                        return Err(AppError::InvalidLabelValue);
                    }
                    Ok(LabelValue::EnumList(vals))
                } else {
                    let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                    if !schema.enum_values.iter().any(|v| v == s) {
                        return Err(AppError::InvalidLabelValue);
                    }
                    Ok(LabelValue::Enum(s.to_string()))
                }
            }
            LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime => {
                let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                let layout = resolve_layout(schema.format.as_deref(), schema.value_type);
                crate::golayout::parse(&layout, s).ok_or(AppError::InvalidLabelValue)?;
                Ok(match schema.value_type {
                    LabelValueType::Date => LabelValue::Date(s.to_string()),
                    LabelValueType::Time => LabelValue::Time(s.to_string()),
                    _ => LabelValue::DateTime(s.to_string()),
                })
            }
            LabelValueType::Currency => value
                .as_f64()
                .map(LabelValue::Currency)
                .ok_or(AppError::InvalidLabelValue),
            LabelValueType::Email => {
                let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                let mut parts = s.split('@');
                let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next())
                else {
                    return Err(AppError::InvalidLabelValue);
                };
                if local.is_empty() || domain.is_empty() {
                    return Err(AppError::InvalidLabelValue);
                }
                Ok(LabelValue::Email(s.to_string()))
            }
            // 只校验「是个账号 id」；「是不是本工作空间成员」需要读成员表，由服务层补。
            LabelValueType::Account => {
                if schema.multi {
                    let vals: Vec<String> = match value {
                        serde_json::Value::Array(a) => a
                            .iter()
                            .map(|x| x.as_str().map(str::to_string))
                            .collect::<Option<Vec<_>>>()
                            .ok_or(AppError::InvalidLabelValue)?,
                        // 容错：单串包装成单元素数组。
                        serde_json::Value::String(s) => vec![s.clone()],
                        _ => return Err(AppError::InvalidLabelValue),
                    };
                    for v in &vals {
                        Ulid::from_string(v).map_err(|_| AppError::InvalidLabelValue)?;
                    }
                    Ok(LabelValue::AccountList(vals))
                } else {
                    let s = value.as_str().ok_or(AppError::InvalidLabelValue)?;
                    Ulid::from_string(s).map_err(|_| AppError::InvalidLabelValue)?;
                    Ok(LabelValue::Account(s.to_string()))
                }
            }
        }
    }

    /// 转为 JSON 值，用于 GraphQL 输出。
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            LabelValue::Null => serde_json::Value::Null,
            LabelValue::Bool(b) => serde_json::Value::Bool(*b),
            LabelValue::Int(i) => serde_json::Value::Number((*i).into()),
            LabelValue::Float(f) => serde_json::json!(f),
            LabelValue::String(s) => serde_json::Value::String(s.clone()),
            LabelValue::Enum(s) => serde_json::Value::String(s.clone()),
            LabelValue::EnumList(v) => serde_json::Value::Array(
                v.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
            LabelValue::Date(s)
            | LabelValue::Time(s)
            | LabelValue::DateTime(s)
            | LabelValue::Email(s)
            | LabelValue::Account(s) => serde_json::Value::String(s.clone()),
            LabelValue::AccountList(v) => serde_json::Value::Array(
                v.iter()
                    .map(|s| serde_json::Value::String(s.clone()))
                    .collect(),
            ),
            LabelValue::Currency(f) => serde_json::json!(f),
        }
    }

    /// Account 值里的账号 id 集合（含多选）；其余类型返回空。
    pub fn account_ids(&self) -> Vec<&str> {
        match self {
            LabelValue::Account(s) => vec![s.as_str()],
            LabelValue::AccountList(v) => v.iter().map(String::as_str).collect(),
            _ => Vec::new(),
        }
    }
}

/// 时间型标签的默认展示布局（schema.format 缺省时使用，Go 布局）。
pub fn default_layout(vt: LabelValueType) -> &'static str {
    match vt {
        LabelValueType::Date => crate::golayout::DATE_LAYOUT,
        LabelValueType::Time => crate::golayout::TIME_LAYOUT,
        LabelValueType::DateTime => crate::golayout::DATETIME_LAYOUT,
        _ => "",
    }
}

/// 时间型标签的默认展示格式（常规表示法，写进 schema.format 与配置界面）。
pub fn default_pattern(vt: LabelValueType) -> &'static str {
    match vt {
        LabelValueType::Date => crate::golayout::DATE_PATTERN,
        LabelValueType::Time => crate::golayout::TIME_PATTERN,
        LabelValueType::DateTime => crate::golayout::DATETIME_PATTERN,
        _ => "",
    }
}

/// `schema.format`（常规表示法，兼容历史 Go 布局）→ 实际用于解析 / 格式化的 Go 布局。
pub fn resolve_layout(format: Option<&str>, vt: LabelValueType) -> String {
    crate::golayout::resolve(format, default_layout(vt))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LabelSchema {
    pub workspace_id: Ulid,
    pub name: String,
    pub title: String,
    pub value_type: LabelValueType,
    pub enum_values: Vec<String>,
    /// 基础色 `#rrggbb`，未配置为 None。
    #[serde(default)]
    pub color: Option<String>,
    /// 值 → 色映射；按顺序取首个命中。
    #[serde(default)]
    pub value_colors: Vec<ValueColor>,
    // 追加在末尾，全部 #[serde(default)]
    #[serde(default)]
    pub multi: bool, // Enum / Account 有效：允许多选
    #[serde(default)]
    pub format: Option<String>, // 时间型 = 常规模式（如 YYYY-MM-DD HH:mm:ss）
    #[serde(default)]
    pub currency_symbol: Option<String>, // 缺省 ¥（仅展示用）
    #[serde(default)]
    pub unit: Option<String>, // 如「元」「万」（仅展示用）
    // 同样只能追加
    /// 给 Entry 打这个标签时预填的值；`None` 表示没有默认值。
    /// 无值标签（Null）用 `Some(LabelValue::Null)` 表示「默认打上」。
    #[serde(default)]
    pub default_value: Option<LabelValue>,
    // 同样只能追加
    /// 本标签与其他标签的继承 / 覆盖关系。见 `LabelLink`。
    #[serde(default)]
    pub links: Vec<LabelLink>,
}

/// 关系的两种写法。二者归一后是同一件事——「源命中即获得目标」——
/// 区别只在于声明在谁的配置里、界面怎么念：
///
/// - `Inherit`（继承）声明在子标签上：打上本标签的条目也算有对方标签。
/// - `Override`（覆盖）声明在覆盖标签上：打上对方标签的条目也算有本标签。
///
/// 于是「L5 覆盖 L4='V1'」与「L4='V1' 继承 L5」是同一张图上的同一条边。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LinkKind {
    Inherit,
    Override,
}

/// 一条标签关系。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelLink {
    pub kind: LinkKind,
    /// 对方标签的 key。
    pub other: String,
    /// 对方标签的值。
    #[serde(default)]
    pub other_value: Option<LabelValue>,
    /// 本标签的取值：继承时是「仅当本标签为该值才触发」，覆盖时是「覆盖后本标签取的值」。
    #[serde(default)]
    pub own_value: Option<LabelValue>,
}

/// 继承推导出的标签：(标签名, 值)。
/// 值为 `None` 表示只继承了 key——只满足存在性判断（`L4` / `!L4`），不参与值比较。
pub type DerivedLabel = (String, Option<LabelValue>);

/// 工作空间内全部标签关系归一成的一张有向图，供 `derive` 求传递闭包。
/// 归一后每条边都是「源标签(可带值) → 获得标签(可带值)」。
#[derive(Debug, Clone, Default)]
pub struct InheritanceGraph {
    by_from: std::collections::HashMap<String, Vec<Edge>>,
}

#[derive(Debug, Clone)]
struct Edge {
    /// 源侧的值限定；`None` = 源标签带任意值（含 key 继承来的无值）都触发。
    from_value: Option<LabelValue>,
    to: String,
    to_value: Option<LabelValue>,
}

impl InheritanceGraph {
    pub fn build(schemas: &[LabelSchema]) -> Self {
        let mut by_from: std::collections::HashMap<String, Vec<Edge>> = std::collections::HashMap::new();
        for s in schemas {
            for l in &s.links {
                // 「继承」以本标签为源、「覆盖」以对方为源。
                let (from, from_value, to, to_value) = match l.kind {
                    LinkKind::Inherit => {
                        (&s.name, l.own_value.clone(), &l.other, l.other_value.clone())
                    }
                    LinkKind::Override => {
                        (&l.other, l.other_value.clone(), &s.name, l.own_value.clone())
                    }
                };
                by_from.entry(from.clone()).or_default().push(Edge {
                    from_value,
                    to: to.clone(),
                    to_value,
                });
            }
        }
        Self { by_from }
    }

    pub fn is_empty(&self) -> bool {
        self.by_from.is_empty()
    }

    /// 找出任意一条环，返回环上的标签名（首尾同名，如 `["L3", "L4", "L3"]`）。
    ///
    /// 只看标签名、不看值：值层面的环必然包含名字层面的环，按名字判更保守，不会漏。
    /// 先跑 Kahn 拓扑剥离圈出所有处在环里的节点，再在剩下的子图里走出一条具体的环，
    /// 只为了报错时能指出是哪儿打结了。
    pub fn find_cycle(&self) -> Option<Vec<String>> {
        use std::collections::{HashMap, HashSet};

        let mut indeg: HashMap<&str, usize> = HashMap::new();
        for (from, edges) in &self.by_from {
            indeg.entry(from.as_str()).or_insert(0);
            for e in edges {
                *indeg.entry(e.to.as_str()).or_insert(0) += 1;
            }
        }
        let mut queue: Vec<&str> = indeg
            .iter()
            .filter(|(_, d)| **d == 0)
            .map(|(n, _)| *n)
            .collect();
        let mut settled: HashSet<&str> = HashSet::new();
        while let Some(n) = queue.pop() {
            settled.insert(n);
            for e in self.by_from.get(n).into_iter().flatten() {
                if let Some(d) = indeg.get_mut(e.to.as_str()) {
                    *d -= 1;
                    if *d == 0 {
                        queue.push(e.to.as_str());
                    }
                }
            }
        }
        let stuck: HashSet<&str> = indeg
            .keys()
            .copied()
            .filter(|n| !settled.contains(n))
            .collect();
        let start = *stuck.iter().next()?;

        let mut path = vec![start.to_string()];
        let mut cur = start;
        // 环上的点必有指向环内的出边，所以这里不会空手而归。
        loop {
            let next = self
                .by_from
                .get(cur)?
                .iter()
                .map(|e| e.to.as_str())
                .find(|t| stuck.contains(t))?;
            if let Some(i) = path.iter().position(|p| p == next) {
                let mut cycle = path[i..].to_vec();
                cycle.push(next.to_string());
                return Some(cycle);
            }
            path.push(next.to_string());
            cur = next;
        }
    }

    /// 由直接打标推出全部继承来的标签。环安全：同一份 (标签, 值) 只走一次。
    /// 已经有直接打标的标签名不出现在结果里——直接打上的值优先，免得两处打架。
    pub fn derive(&self, direct: &[Labeling]) -> Vec<DerivedLabel> {
        if self.by_from.is_empty() {
            return Vec::new();
        }
        let direct_names: std::collections::HashSet<&str> =
            direct.iter().map(|l| l.label_name.as_str()).collect();
        let mut seen: Vec<DerivedLabel> = Vec::new();
        let mut queue: Vec<DerivedLabel> = direct
            .iter()
            .map(|l| (l.label_name.clone(), Some(l.value.clone())))
            .collect();
        while let Some((name, value)) = queue.pop() {
            let Some(edges) = self.by_from.get(&name) else {
                continue;
            };
            for e in edges {
                // 值限定只认带值的持有：key 继承来的「无值」不能冒充某个具体值。
                if let Some(want) = &e.from_value {
                    if value.as_ref() != Some(want) {
                        continue;
                    }
                }
                let item: DerivedLabel = (e.to.clone(), e.to_value.clone());
                if direct_names.contains(item.0.as_str()) || seen.contains(&item) {
                    continue;
                }
                seen.push(item.clone());
                queue.push(item);
            }
        }
        seen
    }
}

/// 标签值到颜色的映射规则。用普通 struct 而非 tagged enum：`LabelSchema` 以 bincode
/// 持久化，bincode 不支持 `deserialize_any`（internally-tagged enum 依赖它）。
/// 语义：`value` 为 `Some` → 枚举精确匹配；否则按 `min`（含）/`max`（不含）数值区间，None 无界。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValueColor {
    pub color: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub value: Option<String>,
}

impl LabelSchema {
    pub fn new(
        workspace_id: Ulid,
        name: String,
        title: String,
        value_type: LabelValueType,
        enum_values: Vec<String>,
    ) -> Self {
        Self {
            workspace_id,
            name,
            title,
            value_type,
            enum_values,
            color: None,
            value_colors: Vec::new(),
            multi: false,
            format: None,
            currency_symbol: None,
            unit: None,
            default_value: None,
            links: Vec::new(),
        }
    }

    /// 链式设置基础色与值色。
    pub fn with_colors(mut self, color: Option<String>, value_colors: Vec<ValueColor>) -> Self {
        self.color = color;
        self.value_colors = value_colors;
        self
    }

    /// 链式设置类型属性，与 `with_colors` 并列。
    pub fn with_attrs(
        mut self,
        multi: bool,
        format: Option<String>,
        currency_symbol: Option<String>,
        unit: Option<String>,
    ) -> Self {
        self.multi = multi;
        self.format = format;
        self.currency_symbol = currency_symbol;
        self.unit = unit;
        self
    }

    /// 内置「任务」：无值标签，只有「打上 / 没打上」两种状态（表达式里写作 `Task` / `!Task`）。
    pub fn task(workspace_id: Ulid) -> Self {
        Self::new(
            workspace_id,
            "Task".to_string(),
            "任务".to_string(),
            LabelValueType::Null,
            Vec::new(),
        )
    }

    /// 内置「缺陷」：同 `task`，无值标签。
    pub fn bug(workspace_id: Ulid) -> Self {
        Self::new(
            workspace_id,
            "Bug".to_string(),
            "缺陷".to_string(),
            LabelValueType::Null,
            Vec::new(),
        )
    }
}

/// 标签值 → 颜色：Enum 精确匹配；数值按区间（左闭右开，首个命中）；未命中回退基础色。
pub fn resolve_color(schema: &LabelSchema, value: &serde_json::Value) -> Option<String> {
    for vc in &schema.value_colors {
        let hit = if let Some(want) = &vc.value {
            match value {
                serde_json::Value::Array(a) => a.iter().any(|x| x.as_str() == Some(want.as_str())),
                _ => value.as_str() == Some(want.as_str()),
            }
        } else if let Some(v) = value.as_f64() {
            vc.min.map_or(true, |lo| v >= lo) && vc.max.map_or(true, |hi| v < hi)
        } else {
            false
        };
        if hit {
            return Some(vc.color.clone());
        }
    }
    schema.color.clone()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Labeling {
    pub entry_code: String,
    pub label_name: String,
    pub value: LabelValue,
    pub set_by: Ulid,
    pub set_at: DateTime<Utc>,
}

impl Labeling {
    pub fn new(entry_code: String, label_name: String, value: LabelValue, actor: Ulid) -> Self {
        Self {
            entry_code,
            label_name,
            value,
            set_by: actor,
            set_at: Utc::now(),
        }
    }
}
