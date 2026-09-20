use std::sync::Arc;

use chrono::Utc;
use ulid::Ulid;

use crate::domain::{
    Account, AuditAction, AuditLog, Invite, LabelSchema, Workspace, WorkspaceMember, WorkspaceRole,
};
use crate::error::AppError;
use crate::service::audit::audit_ops;
use crate::storage::{cf, keys, BatchOp, DocStore};

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

    /// 返回 (工作空间, 我的角色, 删除时间)。已删除的也返回，交给上层分组进「回收站」。
    pub fn list_for(
        &self,
        account_id: Ulid,
    ) -> Result<Vec<(Workspace, WorkspaceRole, Option<String>)>, AppError> {
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
            let deleted_at = self.deleted_at(ws_id)?;
            out.push((ws, member.role, deleted_at));
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

    /// 修改名称 / 描述 / 地址（slug）。slug 仅在显式传入时变更；撞车直接报错，不静默加后缀。
    pub fn update(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        name: &str,
        description: &str,
        slug: Option<&str>,
    ) -> Result<Workspace, AppError> {
        let mut ws = self.get_by_id(ws_id)?.ok_or(AppError::NotFound)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Internal("Workspace 名称不能为空".to_string()));
        }
        let old_slug = ws.slug.clone();
        let before = workspace_snapshot(&ws, None);

        ws.name = name.to_string();
        ws.description = description.trim().to_string();

        let mut new_slug = None;
        if let Some(raw) = slug {
            let next = slugify(raw);
            let next = if next.is_empty() {
                "workspace".to_string()
            } else {
                next
            };
            if next != old_slug {
                if let Some(existing) = self.get_by_slug(&next)? {
                    if existing.id != ws_id {
                        return Err(AppError::InvalidQuery(format!("slug 已被占用: {next}")));
                    }
                }
                new_slug = Some(next);
            }
        }
        if let Some(s) = &new_slug {
            ws.slug = s.clone();
        }

        let after = workspace_snapshot(&ws, None);
        let log = AuditLog::new(
            AuditAction::WorkspaceUpdated,
            actor,
            "workspace",
            &ws.id.to_string(),
            Some(ws_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::put(cf::WORKSPACES, ws.id.to_bytes().to_vec(), &ws)?);
        if let Some(s) = new_slug {
            // 旧地址失效 + 新地址生效，必须与其他写入同批，避免中间态。
            ops.push(BatchOp::delete(
                cf::WORKSPACES_SLUG_IDX,
                old_slug.as_bytes().to_vec(),
            ));
            ops.push(BatchOp::put_raw(
                cf::WORKSPACES_SLUG_IDX,
                s.as_bytes().to_vec(),
                ws.id.to_bytes().to_vec(),
            ));
        }
        self.store.write_batch(ops)?;
        Ok(ws)
    }

    pub fn get_member(&self, ws_id: Ulid, account_id: Ulid) -> Result<Option<WorkspaceMember>, AppError> {
        self.store
            .get(cf::WORKSPACE_MEMBERS, &keys::member_key(ws_id, account_id))
    }

    /// 是否处于软删除状态。
    pub fn is_deleted(&self, ws_id: Ulid) -> Result<bool, AppError> {
        Ok(self.deleted_at(ws_id)?.is_some())
    }

    /// 删除时间（RFC3339）；未删除时为 None。
    pub fn deleted_at(&self, ws_id: Ulid) -> Result<Option<String>, AppError> {
        let Some(raw) = self.store.get_raw(cf::WORKSPACES_DELETED, &ws_id.to_bytes())? else {
            return Ok(None);
        };
        Ok(Some(String::from_utf8_lossy(&raw).into_owned()))
    }

    /// 软删除：只写标记。工作空间文档、成员关系、slug 索引全部保留，URL 仍可解析，可恢复。幂等。
    pub fn delete(&self, actor: Ulid, ws_id: Ulid) -> Result<(), AppError> {
        let ws = self.get_by_id(ws_id)?.ok_or(AppError::NotFound)?;
        if self.is_deleted(ws_id)? {
            return Ok(());
        }
        let stamp = Utc::now().to_rfc3339();
        let log = AuditLog::new(
            AuditAction::WorkspaceDeleted,
            actor,
            "workspace",
            &ws.id.to_string(),
            Some(ws_id),
            Some(workspace_snapshot(&ws, None)),
            Some(workspace_snapshot(&ws, Some(&stamp))),
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::put_raw(
            cf::WORKSPACES_DELETED,
            ws.id.to_bytes().to_vec(),
            stamp.into_bytes(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 取消软删除。未删除时直接返回（幂等）。
    pub fn restore(&self, actor: Ulid, ws_id: Ulid) -> Result<(), AppError> {
        let ws = self.get_by_id(ws_id)?.ok_or(AppError::NotFound)?;
        let Some(stamp) = self.deleted_at(ws_id)? else {
            return Ok(());
        };
        let log = AuditLog::new(
            AuditAction::WorkspaceRestored,
            actor,
            "workspace",
            &ws.id.to_string(),
            Some(ws_id),
            Some(workspace_snapshot(&ws, Some(&stamp))),
            Some(workspace_snapshot(&ws, None)),
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::delete(
            cf::WORKSPACES_DELETED,
            ws.id.to_bytes().to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 列出成员及其账号信息，按角色降序、邮箱升序排列。
    pub fn list_members(&self, ws_id: Ulid) -> Result<Vec<(WorkspaceMember, Account)>, AppError> {
        let rows = self.store.scan_prefix(cf::WORKSPACE_MEMBERS, &ws_id.to_bytes())?;
        let mut out = Vec::new();
        for (key, _) in rows {
            if key.len() != 32 {
                continue;
            }
            let account_id = Ulid::from_bytes(key[16..32].try_into().expect("16 字节账号 id"));
            let Some(member) = self
                .store
                .get::<WorkspaceMember>(cf::WORKSPACE_MEMBERS, &keys::member_key(ws_id, account_id))?
            else {
                continue;
            };
            let Some(account) = self.store.get::<Account>(cf::ACCOUNTS, &account_id.to_bytes())? else {
                continue;
            };
            out.push((member, account));
        }
        out.sort_by(|a, b| b.0.role.cmp(&a.0.role).then_with(|| a.1.email.cmp(&b.1.email)));
        Ok(out)
    }

    fn owner_count(&self, ws_id: Ulid) -> Result<usize, AppError> {
        Ok(self
            .list_members(ws_id)?
            .into_iter()
            .filter(|(m, _)| m.role == WorkspaceRole::Owner)
            .count())
    }

    /// 账号邮箱，供审计快照显示可读的「谁」。账号缺失时返回 None，快照退化成账号 id。
    fn account_email(&self, account_id: Ulid) -> Result<Option<String>, AppError> {
        Ok(self
            .store
            .get::<Account>(cf::ACCOUNTS, &account_id.to_bytes())?
            .map(|a| a.email))
    }

    fn find_account_by_email(&self, email: &str) -> Result<Option<Account>, AppError> {
        let email = email.trim().to_lowercase();
        let Some(raw) = self.store.get_raw(cf::ACCOUNTS_EMAIL_IDX, email.as_bytes())? else {
            return Ok(None);
        };
        let id: [u8; 16] = raw
            .try_into()
            .map_err(|_| AppError::Internal("邮箱索引损坏".to_string()))?;
        self.store.get(cf::ACCOUNTS, &id)
    }

    /// 按邮箱邀请已有账号加入工作空间。只写「待接受邀请」，不写成员关系——对方接受后
    /// 才成为成员。对同一账号重复邀请会改写角色（同角色则视为已完成，不重复写）。
    pub fn invite(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        email: &str,
        role: WorkspaceRole,
    ) -> Result<Invite, AppError> {
        let account = self
            .find_account_by_email(email)?
            .ok_or_else(|| AppError::InvalidQuery(format!("未找到账号: {}", email.trim())))?;
        if self.get_member(ws_id, account.id)?.is_some() {
            return Err(AppError::InvalidQuery("该账号已是工作空间成员".to_string()));
        }

        // 已有待接受邀请：同角色直接返回，不同角色改写这条记录。
        if let Some(existing) = self.get_invite(ws_id, account.id)? {
            if existing.role == role {
                return Ok(existing);
            }
            let before = invite_snapshot(&existing, Some(&account.email));
            let mut updated = existing;
            updated.role = role;
            let log = AuditLog::new(
                AuditAction::MemberInvited,
                actor,
                "member",
                &account.id.to_string(),
                Some(ws_id),
                Some(before),
                Some(invite_snapshot(&updated, Some(&account.email))),
            );
            let mut ops = audit_ops(&log)?;
            ops.push(BatchOp::put(
                cf::INVITES,
                keys::invite_key(ws_id, account.id).to_vec(),
                &updated,
            )?);
            self.store.write_batch(ops)?;
            return Ok(updated);
        }

        let invite = Invite::new(ws_id, account.id, role, actor);
        let log = AuditLog::new(
            AuditAction::MemberInvited,
            actor,
            "member",
            &account.id.to_string(),
            Some(ws_id),
            None,
            Some(invite_snapshot(&invite, Some(&account.email))),
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::put(
            cf::INVITES,
            keys::invite_key(ws_id, account.id).to_vec(),
            &invite,
        )?);
        ops.push(BatchOp::put_raw(
            cf::INVITES_BY_ACCOUNT,
            keys::invite_by_account_key(account.id, ws_id).to_vec(),
            Vec::new(),
        ));
        self.store.write_batch(ops)?;
        Ok(invite)
    }

    pub fn get_invite(&self, ws_id: Ulid, account_id: Ulid) -> Result<Option<Invite>, AppError> {
        self.store
            .get(cf::INVITES, &keys::invite_key(ws_id, account_id))
    }

    /// 列出某工作空间待接受的邀请及账号信息，按角色降序、邮箱升序排列。
    pub fn list_invites(&self, ws_id: Ulid) -> Result<Vec<(Invite, Account)>, AppError> {
        let rows = self.store.scan_prefix(cf::INVITES, &ws_id.to_bytes())?;
        let mut out = Vec::new();
        for (key, _) in rows {
            if key.len() != 32 {
                continue;
            }
            let account_id = Ulid::from_bytes(key[16..32].try_into().expect("16 字节账号 id"));
            let Some(invite) = self
                .store
                .get::<Invite>(cf::INVITES, &keys::invite_key(ws_id, account_id))?
            else {
                continue;
            };
            let Some(account) = self.store.get::<Account>(cf::ACCOUNTS, &account_id.to_bytes())?
            else {
                continue;
            };
            out.push((invite, account));
        }
        out.sort_by(|a, b| b.0.role.cmp(&a.0.role).then_with(|| a.1.email.cmp(&b.1.email)));
        Ok(out)
    }

    /// 列出某账号收到、尚未接受的邀请及其工作空间，按邀请时间升序排列。
    pub fn list_invites_for(&self, account_id: Ulid) -> Result<Vec<(Invite, Workspace)>, AppError> {
        let rows = self.store.scan_prefix(cf::INVITES_BY_ACCOUNT, &account_id.to_bytes())?;
        let mut out = Vec::new();
        for (key, _) in rows {
            if key.len() != 32 {
                continue;
            }
            let ws_id = Ulid::from_bytes(key[16..32].try_into().expect("16 字节工作空间 id"));
            let Some(invite) = self
                .store
                .get::<Invite>(cf::INVITES, &keys::invite_key(ws_id, account_id))?
            else {
                continue;
            };
            let Some(ws) = self.store.get::<Workspace>(cf::WORKSPACES, &ws_id.to_bytes())? else {
                continue;
            };
            out.push((invite, ws));
        }
        out.sort_by_key(|a| a.0.created_at);
        Ok(out)
    }

    /// 接受邀请：写入成员关系（正向 + 反向索引），删除待接受记录，记一条 MemberJoined。
    pub fn accept_invite(&self, actor: Ulid, ws_id: Ulid) -> Result<WorkspaceMember, AppError> {
        let invite = self.get_invite(ws_id, actor)?.ok_or(AppError::NotFound)?;
        // 邀请挂着的时候工作空间可能已被删除，此时不能再加入。
        if self.is_deleted(ws_id)? {
            return Err(AppError::InvalidQuery("工作空间已被删除，无法加入".to_string()));
        }
        let email = self.account_email(actor)?;

        let member = WorkspaceMember::new(ws_id, actor, invite.role);
        let log = AuditLog::new(
            AuditAction::MemberJoined,
            actor,
            "member",
            &actor.to_string(),
            Some(ws_id),
            None,
            Some(member_snapshot(&member, email.as_deref())),
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::put(
            cf::WORKSPACE_MEMBERS,
            keys::member_key(ws_id, actor).to_vec(),
            &member,
        )?);
        ops.push(BatchOp::put_raw(
            cf::WORKSPACE_MEMBERS_BY_ACCOUNT,
            keys::member_by_account_key(actor, ws_id).to_vec(),
            Vec::new(),
        ));
        ops.push(BatchOp::delete(
            cf::INVITES,
            keys::invite_key(ws_id, actor).to_vec(),
        ));
        ops.push(BatchOp::delete(
            cf::INVITES_BY_ACCOUNT,
            keys::invite_by_account_key(actor, ws_id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(member)
    }

    /// 拒绝邀请：删掉待接受记录，记一条 InviteDeclined。
    pub fn decline_invite(&self, actor: Ulid, ws_id: Ulid) -> Result<(), AppError> {
        let invite = self.get_invite(ws_id, actor)?.ok_or(AppError::NotFound)?;
        let email = self.account_email(actor)?;
        let log = AuditLog::new(
            AuditAction::InviteDeclined,
            actor,
            "member",
            &actor.to_string(),
            Some(ws_id),
            Some(invite_snapshot(&invite, email.as_deref())),
            None,
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::delete(
            cf::INVITES,
            keys::invite_key(ws_id, actor).to_vec(),
        ));
        ops.push(BatchOp::delete(
            cf::INVITES_BY_ACCOUNT,
            keys::invite_by_account_key(actor, ws_id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 撤销邀请（发起方视角）：删掉待接受记录，记一条 InviteRevoked。
    pub fn revoke_invite(&self, actor: Ulid, ws_id: Ulid, account_id: Ulid) -> Result<(), AppError> {
        let invite = self.get_invite(ws_id, account_id)?.ok_or(AppError::NotFound)?;
        let email = self.account_email(account_id)?;
        let log = AuditLog::new(
            AuditAction::InviteRevoked,
            actor,
            "member",
            &account_id.to_string(),
            Some(ws_id),
            Some(invite_snapshot(&invite, email.as_deref())),
            None,
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::delete(
            cf::INVITES,
            keys::invite_key(ws_id, account_id).to_vec(),
        ));
        ops.push(BatchOp::delete(
            cf::INVITES_BY_ACCOUNT,
            keys::invite_by_account_key(account_id, ws_id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 变更成员角色。降级最后一名 Owner 会被拒绝，避免工作空间失去管理员。
    pub fn update_role(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        account_id: Ulid,
        role: WorkspaceRole,
    ) -> Result<WorkspaceMember, AppError> {
        let mut member = self.get_member(ws_id, account_id)?.ok_or(AppError::NotFound)?;
        if member.role == role {
            return Ok(member);
        }
        if member.role == WorkspaceRole::Owner
            && role != WorkspaceRole::Owner
            && self.owner_count(ws_id)? <= 1
        {
            return Err(AppError::InvalidQuery(
                "至少保留一名 Owner；请先把他人的角色提升为 Owner".to_string(),
            ));
        }
        let before = member_snapshot(&member, None);
        member.role = role;
        let after = member_snapshot(&member, None);
        let log = AuditLog::new(
            AuditAction::RoleChanged,
            actor,
            "member",
            &account_id.to_string(),
            Some(ws_id),
            Some(before),
            Some(after),
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::put(
            cf::WORKSPACE_MEMBERS,
            keys::member_key(ws_id, account_id).to_vec(),
            &member,
        )?);
        self.store.write_batch(ops)?;
        Ok(member)
    }

    /// 一步转让所有权：目标升为 Owner，发起人降为 Maintainer。
    ///
    /// 不走 `update_role`——那样第一步降级就会被「至少保留一名 Owner」拦下。两处角色变更
    /// 与审计同批写入，保证不会出现零 Owner 或双 Owner 的中间态。
    pub fn transfer_owner(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        to_account_id: Ulid,
    ) -> Result<(), AppError> {
        let from = self.get_member(ws_id, actor)?.ok_or(AppError::Forbidden)?;
        if from.role != WorkspaceRole::Owner {
            return Err(AppError::Forbidden);
        }
        let target_before = self
            .get_member(ws_id, to_account_id)?
            .ok_or(AppError::NotFound)?;
        // 转给自己、或对方已是 Owner（不会改变任何东西）都视为已完成。
        if to_account_id == actor || target_before.role == WorkspaceRole::Owner {
            return Ok(());
        }

        let mut from_after = from.clone();
        from_after.role = WorkspaceRole::Maintainer;
        let mut target_after = target_before.clone();
        target_after.role = WorkspaceRole::Owner;

        let log_demote = AuditLog::new(
            AuditAction::RoleChanged,
            actor,
            "member",
            &actor.to_string(),
            Some(ws_id),
            Some(member_snapshot(&from, None)),
            Some(member_snapshot(&from_after, None)),
        );
        let log_promote = AuditLog::new(
            AuditAction::RoleChanged,
            actor,
            "member",
            &target_after.account_id.to_string(),
            Some(ws_id),
            Some(member_snapshot(&target_before, None)),
            Some(member_snapshot(&target_after, None)),
        );

        let mut ops = audit_ops(&log_demote)?;
        ops.extend(audit_ops(&log_promote)?);
        ops.push(BatchOp::put(
            cf::WORKSPACE_MEMBERS,
            keys::member_key(ws_id, actor).to_vec(),
            &from_after,
        )?);
        ops.push(BatchOp::put(
            cf::WORKSPACE_MEMBERS,
            keys::member_key(ws_id, to_account_id).to_vec(),
            &target_after,
        )?);
        self.store.write_batch(ops)?;
        Ok(())
    }

    /// 移除成员。最后一名 Owner 不可移除。
    pub fn remove_member(
        &self,
        actor: Ulid,
        ws_id: Ulid,
        account_id: Ulid,
    ) -> Result<(), AppError> {
        let member = self.get_member(ws_id, account_id)?.ok_or(AppError::NotFound)?;
        if member.role == WorkspaceRole::Owner && self.owner_count(ws_id)? <= 1 {
            return Err(AppError::InvalidQuery(
                "至少保留一名 Owner；请先转让所有权".to_string(),
            ));
        }
        let before = member_snapshot(&member, None);
        let log = AuditLog::new(
            AuditAction::MemberRemoved,
            actor,
            "member",
            &account_id.to_string(),
            Some(ws_id),
            Some(before),
            None,
        );
        let mut ops = audit_ops(&log)?;
        ops.push(BatchOp::delete(
            cf::WORKSPACE_MEMBERS,
            keys::member_key(ws_id, account_id).to_vec(),
        ));
        ops.push(BatchOp::delete(
            cf::WORKSPACE_MEMBERS_BY_ACCOUNT,
            keys::member_by_account_key(account_id, ws_id).to_vec(),
        ));
        self.store.write_batch(ops)?;
        Ok(())
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

/// 工作空间审计快照：字段名与前端 field_label 对齐，便于 diff 出「名称: 甲 → 乙」。
fn workspace_snapshot(ws: &Workspace, deleted_at: Option<&str>) -> String {
    serde_json::json!({
        "name": ws.name,
        "slug": ws.slug,
        "description": ws.description,
        "deleted_at": deleted_at,
    })
    .to_string()
}

/// 成员与邀请的审计快照同构：都是「某账号在某工作空间的角色」。role 用 as_str，
/// 便于前端 diff 出「角色: worker → owner」。
fn subject_snapshot(account_id: Ulid, role: WorkspaceRole, email: Option<&str>) -> String {
    let mut v = serde_json::json!({
        "account_id": account_id.to_string(),
        "role": role.as_str(),
    });
    if let Some(e) = email {
        v["email"] = serde_json::Value::String(e.to_string());
    }
    v.to_string()
}

fn member_snapshot(m: &WorkspaceMember, email: Option<&str>) -> String {
    subject_snapshot(m.account_id, m.role, email)
}

fn invite_snapshot(inv: &Invite, email: Option<&str>) -> String {
    subject_snapshot(inv.account_id, inv.role, email)
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
    use crate::service::AuthService;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-ws-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  Foo   Bar  "), "foo-bar");
        assert_eq!(slugify("研发 团队"), "研发-团队");
    }

    #[test]
    fn update_renames_and_reindexes_slug() {
        let dir = temp_dir("update");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let ws = ws_svc.create(owner.id, "原名", Some("old-slug"), "旧描述").unwrap();
        assert_eq!(ws_svc.get_by_slug("old-slug").unwrap().unwrap().id, ws.id);

        let updated = ws_svc
            .update(owner.id, ws.id, "新名", "新描述", Some("new-slug"))
            .unwrap();
        assert_eq!(updated.name, "新名");
        assert_eq!(updated.slug, "new-slug");
        assert_eq!(updated.description, "新描述");

        // slug 索引重建：旧地址失效，新地址命中同一个工作空间。
        assert!(ws_svc.get_by_slug("old-slug").unwrap().is_none());
        assert_eq!(ws_svc.get_by_slug("new-slug").unwrap().unwrap().id, ws.id);

        let same = ws_svc.get_by_id(ws.id).unwrap().unwrap();
        assert_eq!(same.name, "新名");
        assert_eq!(same.slug, "new-slug");
        assert_eq!(same.description, "新描述");

        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws.id, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&AuditAction::WorkspaceUpdated));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_rejects_taken_slug_and_keeps_old_one() {
        let dir = temp_dir("slug-clash");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let a = ws_svc.create(owner.id, "甲", Some("jia"), "").unwrap();
        ws_svc.create(owner.id, "乙", Some("yi"), "").unwrap();

        // 撞车报错，而不是静默加后缀。
        assert!(ws_svc.update(owner.id, a.id, "甲", "", Some("yi")).is_err());
        // 失败后原地址仍然可用，没有被改坏。
        assert_eq!(ws_svc.get_by_slug("jia").unwrap().unwrap().id, a.id);
        assert_eq!(ws_svc.get_by_id(a.id).unwrap().unwrap().slug, "jia");

        // 空名称同样被拒。
        assert!(ws_svc.update(owner.id, a.id, "   ", "", None).is_err());

        // 不传 slug 时保持原地址。
        let kept = ws_svc.update(owner.id, a.id, "甲改", "有描述了", None).unwrap();
        assert_eq!(kept.slug, "jia");
        assert_eq!(kept.name, "甲改");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_marks_workspace_and_restore_clears_it() {
        let dir = temp_dir("delete");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let ws = ws_svc.create(owner.id, "团队", Some("team"), "").unwrap();

        assert!(!ws_svc.is_deleted(ws.id).unwrap());
        assert!(ws_svc.list_for(owner.id).unwrap()[0].2.is_none());

        ws_svc.delete(owner.id, ws.id).unwrap();
        assert!(ws_svc.is_deleted(ws.id).unwrap());
        assert!(ws_svc.deleted_at(ws.id).unwrap().is_some());
        // 软删除：工作空间本身和 slug 索引都还在，URL 仍能解析到它。
        assert!(ws_svc.get_by_id(ws.id).unwrap().is_some());
        assert_eq!(ws_svc.get_by_slug("team").unwrap().unwrap().id, ws.id);
        // list_for 仍返回，但带 deleted 标记，交由上层分组展示。
        let listed = ws_svc.list_for(owner.id).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].2.is_some());

        // 重复删除幂等。
        ws_svc.delete(owner.id, ws.id).unwrap();

        ws_svc.restore(owner.id, ws.id).unwrap();
        assert!(!ws_svc.is_deleted(ws.id).unwrap());
        assert!(ws_svc.deleted_at(ws.id).unwrap().is_none());
        assert!(ws_svc.list_for(owner.id).unwrap()[0].2.is_none());
        // 重复恢复同样幂等。
        ws_svc.restore(owner.id, ws.id).unwrap();

        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws.id, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&AuditAction::WorkspaceDeleted));
        assert!(actions.contains(&AuditAction::WorkspaceRestored));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_unknown_workspace_is_not_found() {
        let dir = temp_dir("delete-missing");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let ws_svc = WorkspaceService::new(store.clone());
        assert!(matches!(
            ws_svc.delete(Ulid::new(), Ulid::new()),
            Err(AppError::NotFound)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn transfer_owner_swaps_roles_atomically_and_is_audited() {
        let dir = temp_dir("transfer");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let bob = auth.register("bob@x.io", "Bob", "Passw0rd!").unwrap();
        let carol = auth.register("carol@x.io", "Carol", "Passw0rd!").unwrap();
        let ws = ws_svc.create(owner.id, "团队", None, "").unwrap();
        ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Worker).unwrap();
        ws_svc.invite(owner.id, ws.id, "carol@x.io", WorkspaceRole::Maintainer).unwrap();
        ws_svc.accept_invite(bob.id, ws.id).unwrap();
        ws_svc.accept_invite(carol.id, ws.id).unwrap();

        let role_of = |id: Ulid| ws_svc.get_member(ws.id, id).unwrap().unwrap().role;
        let owners = |svc: &WorkspaceService| {
            svc.list_members(ws.id)
                .unwrap()
                .into_iter()
                .filter(|(m, _)| m.role == WorkspaceRole::Owner)
                .count()
        };

        // 非 Owner 发起被拒。
        assert!(matches!(
            ws_svc.transfer_owner(bob.id, ws.id, carol.id),
            Err(AppError::Forbidden)
        ));

        // 转给自己是 no-op。
        ws_svc.transfer_owner(owner.id, ws.id, owner.id).unwrap();
        assert_eq!(role_of(owner.id), WorkspaceRole::Owner);

        // 真正的转让：一步完成，不存在零 Owner 的中间态。
        ws_svc.transfer_owner(owner.id, ws.id, bob.id).unwrap();
        assert_eq!(role_of(bob.id), WorkspaceRole::Owner);
        assert_eq!(role_of(owner.id), WorkspaceRole::Maintainer);
        assert_eq!(owners(&ws_svc), 1);

        // 对方已是 Owner 时幂等。
        ws_svc.transfer_owner(bob.id, ws.id, bob.id).unwrap();
        assert_eq!(owners(&ws_svc), 1);

        // 目标不是成员 → NotFound。
        assert!(matches!(
            ws_svc.transfer_owner(bob.id, ws.id, Ulid::new()),
            Err(AppError::NotFound)
        ));

        // 审计：两条 RoleChanged（降 + 升）。
        let audit = crate::service::AuditService::new(store.clone());
        let role_changes = audit
            .list(ws.id, 100)
            .unwrap()
            .into_iter()
            .filter(|l| l.action == AuditAction::RoleChanged)
            .count();
        assert_eq!(role_changes, 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn member_invite_accept_role_change_and_remove() {
        let dir = temp_dir("members");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let bob = auth.register("bob@x.io", "Bob", "Passw0rd!").unwrap();
        let ws = ws_svc.create(owner.id, "团队", None, "").unwrap();

        // 初始只有 Owner。
        let members = ws_svc.list_members(ws.id).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].0.role, WorkspaceRole::Owner);

        // 邀请 → 接受，Bob 成为 Worker。
        ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Worker).unwrap();
        ws_svc.accept_invite(bob.id, ws.id).unwrap();
        assert_eq!(ws_svc.list_members(ws.id).unwrap().len(), 2);

        // 升为 Maintainer。
        ws_svc.update_role(owner.id, ws.id, bob.id, WorkspaceRole::Maintainer).unwrap();
        assert_eq!(
            ws_svc.get_member(ws.id, bob.id).unwrap().unwrap().role,
            WorkspaceRole::Maintainer
        );

        // 最后一名 Owner 不可降级 / 不可移除。
        assert!(ws_svc.update_role(owner.id, ws.id, owner.id, WorkspaceRole::Worker).is_err());
        assert!(ws_svc.remove_member(owner.id, ws.id, owner.id).is_err());

        // 移除 Bob 后仅剩 Owner。
        ws_svc.remove_member(owner.id, ws.id, bob.id).unwrap();
        assert_eq!(ws_svc.list_members(ws.id).unwrap().len(), 1);

        // 反向索引同步：Bob 的「我的工作空间」里不再包含它。
        assert!(ws_svc.list_for(bob.id).unwrap().is_empty());

        // 审计记录了成员操作。
        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws.id, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&AuditAction::MemberInvited));
        assert!(actions.contains(&AuditAction::MemberJoined));
        assert!(actions.contains(&AuditAction::RoleChanged));
        assert!(actions.contains(&AuditAction::MemberRemoved));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invite_only_becomes_membership_after_accept() {
        let dir = temp_dir("invite-pending");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let bob = auth.register("bob@x.io", "Bob", "Passw0rd!").unwrap();
        let ws = ws_svc.create(owner.id, "团队", None, "").unwrap();

        let inv = ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Worker).unwrap();
        assert_eq!(inv.account_id, bob.id);
        assert_eq!(inv.role, WorkspaceRole::Worker);
        assert_eq!(inv.invited_by, owner.id);

        // 待接受期间：不是成员，但双方都能看到这条邀请。
        assert!(ws_svc.get_member(ws.id, bob.id).unwrap().is_none());
        assert!(ws_svc.list_for(bob.id).unwrap().is_empty());
        assert_eq!(ws_svc.list_invites(ws.id).unwrap().len(), 1);
        let inbox = ws_svc.list_invites_for(bob.id).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].1.id, ws.id);

        // 同角色重复邀请幂等，不新增记录、不改写创建时间。
        let again = ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Worker).unwrap();
        assert_eq!(again.created_at, inv.created_at);
        assert_eq!(ws_svc.list_invites(ws.id).unwrap().len(), 1);

        // 改角色则改写同一条记录。
        ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Maintainer).unwrap();
        assert_eq!(
            ws_svc.get_invite(ws.id, bob.id).unwrap().unwrap().role,
            WorkspaceRole::Maintainer
        );
        assert_eq!(ws_svc.list_invites(ws.id).unwrap().len(), 1);

        // 未注册邮箱被拒。
        assert!(ws_svc.invite(owner.id, ws.id, "ghost@x.io", WorkspaceRole::Worker).is_err());

        // 接受后成为成员，待接受记录消失。
        let member = ws_svc.accept_invite(bob.id, ws.id).unwrap();
        assert_eq!(member.role, WorkspaceRole::Maintainer);
        assert_eq!(
            ws_svc.get_member(ws.id, bob.id).unwrap().unwrap().role,
            WorkspaceRole::Maintainer
        );
        assert!(ws_svc.get_invite(ws.id, bob.id).unwrap().is_none());
        assert!(ws_svc.list_invites(ws.id).unwrap().is_empty());
        assert!(ws_svc.list_invites_for(bob.id).unwrap().is_empty());
        assert_eq!(ws_svc.list_for(bob.id).unwrap().len(), 1);

        // 已是成员后不能再被邀请。
        assert!(ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Worker).is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decline_and_revoke_drop_the_pending_invite() {
        let dir = temp_dir("invite-drop");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let bob = auth.register("bob@x.io", "Bob", "Passw0rd!").unwrap();
        let carol = auth.register("carol@x.io", "Carol", "Passw0rd!").unwrap();
        let ws = ws_svc.create(owner.id, "团队", None, "").unwrap();

        // Bob 拒绝。
        ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Worker).unwrap();
        ws_svc.decline_invite(bob.id, ws.id).unwrap();
        assert!(ws_svc.get_invite(ws.id, bob.id).unwrap().is_none());
        assert!(ws_svc.list_invites(ws.id).unwrap().is_empty());
        assert!(ws_svc.list_invites_for(bob.id).unwrap().is_empty());
        assert!(ws_svc.get_member(ws.id, bob.id).unwrap().is_none());

        // Carol 的邀请被发起方撤销。
        ws_svc.invite(owner.id, ws.id, "carol@x.io", WorkspaceRole::Maintainer).unwrap();
        ws_svc.revoke_invite(owner.id, ws.id, carol.id).unwrap();
        assert!(ws_svc.get_invite(ws.id, carol.id).unwrap().is_none());
        assert!(ws_svc.list_invites_for(carol.id).unwrap().is_empty());

        // 没有待接受邀请时，拒绝 / 撤销都报 NotFound。
        assert!(matches!(ws_svc.decline_invite(bob.id, ws.id), Err(AppError::NotFound)));
        assert!(matches!(ws_svc.revoke_invite(owner.id, ws.id, bob.id), Err(AppError::NotFound)));

        let audit = crate::service::AuditService::new(store.clone());
        let actions: Vec<_> = audit.list(ws.id, 100).unwrap().into_iter().map(|l| l.action).collect();
        assert!(actions.contains(&AuditAction::InviteDeclined));
        assert!(actions.contains(&AuditAction::InviteRevoked));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn accept_requires_a_pending_invite_and_a_live_workspace() {
        let dir = temp_dir("invite-accept");
        let store = Arc::new(DocStore::open(&dir).unwrap());
        let auth = AuthService::new(store.clone(), Arc::new(crate::config::Config::default()));
        let ws_svc = WorkspaceService::new(store.clone());

        let owner = auth.register("owner@x.io", "Owner", "Passw0rd!").unwrap();
        let bob = auth.register("bob@x.io", "Bob", "Passw0rd!").unwrap();
        let ws = ws_svc.create(owner.id, "团队", None, "").unwrap();

        // 没有邀请 → NotFound。
        assert!(matches!(ws_svc.accept_invite(bob.id, ws.id), Err(AppError::NotFound)));

        // 邀请还在，但工作空间被删 → 拒绝，且邀请原样保留。
        ws_svc.invite(owner.id, ws.id, "bob@x.io", WorkspaceRole::Worker).unwrap();
        ws_svc.delete(owner.id, ws.id).unwrap();
        assert!(matches!(
            ws_svc.accept_invite(bob.id, ws.id),
            Err(AppError::InvalidQuery(_))
        ));
        assert!(ws_svc.get_member(ws.id, bob.id).unwrap().is_none());
        assert!(ws_svc.get_invite(ws.id, bob.id).unwrap().is_some());

        // 恢复后可接受。
        ws_svc.restore(owner.id, ws.id).unwrap();
        ws_svc.accept_invite(bob.id, ws.id).unwrap();
        assert!(ws_svc.get_member(ws.id, bob.id).unwrap().is_some());

        std::fs::remove_dir_all(&dir).ok();
    }
}
