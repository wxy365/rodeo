use leptos::ev::SubmitEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::{json, Value};

use super::graphql_client::{
    automation_rules, create_automation_rule, delete_automation_rule, parse_rule_trigger,
    update_automation_rule, AutomationRule, LabelSchema,
};

/// 编辑表单里的一行动作。信号放在行内，删除行时随之释放。
/// 标记 Copy 才能被同一行的多个事件闭包各自捕获。
#[derive(Clone, Copy)]
struct DraftWrite {
    label_name: RwSignal<String>,
    op: RwSignal<String>,
    value_kind: RwSignal<String>,
    value: RwSignal<String>,
}

impl DraftWrite {
    fn new(schema: &LabelSchema) -> Self {
        Self {
            label_name: RwSignal::new(schema.name.clone()),
            op: RwSignal::new("set".to_string()),
            value_kind: RwSignal::new("literal".to_string()),
            value: RwSignal::new(String::new()),
        }
    }

    /// `value_type` 是目标标签的声明类型，只有它决定字面量怎么编码。
    /// 若按「文本像不像 JSON」去猜，string 标签里填 `3` 会被发成数字而被服务端
    /// 按类型拒收（`LabelValue::from_json` 对 String 只认 `as_str`），用户只能
    /// 加引号绕开。
    /// 数值型都按字面解析——currency 也是（服务端对它 `as_f64`），漏掉它会让金额
    /// 字面量被发成字符串而整条规则写入失败。
    fn to_input(&self, value_type: Option<&str>) -> Value {
        let kind = self.value_kind.get_untracked();
        let raw = self.value.get_untracked();
        // 其它来源（now/new/old）没有可填的值，value 一律传 null。
        let value = if kind == "literal" {
            match value_type {
                Some("integer" | "float" | "boolean" | "currency") => {
                    serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw))
                }
                _ => Value::String(raw),
            }
        } else {
            Value::Null
        };
        json!({
            "labelName": self.label_name.get_untracked(),
            "op": self.op.get_untracked(),
            "valueKind": kind,
            "value": value,
        })
    }
}

/// 设置页的「自动化」标签页。`can_manage` 为 false 时只读。
#[component]
pub fn AutomationTab(
    #[prop(into)] ws_id: Signal<String>,
    schemas: Signal<Vec<LabelSchema>>,
    can_manage: bool,
    on_changed: Callback<()>,
) -> impl IntoView {
    let rules = RwSignal::new(Vec::<AutomationRule>::new());
    let error = RwSignal::new(None::<String>);
    let loaded = RwSignal::new(false);

    // 编辑态：None = 未打开表单。
    let editing_id = RwSignal::new(None::<String>);
    let form_open = RwSignal::new(false);
    let f_name = RwSignal::new(String::new());
    let f_enabled = RwSignal::new(true);
    let f_trigger = RwSignal::new(String::new());
    let f_event_source = RwSignal::new(true);
    let f_target = RwSignal::new(String::new());
    let f_writes = RwSignal::new(Vec::<DraftWrite>::new());
    let busy = RwSignal::new(false);
    let form_error = RwSignal::new(None::<String>);

    let load = move || {
        let id = ws_id.get_untracked();
        if id.is_empty() {
            return;
        }
        spawn_local(async move {
            match automation_rules(&id).await {
                Ok(list) => {
                    rules.set(list);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
            loaded.set(true);
        });
    };
    Effect::new_sync(move |_| {
        let _ = ws_id.get();
        load();
    });

    // 没有标签定义时也能打开表单（零行动作），避免 `list[0]` 越界；用户随后可用「添加动作」补行。
    let open_new = move |_| {
        let list = schemas.get_untracked();
        editing_id.set(None);
        f_name.set(String::new());
        f_enabled.set(true);
        f_trigger.set(format!(
            "$label = \"{}\" AND $new = \"\"",
            list.first().map(|s| s.name.clone()).unwrap_or_default()
        ));
        f_event_source.set(true);
        f_target.set(String::new());
        f_writes.set(if let Some(first) = list.first() {
            vec![DraftWrite::new(first)]
        } else {
            Vec::new()
        });
        form_error.set(None);
        form_open.set(true);
    };

    let open_edit = Callback::new(move |rule: AutomationRule| {
        let list = schemas.get_untracked();
        editing_id.set(Some(rule.id.clone()));
        f_name.set(rule.name.clone());
        f_enabled.set(rule.enabled);
        f_trigger.set(rule.trigger_expr.clone());
        f_event_source.set(rule.target_event_source);
        f_target.set(rule.target_expr.clone());
        let drafts: Vec<DraftWrite> = rule
            .writes
            .iter()
            .map(|w| DraftWrite {
                label_name: RwSignal::new(w.label_name.clone()),
                op: RwSignal::new(w.op.clone()),
                // 服务端对 remove 写返回空 valueKind，回填成「固定值」占位以便下拉展示。
                value_kind: RwSignal::new(if w.value_kind.is_empty() {
                    "literal".to_string()
                } else {
                    w.value_kind.clone()
                }),
                value: RwSignal::new(match &w.value {
                    Value::Null => String::new(),
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                }),
            })
            .collect();
        f_writes.set(if drafts.is_empty() {
            list.first().map(DraftWrite::new).into_iter().collect()
        } else {
            drafts
        });
        form_error.set(None);
        form_open.set(true);
    });

    let add_write = move |_| {
        let list = schemas.get_untracked();
        if let Some(first) = list.first() {
            f_writes.update(|w| w.push(DraftWrite::new(first)));
        }
    };

    let check_trigger = move |_| {
        let id = ws_id.get_untracked();
        let expr = f_trigger.get_untracked();
        spawn_local(async move {
            match parse_rule_trigger(&id, &expr).await {
                Ok(_) => form_error.set(None),
                Err(e) => form_error.set(Some(e)),
            }
        });
    };

    let submit = move |ev: SubmitEvent| {
        ev.prevent_default();
        let id = ws_id.get_untracked();
        let name = f_name.get_untracked();
        if name.trim().is_empty() {
            form_error.set(Some("请填写规则名称".to_string()));
            return;
        }
        let writes: Vec<Value> = f_writes
            .get_untracked()
            .iter()
            .map(|w| {
                let name = w.label_name.get_untracked();
                let vt = schemas
                    .get_untracked()
                    .into_iter()
                    .find(|s| s.name == name)
                    .map(|s| s.value_type);
                w.to_input(vt.as_deref())
            })
            .collect();
        let writes = Value::Array(writes);
        let target = f_target.get_untracked();
        let target_arg: Option<String> = (!f_event_source.get_untracked()).then_some(target);
        let editing = editing_id.get_untracked();
        busy.set(true);
        spawn_local(async move {
            let result = match &editing {
                Some(rid) => {
                    update_automation_rule(
                        rid,
                        &name,
                        f_enabled.get_untracked(),
                        &f_trigger.get_untracked(),
                        f_event_source.get_untracked(),
                        target_arg.as_deref(),
                        &writes,
                    )
                    .await
                }
                None => {
                    create_automation_rule(
                        &id,
                        &name,
                        f_enabled.get_untracked(),
                        &f_trigger.get_untracked(),
                        f_event_source.get_untracked(),
                        target_arg.as_deref(),
                        &writes,
                    )
                    .await
                }
            };
            busy.set(false);
            match result {
                Ok(_) => {
                    form_open.set(false);
                    form_error.set(None);
                    load();
                    on_changed.run(());
                }
                Err(e) => form_error.set(Some(e)),
            }
        });
    };

    let remove = Callback::new(move |rid: String| {
        spawn_local(async move {
            match delete_automation_rule(&rid).await {
                Ok(_) => {
                    load();
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    });

    view! {
        <h2>"自动化规则"</h2>
        <p class="mut">
            "标签被写入时触发。触发条件用 $label / $old / $new 引用本次变更，"
            "也可以直接用标签名（如 Priority >= 3）判断事件源条目的写入后状态。"
        </p>
        {move || error.get().map(|e| view! { <p class="error">{e}</p> })}
        {move || {
            if !loaded.get() {
                view! { <div class="empty">"加载中…"</div> }.into_any()
            } else if rules.get().is_empty() {
                view! { <div class="empty">"还没有规则"</div> }.into_any()
            } else {
                view! {
                    <table class="tbl">
                        <thead>
                            <tr>
                                <th>"名称"</th>
                                <th>"触发条件"</th>
                                <th>"动作"</th>
                                <th>"启用"</th>
                                <th></th>
                            </tr>
                        </thead>
                        <tbody>
                            {rules
                                .get()
                                .into_iter()
                                .map(|r| {
                                    let id = r.id.clone();
                                    let open = r.clone();
                                    let enabled = r.enabled;
                                    view! {
                                        <tr>
                                            <td>{r.name.clone()}</td>
                                            <td class="mono">{r.trigger_expr.clone()}</td>
                                            <td>
                                                {format!(
                                                    "{} → {}",
                                                    if r.target_event_source { "事件源条目" } else { "表达式圈定" },
                                                    r.writes
                                                        .iter()
                                                        .map(|w| format!("{}({})", w.label_name, w.op))
                                                        .collect::<Vec<_>>()
                                                        .join("、"),
                                                )}
                                            </td>
                                            <td>{if enabled { "是" } else { "否" }}</td>
                                            <td>
                                                {if can_manage {
                                                    view! {
                                                        <button class="btn sm" on:click=move |_| open_edit.run(open.clone())>
                                                            "编辑"
                                                        </button>
                                                        <button class="btn sm dgr" on:click=move |_| remove.run(id.clone())>
                                                            "删除"
                                                        </button>
                                                    }.into_any()
                                                } else {
                                                    view! { <span class="mut">"—"</span> }.into_any()
                                                }}
                                            </td>
                                        </tr>
                                    }
                                })
                                .collect::<Vec<_>>()}
                        </tbody>
                    </table>
                }.into_any()
            }
        }}
        {if can_manage {
            view! {
                <button class="btn pri" on:click=open_new style="align-self:flex-start">
                    "新建规则"
                </button>
            }.into_any()
        } else {
            view! { <p class="mut">"仅 Maintainer 及以上可编辑"</p> }.into_any()
        }}

        {move || {
            if !form_open.get() {
                return view! { <div></div> }.into_any();
            }
            let schema_list = schemas.get();
            view! {
                <div class="dmodal on">
                    <form class="dmbox stack" on:submit=submit>
                        <h3>{move || if editing_id.get().is_some() { "编辑规则" } else { "新建规则" }}</h3>
                        {move || form_error.get().map(|e| view! { <p class="error">{e}</p> })}
                        <label class="fld">
                            <span>"名称"</span>
                            <input class="inp" prop:value=f_name
                                on:input=move |ev| f_name.set(event_target_value(&ev)) />
                        </label>
                        <label class="fld">
                            <span>"启用"</span>
                            <input type="checkbox" prop:checked=move || f_enabled.get()
                                on:change=move |ev| f_enabled.set(event_target_checked(&ev)) />
                        </label>
                        <label class="fld">
                            <span>"触发条件"</span>
                            <textarea class="inp mono" rows="3" prop:value=f_trigger
                                on:input=move |ev| f_trigger.set(event_target_value(&ev))></textarea>
                        </label>
                        <div class="mut" style="font-size:12px">
                            "可用关键字：" <code>"$label"</code> "（本次变更的标签名）、"
                            <code>"$old"</code> "（旧值，新增时用 " <code>"!$old"</code> "）、"
                            <code>"$new"</code> "（新值，删除时用 " <code>"!$new"</code> "）。"
                        </div>
                        <div style="display:flex;gap:8px;flex-wrap:wrap">
                            {schema_list
                                .iter()
                                .map(|s| {
                                    let name = s.name.clone();
                                    view! {
                                        <button type="button" class="btn sm"
                                            on:click=move |_| {
                                                let cur = f_trigger.get_untracked();
                                                f_trigger.set(format!("{cur} {name}"));
                                            }>
                                            {s.name.clone()}
                                        </button>
                                    }
                                })
                                .collect::<Vec<_>>()}
                            <button type="button" class="btn sm" on:click=check_trigger>"校验"</button>
                        </div>
                        <label class="fld">
                            <span>"动作目标"</span>
                            <select class="inp" prop:value=move || if f_event_source.get() { "event" } else { "query" }
                                on:change=move |ev| f_event_source.set(event_target_value(&ev) == "event")>
                                <option value="event">"事件源条目"</option>
                                <option value="query">"表达式圈定"</option>
                            </select>
                        </label>
                        {move || {
                            if f_event_source.get() {
                                return view! { <div></div> }.into_any();
                            }
                            view! {
                                <label class="fld">
                                    <span>"圈定表达式"</span>
                                    <input class="inp mono" prop:value=f_target
                                        on:input=move |ev| f_target.set(event_target_value(&ev)) />
                                </label>
                            }.into_any()
                        }}
                        <div class="fld">
                            <span>"标签动作"</span>
                            {move || {
                                f_writes
                                    .get()
                                    .into_iter()
                                    .enumerate()
                                    .map(|(idx, w)| {
                                        let opts: Vec<(String, String)> = schemas
                                            .get()
                                            .into_iter()
                                            .map(|s| (s.name.clone(), s.title.clone()))
                                            .collect();
                                        let is_remove = w.op.get() == "remove";
                                        let is_literal = w.value_kind.get() == "literal";
                                        view! {
                                            <div style="display:flex;gap:6px;align-items:center;margin:4px 0">
                                                <select class="inp" style="width:160px"
                                                    prop:value=move || w.label_name.get()
                                                    on:change=move |ev| w.label_name.set(event_target_value(&ev))>
                                                    {opts
                                                        .iter()
                                                        .map(|(n, t)| view! { <option value=n.clone()>{t.clone()}</option> })
                                                        .collect::<Vec<_>>()}
                                                </select>
                                                <select class="inp" style="width:110px"
                                                    prop:value=move || w.op.get()
                                                    on:change=move |ev| w.op.set(event_target_value(&ev))>
                                                    <option value="set">"写入"</option>
                                                    <option value="remove">"删除"</option>
                                                </select>
                                                {(!is_remove)
                                                    .then(|| {
                                                        view! {
                                                            <select class="inp" style="width:130px" prop:value=move || w.value_kind.get()
                                                                on:change=move |ev| w.value_kind.set(event_target_value(&ev))>
                                                                <option value="literal">"固定值"</option>
                                                                <option value="now">"当前时间"</option>
                                                                <option value="new">"事件新值"</option>
                                                                <option value="old">"事件旧值"</option>
                                                            </select>
                                                            <input class="inp" style="width:160px"
                                                                prop:value=move || w.value.get()
                                                                disabled=!is_literal
                                                                on:input=move |ev| w.value.set(event_target_value(&ev)) />
                                                        }.into_any()
                                                    })}
                                                <button type="button" class="btn sm dgr"
                                                    on:click=move |_| f_writes.update(|ws| {
                                                        if idx < ws.len() {
                                                            ws.remove(idx);
                                                        }
                                                    })>
                                                    "移除"
                                                </button>
                                            </div>
                                        }
                                    })
                                    .collect::<Vec<_>>()
                            }}
                            <button type="button" class="btn sm" on:click=add_write>"＋ 添加动作"</button>
                        </div>
                        <div style="display:flex;gap:8px">
                            <button class="btn pri" type="submit" disabled=move || busy.get()>"保存"</button>
                            <button class="btn" type="button" on:click=move |_| form_open.set(false)>"取消"</button>
                        </div>
                    </form>
                </div>
            }.into_any()
        }}
    }
}
