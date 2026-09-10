use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

use super::components::{display_enum_value, value_to_string};
use super::graphql_client::{remove_labeling, set_labeling, Labeling, LabelSchema};
use super::icons::ic_tag;

/// 标签编辑器：按 schema 类型渲染输入，变更即 upsert（保存并触发 on_changed 软重载）。
#[component]
pub fn LabelEditor(
    #[prop(into)] code: String,
    schemas: RwSignal<Vec<LabelSchema>>,
    labels: RwSignal<Vec<Labeling>>,
    on_changed: Callback<()>,
) -> impl IntoView {
    view! {
        <div>
            {move || schemas.get().into_iter().map(|s| {
                let name = s.name.clone();
                let title_text = s.title.clone();
                let current = labels
                    .get()
                    .into_iter()
                    .find(|l| l.label_name == name)
                    .map(|l| l.value);
                let current_str = current.as_ref().map(value_to_string).unwrap_or_default();

                if s.value_type == "enum" {
                    let opts = s.enum_values.clone();
                    let cur = current_str.clone();
                    let nm = name.clone();
                    let c = code.clone();
                    view! {
                        <div class="lblrow">
                            <span class="k">{ic_tag()}{title_text.clone()}</span>
                            <select on:change=move |ev| {
                                let v = event_target_value(&ev);
                                let n = nm.clone();
                                let c = c.clone();
                                spawn_local(async move {
                                    if v.is_empty() {
                                        let _ = remove_labeling(&c, &n).await;
                                    } else {
                                        let _ = set_labeling(&c, &n, &Value::String(v)).await;
                                    }
                                    on_changed.run(());
                                });
                            }>
                                <option value="" selected=cur.is_empty()>"（清除）"</option>
                                {opts.iter().cloned().map(|o| view! {
                                    <option value=o.clone() selected=cur == o>{display_enum_value(&o)}</option>
                                }).collect::<Vec<_>>()}
                            </select>
                        </div>
                    }.into_any()
                } else if s.value_type == "boolean" {
                    let checked = current_str == "true";
                    let nm = name.clone();
                    let c = code.clone();
                    view! {
                        <div class="lblrow">
                            <span class="k">{ic_tag()}{title_text.clone()}</span>
                            <input type="checkbox" prop:checked=checked on:change=move |ev| {
                                let v = event_target_checked(&ev);
                                let n = nm.clone();
                                let c = c.clone();
                                spawn_local(async move {
                                    let _ = set_labeling(&c, &n, &Value::Bool(v)).await;
                                    on_changed.run(());
                                });
                            } />
                        </div>
                    }.into_any()
                } else {
                    let vt = s.value_type.clone();
                    let input = RwSignal::new(current_str.clone());
                    let nm = name.clone();
                    let c = code.clone();
                    let input_type = if vt == "string" { "text" } else { "number" };
                    view! {
                        <div class="lblrow">
                            <span class="k">{ic_tag()}{title_text.clone()}</span>
                            <div style="display:flex;gap:6px">
                                <input class="inp" style="flex:1" type=input_type prop:value=input on:input=move |ev| input.set(event_target_value(&ev)) />
                                <button class="btn" on:click=move |_| {
                                    let v = input.get();
                                    let n = nm.clone();
                                    let c = c.clone();
                                    let vt = vt.clone();
                                    spawn_local(async move {
                                        if v.is_empty() {
                                            let _ = remove_labeling(&c, &n).await;
                                        } else {
                                            let value = match vt.as_str() {
                                                "integer" => v.parse::<i64>().ok().map(|i| Value::Number(i.into())),
                                                "float" => v.parse::<f64>().ok().and_then(serde_json::Number::from_f64).map(Value::Number),
                                                _ => Some(Value::String(v)),
                                            };
                                            if let Some(val) = value {
                                                let _ = set_labeling(&c, &n, &val).await;
                                            }
                                        }
                                        on_changed.run(());
                                    });
                                }>"设置"</button>
                            </div>
                        </div>
                    }.into_any()
                }
            }).collect::<Vec<_>>().into_any()}
        </div>
    }
}
