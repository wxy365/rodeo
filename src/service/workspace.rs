use std::sync::Arc;

use ulid::Ulid;

use crate::domain::{LabelSchema, Workspace, WorkspaceMember, WorkspaceRole};
use crate::error::AppError;
use crate::storage::{cf, keys, DocStore};

pub struct WorkspaceService {
    store: Arc<DocStore>,
}

impl WorkspaceService {
    pub fn new(store: Arc<DocStore>) -> Self {
        Self { store }
    }

    pub fn create(
        &self,
        actor: Ulid,
        name: &str,
        slug: Option<&str>,
        description: &str,
    ) -> Result<Workspace, AppError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Internal("Workspace 名称不能为空".to_string()));
        }
        let slug = self.ensure_unique_slug(slug.unwrap_or(""), name)?;
        let ws = Workspace::new(
            name.to_string(),
            slug.clone(),
            description.trim().to_string(),
            actor,
        );

        // 主文档 + slug 唯一索引
        let id_key = ws.id.to_bytes();
        self.store.put(cf::WORKSPACES, &id_key, &ws)?;
        self.store
            .put_raw(cf::WORKSPACES_SLUG_IDX, slug.as_bytes(), &id_key)?;

        // Owner 成员关系（正向 + 反向索引）
        let member = WorkspaceMember::new(ws.id, actor, WorkspaceRole::Owner);
        self.store
            .put(cf::WORKSPACE_MEMBERS, &keys::member_key(ws.id, actor), &member)?;
        self.store.put_raw(
            cf::WORKSPACE_MEMBERS_BY_ACCOUNT,
            &keys::member_by_account_key(actor, ws.id),
            b"",
        )?;

        // 内置标签
        let task = LabelSchema::task(ws.id);
        let bug = LabelSchema::bug(ws.id);
        self.store.put(
            cf::LABEL_SCHEMAS,
            &keys::label_schema_key(ws.id, &task.name),
            &task,
        )?;
        self.store.put(
            cf::LABEL_SCHEMAS,
            &keys::label_schema_key(ws.id, &bug.name),
            &bug,
        )?;

        Ok(ws)
    }

    pub fn list_for(&self, account_id: Ulid) -> Result<Vec<(Workspace, WorkspaceRole)>, AppError> {
        let prefix = account_id.to_bytes();
        let rows = self
            .store
            .scan_prefix(cf::WORKSPACE_MEMBERS_BY_ACCOUNT, &prefix)?;
        let mut out = Vec::new();
        for (key, _) in rows {
            if key.len() != 32 {
                continue;
            }
            let ws_id = Ulid::from_bytes(
                key[16..32]
                    .try_into()
                    .expect("slice of 16 bytes"),
            );
            let Some(member) = self
                .store
                .get::<WorkspaceMember>(cf::WORKSPACE_MEMBERS, &keys::member_key(ws_id, account_id))?
            else {
                continue;
            };
            let Some(ws) = self.store.get::<Workspace>(cf::WORKSPACES, &ws_id.to_bytes())? else {
                continue;
            };
            out.push((ws, member.role));
        }
        out.sort_by(|a, b| a.0.created_at.cmp(&b.0.created_at));
        Ok(out)
    }

    pub fn get_by_slug(&self, slug: &str) -> Result<Option<Workspace>, AppError> {
        let Some(raw) = self.store.get_raw(cf::WORKSPACES_SLUG_IDX, slug.as_bytes())? else {
            return Ok(None);
        };
        let id: [u8; 16] = raw
            .try_into()
            .map_err(|_| AppError::Internal("slug 索引损坏".to_string()))?;
        self.store.get(cf::WORKSPACES, &id)
    }

    pub fn get_by_id(&self, id: Ulid) -> Result<Option<Workspace>, AppError> {
        self.store.get(cf::WORKSPACES, &id.to_bytes())
    }

    pub fn get_member(&self, ws_id: Ulid, account_id: Ulid) -> Result<Option<WorkspaceMember>, AppError> {
        self.store
            .get(cf::WORKSPACE_MEMBERS, &keys::member_key(ws_id, account_id))
    }

    fn ensure_unique_slug(&self, requested: &str, name: &str) -> Result<String, AppError> {
        let base = slugify(if requested.trim().is_empty() {
            name
        } else {
            requested
        });
        let base = if base.is_empty() {
            "workspace".to_string()
        } else {
            base
        };
        let mut candidate = base.clone();
        let mut n = 1;
        while self.get_by_slug(&candidate)?.is_some() {
            candidate = format!("{base}-{n}");
            n += 1;
        }
        Ok(candidate)
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in s.trim().to_lowercase().chars() {
        if c.is_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  Foo   Bar  "), "foo-bar");
        assert_eq!(slugify("研发 团队"), "研发-团队");
    }
}
