use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

use super::components::{display_enum_value, from_native, is_native_time_layout, value_to_string};
use super::graphql_client::{remove_labeling, set_labeling, Labeling, LabelSchema};
use super::icons::{ic_close, ic_tag};
use super::query_eval::resolve_label_color;

/// 标签编辑器（Entry 详情用）：只展示**这个条目已拥有**的标签，支持增 / 删 / 改。
///
/// - 增：底部的「＋ 添加标签」只列出尚未打上的标签定义。无值标签（value_type = null）
///   选中即落库；有值标签先生成一个待定行，填值后再落库。
/// - 删：每行的 × 直接 `remove_labeling`。
/// - 改：改值即 `set_labeling`，即时落库。
///
/// 每行按标签定义的配色（基础色 / 值色）着色，与列表里的标签列一致。
#[component]
pub fn LabelEditor(
    #[prop(into)] code: Signal<String>,
    schemas: RwSignal<Vec<LabelSchema>>,
    labels: RwSignal<Vec<Labeling>>,
    on_changed: Callback<()>,
) -> impl IntoView {
    // 正在新增、还没落库的标签名（本地待定行）。
    let pending = RwSignal::new(None::<String>);

    // 换条目时丢掉没填完的待定行。
    Effect::new(move |_| {
        code.get();
        pending.set(None);
    });

    // 写值 / 删值：落库后清掉待定行，并通知外层重载标签与列表列。
    let apply = Callback::new(move |(name, value): (String, Option<Value>)| {
        let c = code.get_untracked();
        spawn_local(async move {
            match value {
                Some(v) => {
                    let _ = set_labeling(&c, &name, &v).await;
                }
                None => {
                    let _ = remove_labeling(&c, &name).await;
                }
            }
            pending.set(None);
            on_changed.run(());
        });
    });

    view! {
        <div class="lbledit">
            {move || {
                let schemas_now = schemas.get();
                labels
                    .get()
                    .into_iter()
                    .map(|l| {
                        let name = l.label_name.clone();
                        let schema = schemas_now.iter().find(|s| s.name == name).cloned();
                        let title = row_title(&schema, &name);
                        let color = row_color(&schema, &l.value);
                        let on_set = set_callback(apply, name.clone());
                        let on_remove = remove_callback(apply, name);
                        view! {
                            <LabelRow title=title schema=schema value=Some(l.value) color=color
                                on_set on_remove />
                        }
                    })
                    .collect::<Vec<_>>()
            }}
            {move || {
                // 待定行：选中但还没填值的新标签（无值标签不会走到这里）。
                pending
                    .get()
                    .map(|name| {
                        let schema = schemas.get().into_iter().find(|s| s.name == name);
                        let title = row_title(&schema, &name);
                        let color = row_color(&schema, &Value::Null);
                        let cancel = Callback::new(move |_| pending.set(None));
                        view! {
                            <LabelRow title=title schema=schema value=None color=color
                                on_set=set_callback(apply, name) on_remove=cancel />
                        }
                    })
            }}
            {move || {
                // 「＋ 添加标签」：只列还没打上的标签定义，避免和已有行重复。
                let taken: Vec<String> = labels.get().into_iter().map(|l| l.label_name).collect();
                let pend = pending.get();
                let avail: Vec<LabelSchema> = schemas
                    .get()
                    .into_iter()
                    .filter(|s| {
                        !taken.contains(&s.name) && pend.as_deref() != Some(s.name.as_str())
                    })
                    .collect();
                if avail.is_empty() {
                    return ().into_any();
                }
                view! {
                    <select class="lbladd" on:change=move |ev| {
                        let v = event_target_value(&ev);
                        if v.is_empty() {
                            return;
                        }
                        // 无值标签没有可填的东西，选中即视为「打上」。
                        let is_valueless = schemas
                            .get_untracked()
                            .iter()
                            .find(|s| s.name == v)
                            .is_some_and(|s| s.value_type == "null");
                        if is_valueless {
                            apply.run((v, Some(Value::Null)));
                        } else {
                            pending.set(Some(v));
                        }
                    }>
                        <option value="" selected=true>"＋ 添加标签"</option>
                        {avail
                            .into_iter()
                            .map(|s| {
                                let title = row_title(&Some(s.clone()), &s.name);
                                view! { <option value=s.name.clone()>{title}</option> }
                            })
                            .collect::<Vec<_>>()}
                    </select>
                }
                .into_any()
            }}
        </div>
    }
}

/// 展示名：优先标签定义的标题，缺失（或为空、或标签定义已不存在）时回退到 name。
fn row_title(schema: &Option<LabelSchema>, name: &str) -> String {
    schema
        .as_ref()
        .map(|s| s.title.trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| name.to_string())
}

/// 行配色：标签定义的值色优先，未命中回退基础色；定义已不存在则不着色。
fn row_color(schema: &Option<LabelSchema>, value: &Value) -> Option<String> {
    let s = schema.as_ref()?;
    let base = s.color.clone().map(Value::String);
    resolve_label_color(base.as_ref(), &s.value_colors, value)
}

/// 把「按名字写值」适配成单行的 `on_set`。
fn set_callback(apply: Callback<(String, Option<Value>)>, name: String) -> Callback<Value> {
    Callback::new(move |v: Value| apply.run((name.clone(), Some(v))))
}

/// 把「按名字删值」适配成单行的 `on_remove`。
fn remove_callback(apply: Callback<(String, Option<Value>)>, name: String) -> Callback<()> {
    Callback::new(move |_| apply.run((name.clone(), None)))
}

/// 单行的值控件 + 删除按钮。`value=None` 表示这是还没落库的待定行。
#[component]
fn LabelRow(
    title: String,
    schema: Option<LabelSchema>,
    value: Option<Value>,
    color: Option<String>,
    on_set: Callback<Value>,
    on_remove: Callback<()>,
) -> impl IntoView {
    let current = value.as_ref().map(value_to_string).unwrap_or_default();
    // 各分支的闭包都要各自持有一份值，故在 match 之前克隆一份（`value` 之后不再使用）。
    let value_owned = value.clone();
    let control = match schema {
        // 标签定义已被删除，但条目上还留着打标：只读显示原值，仍可移除。
        None => view! { <span class="mut">{current.clone()}</span> }.into_any(),
        // 无值标签：没有值可编辑，只剩「打上了」这一事实。
        Some(s) if s.value_type == "null" => ().into_any(),
        // 多选 Enum：勾选集合整体作为一个数组值写入；清空则移除该标签。
        Some(s) if s.value_type == "enum" && s.multi => {
            let opts = s.enum_values.clone();
            let selected: std::collections::HashSet<String> = value_owned
                .as_ref()
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            view! {
                <div class="multienum">
                    {opts
                        .into_iter()
                        .map(|o| {
                            let checked = selected.contains(&o);
                            let cur = value_owned.clone();
                            let oc = o.clone();
                            view! {
                                <label class="mut">
                                    <input type="checkbox" prop:checked=checked on:change=move |ev| {
                                        let mut set: Vec<String> = cur
                                            .as_ref()
                                            .and_then(|v| v.as_array())
                                            .map(|a| {
                                                a.iter()
                                                    .filter_map(|x| x.as_str().map(str::to_string))
                                                    .collect()
                                            })
                                            .unwrap_or_default();
                                        if event_target_checked(&ev) {
                                            if !set.contains(&oc) {
                                                set.push(oc.clone());
                                            }
                                        } else {
                                            set.retain(|x| x != &oc);
                                        }
                                        // 清空即移除该标签，否则写数组（服务端按 multi schema 接受）。
                                        if set.is_empty() {
                                            on_remove.run(());
                                        } else {
                                            on_set
                                                .run(
                                                    Value::Array(
                                                        set.into_iter().map(Value::String).collect(),
                                                    ),
                                                );
                                        }
                                    } />
                                    {display_enum_value(&o)}
                                </label>
                            }
                        })
                        .collect::<Vec<_>>()}
                </div>
            }
            .into_any()
        }
        Some(s) if s.value_type == "enum" => {
            let opts = s.enum_values.clone();
            let cur = current.clone();
            view! {
                <select on:change=move |ev| {
                    let v = event_target_value(&ev);
                    if !v.is_empty() {
                        on_set.run(Value::String(v));
                    }
                }>
                    <option value="" selected=cur.is_empty() disabled>"选择…"</option>
                    {opts
                        .into_iter()
                        .map(|o| {
                            view! { <option value=o.clone() selected=cur == o>{display_enum_value(&o)}</option> }
                        })
                        .collect::<Vec<_>>()}
                </select>
            }
            .into_any()
        }
        Some(s) if s.value_type == "boolean" => {
            let checked = current == "true";
            view! {
                <input type="checkbox" prop:checked=checked on:change=move |ev| {
                    on_set.run(Value::Bool(event_target_checked(&ev)));
                } />
            }
            .into_any()
        }
        // 日期 / 时间 / 日期时间：默认布局直接用原生控件；自定义布局退回文本 + 解析校验。
        Some(s) if matches!(s.value_type.as_str(), "date" | "time" | "datetime") => {
            let vt = s.value_type.clone();
            let layout = s
                .format
                .clone()
                .unwrap_or_else(|| match vt.as_str() {
                    "date" => crate::golayout::DATE_LAYOUT.to_string(),
                    "time" => crate::golayout::TIME_LAYOUT.to_string(),
                    _ => crate::golayout::DATETIME_LAYOUT.to_string(),
                });
            let native_ok = is_native_time_layout(&vt, s.format.as_deref());
            if !native_ok {
                // 自定义布局：原生控件表达不了，退回文本输入；解析通过才落库。
                let input = RwSignal::new(current.clone());
                let layout_cb = layout.clone();
                view! {
                    <input class="inp" style="width:180px" prop:value=input on:change=move |ev| {
                        let v = event_target_value(&ev);
                        input.set(v.clone());
                        if crate::golayout::parse(&layout_cb, &v).is_some() {
                            on_set.run(Value::String(v));
                        }
                    } />
                }
                .into_any()
            } else {
                let input_type = match vt.as_str() {
                    "date" => "date",
                    "time" => "time",
                    _ => "datetime-local",
                };
                let current_native = to_native(&vt, &current);
                view! {
                    <input class="inp" type=input_type prop:value=current_native on:change=move |ev| {
                        let v = event_target_value(&ev);
                        match from_native(&vt, &v) {
                            Some(stored) => on_set.run(Value::String(stored)),
                            // 清空原生控件 → 移除该标签，与 currency / email 分支一致。
                            None => on_remove.run(()),
                        }
                    } />
                }
                .into_any()
            }
        }
        // 金额：数值输入 + 货币符号 / 单位前缀；清空即移除标签。
        Some(s) if s.value_type == "currency" => {
            let input = RwSignal::new(current.clone());
            let prefix = format!(
                "{}{}",
                s.currency_symbol.clone().unwrap_or_else(|| "¥".to_string()),
                s.unit.clone().map(|u| format!(" {u}")).unwrap_or_default()
            );
            view! {
                <div style="display:flex;gap:6px;align-items:center">
                    <span class="mut">{prefix}</span>
                    <input class="inp" style="width:110px" type="number" prop:value=input
                        on:input=move |ev| input.set(event_target_value(&ev)) />
                    <button class="btn" on:click=move |_| {
                        let v = input.get_untracked();
                        if v.is_empty() {
                            on_remove.run(());
                            return;
                        }
                        if let Some(n) = v
                            .parse::<f64>()
                            .ok()
                            .and_then(serde_json::Number::from_f64)
                        {
                            on_set.run(Value::Number(n));
                        }
                    }>"设置"</button>
                </div>
            }
            .into_any()
        }
        // 邮箱：文本输入，仅做「含 @」的提示性校验（真正的校验在服务端）；清空即移除标签。
        Some(s) if s.value_type == "email" => {
            let input = RwSignal::new(current.clone());
            view! {
                <div style="display:flex;gap:6px">
                    <input class="inp" style="width:180px" type="email" prop:value=input
                        on:input=move |ev| input.set(event_target_value(&ev)) />
                    <button class="btn" on:click=move |_| {
                        let v = input.get_untracked();
                        if v.is_empty() {
                            on_remove.run(());
                            return;
                        }
                        if v.contains('@') {
                            on_set.run(Value::String(v));
                        }
                    }>"设置"</button>
                </div>
            }
            .into_any()
        }
        Some(s) => {
            let vt = s.value_type.clone();
            let input = RwSignal::new(current.clone());
            view! {
                <div style="display:flex;gap:6px">
                    <input class="inp" style="width:110px"
                        type=if vt == "string" { "text" } else { "number" }
                        prop:value=input
                        on:input=move |ev| input.set(event_target_value(&ev)) />
                    <button class="btn" on:click=move |_| {
                        let v = input.get_untracked();
                        if v.is_empty() {
                            on_remove.run(());
                            return;
                        }
                        // 数字解析失败时保持静默，与改动前一致。
                        let value = match vt.as_str() {
                            "integer" => v.parse::<i64>().ok().map(|i| Value::Number(i.into())),
                            "float" => v
                                .parse::<f64>()
                                .ok()
                                .and_then(serde_json::Number::from_f64)
                                .map(Value::Number),
                            _ => Some(Value::String(v)),
                        };
                        if let Some(val) = value {
                            on_set.run(val);
                        }
                    }>"设置"</button>
                </div>
            }
            .into_any()
        }
    };

    // 配色直接落在 chip 上：底色淡、字色与描边用原色。
    let style = color
        .map(|c| {
            format!("border-color:{c};background:color-mix(in srgb, {c} 12%, transparent);color:{c}")
        })
        .unwrap_or_default();

    view! {
        <div class="lblrow" style=style>
            <span class="k">{ic_tag()}{title}</span>
            {control}
            <button class="ibtn" title="移除标签" on:click=move |_| on_remove.run(())>{ic_close()}</button>
        </div>
    }
}

/// 存储串（Go 默认布局）→ 原生控件的值。
fn to_native(vt: &str, s: &str) -> String {
    match vt {
        "datetime" => s.replacen(' ', "T", 1).get(..16).unwrap_or(s).to_string(),
        "time" => s.get(..5).unwrap_or(s).to_string(),
        _ => s.to_string(),
    }
}

