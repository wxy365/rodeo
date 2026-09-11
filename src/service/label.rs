use std::sync::Arc;

use ulid::Ulid;

use crate::domain::{AuditAction, AuditLog, LabelSchema, LabelValueType, ValueColor};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

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

/// 自定义标签 schema CRUD。schema 以 workspace 内唯一的 name 作为稳定键，
/// 一经创建不可改名；update_schema 可改 title / enum_values / color / value_colors。
/// 与 WorkspaceService::create 内置的 Task/Bug schema 存放在同一 column family，
/// list_schemas 天然返回内置 + 自定义。
pub struct LabelService {
    store: Arc<DocStore>,
}

impl LabelService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
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
        name: &str,
        title: &str,
        value_type: LabelValueType,
        enum_values: Vec<String>,
        color: Option<String>,
        value_colors: Vec<ValueColor>,
    ) -> Result<LabelSchema, AppError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Internal("标签名称不能为空".to_string()));
        }
        if self.get_schema(ws_id, name)?.is_some() {
            return Err(AppError::LabelNameExists);
        }
        validate_colors(value_type, &enum_values, &color, &value_colors)?;
        let schema = LabelSchema::new(
            ws_id,
            name.to_string(),
            title.trim().to_string(),
            value_type,
            enum_values,
        )
        .with_colors(color, value_colors);
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
        name: &str,
        title: &str,
        enum_values: Vec<String>,
        color: Option<String>,
        value_colors: Vec<ValueColor>,
    ) -> Result<LabelSchema, AppError> {
        let mut schema = self.get_schema(ws_id, name)?.ok_or(AppError::NotFound)?;
        validate_colors(schema.value_type, &enum_values, &color, &value_colors)?;
        let before = serde_json::to_string(&schema).unwrap_or_default();
        schema.title = title.trim().to_string();
        schema.enum_values = enum_values;
        schema.color = color;
        schema.value_colors = value_colors;
        let after = serde_json::to_string(&schema).unwrap_or_default();
        let audit = AuditLog::new(
            AuditAction::LabelSchemaUpdated,
            actor,
            "label_schema",
            name,
            Some(ws_id),
            Some(before),
            Some(after),
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
                "Priority",
                "优先级",
                LabelValueType::Enum,
                vec!["High".into(), "Low".into()],
                None,
                vec![],
            )
            .unwrap();
        assert_eq!(s.name, "Priority");
        assert_eq!(s.workspace_id, ws_id);

        // 重复名称报错：客户端可修复，需有专用变体而非笼统的 Internal。
        let err = svc
            .create_schema(actor, ws_id, "Priority", "x", LabelValueType::String, vec![], None, vec![])
            .unwrap_err();
        assert!(matches!(err, AppError::LabelNameExists));

        // 列表包含
        let all = svc.list_schemas(ws_id).unwrap();
        assert!(all.iter().any(|x| x.name == "Priority"));

        // 更新 title/enum，name 不变
        let u = svc
            .update_schema(
                actor,
                ws_id,
                "Priority",
                "优先级2",
                vec!["High".into(), "Mid".into()],
                None,
                vec![],
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
            .create_schema(actor, ws_id, "   ", "x", LabelValueType::String, vec![], None, vec![])
            .is_err());
        assert!(svc
            .create_schema(actor, ws_id, "", "x", LabelValueType::String, vec![], None, vec![])
            .is_err());

        let s = svc
            .create_schema(actor, ws_id, "  Priority  ", "  优先级  ", LabelValueType::String, vec![], None, vec![])
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
            .update_schema(actor, ws_id, "DoesNotExist", "t", vec![], None, vec![])
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
        assert_eq!(task.value_type, LabelValueType::Enum);

        // 自定义 schema 与内置并存
        label
            .create_schema(
                actor,
                ws_id,
                "Priority",
                "优先级",
                LabelValueType::Enum,
                vec!["High".into(), "Low".into()],
                None,
                vec![],
            )
            .unwrap();
        let all = label.list_schemas(ws_id).unwrap();
        assert_eq!(all.len(), 3);
        let all_names: Vec<&str> = all.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(all_names, vec!["Bug", "Priority", "Task"]);

        // 自定义 schema 不能覆盖内置名称
        assert!(label
            .create_schema(actor, ws_id, "Task", "自定义任务", LabelValueType::String, vec![], None, vec![])
            .is_err());

        // 其他 workspace 看不到这些 schema
        assert!(label.list_schemas(Ulid::new()).unwrap().is_empty());

        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
