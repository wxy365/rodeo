use leptos::prelude::*;

use crate::frontend::graphql_client::NamedPrompt;

/// 一行「名称 + 提示词」。两个字段各自独立更新，和 `workspace_main.rs` 里的
/// `DraftLabel` 同一路数：RwSignal 字段让单行可以就地改，不必整表重建。
#[derive(Clone, Copy)]
pub struct PromptRow {
    pub name: RwSignal<String>,
    pub prompt: RwSignal<String>,
}

impl PromptRow {
    pub fn new(name: String, prompt: String) -> Self {
        Self {
            name: RwSignal::new(name),
            prompt: RwSignal::new(prompt),
        }
    }

    /// 名称去空白后为空的行会被丢掉：空行等于用户加了行又没填。
    /// 提示词允许为空——服务端把它当作「这条没配」。
    pub fn to_value(&self) -> Option<serde_json::Value> {
        let name = self.name.get_untracked().trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some(serde_json::json!({
            "name": name,
            "prompt": self.prompt.get_untracked(),
        }))
    }
}

pub fn rows_from(list: &[NamedPrompt]) -> Vec<PromptRow> {
    list.iter()
        .map(|p| PromptRow::new(p.name.clone(), p.prompt.clone()))
        .collect()
}

pub fn rows_to_value(rows: &RwSignal<Vec<PromptRow>>) -> Vec<serde_json::Value> {
    rows.get_untracked()
        .iter()
        .filter_map(PromptRow::to_value)
        .collect()
}

/// 一组可增删的「名称 + 提示词」行，场景与语气共用。
#[component]
pub fn PromptRows(rows: RwSignal<Vec<PromptRow>>, placeholder: String) -> impl IntoView {
    view! {
        <div class="stack">
            {move || {
                rows.get()
                    .into_iter()
                    .enumerate()
                    .map(|(i, r)| {
                        view! {
                            <div style="display:flex;gap:8px;align-items:flex-start">
                                <input class="inp" style="width:160px" placeholder=placeholder.clone()
                                    prop:value=move || r.name.get()
                                    on:input=move |ev| r.name.set(event_target_value(&ev)) />
                                <textarea class="inp" rows="2" placeholder="提示词"
                                    prop:value=move || r.prompt.get()
                                    on:input=move |ev| r.prompt.set(event_target_value(&ev))></textarea>
                                <button class="btn sm" type="button"
                                    on:click=move |_| rows.update(|v| { if i < v.len() { v.remove(i); } })>
                                    "删除"
                                </button>
                            </div>
                        }
                    })
                    .collect::<Vec<_>>()
            }}
            <button class="btn sm" type="button" style="align-self:flex-start"
                on:click=move |_| rows.update(|v| v.push(PromptRow::new(String::new(), String::new())))>
                "添加一行"
            </button>
        </div>
    }
}
