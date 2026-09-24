use std::sync::Arc;

use ulid::Ulid;

use crate::domain::{
    AuditAction, AuditLog, InheritanceGraph, LabelLink, LabelSchema, LabelValue, LabelValueType,
    LinkKind, ValueColor, RESERVED_FIELDS,
};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

/// 创建 / 更新标签 schema 的集中入参。`update_schema` 只以 `name` 为键，
/// 忽略 `value_type`（类型在创建时固定）。
#[derive(Debug, Clone)]
pub struct LabelSchemaInput {
    pub name: String,
    pub title: String,
    pub value_type: LabelValueType,
    pub enum_values: Vec<String>,
    pub multi: bool,
    pub format: Option<String>,
    pub currency_symbol: Option<String>,
    pub unit: Option<String>,
    pub color: Option<String>,
    pub value_colors: Vec<ValueColor>,
    /// 默认值（原始 JSON）。`None` / `null` 表示没有默认值。
    pub default_value: Option<serde_json::Value>,
    /// 继承 / 覆盖关系。两侧的值都是原始 JSON，由 `resolve_links` 按对应 schema 解析。
    pub links: Vec<LabelLinkInput>,
}

/// 一条关系的原始输入。
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelLinkInput {
    pub kind: LinkKind,
    pub other: String,
    #[serde(default)]
    pub other_value: Option<serde_json::Value>,
    #[serde(default)]
    pub own_value: Option<serde_json::Value>,
}

/// 按 schema 解析一个可选值：给了值但类型对不上就报错。
fn resolve_link_value(
    raw: &Option<serde_json::Value>,
    schema: &LabelSchema,
    what: &str,
) -> Result<Option<LabelValue>, AppError> {
    match raw {
        // `null` 与不传等价：都是「不带值」。
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(_) if schema.value_type == LabelValueType::Null => Err(AppError::InvalidQuery(
            format!("{what}是无值标签，不能指定值"),
        )),
        Some(v) => Ok(Some(LabelValue::from_json(v, schema)?)),
    }
}

/// 校验并解析本标签的关系列表。`schema` 是本标签（尚未落库的新定义也行）。
///
/// 只校验「能不能写进去」：关系指向的标签必须存在、两侧的值要各自符合对方的类型、
/// 不许自引用、不许重复。环不做拦截——推导本身是环安全的（同一份「标签+值」只走一次），
/// 而互相继承在语义上就是「这两个标签等价」，未必是笔误。
fn resolve_links(
    store: &DocStore,
    ws_id: Ulid,
    schema: &LabelSchema,
    inputs: Vec<LabelLinkInput>,
) -> Result<Vec<LabelLink>, AppError> {
    let mut out: Vec<LabelLink> = Vec::with_capacity(inputs.len());
    for input in inputs {
        let other_name = input.other.trim();
        if other_name.is_empty() {
            return Err(AppError::InvalidQuery("关系里没有填对方标签".to_string()));
        }
        if other_name == schema.name {
            return Err(AppError::InvalidQuery(format!(
                "标签 {} 不能与自己建立继承 / 覆盖关系",
                schema.name
            )));
        }
        let other = store
            .get::<LabelSchema>(cf::LABEL_SCHEMAS, &keys::label_schema_key(ws_id, other_name))?
            .ok_or_else(|| AppError::InvalidQuery(format!("标签不存在: {other_name}")))?;
        let link = LabelLink {
            kind: input.kind,
            other: other_name.to_string(),
            other_value: resolve_link_value(
                &input.other_value,
                &other,
                &format!("标签 {other_name}"),
            )?,
            own_value: resolve_link_value(
                &input.own_value,
                schema,
                &format!("标签 {}", schema.name),
            )?,
        };
        if out.contains(&link) {
            return Err(AppError::InvalidQuery(format!(
                "标签 {} 上有一条重复的关系: {other_name}",
                schema.name
            )));
        }
        out.push(link);
    }
    Ok(out)
}

/// 拒绝会形成环的关系。互相继承在语义上就是「这两个标签等价」，实际几乎都是笔误，
/// 所以建关系时就拦掉，而不是等推导时安静地绕圈。
///
/// 判定把本工作空间里所有标签的关系拼成一张图（本标签用刚解析好的 `links` 覆盖），
/// 再看有没有环——环可能不是这一条关系自己造的，而是「A 已指向 B，现在给 B 加一条
/// 指回 A」这种后加的一笔，所以必须带上全量关系一起判。
fn check_no_cycle(store: &DocStore, ws_id: Ulid, schema: &LabelSchema) -> Result<(), AppError> {
    let mut all: Vec<LabelSchema> = Vec::new();
    for (_, v) in store.scan_prefix(cf::LABEL_SCHEMAS, &ws_id.to_bytes())? {
        let s: LabelSchema = bincode::deserialize(&v)?;
        if s.name != schema.name {
            all.push(s);
        }
    }
    all.push(schema.clone());
    if let Some(cycle) = InheritanceGraph::build(&all).find_cycle() {
        return Err(AppError::InvalidQuery(format!(
            "标签关系存在环: {}",
            cycle.join(" → ")
        )));
    }
    Ok(())
}

/// 校验颜色字符串为 `#rrggbb` 形式（不引入 regex 依赖）。
pub(crate) fn check_color(c: &str) -> Result<(), AppError> {
    let bytes = c.as_bytes();
    let valid = bytes.len() == 7
        && bytes[0] == b'#'
        && bytes[1..].iter().all(|b| b.is_ascii_hexdigit());
    if valid {
        Ok(())
    } else {
        Err(AppError::InvalidQuery("颜色格式应为 #rrggbb".to_string()))
    }
}

/// 校验基础色与值色：基础色格式；每个值色格式 + 枚举值须属于 `enum_values`。
fn validate_colors(
    value_type: LabelValueType,
    enum_values: &[String],
    color: &Option<String>,
    value_colors: &[ValueColor],
) -> Result<(), AppError> {
    if let Some(c) = color {
        check_color(c)?;
    }
    for vc in value_colors {
        check_color(&vc.color)?;
        if let Some(v) = &vc.value {
            if value_type != LabelValueType::Enum || !enum_values.iter().any(|e| e == v) {
                return Err(AppError::InvalidQuery(format!(
                    "枚举值色引用了无效的值: {v}"
                )));
            }
        }
    }
    Ok(())
}

/// 标签属性的集中校验：保留名、multi 与时间布局。
fn validate_attrs(input: &LabelSchemaInput) -> Result<(), AppError> {
    if RESERVED_FIELDS
        .iter()
        .any(|r| r.eq_ignore_ascii_case(input.name.trim()))
    {
        return Err(AppError::LabelNameReserved);
    }
    if input.multi && !matches!(input.value_type, LabelValueType::Enum | LabelValueType::Account) {
        return Err(AppError::InvalidQuery("「多选」仅适用于枚举或账号标签".to_string()));
    }
    if matches!(
        input.value_type,
        LabelValueType::Date | LabelValueType::Time | LabelValueType::DateTime
    ) {
        // 库里存的是常规表示法（`YYYY-MM-DD HH:mm:ss`），只有历史数据才可能是 Go 布局。
        if let Some(raw) = input.format.as_deref() {
            let layout = if raw.bytes().any(|b| b.is_ascii_digit()) {
                raw.to_string()
            } else {
                crate::golayout::to_go(raw)
                    .ok_or_else(|| AppError::InvalidQuery(format!("时间格式无效: {raw}")))?
            };
            let probe = crate::golayout::YmdHms {
                year: 2006,
                month: 1,
                day: 2,
                hour: 15,
                minute: 4,
                second: 5,
            };
            let rendered = crate::golayout::format(&layout, probe);
            // 布局必须能往返：否则它既格式化不出东西，也解析不回来。
            if crate::golayout::parse(&layout, &rendered).is_none() {
                return Err(AppError::InvalidQuery(format!("时间格式无效: {raw}")));
            }
        }
    }
    Ok(())
}

/// Account 型标签的值必须是本工作空间的成员——否则打上去也没人认得。
/// 非 Account 值直接放行。
pub(crate) fn check_account_member(
    store: &DocStore,
    ws_id: Ulid,
    lv: &LabelValue,
) -> Result<(), AppError> {
    // 多选账号：逐个 id 校验；任一不是成员即整批失败，与单选同语义。
    let ids = lv.account_ids();
    if ids.is_empty() {
        return Ok(());
    }
    for account_id in ids {
        let member = store.get::<crate::domain::WorkspaceMember>(
            cf::WORKSPACE_MEMBERS,
            &keys::member_key(ws_id, Ulid::from_string(account_id).map_err(|_| AppError::InvalidLabelValue)?),
        )?;
        if member.is_none() {
            return Err(AppError::InvalidQuery(
                "账号标签的值必须是该工作空间的成员".to_string(),
            ));
        }
    }
    Ok(())
}

/// `format` 归一：去空白，空串即「未配置」（用该类型的默认格式）。
fn normalize_format(format: Option<String>) -> Option<String> {
    format
        .map(|f| f.trim().to_string())
        .filter(|f| !f.is_empty())
}

/// 原始 JSON → 默认值。`None` / `null` 表示没有默认值；给了值但按 schema 非法则报错。
fn resolve_default(
    raw: Option<serde_json::Value>,
    schema: &LabelSchema,
) -> Result<Option<LabelValue>, AppError> {
    match raw {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => Ok(Some(LabelValue::from_json(&v, schema)?)),
    }
}

/// 自定义标签 schema CRUD。schema 以 workspace 内唯一的 name 作为稳定键，
/// 一经创建不可改名；update_schema 可改 title / enum_values / color / value_colors
/// 以及 multi / format / currency_symbol / unit。
/// 与 WorkspaceService::create 内置的 Task/Bug schema 存放在同一 column family，
/// list_schemas 天然返回内置 + 自定义。
pub struct LabelService {
    store: Arc<DocStore>,
}

impl LabelService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    /// 修复末尾新增字段之前落库的标签定义。bincode 按位置编码，末尾新增的字段会让
    /// 存量记录解码时直接读到 EOF——`#[serde(default)]` 拦不住这一步，因为解码器根本
    /// 走不到「用默认值补上缺失字段」那一步。
    ///
    /// 缺的字节数取决于落库时究竟到哪一版为止，所以这里按 1..=[`MAX_PAD_BYTES`] 依次
    /// 试补零，取第一个能解出来的：`Option` 的缺失是 1 个零字节，`Vec` 的缺失是 8 个
    /// （u64 长度 0），两个一起缺就是 9 个。读回后按当前编码写回，此后所有读取路径都
    /// 不再需要兼容分支。幂等：修完再扫不会命中。返回修好的条数。
    pub fn repair_legacy_schemas(&self) -> Result<usize, AppError> {
        const MAX_PAD_BYTES: usize = 16;
        let mut ops = Vec::new();
        for (key, value) in self.store.scan_prefix(cf::LABEL_SCHEMAS, b"")? {
            if bincode::deserialize::<LabelSchema>(&value).is_ok() {
                continue;
            }
            let repaired = (1..=MAX_PAD_BYTES).find_map(|pad| {
                let mut padded = value.clone();
                padded.resize(padded.len() + pad, 0);
                bincode::deserialize::<LabelSchema>(&padded).ok()
            });
            match repaired {
                Some(schema) => ops.push(BatchOp::put(cf::LABEL_SCHEMAS, key, &schema)?),
                // 补零也读不出来，说明不是缺末尾字段的问题，原样留着交给读取路径报错。
                None => tracing::warn!(
                    "标签定义无法修复，保留原样: {}",
                    String::from_utf8_lossy(&key)
                ),
            }
        }
        let repaired = ops.len();
        if repaired > 0 {
            self.store.write_batch(ops)?;
            tracing::info!("修复 {repaired} 条旧编码的标签定义");
        }
        Ok(repaired)
    }

    pub fn list_schemas(&self, ws_id: Ulid) -> Result<Vec<LabelSchema>, AppError> {
        let prefix = ws_id.to_bytes();
        let rows = self.store.scan_prefix(cf::LABEL_SCHEMAS, &prefix)?;
        let mut out = Vec::new();
        for (_, v) in rows {
            out.push(bincode::deserialize(&v)?);
        }
        out.sort_by(|a: &LabelSchema, b: &LabelSchema| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get_schema(&self, ws_id: Ulid, name: &str) -> Result<Option<LabelSchema>, AppError> {
        self.store
            .get(cf::LABEL_SCHEMAS, &keys::label_schema_key(ws_id, name))
    }

    pub fn create_schema(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        mut input: LabelSchemaInput,
    ) -> Result<LabelSchema, AppError> {
        input.format = normalize_format(input.format);
        let name = input.name.trim();
        if name.is_empty() {
            return Err(AppError::Internal("标签名称不能为空".to_string()));
        }
        if self.get_schema(ws_id, name)?.is_some() {
            return Err(AppError::LabelNameExists);
        }
        validate_attrs(&input)?;
        validate_colors(
            input.value_type,
            &input.enum_values,
            &input.color,
            &input.value_colors,
        )?;
        let mut schema = LabelSchema::new(
            ws_id,
            name.to_string(),
            input.title.trim().to_string(),
            input.value_type,
            input.enum_values,
        )
        .with_colors(input.color, input.value_colors)
        .with_attrs(
            input.multi,
            input.format,
            input.currency_symbol,
            input.unit,
        );
        // 默认值按刚组装好的 schema 校验（枚举范围、时间格式、multi 都在其中）。
        schema.default_value = resolve_default(input.default_value, &schema)?;
        if let Some(dv) = &schema.default_value {
            check_account_member(&self.store, ws_id, dv)?;
        }
        schema.links = resolve_links(&self.store, ws_id, &schema, input.links)?;
        check_no_cycle(&self.store, ws_id, &schema)?;
        let audit = AuditLog::new(
            AuditAction::LabelSchemaCreated,
            actor,
            "label_schema",
            name,
            Some(ws_id),
            None,
            Some(serde_json::to_string(&schema).unwrap_or_default()),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::LABEL_SCHEMAS,
            keys::label_schema_key(ws_id, name),
            &schema,
        )?);
        self.store.write_batch(ops)?;
        Ok(schema)
    }

    pub fn update_schema(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        input: LabelSchemaInput,
    ) -> Result<LabelSchema, AppError> {
        // 先落成 owned：结构更新语法会整体 move `input`，之后仍需借 `name`。
        let name = input.name.trim().to_string();
        let mut schema = self.get_schema(ws_id, &name)?.ok_or(AppError::NotFound)?;
        // 标签类型创建后固定：沿用库中已有类型，忽略传入值。
        let check = LabelSchemaInput {
            name: name.clone(),
            title: input.title,
            value_type: schema.value_type,
            enum_values: input.enum_values,
            multi: input.multi,
            format: normalize_format(input.format),
            currency_symbol: input.currency_symbol,
            unit: input.unit,
            color: input.color,
            value_colors: input.value_colors,
            default_value: input.default_value,
            links: input.links,
        };
        validate_attrs(&check)?;
        validate_colors(
            schema.value_type,
            &check.enum_values,
            &check.color,
            &check.value_colors,
        )?;
        let before = serde_json::to_string(&schema).unwrap_or_default();
        schema.title = check.title.trim().to_string();
        schema.enum_values = check.enum_values;
        schema.color = check.color;
        schema.value_colors = check.value_colors;
        schema.multi = check.multi;
        schema.format = check.format;
        schema.currency_symbol = check.currency_symbol;
        schema.unit = check.unit;
        schema.default_value = resolve_default(check.default_value, &schema)?;
        if let Some(dv) = &schema.default_value {
            check_account_member(&self.store, ws_id, dv)?;
        }
        schema.links = resolve_links(&self.store, ws_id, &schema, check.links)?;
        check_no_cycle(&self.store, ws_id, &schema)?;
        let after = serde_json::to_string(&schema).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::LabelSchemaUpdated,
            actor,
            "label_schema",
            &name,
            Some(ws_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&audit)?;
        ops.push(BatchOp::put(
            cf::LABEL_SCHEMAS,
            keys::label_schema_key(ws_id, &name),
            &schema,
        )?);
        self.store.write_batch(ops)?;
        Ok(schema)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::workspace::WorkspaceService;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-label-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn create_list_and_update_schema() {
        let dir = temp_dir("crud");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = LabelService::new(store.clone());
        let ws_id = Ulid::new();
        let actor = Ulid::new();

        let s = svc
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "Priority".into(),
                    title: "优先级".into(),
                    value_type: LabelValueType::Enum,
                    enum_values: vec!["High".into(), "Low".into()],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap();
        assert_eq!(s.name, "Priority");
        assert_eq!(s.workspace_id, ws_id);

        // 重复名称报错：客户端可修复，需有专用变体而非笼统的 Internal。
        let err = svc
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "Priority".into(),
                    title: "x".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap_err();
        assert!(matches!(err, AppError::LabelNameExists));

        // 保留名（内置元数据字段）不能用作标签名。
        let reserved = svc
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "Title".into(),
                    title: "标题".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap_err();
        assert!(matches!(reserved, AppError::LabelNameReserved));

        // multi 只对枚举标签有意义。
        let bad_multi = svc
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "Tags".into(),
                    title: "标签".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: true,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap_err();
        assert!(matches!(bad_multi, AppError::InvalidQuery(_)));

        // 列表包含
        let all = svc.list_schemas(ws_id).unwrap();
        assert!(all.iter().any(|x| x.name == "Priority"));

        // 更新 title/enum，name 不变
        let u = svc
            .update_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "Priority".into(),
                    title: "优先级2".into(),
                    value_type: LabelValueType::Enum,
                    enum_values: vec!["High".into(), "Mid".into()],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap();
        assert_eq!(u.name, "Priority");
        assert_eq!(u.title, "优先级2");
        assert_eq!(u.enum_values, vec!["High", "Mid"]);

        // 更新后 get 读到新值，且 value_type 未被 update 改动
        let reloaded = svc.get_schema(ws_id, "Priority").unwrap().unwrap();
        assert_eq!(reloaded.title, "优先级2");
        assert_eq!(reloaded.enum_values, vec!["High", "Mid"]);
        assert_eq!(reloaded.value_type, LabelValueType::Enum);

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_trims_and_rejects_empty_name_and_get_returns_none_for_missing() {
        let dir = temp_dir("trim");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = LabelService::new(store.clone());
        let ws_id = Ulid::new();
        let actor = Ulid::new();

        assert!(svc
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "   ".into(),
                    title: "x".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .is_err());
        assert!(svc
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "".into(),
                    title: "x".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .is_err());

        let s = svc
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "  Priority  ".into(),
                    title: "  优先级  ".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap();
        assert_eq!(s.name, "Priority");
        assert_eq!(s.title, "优先级");

        let got = svc.get_schema(ws_id, "Priority").unwrap().unwrap();
        assert_eq!(got, s);
        assert!(svc.get_schema(ws_id, "Nope").unwrap().is_none());

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_schema_on_missing_name_returns_not_found() {
        let dir = temp_dir("update_missing");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let svc = LabelService::new(store.clone());
        let ws_id = Ulid::new();
        let actor = Ulid::new();

        let err = svc
            .update_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "DoesNotExist".into(),
                    title: "t".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound));

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn custom_schemas_coexist_with_builtin_task_bug() {
        let dir = temp_dir("builtin");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let label = LabelService::new(store.clone());
        let ws = WorkspaceService::new(store.clone());
        let actor = Ulid::new();

        // WorkspaceService::create 会为每个 workspace 内置 Task/Bug 两个 schema。
        let workspace = ws
            .create(actor, "Rodeo", None, "默认 workspace")
            .unwrap();
        let ws_id = workspace.id;

        let builtin = label.list_schemas(ws_id).unwrap();
        assert_eq!(builtin.len(), 2);
        let names: Vec<&str> = builtin.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, vec!["Bug", "Task"], "按 name 升序");
        let task = builtin.iter().find(|x| x.name == "Task").unwrap();
        assert_eq!(task.title, "任务");
        assert_eq!(task.value_type, LabelValueType::Null);

        // 自定义 schema 与内置并存
        label
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "Priority".into(),
                    title: "优先级".into(),
                    value_type: LabelValueType::Enum,
                    enum_values: vec!["High".into(), "Low".into()],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .unwrap();
        let all = label.list_schemas(ws_id).unwrap();
        assert_eq!(all.len(), 3);
        let all_names: Vec<&str> = all.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(all_names, vec!["Bug", "Priority", "Task"]);

        // 自定义 schema 不能覆盖内置名称
        assert!(label
            .create_schema(
                actor,
                ws_id,
                LabelSchemaInput {
                    name: "Task".into(),
                    title: "自定义任务".into(),
                    value_type: LabelValueType::String,
                    enum_values: vec![],
                    multi: false,
                    format: None,
                    currency_symbol: None,
                    unit: None,
                    color: None,
                    value_colors: vec![],
                    default_value: None,
                    links: vec![],
                },
            )
            .is_err());

        // 其他 workspace 看不到这些 schema
        assert!(label.list_schemas(Ulid::new()).unwrap().is_empty());

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
