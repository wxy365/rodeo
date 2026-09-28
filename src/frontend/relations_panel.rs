//! 条目关联侧栏：列出当前条目的全部关联（from + to 两侧），并提供新增 / 删除。
//!
//! 形态刻意做成"邻居式"——一行就是一个关联，从语义图标、对方条目、动作
//! 三段拼起来。最热路径是「选条目 + 选语义 → 新建」，所以新建表单始终
//! 展开。修改语义目前走删除重建（方向 / 端点锁死后语义再变意义不大，
//! 改语义通常意味着"当初选错了"，重建反而更直观）。

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::components::{logged_out, role_at_least};
use crate::frontend::graphql_client::{
    add_entry_relation, delete_entry_relation, entry_relations, Relation, RelationSemantic,
};

/// 与服务端语义枚举一一对应：内置 key + 中文展示名。自定义形态 key="custom"，
/// 实际词走 `value`，表单里临时给一个 input 框。
const PRESETS: &[(&str, &str)] = &[
    ("contains", "包含"),
    ("derives", "派生"),
    ("belongs_to", "归属"),
    ("blocks", "阻塞"),
    ("relates_to", "关联"),
    ("custom", "自定义"),
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
    /// 列出条目缓存用的轻量索引（code → title）。空 Map 时跳过对方条目名解析。
    title_index: Signal<std::collections::HashMap<String, String>>,
    /// 关联变更后通知外层：详情面板的徽标与更新时间。
    on_changed: Callback<()>,
) -> impl IntoView {
    let items = RwSignal::new(None::<Result<Vec<Relation>, String>>);
    let role = RwSignal::new(String::new());
    let new_target = RwSignal::new(String::new());
    let new_kind = RwSignal::new("relates_to".to_string());
    let new_custom = RwSignal::new(String::new());
    let confirm_del = RwSignal::new(None::<String>);
    let error = RwSignal::new(None::<String>);

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
        workspace_id.get();
        if logged_out() {
            return;
        }
        load();
    });

    let submit = move |_| {
        let from = code.get();
        let to = new_target.get().trim().to_string();
        let kind = new_kind.get();
        let custom = new_custom.get();
        if from.is_empty() || to.is_empty() {
            error.set(Some("请填写目标条目编码".to_string()));
            return;
        }
        if from == to {
            error.set(Some("不能关联到自身".to_string()));
            return;
        }
        if kind == "custom" && custom.trim().is_empty() {
            error.set(Some("自定义语义需要给个名字".to_string()));
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
                    new_target.set(String::new());
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
            <div class="grp-h">
                <span style="margin-right:0.4em">"↪"</span>
                {move || format!(
                    "关联（{}）",
                    items.get().and_then(|r| r.ok()).map(|v| v.len()).unwrap_or(0),
                )}
            </div>

            {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}

            {move || {
                match items.get() {
                    None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            view! { <div class="mut">"暂无关联"</div> }.into_any()
                        } else {
                            let titles = title_index.get();
                            view! {
                                <ul class="rel-list">
                                    {list.into_iter().map(|r| {
                                        let rid = r.id.clone();
                                        let other_code = if r.from_code == code.get_untracked() {
                                            r.to_code.clone()
                                        } else if r.to_code == code.get_untracked() {
                                            r.from_code.clone()
                                        } else {
                                            r.from_code.clone()
                                        };
                                        let other_title = titles
                                            .get(&other_code)
                                            .cloned()
                                            .unwrap_or_else(|| other_code.clone());
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
                                                    <span class="mut" style="font-size:0.85em">" (来自)"</span>
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
                                                                >"确认删除"</button>
                                                                <button
                                                                    class="btn sm"
                                                                    on:click=move |_| confirm_del.set(None)
                                                                >"取消"</button>
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
                                                            >"删除"</button>
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
                    return view! { <div></div> }.into_any();
                }
                let cur = code.get();
                if cur.is_empty() {
                    return view! { <div></div> }.into_any();
                }
                view! {
                    <div class="rel-composer">
                        <input
                            class="rel-input"
                            placeholder="目标条目编码"
                            prop:value=move || new_target.get()
                            on:input=move |ev| new_target.set(event_target_value(&ev))
                        />
                        <select
                            class="rel-select"
                            on:change=move |ev| new_kind.set(event_target_value(&ev))
                        >
                            {PRESETS.iter().map(|(k, v)| {
                                let v = v.to_string();
                                let k = k.to_string();
                                view! {
                                    <option value=k.clone()>{v}</option>
                                }
                            }).collect::<Vec<_>>()}
                        </select>
                        {move || if new_kind.get() == "custom" {
                            view! {
                                <input
                                    class="rel-input"
                                    placeholder="自定义语义名"
                                    prop:value=move || new_custom.get()
                                    on:input=move |ev| new_custom.set(event_target_value(&ev))
                                />
                            }.into_any()
                        } else {
                            view! { <div></div> }.into_any()
                        }}
                        <button class="btn pri sm" on:click=submit>"添加"</button>
                    </div>
                }.into_any()
            }}
        </div>
    }
}