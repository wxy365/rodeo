use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use crate::frontend::components::{
    action_label, audit_change, display_enum_value, logged_out, role_label, short_time,
    value_type_label,
};
use crate::frontend::graphql_client::{
    audit_logs, create_label_schema, delete_workspace, invite_member, invites, label_schemas,
    members, my_role, remove_member, restore_workspace, revoke_invite, transfer_owner,
    update_label_schema, update_member_role, update_view, update_workspace, views,
    workspace_by_slug, AuditLog, Invite, LabelSchema, Member, View, Workspace,
};
use crate::frontend::use_auth;
use crate::frontend::icons::{
    ic_add, ic_back, ic_close, ic_history, ic_profile, ic_setting, ic_share, ic_tag,
};

fn is_builtin(schema: &LabelSchema) -> bool {
    schema.name == "Task" || schema.name == "Bug"
}

#[component]
pub fn WorkspaceSettings() -> impl IntoView {
    let params = use_params_map();
    let slug = move || params.get().get("slug").unwrap_or_default();
    let auth = use_auth();
    let navigate = use_navigate();
    let nav_back = navigate.clone();
    // 存成 StoredValue（Copy），事件闭包才能保持 Copy 而被视图重复使用。
    let nav_store = StoredValue::new(navigate.clone());
    let back = move |_| nav_back(&format!("/{}", slug()), Default::default());

    let data: RwSignal<
        Option<
            Result<
                (
                    Workspace,
                    String,
                    Vec<LabelSchema>,
                    Vec<AuditLog>,
                    Vec<Member>,
                    Vec<Invite>,
                    Vec<View>,
                ),
                String,
            >,
        >,
    > = RwSignal::new(None);
    let refresh = RwSignal::new(0u32);
    let tab = RwSignal::new(String::from("general"));
    let error = RwSignal::new(None::<String>);

    // 基础信息表单：数据到位后回填一次；保存刷新后回填的是服务端已落库的值。
    let edit_name = RwSignal::new(String::new());
    let edit_desc = RwSignal::new(String::new());
    let edit_slug = RwSignal::new(String::new());
    let show_delete_confirm = RwSignal::new(false);
    // 待转让的对象 (account_id, email)；Some 时弹出确认框。
    let pending_transfer = RwSignal::new(None::<(String, String)>);

    // 新建标签表单
    let new_name = RwSignal::new(String::new());
    let new_title = RwSignal::new(String::new());
    let new_type = RwSignal::new(String::from("enum"));
    let new_enum = RwSignal::new(String::new());

    // 邀请成员表单
    let invite_email = RwSignal::new(String::new());
    let invite_role = RwSignal::new(String::from("worker"));

    Effect::new_sync(move |_| {
        let s = slug();
        let _ = refresh.get();
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        if logged_out() {
            navigate("/login", Default::default());
            return;
        }
        spawn_local(async move {
            let result = async {
                let ws = workspace_by_slug(&s).await?.ok_or("工作空间不存在".to_string())?;
                let role = my_role(&ws.id).await?;
                let schemas = label_schemas(&ws.id).await?;
                let logs = audit_logs(&ws.id).await?;
                let members = members(&ws.id).await?;
                let invite_list = invites(&ws.id).await?;
                let view_list = views(&ws.id).await?;
                Ok::<_, String>((ws, role, schemas, logs, members, invite_list, view_list))
            }
            .await;
            data.set(Some(result));
        });
    });

    // 加载完成后回填基础信息表单。
    Effect::new_sync(move |_| {
        if let Some(Ok((ws, _, _, _, _, _, _))) = data.get() {
            edit_name.set(ws.name.clone());
            edit_desc.set(ws.description.clone());
            edit_slug.set(ws.slug.clone());
        }
    });

    let save_general = move |ev: SubmitEvent| {
        ev.prevent_default();
        let Some((ws_id, cur_slug)) = data
            .get()
            .and_then(|r| r.ok())
            .map(|(w, _, _, _, _, _, _)| (w.id.clone(), w.slug.clone()))
        else {
            return;
        };
        let name = edit_name.get();
        let description = edit_desc.get();
        let typed_slug = edit_slug.get();
        // slug 没动就不传：服务端对「传了 slug」才做 Owner 校验，
        // Maintainer 只保存名称时不能因此被拦。
        let slug_arg = (typed_slug.trim() != cur_slug).then_some(typed_slug);
        let nav = nav_store.get_value();
        spawn_local(async move {
            match update_workspace(&ws_id, &name, &description, slug_arg.as_deref()).await {
                Ok(ws) => {
                    error.set(None);
                    if ws.slug != cur_slug {
                        // 地址变了，必须跳到新路由，否则当前页面同步的是旧 slug。
                        nav(&format!("/{}/settings", ws.slug), Default::default());
                    } else {
                        refresh.update(|x| *x += 1);
                    }
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let create = move |ev: SubmitEvent| {
        ev.prevent_default();
        let Some(ws_id) = data
            .get()
            .and_then(|r| r.ok())
            .map(|(w, _, _, _, _, _, _)| w.id.clone())
        else {
            return;
        };
        let n = new_name.get();
        let t = new_title.get();
        let vt = new_type.get();
        let evals: Vec<String> = new_enum
            .get()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        spawn_local(async move {
            match create_label_schema(&ws_id, &n, &t, &vt, &evals, None, &serde_json::json!([])).await {
                Ok(_) => {
                    new_name.set(String::new());
                    new_title.set(String::new());
                    new_enum.set(String::new());
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let ws_id_of = move || -> Option<String> {
        data.get()
            .and_then(|r| r.ok())
            .map(|(w, _, _, _, _, _, _)| w.id.clone())
    };

    let do_invite = move |ev: SubmitEvent| {
        ev.prevent_default();
        let Some(ws_id) = ws_id_of() else { return };
        let email = invite_email.get();
        let role = invite_role.get();
        if email.trim().is_empty() {
            error.set(Some("请填写要邀请的邮箱".to_string()));
            return;
        }
        spawn_local(async move {
            match invite_member(&ws_id, &email, &role).await {
                Ok(_) => {
                    invite_email.set(String::new());
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let change_role = Callback::new(move |(account_id, role): (String, String)| {
        let Some(ws_id) = ws_id_of() else { return };
        spawn_local(async move {
            match update_member_role(&ws_id, &account_id, &role).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    let do_remove = Callback::new(move |account_id: String| {
        let Some(ws_id) = ws_id_of() else { return };
        spawn_local(async move {
            match remove_member(&ws_id, &account_id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    let do_revoke = Callback::new(move |account_id: String| {
        let Some(ws_id) = ws_id_of() else { return };
        spawn_local(async move {
            match revoke_invite(&ws_id, &account_id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    let ask_transfer = Callback::new(move |(account_id, email): (String, String)| {
        pending_transfer.set(Some((account_id, email)));
    });

    let do_transfer = Callback::new(move |account_id: String| {
        let Some(ws_id) = ws_id_of() else { return };
        pending_transfer.set(None);
        spawn_local(async move {
            match transfer_owner(&ws_id, &account_id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    let toggle_shared = Callback::new(move |(id, shared): (String, bool)| {
        let Some(v) = data
            .get()
            .and_then(|r| r.ok())
            .and_then(|(_, _, _, _, _, _, vs)| vs.into_iter().find(|v| v.id == id))
        else {
            return;
        };
        spawn_local(async move {
            match update_view(
                &v.id,
                &v.name,
                &v.query,
                &v.sort.field,
                v.sort.desc,
                &v.columns,
                shared,
                &v.title_colors,
            )
            .await
            {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    // 删除 / 恢复（危险操作）。两者都需 Owner，权限由服务端再校一遍。
    let do_delete = Callback::new(move |_: ()| {
        let Some(ws_id) = ws_id_of() else { return };
        show_delete_confirm.set(false);
        spawn_local(async move {
            match delete_workspace(&ws_id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    let do_restore = Callback::new(move |_: ()| {
        let Some(ws_id) = ws_id_of() else { return };
        spawn_local(async move {
            match restore_workspace(&ws_id).await {
                Ok(_) => {
                    error.set(None);
                    refresh.update(|x| *x += 1);
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    view! {
        <div class="page">
            <div class="crumb" style="display:flex;align-items:center;gap:8px">
                <button class="btn sm" on:click=back>{ic_back()}"返回工作空间"</button>
                <span>{move || format!("/{} · 设置（Maintainer 及以上）", slug())}</span>
            </div>
            <div class="set-layout">
                <aside class="panel set-nav">
                    <div class="grp" style="padding:8px 12px 4px;font-size:12px;color:var(--ink3)">
                        {move || data.get().and_then(|r| r.ok()).map(|(w, _, _, _, _, _, _)| w.name.clone()).unwrap_or_default()}
                        " · 设置"
                    </div>
                    <div class="it" class:on=move || tab.get() == "general" on:click=move |_| tab.set("general".into())>
                        {ic_setting()}"基础信息"
                    </div>
                    <div class="it" class:on=move || tab.get() == "members" on:click=move |_| tab.set("members".into())>
                        {ic_profile()}"成员"
                    </div>
                    <div class="it" class:on=move || tab.get() == "labels" on:click=move |_| tab.set("labels".into())>
                        {ic_tag()}"标签定义"
                    </div>
                    <div class="it" class:on=move || tab.get() == "views" on:click=move |_| tab.set("views".into())>
                        {ic_share()}"视图共享"
                    </div>
                    <div class="it" class:on=move || tab.get() == "audit" on:click=move |_| tab.set("audit".into())>
                        {ic_history()}"审计日志"
                    </div>
                    <div class="it dgr" class:on=move || tab.get() == "danger" on:click=move |_| tab.set("danger".into())>
                        "危险操作"
                    </div>
                </aside>

                <div class="panel set-body">
                    {move || error.get().map(|e| view! { <p class="error">{e}</p> })}

                    {move || match data.get() {
                        None => view! { <div class="empty">"加载中…"</div> }.into_any(),
                        Some(Err(e)) => view! { <div class="empty error">{e.clone()}</div> }.into_any(),
                        Some(Ok((_ws, role, schemas, logs, member_list, invite_list, view_list))) => {
                            let can_manage = role == "owner" || role == "maintainer";
                            let is_owner = role == "owner";
                            let cur_tab = tab.get();
                            if cur_tab == "general" {
                                view! {
                                    <h2>"基础信息"</h2>
                                    <p class="mut">"名称与描述 Maintainer 及以上可改；地址（slug）是工作空间的 URL 身份，变更需 Owner，且会让既有链接失效。"</p>
                                    <form class="stack" on:submit=save_general>
                                        <label class="fld">
                                            <span>"名称"</span>
                                            <input class="inp" prop:value=edit_name disabled=!can_manage
                                                on:input=move |ev| edit_name.set(event_target_value(&ev)) />
                                        </label>
                                        <label class="fld">
                                            <span>"描述"</span>
                                            <textarea class="inp" rows="3" prop:value=edit_desc disabled=!can_manage
                                                on:input=move |ev| edit_desc.set(event_target_value(&ev))></textarea>
                                        </label>
                                        <label class="fld">
                                            <span>"地址（slug）"</span>
                                            <input class="inp" prop:value=edit_slug disabled=!is_owner
                                                on:input=move |ev| edit_slug.set(event_target_value(&ev)) />
                                        </label>
                                        {if can_manage {
                                            view! {
                                                <button class="btn pri" type="submit" style="align-self:flex-start">"保存"</button>
                                            }.into_any()
                                        } else {
                                            view! { <p class="mut">"仅 Maintainer 及以上可修改"</p> }.into_any()
                                        }}
                                    </form>
                                }.into_any()
                            } else if cur_tab == "members" {
                                view! {
                                    <h2>"成员管理"</h2>
                                    {if can_manage {
                                        view! {
                                            <form class="invite" on:submit=do_invite>
                                                <input class="inp" placeholder="邮箱（须已注册）" prop:value=invite_email
                                                    on:input=move |ev| invite_email.set(event_target_value(&ev)) />
                                                <select class="inp" style="width:140px" prop:value=invite_role
                                                    on:change=move |ev| invite_role.set(event_target_value(&ev))>
                                                    <option value="reader">"Reader（只读）"</option>
                                                    <option value="worker">"Worker（可编辑条目）"</option>
                                                    <option value="maintainer">"Maintainer（可管理）"</option>
                                                    <option value="owner">"Owner（所有者）"</option>
                                                </select>
                                                <button class="btn pri" type="submit">{ic_add()}"邀请"</button>
                                            </form>
                                        }.into_any()
                                    } else {
                                        view! { <p class="mut">"仅 Maintainer 及以上可管理成员"</p> }.into_any()
                                    }}
                                    <table class="tbl">
                                        <thead><tr><th>"邮箱"</th><th>"姓名"</th><th>"角色"</th><th>"加入时间"</th><th style="width:60px"></th></tr></thead>
                                        <tbody>
                                            {let me = auth.user.get().map(|u| u.id).unwrap_or_default();
                                            member_list.into_iter().map(|m| {
                                                // 只有 Owner 能转让，且不能转给自己（那没有意义）。
                                                let can_transfer = is_owner && m.account_id != me;
                                                member_row(m, can_manage, can_transfer, change_role, do_remove, ask_transfer)
                                            }).collect::<Vec<_>>()}
                                        </tbody>
                                    </table>
                                    <div class="mut">"权限：Owner > Maintainer > Worker > Reader；工作空间至少保留一名 Owner。"</div>
                                    {(!invite_list.is_empty()).then(move || view! {
                                        <h3 style="margin:18px 0 8px">"待接受的邀请"</h3>
                                        <table class="tbl">
                                            <thead><tr><th>"邮箱"</th><th>"姓名"</th><th>"角色"</th><th>"邀请时间"</th><th style="width:60px"></th></tr></thead>
                                            <tbody>
                                                {invite_list.into_iter().map(|inv| invite_row(inv, can_manage, do_revoke)).collect::<Vec<_>>()}
                                            </tbody>
                                        </table>
                                        <div class="mut">"对方接受后才成为成员；撤销会直接删掉这条邀请。"</div>
                                    })}
                                }.into_any()
                            } else if cur_tab == "labels" {
                                view! {
                                    <div style="display:flex;align-items:center">
                                        <h2 style="margin-right:auto">"标签定义（LabelSchema）"</h2>
                                    </div>
                                    {if can_manage {
                                        view! {
                                            <form class="invite" on:submit=create>
                                                <input class="inp" placeholder="名称（不可改，如 Priority）" prop:value=new_name on:input=move |ev| new_name.set(event_target_value(&ev)) />
                                                <input class="inp" placeholder="显示名称" prop:value=new_title on:input=move |ev| new_title.set(event_target_value(&ev)) />
                                                <select class="inp" style="width:120px" prop:value=new_type on:change=move |ev| new_type.set(event_target_value(&ev))>
                                                    <option value="null">"Null（无值）"</option>
                                                    <option value="enum">"Enum"</option>
                                                    <option value="string">"String"</option>
                                                    <option value="boolean">"Boolean"</option>
                                                    <option value="integer">"Integer"</option>
                                                    <option value="float">"Float"</option>
                                                </select>
                                                <input class="inp" placeholder="枚举值（逗号分隔）" prop:value=new_enum on:input=move |ev| new_enum.set(event_target_value(&ev)) />
                                                <button class="btn pri" type="submit">{ic_add()}"新建标签"</button>
                                            </form>
                                        }.into_any()
                                    } else {
                                        view! { <p class="mut">"仅 Maintainer 及以上可管理标签"</p> }.into_any()
                                    }}
                                    <table class="tbl">
                                        <thead><tr><th>"name（不可改）"</th><th>"title"</th><th>"值类型"</th><th>"可选值 / 说明"</th><th>"颜色"</th><th>"来源"</th><th style="width:80px"></th></tr></thead>
                                        <tbody>
                                            {schemas.iter().map(|s| schema_row(s, can_manage, _ws.id.clone(), refresh, error)).collect::<Vec<_>>()}
                                        </tbody>
                                    </table>
                                    <div class="mut">"同一 Entry 对同一 Schema 仅一条 Labeling，更新即 upsert；变更自动记录操作人与时间。"</div>
                                }.into_any()
                            } else if cur_tab == "audit" {
                                view! {
                                    <h2>"审计日志"</h2>
                                    <table class="tbl">
                                        <thead><tr><th>"时间"</th><th>"操作"</th><th>"变更内容"</th><th>"资源"</th></tr></thead>
                                        <tbody>
                                            {if logs.is_empty() {
                                                view! { <tr><td colspan="4" class="empty">"暂无记录"</td></tr> }.into_any()
                                            } else {
                                                logs.iter().map(|l| view! {
                                                    <tr class="static">
                                                        <td class="mut">{short_time(&l.at)}</td>
                                                        <td>{action_label(&l.action)}</td>
                                                        <td class="mut">{audit_change(l.before.as_deref(), l.after.as_deref())}</td>
                                                        <td class="code">{format!("{} {}", l.resource_type.clone(), l.resource_id.clone())}</td>
                                                    </tr>
                                                }).collect::<Vec<_>>().into_any()
                                            }}
                                        </tbody>
                                    </table>
                                }.into_any()
                            } else if cur_tab == "views" {
                                // 视图共享：所有视图汇总一处，Owner 邮箱由成员列表就地映射。
                                let email_of = |account_id: &str| -> String {
                                    member_list
                                        .iter()
                                        .find(|m| m.account_id == account_id)
                                        .map(|m| m.email.clone())
                                        .unwrap_or_else(|| account_id.to_string())
                                };
                                view! {
                                    <h2>"视图共享"</h2>
                                    <table class="tbl">
                                        <thead><tr><th>"视图名称"</th><th>"所有者"</th><th>"条目"</th><th>"共享给工作空间"</th></tr></thead>
                                        <tbody>
                                            {view_list.into_iter().map(|v| {
                                                let owner = email_of(&v.owner_id);
                                                let is_default = v.is_default;
                                                let id = v.id.clone();
                                                view! {
                                                    <tr class="static">
                                                        <td>
                                                            {v.name.clone()}
                                                            {is_default.then(|| view! { <span class="chip dim" style="font-size:11px;margin-left:6px">"默认"</span> })}
                                                        </td>
                                                        <td class="mut">{owner}</td>
                                                        <td class="mut">{v.entry_count}</td>
                                                        <td>
                                                            <label style="display:flex;align-items:center;gap:6px">
                                                                <input type="checkbox" prop:checked=v.is_shared disabled=!can_manage || is_default
                                                                    on:change=move |ev| toggle_shared.run((id.clone(), event_target_checked(&ev))) />
                                                                {if is_default {
                                                                    view! { <span class="mut">"默认视图始终共享"</span> }.into_any()
                                                                } else {
                                                                    ().into_any()
                                                                }}
                                                            </label>
                                                        </td>
                                                    </tr>
                                                }
                                            }).collect::<Vec<_>>()}
                                        </tbody>
                                    </table>
                                    <div class="mut">"共享视图对工作空间内所有人可见；个人视图仅所有者本人可见。"</div>
                                }.into_any()
                            } else if cur_tab == "danger" {
                                let deleted_at = _ws.deleted_at.clone();
                                view! {
                                    <h2>"危险操作"</h2>
                                    {match deleted_at {
                                        Some(at) => view! {
                                            <div class="danger-box">
                                                <p><b>"此工作空间已被删除。"</b>
                                                    <span class="mut">{format!("（{}）", short_time(&at))}</span>
                                                </p>
                                                <p class="mut">"条目、标签、视图、成员等数据全部保留；恢复后立即重新出现在工作空间列表中。"</p>
                                                {if is_owner {
                                                    view! {
                                                        <button class="btn pri" on:click=move |_| do_restore.run(())>"恢复工作空间"</button>
                                                    }.into_any()
                                                } else {
                                                    view! { <p class="mut">"仅 Owner 可恢复。"</p> }.into_any()
                                                }}
                                            </div>
                                        }.into_any(),
                                        None => view! {
                                            <div class="danger-box">
                                                <p><b>"删除工作空间"</b></p>
                                                <p class="mut">"软删除：工作空间会移入工作空间列表的「回收站」，数据全部保留，可随时恢复。"</p>
                                                {if is_owner {
                                                    view! {
                                                        <button class="btn dgr" on:click=move |_| show_delete_confirm.set(true)>"删除工作空间"</button>
                                                    }.into_any()
                                                } else {
                                                    view! { <p class="mut">"仅 Owner 可删除。"</p> }.into_any()
                                                }}
                                            </div>
                                        }.into_any(),
                                    }}
                                }.into_any()
                            } else {
                                view! {
                                    <h2>"即将上线"</h2>
                                    <p class="mut">"敬请期待。"</p>
                                }.into_any()
                            }
                        }
                    }}
                </div>
            </div>

            {move || pending_transfer.get().map(|(account_id, email)| view! {
                <div class="dmodal" on:click=move |_| pending_transfer.set(None)>
                    <div class="panel dmbox" on:click=|ev| ev.stop_propagation()>
                        <h3>"转让所有权？"</h3>
                        <p class="mut">{format!("转让后 {} 成为 Owner，你降为 Maintainer。对方可以再转回给你。", email)}</p>
                        <div style="display:flex;gap:8px;justify-content:flex-end">
                            <button class="btn" on:click=move |_| pending_transfer.set(None)>"取消"</button>
                            <button class="btn pri" on:click=move |_| do_transfer.run(account_id.clone())>"确认转让"</button>
                        </div>
                    </div>
                </div>
            })}

            {move || show_delete_confirm.get().then(|| {
                let ws_name = data
                    .get()
                    .and_then(|r| r.ok())
                    .map(|(w, _, _, _, _, _, _)| w.name)
                    .unwrap_or_default();
                view! {
                    <div class="dmodal" on:click=move |_| show_delete_confirm.set(false)>
                        <div class="panel dmbox" on:click=|ev| ev.stop_propagation()>
                            <h3>"删除工作空间？"</h3>
                            <p class="mut">{format!("「{}」会被移入回收站，条目、标签、视图、成员全部保留，可随时恢复。", ws_name)}</p>
                            <div style="display:flex;gap:8px;justify-content:flex-end">
                                <button class="btn" on:click=move |_| show_delete_confirm.set(false)>"取消"</button>
                                <button class="btn dgr" on:click=move |_| do_delete.run(())>"确认删除"</button>
                            </div>
                        </div>
                    </div>
                }
            })}
        </div>
    }
}

/// 单个成员行：邮箱 / 姓名 / 角色下拉 / 加入时间 / 转让 / 移除按钮。
fn member_row(
    m: Member,
    can_manage: bool,
    can_transfer: bool,
    on_role: Callback<(String, String)>,
    on_remove: Callback<String>,
    on_transfer: Callback<(String, String)>,
) -> impl IntoView {
    let account_id = m.account_id.clone();
    let remove_id = m.account_id.clone();
    let transfer_id = m.account_id.clone();
    let transfer_email = m.email.clone();
    view! {
        <tr class="static">
            <td>{m.email.clone()}</td>
            <td class="mut">{m.name.clone()}</td>
            <td>
                <select class="inp" style="width:150px" prop:value=m.role.clone() disabled=!can_manage
                    on:change=move |ev| on_role.run((account_id.clone(), event_target_value(&ev)))>
                    <option value="reader">"Reader（只读）"</option>
                    <option value="worker">"Worker（可编辑条目）"</option>
                    <option value="maintainer">"Maintainer（可管理）"</option>
                    <option value="owner">"Owner（所有者）"</option>
                </select>
            </td>
            <td class="mut">{short_time(&m.joined_at)}</td>
            <td style="display:flex;gap:6px;align-items:center">
                {can_transfer.then(|| view! {
                    <button class="btn sm" title="把所有权转让给对方，自己降为 Maintainer"
                        on:click=move |_| on_transfer.run((transfer_id.clone(), transfer_email.clone()))>
                        "转让"
                    </button>
                })}
                <button class="ibtn" title="移除成员" disabled=!can_manage
                    on:click=move |_| on_remove.run(remove_id.clone())>{ic_close()}</button>
            </td>
        </tr>
    }
}

/// 单个待接受邀请行：邮箱 / 姓名 / 角色 / 邀请时间 / 撤销按钮。
/// 角色此处只读——要换角色，撤销后重新邀请即可。
fn invite_row(inv: Invite, can_manage: bool, on_revoke: Callback<String>) -> impl IntoView {
    let account_id = inv.account_id.clone();
    view! {
        <tr class="static">
            <td>{inv.email.clone()}</td>
            <td class="mut">{inv.name.clone()}</td>
            <td class="mut">{role_label(&inv.role)}</td>
            <td class="mut">{short_time(&inv.created_at)}</td>
            <td>
                <button class="ibtn" title="撤销邀请" disabled=!can_manage
                    on:click=move |_| on_revoke.run(account_id.clone())>{ic_close()}</button>
            </td>
        </tr>
    }
}

/// 值色编辑行状态（`RwSignal` 便于逐字段就地更新）。
#[derive(Clone, Copy)]
struct VcRow {
    id: usize,
    color: RwSignal<String>,
    min: RwSignal<String>,
    max: RwSignal<String>,
    value: RwSignal<String>,
}

/// JSON 数值 → 编辑框文本（整数不带小数点）。
fn fmt_num(n: &serde_json::Number) -> String {
    if let Some(f) = n.as_f64() {
        if f.fract() == 0.0 && f.abs() < 9e15 {
            return format!("{}", f as i64);
        }
        return f.to_string();
    }
    n.to_string()
}

/// 编辑框文本 → JSON 数值（空/非法为 null）。
fn num_value(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() {
        return Value::Null;
    }
    if let Ok(i) = t.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = t.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::Null
}

/// 值色行 → `{color,min,max,value}` JSON 数组。
fn build_value_colors(rows: Vec<VcRow>, value_type: &str) -> Value {
    let arr: Vec<Value> = rows
        .into_iter()
        .filter_map(|r| {
            let color = r.color.get();
            if color.is_empty() {
                return None;
            }
            let mut obj = serde_json::Map::new();
            obj.insert("color".to_string(), Value::String(color));
            if value_type == "enum" {
                let v = r.value.get();
                obj.insert(
                    "value".to_string(),
                    if v.is_empty() {
                        Value::Null
                    } else {
                        Value::String(v)
                    },
                );
                obj.insert("min".to_string(), Value::Null);
                obj.insert("max".to_string(), Value::Null);
            } else {
                obj.insert("value".to_string(), Value::Null);
                obj.insert("min".to_string(), num_value(&r.min.get()));
                obj.insert("max".to_string(), num_value(&r.max.get()));
            }
            Some(Value::Object(obj))
        })
        .collect();
    Value::Array(arr)
}

fn schema_row(
    s: &LabelSchema,
    can_manage: bool,
    ws_id: String,
    refresh: RwSignal<u32>,
    error: RwSignal<Option<String>>,
) -> impl IntoView {
    let builtin = is_builtin(s);
    let name = s.name.clone();
    let value_type = s.value_type.clone();
    let enum_str = s.enum_values.join(",");

    let title_input = RwSignal::new(s.title.clone());
    let enum_input = RwSignal::new(enum_str.clone());
    let base_color = RwSignal::new(s.color.clone());

    let editable = !builtin && can_manage;
    let is_numeric = value_type == "integer" || value_type == "float";
    let vc_enabled = is_numeric || value_type == "enum";

    let init_rows: Vec<VcRow> = s
        .value_colors
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .map(|(i, v)| VcRow {
            id: i,
            color: RwSignal::new(
                v.get("color")
                    .and_then(|c| c.as_str())
                    .unwrap_or("#3b82f6")
                    .to_string(),
            ),
            min: RwSignal::new(
                v.get("min")
                    .and_then(|m| m.as_number())
                    .map(fmt_num)
                    .unwrap_or_default(),
            ),
            max: RwSignal::new(
                v.get("max")
                    .and_then(|m| m.as_number())
                    .map(fmt_num)
                    .unwrap_or_default(),
            ),
            value: RwSignal::new(
                v.get("value")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
        })
        .collect();
    let next_id = RwSignal::new(init_rows.len());
    let vc_rows = RwSignal::new(init_rows);
    // 无未保存改动时「保存」置灰：保存成功后整行按服务端数据重建，按钮自动回到灰态，
    // 用户据此确认改动已落库。
    let dirty = RwSignal::new(false);

    let enum_opts = s.enum_values.clone();
    let enum_add = s.enum_values.clone();
    let vt_add = value_type.clone();

    let add_row = move |_| {
        dirty.set(true);
        let id = next_id.get_untracked();
        next_id.set(id + 1);
        let default_value = if vt_add == "enum" {
            enum_add.first().cloned().unwrap_or_default()
        } else {
            String::new()
        };
        vc_rows.update(|rows| {
            rows.push(VcRow {
                id,
                color: RwSignal::new("#3b82f6".to_string()),
                min: RwSignal::new(String::new()),
                max: RwSignal::new(String::new()),
                value: RwSignal::new(default_value),
            })
        });
    };

    let swatch = move |sig: RwSignal<String>| {
        view! {
            <input
                type="color"
                class="sw sm"
                disabled=!editable
                prop:value=move || {
                    let c = sig.get();
                    if c.is_empty() { "#3b82f6".to_string() } else { c }
                }
                on:input=move |ev| {
                    sig.set(event_target_value(&ev));
                    dirty.set(true);
                }
            />
        }
    };

    let ops = move |r: VcRow| {
        view! {
            <button class="vc-op" title="上移" disabled=!editable on:click=move |_| {
                dirty.set(true);
                vc_rows.update(|rows| {
                    if let Some(i) = rows.iter().position(|x| x.id == r.id) {
                        if i > 0 { rows.swap(i, i - 1); }
                    }
                });
            }>"↑"</button>
            <button class="vc-op" title="下移" disabled=!editable on:click=move |_| {
                dirty.set(true);
                vc_rows.update(|rows| {
                    if let Some(i) = rows.iter().position(|x| x.id == r.id) {
                        if i + 1 < rows.len() { rows.swap(i, i + 1); }
                    }
                });
            }>"↓"</button>
            <button class="vc-op vc-del" title="删除" disabled=!editable on:click=move |_| {
                dirty.set(true);
                vc_rows.update(|rows| rows.retain(|x| x.id != r.id));
            }>"×"</button>
        }
    };

    view! {
        <tr class="static">
            <td class="code">{name.clone()}</td>
            <td>
                {if builtin {
                    view! { <span>{s.title.clone()}</span> }.into_any()
                } else if can_manage {
                    view! {
                        <input class="inp" style="width:100%" prop:value=title_input on:input=move |ev| {
                            title_input.set(event_target_value(&ev));
                            dirty.set(true);
                        } />
                    }.into_any()
                } else {
                    view! { <span>{s.title.clone()}</span> }.into_any()
                }}
            </td>
            <td class="mut">{value_type_label(&value_type)}</td>
            <td>
                {if value_type == "enum" {
                    if builtin {
                        view! { <span class="mut">{enum_str.clone()}</span> }.into_any()
                    } else if can_manage {
                        view! {
                            <input class="inp" style="width:100%" placeholder="逗号分隔" prop:value=enum_input on:input=move |ev| {
                                enum_input.set(event_target_value(&ev));
                                dirty.set(true);
                            } />
                        }.into_any()
                    } else {
                        view! { <span class="mut">{enum_str.clone()}</span> }.into_any()
                    }
                } else {
                    view! { <span class="mut">"—"</span> }.into_any()
                }}
            </td>
            <td>
                <div class="color-cell">
                    <div class="color-base">
                        <input
                            type="color"
                            class="sw"
                            disabled=!editable
                            prop:value=move || base_color.get().unwrap_or_else(|| "#3b82f6".to_string())
                            on:input=move |ev| {
                                base_color.set(Some(event_target_value(&ev)));
                                dirty.set(true);
                            }
                        />
                        <span class="code" style="font-size:11px">
                            {move || base_color.get().unwrap_or_else(|| "未设置".to_string())}
                        </span>
                        <button class="vc-op" title="清除基础色" disabled=!editable on:click=move |_| {
                            base_color.set(None);
                            dirty.set(true);
                        }>"清除"</button>
                    </div>
                    {if vc_enabled {
                        view! {
                            <div class="vc-list">
                                <For
                                    each=move || vc_rows.get()
                                    key=|r| r.id
                                    children=move |r: VcRow| {
                                        if is_numeric {
                                            view! {
                                                <div class="vc-row">
                                                    <input class="inp vc-num" type="number" placeholder="最小" disabled=!editable
                                                        prop:value=move || r.min.get()
                                                        on:input=move |ev| {
                                                            r.min.set(event_target_value(&ev));
                                                            dirty.set(true);
                                                        } />
                                                    <input class="inp vc-num" type="number" placeholder="最大" disabled=!editable
                                                        prop:value=move || r.max.get()
                                                        on:input=move |ev| {
                                                            r.max.set(event_target_value(&ev));
                                                            dirty.set(true);
                                                        } />
                                                    {swatch(r.color)}
                                                    {ops(r)}
                                                </div>
                                            }.into_any()
                                        } else {
                                            let evs = enum_opts.clone();
                                            view! {
                                                <div class="vc-row">
                                                    <select class="inp vc-enum" disabled=!editable
                                                        prop:value=move || r.value.get()
                                                        on:change=move |ev| {
                                                            r.value.set(event_target_value(&ev));
                                                            dirty.set(true);
                                                        }>
                                                        <option value="">"（选择枚举值）"</option>
                                                        {evs.iter().cloned().map(|o| view! {
                                                            <option value=o.clone()>{display_enum_value(&o)}</option>
                                                        }).collect::<Vec<_>>()}
                                                    </select>
                                                    {swatch(r.color)}
                                                    {ops(r)}
                                                </div>
                                            }.into_any()
                                        }
                                    }
                                />
                                <button class="btn sm" disabled=!editable on:click=add_row>"＋ 值色"</button>
                            </div>
                        }.into_any()
                    } else {
                        view! { <span></span> }.into_any()
                    }}
                </div>
            </td>
            <td>
                {if builtin {
                    view! { <span class="chip dim">"内置"</span> }.into_any()
                } else {
                    view! { <span class="chip c-open">"自定义"</span> }.into_any()
                }}
            </td>
            <td>
                {if editable {
                    let ws2 = ws_id.clone();
                    let nm = name.clone();
                    let vt_save = value_type.clone();
                    view! {
                        <button class="btn rowact" disabled=move || !dirty.get() on:click=move |_| {
                            let evals: Vec<String> = enum_input.get().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                            let t = title_input.get();
                            let ws = ws2.clone();
                            let n = nm.clone();
                            let clr = base_color.get();
                            let vcs = build_value_colors(vc_rows.get(), &vt_save);
                            spawn_local(async move {
                                if let Err(e) = update_label_schema(&ws, &n, &t, &evals, clr.as_deref(), &vcs).await {
                                    error.set(Some(e));
                                }
                                refresh.update(|x| *x += 1);
                            });
                        }>"保存"</button>
                    }.into_any()
                } else {
                    view! { <span></span> }.into_any()
                }}
            </td>
        </tr>
    }
}
