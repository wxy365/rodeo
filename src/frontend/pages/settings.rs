use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde_json::Value;

use crate::frontend::ai_prompt_editor::{rows_from, rows_to_value, PromptRow, PromptRows};
use crate::frontend::automation_tab::AutomationTab;
use crate::frontend::components::{
    action_label, audit_change, display_enum_value, logged_out, role_label, short_time,
    value_type_label, ColorPick, DefaultValueInput, FormatSelect,
};
use crate::frontend::graphql_client::{
    audit_logs, create_label_schema, delete_workspace, invite_member, invites, label_attrs,
    label_schemas, members, my_role, remove_member, restore_workspace, revoke_invite,
    transfer_owner, update_label_schema, update_member_role, update_view, update_workspace,
    update_workspace_ai_config, views, workspace_ai_config, workspace_by_slug, AuditLog, Invite,
    LabelSchema, Member, View, Workspace,
};
use crate::frontend::use_auth;
use crate::frontend::icons::{
    ic_add, ic_back, ic_close, ic_comment, ic_history, ic_link, ic_profile, ic_setting, ic_share,
    ic_tag,
};

fn is_builtin(schema: &LabelSchema) -> bool {
    schema.name == "Task" || schema.name == "Bug"
}

/// 「枚举值（逗号分隔）」输入框 → 可选值列表（去空白、丢空项）。
fn enum_values_of(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
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
    // 新类型属性：enum 的多选、时间型的展示格式、金额的符号 / 单位。
    let new_multi = RwSignal::new(false);
    let new_format = RwSignal::new(String::new());
    let new_symbol = RwSignal::new(String::new());
    let new_unit = RwSignal::new(String::new());
    // 值默认值：JSON，形状同条目打标的 LabelValue（Null 表示没设）。
    let new_default = RwSignal::new(Value::Null);

    // 邀请成员表单
    let invite_email = RwSignal::new(String::new());
    let invite_role = RwSignal::new(String::from("worker"));

    // ---- AI 总结配置 ----
    // 打开标签页时才拉数据，和「已归档」弹窗一个路数。
    let ai_scenarios = RwSignal::new(Vec::<PromptRow>::new());
    let ai_tones = RwSignal::new(Vec::<PromptRow>::new());
    let ai_loading = RwSignal::new(false);
    let ai_busy = RwSignal::new(false);
    let ai_msg = RwSignal::new(None::<String>);
    let ai_error = RwSignal::new(None::<String>);

    Effect::new_sync(move |_| {
        let s = slug();
        let _ = refresh.get();
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        if logged_out() || auth.session_lost.get() {
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
        // 服务端整体替换 attrs，必须传齐全部键（label_attrs 已保证）。
        // 每个属性只在所属类型下组装：否则切类型时会把手滑填的值一起写进库
        //（如先填日期布局再改成金额），而列表里那类属性不展示，用户再也清不掉。
        // 按类型过滤而非在 on:change 里清信号，是为了保住用户切回原类型时已填的内容。
        // 空串（含纯空白）归一为 None，避免把空白写进库。
        let is_time = matches!(vt.as_str(), "date" | "time" | "datetime");
        let is_currency = vt == "currency";
        let attrs = {
            let fv = new_format.get();
            let sv = new_symbol.get();
            let uv = new_unit.get();
            let f = fv.trim();
            let sy = sv.trim();
            let u = uv.trim();
            label_attrs(
                vt == "enum" && new_multi.get(),
                if is_time && !f.is_empty() { Some(f) } else { None },
                if is_currency && !sy.is_empty() { Some(sy) } else { None },
                if is_currency && !u.is_empty() { Some(u) } else { None },
                // 默认值随类型走：切类型时已清空（见类型下拉框的 on:change）。
                &new_default.get(),
                // 新建时没有关系可建：对方标签要先存在，故只能在建好之后于行内补。
                &serde_json::json!([]),
            )
        };
        spawn_local(async move {
            match create_label_schema(&ws_id, &n, &t, &vt, &evals, &attrs, None, &serde_json::json!([])).await {
                Ok(_) => {
                    new_name.set(String::new());
                    new_title.set(String::new());
                    new_enum.set(String::new());
                    new_multi.set(false);
                    new_format.set(String::new());
                    new_symbol.set(String::new());
                    new_unit.set(String::new());
                    new_default.set(Value::Null);
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

    let load_ai = move |ws_id: String| {
        ai_loading.set(true);
        ai_error.set(None);
        spawn_local(async move {
            match workspace_ai_config(&ws_id).await {
                Ok(cfg) => {
                    ai_scenarios.set(rows_from(&cfg.scenarios));
                    ai_tones.set(rows_from(&cfg.tones));
                }
                Err(e) => ai_error.set(Some(e)),
            }
            ai_loading.set(false);
        });
    };

    let save_ai = move |ev: SubmitEvent| {
        ev.prevent_default();
        let Some(ws_id) = ws_id_of() else { return };
        let scenarios = Value::Array(rows_to_value(&ai_scenarios));
        let tones = Value::Array(rows_to_value(&ai_tones));
        ai_busy.set(true);
        ai_error.set(None);
        ai_msg.set(None);
        spawn_local(async move {
            match update_workspace_ai_config(&ws_id, &scenarios, &tones).await {
                Ok(cfg) => {
                    // 回填服务端落库后的结果：空行被丢掉、名称被 trim，界面应与库内一致。
                    ai_scenarios.set(rows_from(&cfg.scenarios));
                    ai_tones.set(rows_from(&cfg.tones));
                    ai_msg.set(Some("已保存".to_string()));
                }
                Err(e) => ai_error.set(Some(e)),
            }
            ai_busy.set(false);
        });
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
                &v.sorts,
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
        <div class="page page-wide">
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
                    <div class="it" class:on=move || tab.get() == "ai"
                        on:click=move |_| {
                            tab.set("ai".into());
                            if let Some(ws_id) = ws_id_of() {
                                load_ai(ws_id);
                            }
                        }>
                        {ic_comment()}"AI 总结"
                    </div>
                    <div class="it" class:on=move || tab.get() == "views" on:click=move |_| tab.set("views".into())>
                        {ic_share()}"视图共享"
                    </div>
                    <div class="it" class:on=move || tab.get() == "automation" on:click=move |_| tab.set("automation".into())>
                        {ic_history()}"自动化"
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
                                                <select class="inp" style="width:120px" prop:value=new_type on:change=move |ev| {
                                                    new_type.set(event_target_value(&ev));
                                                    // 默认值的形状随类型而变，切类型先清掉，免得把上一个类型的值写到新类型下。
                                                    new_default.set(Value::Null);
                                                }>
                                                    <option value="null">"Null（无值）"</option>
                                                    <option value="enum">"Enum"</option>
                                                    <option value="string">"String"</option>
                                                    <option value="boolean">"Boolean"</option>
                                                    <option value="integer">"Integer"</option>
                                                    <option value="float">"Float"</option>
                                                    <option value="date">"日期"</option>
                                                    <option value="time">"时间"</option>
                                                    <option value="datetime">"日期时间"</option>
                                                    <option value="currency">"金额"</option>
                                                    <option value="email">"邮箱"</option>
                                                    <option value="account">"账号"</option>
                                                </select>
                                                {move || {
                                                    let vt = new_type.get();
                                                    if vt == "enum" {
                                                        view! {
                                                            <input class="inp" placeholder="枚举值（逗号分隔）" prop:value=new_enum
                                                                on:input=move |ev| new_enum.set(event_target_value(&ev)) />
                                                            <label style="display:flex;align-items:center;gap:6px;white-space:nowrap">
                                                                <input type="checkbox" prop:checked=move || new_multi.get()
                                                                    on:change=move |ev| new_multi.set(event_target_checked(&ev)) />
                                                                "多选"
                                                            </label>
                                                        }.into_any()
                                                    } else if vt == "date" || vt == "time" || vt == "datetime" {
                                                        view! { <FormatSelect value_type=vt format=new_format /> }.into_any()
                                                    } else if vt == "currency" {
                                                        view! {
                                                            <input class="inp" style="width:100px" placeholder="符号（如 ¥）" prop:value=new_symbol
                                                                on:input=move |ev| new_symbol.set(event_target_value(&ev)) />
                                                            <input class="inp" style="width:100px" placeholder="单位（如 万）" prop:value=new_unit
                                                                on:input=move |ev| new_unit.set(event_target_value(&ev)) />
                                                        }.into_any()
                                                    } else {
                                                        ().into_any()
                                                    }
                                                }}
                                                {move || {
                                                    let mem = data
                                                        .get()
                                                        .and_then(|r| r.ok())
                                                        .map(|(_, _, _, _, m, _, _)| m)
                                                        .unwrap_or_default();
                                                    view! {
                                                        <DefaultValueInput value_type=new_type.get()
                                                            multi=new_multi.get()
                                                            enum_values=enum_values_of(&new_enum.get())
                                                            format=new_format.get() members=mem
                                                            value=new_default />
                                                    }
                                                }}
                                                <button class="btn pri" type="submit">{ic_add()}"新建标签"</button>
                                            </form>
                                        }.into_any()
                                    } else {
                                        view! { <p class="mut">"仅 Maintainer 及以上可管理标签"</p> }.into_any()
                                    }}
                                    <table class="tbl">
                                        <thead><tr><th>"name（不可改）"</th><th>"title"</th><th>"值类型"</th><th>"可选值 / 说明"</th><th>"属性"</th><th>"颜色"</th><th style="width:56px">"关系"</th><th style="width:90px"></th></tr></thead>
                                        <tbody>
                                            {schemas.iter().map(|s| schema_row(s, can_manage, _ws.id.clone(), member_list.clone(), schemas.clone(), refresh, error)).collect::<Vec<_>>()}
                                        </tbody>
                                    </table>
                                    <div class="mut">"同一 Entry 对同一 Schema 仅一条 Labeling，更新即 upsert；变更自动记录操作人与时间。"</div>
                                }.into_any()
                            } else if cur_tab == "ai" {
                                view! {
                                    <h2>"AI 总结"</h2>
                                    <p class="mut">"生成总结时可选「场景」与「语气」；两项都是工作空间内可维护的「名称 + 提示词」。生成总结会把提示词追加到内置模板之后。"</p>
                                    <p class="mut">"服务端的模型与密钥在 config.toml 的 [ai] 段配置，不在本页。"</p>
                                    {move || ai_loading.get().then(|| view! {
                                        <p class="mut">"正在读取场景与语气…"</p>
                                    })}
                                    {move || ai_error.get().map(|e| view! { <p class="error">{e}</p> })}
                                    {move || ai_msg.get().map(|m| view! { <p class="mut">{m}</p> })}
                                    {if can_manage {
                                        view! {
                                            <form class="stack" on:submit=save_ai>
                                                <h3 style="margin-top:18px">"场景"</h3>
                                                <PromptRows rows=ai_scenarios placeholder="场景名称，如「迭代复盘」".to_string() />
                                                <h3 style="margin-top:18px">"语气"</h3>
                                                <PromptRows rows=ai_tones placeholder="语气名称，如「简洁」".to_string() />
                                                <button class="btn pri" type="submit" disabled=move || ai_busy.get()
                                                    style="align-self:flex-start">
                                                    {move || if ai_busy.get() { "保存中…" } else { "保存" }}
                                                </button>
                                            </form>
                                        }.into_any()
                                    } else {
                                        view! {
                                            <div class="stack">
                                                <h3>"场景"</h3>
                                                {move || ai_scenarios.get().into_iter().map(|r| view! {
                                                    <div class="mut">{r.name.get()}</div>
                                                }).collect::<Vec<_>>()}
                                                <h3>"语气"</h3>
                                                {move || ai_tones.get().into_iter().map(|r| view! {
                                                    <div class="mut">{r.name.get()}</div>
                                                }).collect::<Vec<_>>()}
                                                <p class="mut">"仅 Maintainer 及以上可修改"</p>
                                            </div>
                                        }.into_any()
                                    }}
                                }.into_any()
                            } else if cur_tab == "automation" {
                                // 规则自己加载，不进那个 7 元组；这里只派发工作空间 id 与标签定义。
                                // 先克隆成独立值再 derive，避免把 `_ws` / `schemas` 整体 move 进闭包，
                                // 与下方 labels / danger 分支对这些值的借用冲突。
                                let ws_id_val = _ws.id.clone();
                                let schemas_auto = schemas.clone();
                                let ws_id_signal = Signal::derive(move || ws_id_val.clone());
                                let schemas_signal = Signal::derive(move || schemas_auto.clone());
                                let refresh_cb = Callback::new(move |_: ()| refresh.update(|x| *x += 1));
                                view! {
                                    <AutomationTab ws_id=ws_id_signal schemas=schemas_signal
                                        can_manage=can_manage on_changed=refresh_cb />
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
                                                        <td>{v.name.clone()}</td>
                                                        <td class="mut">{owner}</td>
                                                        <td class="mut">{v.entry_count}</td>
                                                        <td>
                                                            <label style="display:flex;align-items:center;gap:6px">
                                                                <input type="checkbox" prop:checked=v.is_shared disabled=!can_manage || is_default
                                                                    on:change=move |ev| toggle_shared.run((id.clone(), event_target_checked(&ev))) />
                                                                {if is_default {
                                                                    view! { <span class="mut">"基础视图始终共享"</span> }.into_any()
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

/// 关系编辑行状态（`RwSignal` 便于逐字段就地更新）。
/// 与 `VcRow` 同一套路：行集合存在一个 `RwSignal<Vec<..>>` 里，行内字段各自可写。
#[derive(Clone, Copy)]
struct LinkRow {
    id: usize,
    /// `inherit` | `override`。
    kind: RwSignal<String>,
    /// 关系另一端的标签名。
    other: RwSignal<String>,
    /// 另一端的值（可空）。
    other_value: RwSignal<String>,
    /// 本标签的值（可空）。
    own_value: RwSignal<String>,
}

/// 标签元信息：名称 / 值类型 / 枚举值 / 是否多值。
/// 关系编辑器要据此决定值输入框的形态（枚举下拉、多值逗号分隔、无值标签不给输入框）。
#[derive(Clone)]
struct LabelMeta {
    name: String,
    value_type: String,
    enum_values: Vec<String>,
    multi: bool,
}

impl LabelMeta {
    /// 值输入框该不该出现：无值标签的关系只能约束「有没有」，不能约束值。
    fn has_value(&self) -> bool {
        self.value_type != "null"
    }
}

/// 编辑框文本 → 标签值 JSON（空为 null）。服务端会按目标标签的类型再校验一次。
fn link_value_of(s: &str, meta: Option<&LabelMeta>) -> Value {
    let t = s.trim();
    if t.is_empty() {
        return Value::Null;
    }
    match meta.map(|m| m.value_type.as_str()).unwrap_or("") {
        "boolean" => match t {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::String(t.to_string()),
        },
        "integer" | "float" | "currency" => num_value(t),
        // 多值枚举存的是数组；单值枚举在界面上是下拉，到这里已经是合法枚举值。
        "enum" if meta.is_some_and(|m| m.multi) => Value::Array(
            t.split(',')
                .map(|x| Value::String(x.trim().to_string()))
                .filter(|x| x.as_str() != Some(""))
                .collect(),
        ),
        _ => Value::String(t.to_string()),
    }
}

/// 标签值 JSON → 编辑框文本（多值数组用逗号连接）。
fn link_value_text(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => fmt_num(n),
        Value::Array(a) => a
            .iter()
            .map(link_value_text)
            .collect::<Vec<_>>()
            .join(","),
        other => other.to_string(),
    }
}

/// 在指定 owner 下新建一条关系行。
///
/// 关系行的信号必须挂在**行自己**的 owner 上：弹窗（`show_links` 为真时才渲染）随时会被
/// 销毁，点「＋ 关系」新建的信号默认挂在弹窗作用域下，关窗即被 dispose，之后保存再读它们
/// 就会 panic（「you tried to access a reactive value … it has already been disposed」）。
fn new_link_row(
    owner: Option<&Owner>,
    id: usize,
    kind: String,
    other: String,
    other_value: String,
    own_value: String,
) -> LinkRow {
    let build = || LinkRow {
        id,
        kind: RwSignal::new(kind),
        other: RwSignal::new(other),
        other_value: RwSignal::new(other_value),
        own_value: RwSignal::new(own_value),
    };
    match owner {
        Some(o) => o.with(build),
        None => build(),
    }
}

/// 库里的关系 JSON → 编辑行。关弹窗时按库中数据重建，等于丢弃本次编辑。
fn links_to_rows(links: &Value, owner: Option<&Owner>) -> Vec<LinkRow> {
    links
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or(&[])
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let other = l.get("other")?.as_str()?.to_string();
            Some(new_link_row(
                owner,
                i,
                l.get("kind")
                    .and_then(|k| k.as_str())
                    .unwrap_or("inherit")
                    .to_string(),
                other,
                link_value_text(l.get("otherValue").unwrap_or(&Value::Null)),
                link_value_text(l.get("ownValue").unwrap_or(&Value::Null)),
            ))
        })
        .collect()
}

/// 关系行 → `[{kind,other,otherValue,ownValue}]` JSON。未选对方标签的行直接丢掉。
/// 值跟着各自标签的类型走：无值标签（null）一侧留 null。
fn build_links(rows: Vec<LinkRow>, metas: &[LabelMeta], own: Option<&LabelMeta>) -> Value {
    let arr: Vec<Value> = rows
        .into_iter()
        .filter_map(|r| {
            let other = r.other.get();
            if other.is_empty() {
                return None;
            }
            let other_meta = metas.iter().find(|m| m.name == other).filter(|m| m.has_value());
            let own_meta = own.filter(|m| m.has_value());
            Some(serde_json::json!({
                "kind": r.kind.get(),
                "other": other,
                "otherValue": link_value_of(&r.other_value.get(), other_meta),
                "ownValue": link_value_of(&r.own_value.get(), own_meta),
            }))
        })
        .collect();
    Value::Array(arr)
}

/// 单行关系编辑器：本标签 Key（只读）/ 本标签值 / 种类 / 目标标签 / 目标标签值 / 删除。
/// 顺序照着「从本标签读向目标标签」排，与 `link_head` 的表头一一对应。
fn link_row_view(
    r: LinkRow,
    metas: Vec<LabelMeta>,
    own: LabelMeta,
    rows: RwSignal<Vec<LinkRow>>,
    editable: bool,
    dirty: RwSignal<bool>,
) -> impl IntoView {
    let metas_for_other = metas.clone();
    let own_has_value = own.has_value();
    let own_name = own.name.clone();
    view! {
        <div class="link-grid">
            // 本标签 Key 只读：标出这条关系是从哪条标签出去的。
            <span class="code mut" title="本标签">{own_name}</span>
            // 本标签的值：只在有值标签上出现，无值标签的关系只能约束「有没有」。
            // 无值标签也要占住这一格，否则后面的控件会整体左移一列。
            {if own_has_value {
                value_input(r.own_value, own.clone(), "本标签值", editable, dirty).into_any()
            } else {
                view! { <span></span> }.into_any()
            }}
            <select class="inp" disabled=!editable
                prop:value=move || r.kind.get()
                on:change=move |ev| {
                    r.kind.set(event_target_value(&ev));
                    dirty.set(true);
                }>
                <option value="inherit">"继承"</option>
                <option value="override">"覆盖"</option>
            </select>
            <select class="inp" disabled=!editable
                prop:value=move || r.other.get()
                on:change=move |ev| {
                    r.other.set(event_target_value(&ev));
                    // 换了目标标签，原有值多半不再合法，清掉免得带一个错值过去。
                    r.other_value.set(String::new());
                    dirty.set(true);
                }>
                <option value="">"（选择标签）"</option>
                {metas_for_other.iter().map(|m| view! {
                    <option value=m.name.clone()>{m.name.clone()}</option>
                }).collect::<Vec<_>>()}
            </select>
            // 目标标签的值：随所选的标签重渲染，枚举标签给下拉。
            {move || {
                let cur = r.other.get();
                match metas.iter().find(|m| m.name == cur).filter(|m| m.has_value()) {
                    Some(m) => value_input(r.other_value, m.clone(), "目标标签值", editable, dirty).into_any(),
                    None => view! { <span></span> }.into_any(),
                }
            }}
            <button class="vc-op vc-del" title="删除" disabled=!editable on:click=move |_| {
                dirty.set(true);
                rows.update(|rows| rows.retain(|x| x.id != r.id));
            }>"×"</button>
        </div>
    }
}

/// 关系里的一个值输入框：枚举给下拉，多值枚举与其余类型给文本框。
/// 宽度交给 `.link-grid` 的列宽管，不写死，免得和表头对不齐。
fn value_input(
    sig: RwSignal<String>,
    meta: LabelMeta,
    placeholder: &'static str,
    editable: bool,
    dirty: RwSignal<bool>,
) -> impl IntoView {
    if meta.value_type == "enum" && !meta.multi {
        view! {
            <select class="inp" disabled=!editable
                prop:value=move || sig.get()
                on:change=move |ev| {
                    sig.set(event_target_value(&ev));
                    dirty.set(true);
                }>
                <option value="">"（可空）"</option>
                {meta.enum_values.iter().cloned().map(|o| view! {
                    <option value=o.clone()>{display_enum_value(&o)}</option>
                }).collect::<Vec<_>>()}
            </select>
        }
        .into_any()
    } else {
        let ph = if meta.value_type == "enum" && meta.multi {
            "多值，逗号分隔"
        } else {
            placeholder
        };
        view! {
            <input class="inp" placeholder=ph disabled=!editable
                prop:value=move || sig.get()
                on:input=move |ev| {
                    sig.set(event_target_value(&ev));
                    dirty.set(true);
                } />
        }
        .into_any()
    }
}

fn schema_row(
    s: &LabelSchema,
    can_manage: bool,
    ws_id: String,
    // 工作空间成员表：账号型标签的默认值要从这里挑。
    members: Vec<Member>,
    // 全部标签定义：关系编辑器的对方标签候选取自这里（去掉自己）。
    all: Vec<LabelSchema>,
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
    // 新类型属性表单状态，初值取自库中已有属性；保存时一并回写。
    // 服务端 update 是整体替换，必须传齐四键，否则会清空已有属性。
    let multi_input = RwSignal::new(s.multi);
    let format_input = RwSignal::new(s.format.clone().unwrap_or_default());
    let symbol_input = RwSignal::new(s.currency_symbol.clone().unwrap_or_default());
    let unit_input = RwSignal::new(s.unit.clone().unwrap_or_default());
    // 新增 / 改值都直接改这个信号，保存时随 attrs 一起落库。
    let default_input = RwSignal::new(s.default_value.clone());
    // 内置标签只放开显示名与颜色；枚举值、属性、默认值仍只读。
    let can_edit_basic = can_manage;
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
    // 关系编辑器单独放在弹窗里：它一行有四个控件，塞进表格会把整行撑爆。
    let show_links = RwSignal::new(false);

    // 关系编辑：候选是同一工作空间里的其它标签（自己不能跟自己建关系）。
    let self_meta = LabelMeta {
        name: s.name.clone(),
        value_type: value_type.clone(),
        enum_values: s.enum_values.clone(),
        multi: s.multi,
    };
    let metas: Vec<LabelMeta> = all
        .iter()
        .filter(|x| x.name != s.name)
        .map(|x| LabelMeta {
            name: x.name.clone(),
            value_type: x.value_type.clone(),
            enum_values: x.enum_values.clone(),
            multi: x.multi,
        })
        .collect();
    // 关系行的信号全挂在这个 owner 上——它是整行的，比弹窗活得久。理由见 new_link_row。
    let link_owner = Owner::current();
    // 库中的关系原样留一份：关弹窗时照它重建编辑行，等于丢弃本次编辑。
    let links_seed = s.links.clone();
    let init_links = links_to_rows(&links_seed, link_owner.as_ref());
    let next_link_id = RwSignal::new(init_links.len());
    let link_rows = RwSignal::new(init_links);

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

    // 值色行的色块：每个值一个，故用 `small` 的紧凑色点。
    let swatch = {
        let ws_swatch = ws_id.clone();
        move |sig: RwSignal<String>| {
            let ws = ws_swatch.clone();
            view! {
                <ColorPick
                    small=true
                    value=Signal::derive(move || sig.get())
                    ws_id=Signal::derive(move || ws.clone())
                    disabled=!editable
                    on_pick=Callback::new(move |c: String| {
                        sig.set(c);
                        dirty.set(true);
                    })
                />
            }
        }
    };

    // 值色行末尾的操作键。`reorder` 只给数值区间开——数值区间是「从…到…」的列表，
    // 顺序有意义；枚举行是按名字选中某个枚举值，顺序由可选值本身决定，排它没意义。
    let ops = move |r: VcRow, reorder: bool| {
        view! {
            {reorder.then(|| view! {
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
            })}
            <button class="vc-op vc-del" title="删除" disabled=!editable on:click=move |_| {
                dirty.set(true);
                vc_rows.update(|rows| rows.retain(|x| x.id != r.id));
            }>"×"</button>
        }
    };

    // 关弹窗＝丢弃本次编辑：编辑行按库中数据重建。只有「保存」才落库。
    // Owner 是 Arc 句柄，clone 不会让行上的信号提前被清掉，故两个闭包各留一份。
    let close_links = {
        let owner = link_owner.clone();
        move |_| {
            let rows = links_to_rows(&links_seed, owner.as_ref());
            next_link_id.set(rows.len());
            link_rows.set(rows);
            show_links.set(false);
        }
    };

    let add_link = {
        let owner = link_owner.clone();
        move |_| {
            dirty.set(true);
            let id = next_link_id.get_untracked();
            next_link_id.set(id + 1);
            // 信号必须建在行自己的 owner 下：这里是在弹窗里点的，默认会挂到弹窗作用域上。
            let row = new_link_row(
                owner.as_ref(),
                id,
                "inherit".to_string(),
                String::new(),
                String::new(),
                String::new(),
            );
            link_rows.update(|rows| rows.push(row));
        }
    };

    // 整行标签定义的保存。表格行的「保存」和关系弹窗的「保存」共用这一条路径：
    // 服务端 update 是整体替换，两处都得提交全部字段（含关系）。
    // 参数是保存成功后的回调（关弹窗）；失败时留着弹窗并把错误显示在弹窗里。
    let save: Callback<Option<Callback<()>>> = {
        let metas_save = metas.clone();
        let own_save = self_meta.clone();
        let vt_save = value_type.clone();
        let ws_save = ws_id.clone();
        let name_save = name.clone();
        Callback::new(move |after: Option<Callback<()>>| {
            let evals: Vec<String> = enum_input
                .get()
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let t = title_input.get();
            let clr = base_color.get();
            let vcs = build_value_colors(vc_rows.get(), &vt_save);
            let attrs = {
                let fv = format_input.get();
                let sv = symbol_input.get();
                let uv = unit_input.get();
                let f = fv.trim();
                let sy = sv.trim();
                let u = uv.trim();
                label_attrs(
                    multi_input.get(),
                    (!f.is_empty()).then_some(f),
                    (!sy.is_empty()).then_some(sy),
                    (!u.is_empty()).then_some(u),
                    &default_input.get(),
                    &build_links(link_rows.get_untracked(), &metas_save, Some(&own_save)),
                )
            };
            let ws = ws_save.clone();
            let n = name_save.clone();
            spawn_local(async move {
                match update_label_schema(&ws, &n, &t, &evals, &attrs, clr.as_deref(), &vcs).await {
                    Ok(_) => {
                        error.set(None);
                        if let Some(cb) = after {
                            cb.run(());
                        }
                        refresh.update(|x| *x += 1);
                    }
                    // 失败不动数据：整行会按服务端数据重建，那会把用户刚填的东西抹掉。
                    Err(e) => error.set(Some(e)),
                }
            });
        })
    };

    // 保存成功后关弹窗。紧接着 refresh 也会整行重建，但那是异步的，
    // 不先关掉的话，重建期间弹窗会压在新行上。
    let close_saved = Callback::new(move |_: ()| show_links.set(false));

    view! {
        <tr class="static">
            <td class="code">{name.clone()}</td>
            <td>
                {if can_edit_basic {
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
                {if value_type == "enum" {
                    view! {
                        <div class="attrs">
                            <label style="display:flex;align-items:center;gap:6px;white-space:nowrap">
                                <input type="checkbox" disabled=!editable prop:checked=move || multi_input.get()
                                    on:change=move |ev| {
                                        multi_input.set(event_target_checked(&ev));
                                        dirty.set(true);
                                    } />
                                "多选"
                            </label>
                            // 可选值边改边影响默认值的候选，故跟着 enum_input 重渲染。
                            {move || view! {
                                <DefaultValueInput value_type="enum".to_string()
                                    multi=multi_input.get()
                                    enum_values=enum_values_of(&enum_input.get())
                                    value=default_input disabled=!editable />
                            }}
                        </div>
                    }.into_any()
                } else if value_type == "date" || value_type == "time" || value_type == "datetime" {
                    let vt = value_type.clone();
                    let vt2 = value_type.clone();
                    view! {
                        <div class="attrs">
                            <FormatSelect value_type=vt format=format_input disabled=!editable />
                            {move || view! {
                                <DefaultValueInput value_type=vt2.clone()
                                    format=format_input.get() value=default_input
                                    disabled=!editable />
                            }}
                        </div>
                    }
                        .into_any()
                } else if value_type == "currency" {
                    let vt = value_type.clone();
                    view! {
                        <div class="attrs">
                            <div style="display:flex;gap:6px">
                                <input class="inp" style="width:80px" placeholder="符号" disabled=!editable
                                    prop:value=move || symbol_input.get()
                                    on:input=move |ev| {
                                        symbol_input.set(event_target_value(&ev));
                                        dirty.set(true);
                                    } />
                                <input class="inp" style="width:80px" placeholder="单位" disabled=!editable
                                    prop:value=move || unit_input.get()
                                    on:input=move |ev| {
                                        unit_input.set(event_target_value(&ev));
                                        dirty.set(true);
                                    } />
                            </div>
                            <DefaultValueInput value_type=vt value=default_input disabled=!editable />
                        </div>
                    }
                        .into_any()
                } else if value_type == "null" {
                    // 无值标签没有「值」可填：默认值的语义是「新建 Entry 时自动勾上」。
                    view! { <span class="mut">"—"</span> }.into_any()
                } else {
                    let vt = value_type.clone();
                    let evals = enum_opts.clone();
                    let mem = members.clone();
                    // multi 会改控件形态（多选 Enum 用逗号分隔文本），故这里跟着信号重渲染。
                    view! {
                        {move || view! {
                            <DefaultValueInput value_type=vt.clone() multi=multi_input.get()
                                enum_values=evals.clone() members=mem.clone()
                                value=default_input disabled=!editable />
                        }}
                    }
                        .into_any()
                }}
            </td>
            <td>
                <div class="color-cell">
                    <div class="color-base">
                        <ColorPick
                            value=Signal::derive(move || base_color.get().unwrap_or_default())
                            ws_id=Signal::derive({
                                let w = ws_id.clone();
                                move || w.clone()
                            })
                            disabled=!can_edit_basic
                            on_pick=Callback::new(move |c: String| {
                                base_color.set(Some(c));
                                dirty.set(true);
                            })
                        />
                        <span class="code" style="font-size:11px">
                            {move || base_color.get().unwrap_or_else(|| "未设置".to_string())}
                        </span>
                        <button class="vc-op" title="清除基础色" disabled=!can_edit_basic on:click=move |_| {
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
                                                    {ops(r, true)}
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
                                                    {ops(r, false)}
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
            // 关系列：只有一个可点的图标。配置全在弹窗里做，保存也在弹窗里，
            // 这列不放保存按钮。
            <td>
                <button class="icbtn" on:click=move |_| show_links.set(true)
                    title=move || {
                        let n = link_rows.get().len();
                        if n > 0 { format!("配置关系（已 {n} 条）") } else { "配置关系".to_string() }
                    }>
                    {ic_link()}
                </button>
                {let metas_dlg = metas.clone();
                let own_dlg = self_meta.clone();
                // 只读时图标照常在，点开仍能看关系，只是改不动。
                move || show_links.get().then(|| {
                    // then 的闭包只跑一次，外面那个却要能反复调用，所以它捕获的那些句柄
                    // 只能借来 clone，不能整个搬出去——搬出去外面就成 FnOnce 了。
                    let metas_rows = metas_dlg.clone();
                    let own_rows = own_dlg.clone();
                    let add = add_link.clone();
                    let close_a = close_links.clone();
                    let close_b = close_links.clone();
                    view! {
                        <div class="dmodal" on:click=close_a>
                            <div class="panel dmbox" style="width:620px" on:click=|ev| ev.stop_propagation()>
                                <h3>"关系（继承 / 覆盖）"</h3>
                                <p class="mut">"继承：目标标签取到某个值时，本条跟着显示同一个值。覆盖：目标标签取到某个值时，本条改取另一个值。两侧的值都可留空，留空表示只约束「有没有值」。"</p>
                                <div class="vc-list">
                                    <div class="link-grid link-head">
                                        <span>"本标签"</span>
                                        <span>"本标签值"</span>
                                        <span>"种类"</span>
                                        <span>"目标标签"</span>
                                        <span>"目标标签值"</span>
                                        <span></span>
                                    </div>
                                    <For
                                        each=move || link_rows.get()
                                        key=|r| r.id
                                        children=move |r: LinkRow| link_row_view(
                                            r,
                                            metas_rows.clone(),
                                            own_rows.clone(),
                                            link_rows,
                                            editable,
                                            dirty,
                                        )
                                    />
                                    {editable.then(move || view! {
                                        <button class="btn sm" on:click=add>"＋ 关系"</button>
                                    })}
                                    {move || link_rows.get().is_empty().then(|| {
                                        view! { <span class="mut">"尚未配置关系。"</span> }
                                    })}
                                </div>
                                {(!editable).then(|| view! {
                                    <p class="mut">"内置标签或权限不足，关系只能查看。"</p>
                                })}
                                // 保存失败时错误显示在这里：遮罩盖住了页面顶部那条错误提示。
                                {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
                                <div style="display:flex;gap:8px;justify-content:flex-end">
                                    <button class="btn" on:click=close_b>
                                        {move || if editable { "取消" } else { "关闭" }}
                                    </button>
                                    {editable.then(move || view! {
                                        <button class="btn pri" disabled=move || !dirty.get()
                                            on:click=move |_| save.run(Some(close_saved))>"保存"</button>
                                    })}
                                </div>
                            </div>
                        </div>
                    }
                })}
            </td>
            <td>
                {if can_edit_basic {
                    view! {
                        <button class="btn rowact" disabled=move || !dirty.get() on:click=move |_| save.run(None)>"保存"</button>
                    }.into_any()
                } else {
                    view! { <span></span> }.into_any()
                }}
            </td>
        </tr>
    }
}
