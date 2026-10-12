//! 条目关联侧栏：列出当前条目的全部关联（from + to 两侧），并提供新增 / 删除。
//!
//! 形态刻意做成"邻居式"——一行就是一个关联，从语义图标、对方条目、动作
//! 三段拼起来。添加表单：左侧语义下拉 + 右侧 Entry 选择器 + 「添加」按钮，
//! 顺序按需求固定（语义在前、目标条目在后）。修改语义目前走删除重建
//! （方向 / 端点锁死后语义再变意义不大，改语义通常意味着"当初选错了"，
//! 重建反而更直观）。

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::components::{logged_out, role_at_least};
use crate::frontend::entry_picker::EntryPicker;
use crate::frontend::graphql_client::{
    add_entry_relation, delete_entry_relation, entry_relations, my_role, Entry, Relation,
    RelationSemantic,
};
use crate::frontend::i18n::{use_i18n, I18nController};

/// 内置语义 key 列表。自定义形态 key="custom"，实际词走 `value`，
/// 表单里临时给一个 input 框。展示名走 i18n（relation.<kind>）。
const PRESET_KINDS: &[&str] = &[
    "contains",
    "derives",
    "belongs_to",
    "blocks",
    "relates_to",
    "custom",
];

/// 把服务端给的 RelationSemantic 转回 (kind, value)：内置形态 value 永远是 None。
fn split_semantic(s: &RelationSemantic) -> (&str, Option<&str>) {
    match s.kind.as_str() {
        "contains" => ("contains", None),
        "derives" => ("derives", None),
        "belongs_to" => ("belongs_to", None),
        "blocks" => ("blocks", None),
        "relates_to" => ("relates_to", None),
        // 服务端不会发 kind 不是以上五种的值，但万一兜底走 custom。
        _ => ("custom", s.value.as_deref()),
    }
}

/// 反方向：用 curl 上的 关联方向给出箭头字符。outgoing = 当前条目是 from，
/// incoming = 当前条目是 to。
fn direction_arrow(direction: Option<&str>) -> &'static str {
    match direction {
        Some("outgoing") => "→",
        Some("incoming") => "←",
        _ => "↔",
    }
}

#[component]
pub fn RelationsPanel(
    /// 当前条目编码；空串时不取数也不渲染（详情面板未选中条目时）。
    code: Signal<String>,
    /// 当前条目所属工作空间，用于删除等需要工作空间 id 的接口。
    workspace_id: Signal<String>,
    /// 关联变更后通知外层：详情面板的徽标与更新时间。
    on_changed: Callback<()>,
) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let items = RwSignal::new(None::<Result<Vec<Relation>, String>>);
    let role = RwSignal::new(String::new());
    // EntryPicker 选中状态；submit 时从这里拿目标 code。
    let picked = RwSignal::new(Option::<Entry>::None);
    let new_kind = RwSignal::new("relates_to".to_string());
    let new_custom = RwSignal::new(String::new());
    let confirm_del = RwSignal::new(Option::<String>::None);
    let error = RwSignal::new(Option::<String>::None);

    let load = move || {
        if items.is_disposed() {
            return;
        }
        let c = code.get();
        if c.is_empty() {
            return;
        }
        if !cfg!(target_arch = "wasm32") {
            return;
        }
        spawn_local(async move {
            let r = entry_relations(&c).await;
            if items.is_disposed() || code.get_untracked() != c {
                return;
            }
            match r {
                Ok(list) => items.set(Some(Ok(list))),
                Err(e) => items.set(Some(Err(e))),
            }
        });
    };

    Effect::new(move |_| {
        code.get();
        let ws = workspace_id.get();
        if logged_out() {
            return;
        }
        load();
        // 取一下当前用户在该工作空间的角色：「添加 / 删除关联」按钮按角色门控，
        // 否则 owner 才能改就太窄了（worker 也该能写）。后端 mutation 已自带
        // 角色校验，前端先屏蔽 UI 比让用户点完再撞 403 体验更好。
        if !ws.is_empty() && cfg!(target_arch = "wasm32") {
            let role_for_async = role;
            spawn_local(async move {
                if let Ok(r) = my_role(&ws).await {
                    role_for_async.set(r);
                }
            });
        }
    });

    let submit = move |_| {
        let from = code.get();
        let Some(target) = picked.get() else {
            error.set(Some(i18n.t("relation.pick_target")));
            return;
        };
        let to = target.code.clone();
        let kind = new_kind.get();
        let custom = new_custom.get();
        if from.is_empty() {
            error.set(Some(i18n.t("relation.no_entry")));
            return;
        }
        if from == to {
            error.set(Some(i18n.t("relation.no_self")));
            return;
        }
        if kind == "custom" && custom.trim().is_empty() {
            error.set(Some(i18n.t("relation.custom_required")));
            return;
        }
        let custom_for_payload = if kind == "custom" {
            Some(custom.trim().to_string())
        } else {
            None
        };
        spawn_local(async move {
            let r = add_entry_relation(&from, &to, &kind, custom_for_payload.as_deref()).await;
            match r {
                Ok(_) => {
                    error.set(None);
                    picked.set(None);
                    new_custom.set(String::new());
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let do_delete = move |id: String| {
        let ws = workspace_id.get();
        spawn_local(async move {
            match delete_entry_relation(&ws, &id).await {
                Ok(_) => {
                    confirm_del.set(None);
                    error.set(None);
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    view! {
        <div class="relations">
            {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}

            {move || {
                match items.get() {
                    None => view! { <div class="mut">{move || i18n.t("common.loading")}</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            view! { <div class="mut">{move || i18n.t("relation.empty")}</div> }.into_any()
                        } else {
                            view! {
                                <ul class="rel-list">
                                    {list.into_iter().map(|r| {
                                        let rid = r.id.clone();
                                        let other_title = r.other_title.clone();
                                        let arrow = direction_arrow(r.direction.as_deref()).to_string();
                                        let (kind, _value) = split_semantic(&r.semantic);
                                        let badge_class = match kind {
                                            "contains" => "rel-badge rel-contains",
                                            "derives" => "rel-badge rel-derives",
                                            "belongs_to" => "rel-badge rel-belongs",
                                            "blocks" => "rel-badge rel-blocks",
                                            "relates_to" => "rel-badge rel-relates",
                                            _ => "rel-badge rel-custom",
                                        };
                                        let sem_display = r.semantic.display.clone();
                                        let is_incoming = r.direction.as_deref() == Some("incoming");
                                        let rid_for_confirm = rid.clone();
                                        let rid_for_del = rid.clone();
                                        view! {
                                            <li class="rel-item">
                                                <span class=badge_class>{sem_display}</span>
                                                <span class="rel-arrow">{arrow}</span>
                                                <span class="rel-target">{other_title}</span>
                                                {is_incoming.then(|| view! {
                                                    <span class="mut" style="font-size:0.85em">{move || i18n.t("relation.incoming_suffix")}</span>
                                                })}
                                                {move || {
                                                    let pending = confirm_del.get();
                                                    if pending.as_deref() == Some(rid_for_confirm.as_str()) {
                                                        view! {
                                                            <span class="rel-acts">
                                                                <button
                                                                    class="btn sm danger"
                                                                    on:click={
                                                                        let id = rid_for_del.clone();
                                                                        move |_| do_delete(id.clone())
                                                                    }
                                                                >{move || i18n.t("settings.danger.confirm_delete")}</button>
                                                                <button
                                                                    class="btn sm"
                                                                    on:click=move |_| confirm_del.set(None)
                                                                >{move || i18n.t("common.cancel")}</button>
                                                            </span>
                                                        }.into_any()
                                                    } else {
                                                        view! {
                                                            <button
                                                                class="btn sm"
                                                                on:click={
                                                                    let id = rid.clone();
                                                                    move |_| confirm_del.set(Some(id.clone()))
                                                                }
                                                            >{move || i18n.t("common.delete")}</button>
                                                        }.into_any()
                                                    }
                                                }}
                                            </li>
                                        }
                                    }).collect::<Vec<_>>()}
                                </ul>
                            }.into_any()
                        }
                    }
                }
            }}

            {move || {
                if !role_at_least(&role.get(), "worker") {
                    return view! { <span></span> }.into_any();
                }
                let cur = code.get();
                if cur.is_empty() {
                    return view! { <span></span> }.into_any();
                }
                let cur_for_exclude = cur.clone();
                let ws_for_picker = workspace_id;
                let cur_for_exclude_sig = Signal::derive(move || cur_for_exclude.clone());
                view! {
                    <div class="rel-composer">
                        <select
                            class="rel-select"
                            on:change=move |ev| new_kind.set(event_target_value(&ev))
                        >
                            {PRESET_KINDS.iter().map(|k| {
                                let k = k.to_string();
                                let kind_for_label = k.clone();
                                view! {
                                    <option value=k.clone()>{move || {
                                        let key = format!("relation.kind.{}", kind_for_label);
                                        i18n.t(&key)
                                    }}</option>
                                }
                            }).collect::<Vec<_>>()}
                        </select>
                        {move || if new_kind.get() == "custom" {
                            view! {
                                <input
                                    class="rel-input"
                                    placeholder=move || i18n.t("relation.custom_placeholder")
                                    prop:value=move || new_custom.get()
                                    on:input=move |ev| new_custom.set(event_target_value(&ev))
                                />
                            }.into_any()
                        } else {
                            view! { <span></span> }.into_any()
                        }}
                        <EntryPicker
                            exclude_code=cur_for_exclude_sig
                            workspace_id=ws_for_picker
                            selected=picked
                        />
                        <button class="btn pri sm" on:click=submit>{move || i18n.t("relation.add")}</button>
                    </div>
                }.into_any()
            }}
        </div>
    }
}
