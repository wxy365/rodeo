use std::sync::Arc;

use ulid::Ulid;

use crate::domain::{generate_entry_code, Entry, LabelSchema, LabelValue, Labeling};
use crate::error::AppError;
use crate::storage::{cf, keys, DocStore};

pub struct EntryService {
    store: Arc<DocStore>,
}

impl EntryService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    pub fn create(&self, actor: Ulid, workspace_id: Ulid, title: &str) -> Result<Entry, AppError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(AppError::Internal("标题不能为空".to_string()));
        }
        let mut entry = Entry::new(workspace_id, title.to_string(), actor);
        // code 冲突时重试（概率极低）。
        while self
            .store
            .get::<Entry>(cf::ENTRIES, entry.code.as_bytes())?
            .is_some()
        {
            entry.code = generate_entry_code();
        }
        let ws_key = keys::entry_by_workspace_key(workspace_id, &entry.code);
        self.store.put(cf::ENTRIES, entry.code.as_bytes(), &entry)?;
        self.store.put_raw(cf::ENTRIES_BY_WORKSPACE, &ws_key, b"")?;
        Ok(entry)
    }

    pub fn get(&self, code: &str) -> Result<Option<Entry>, AppError> {
        self.store.get(cf::ENTRIES, code.as_bytes())
    }

    pub fn list(&self, workspace_id: Ulid) -> Result<Vec<Entry>, AppError> {
        let prefix = workspace_id.to_bytes();
        let rows = self.store.scan_prefix(cf::ENTRIES_BY_WORKSPACE, &prefix)?;
        let mut entries = Vec::new();
        for (key, _) in rows {
            if key.len() <= 16 {
                continue;
            }
            let code = std::str::from_utf8(&key[16..]).unwrap_or("").to_string();
            if let Some(e) = self.store.get::<Entry>(cf::ENTRIES, code.as_bytes())? {
                entries.push(e);
            }
        }
        entries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(entries)
    }

    pub fn set_labeling(
        &self,
        actor: Ulid,
        entry_code: &str,
        label_name: &str,
        value: &serde_json::Value,
    ) -> Result<Labeling, AppError> {
        let entry = self.get(entry_code)?.ok_or(AppError::NotFound)?;
        let schema = self
            .store
            .get::<LabelSchema>(
                cf::LABEL_SCHEMAS,
                &keys::label_schema_key(entry.workspace_id, label_name),
            )?
            .ok_or(AppError::NotFound)?;
        let lv = LabelValue::from_json(value, &schema)?;
        let labeling = Labeling::new(entry_code.to_string(), label_name.to_string(), lv, actor);
        self.store.put(
            cf::LABELINGS,
            &keys::labeling_key(entry_code, label_name),
            &labeling,
        )?;
        Ok(labeling)
    }

    pub fn labelings(&self, entry_code: &str) -> Result<Vec<Labeling>, AppError> {
        let prefix = entry_code.as_bytes();
        let rows = self.store.scan_prefix(cf::LABELINGS, prefix)?;
        let mut out = Vec::new();
        for (_, v) in rows {
            out.push(bincode::deserialize(&v)?);
        }
        Ok(out)
    }
}
