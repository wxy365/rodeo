//! Entry 选择器：按标题/编码自动补全的下拉。
//!
//! 用作「添加关联」等需要挑一条 entry 的场景。输入框接收查询词，
//! 防抖 200ms 触发后端 `searchEntries`（Tantivy ngram 跨 title/content/labels/
//! code/comments），下拉展示命中项 (title + code)；点击命中项 → 选定、
//! 把 `selected` 写成 `Some(entry)`。当前 entry 会被过滤掉，防止自环。
//!
//! 选中状态由调用方通过 `selected: RwSignal<Option<Entry>>` 持有——
//! picker 只负责写入，提交逻辑由父组件在读 `selected.get()` 后自行拼请求。
//! 这样 RelationsPanel 等其它复用方只需关心「选了哪条 entry」。

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::graphql_client::{search_entries, Entry};

const DEBOUNCE_MS: u32 = 200;
const SEARCH_LIMIT: i64 = 10;

#[cfg(target_arch = "wasm32")]
fn after_ms<F>(ms: u32, f: F)
where
    F: FnOnce() + 'static,
{
    use gloo_timers::callback::Timeout;
    let t = Timeout::new(ms, f);
    t.forget();
}

#[cfg(not(target_arch = "wasm32"))]
fn after_ms<F>(_ms: u32, _f: F)
where
    F: FnOnce() + 'static,
{
    // SSR 下不触发——pickup 的搜索没有可见的 DOM，过滤掉避免编译期依赖 gloo_timers。
}

#[component]
pub fn EntryPicker(
    /// 当前 entry 的 code，从结果中过滤掉（防自环）。空串 = 不过滤。
    exclude_code: Signal<String>,
    /// 工作空间 id，请求 searchEntries 时用。
    workspace_id: Signal<String>,
    /// 选中状态（外部持有，提交时由父组件读）。
    selected: RwSignal<Option<Entry>>,
) -> impl IntoView {
    let query = RwSignal::new(String::new());
    let results = RwSignal::new(Vec::<Entry>::new());
    let loading = RwSignal::new(false);
    let open = RwSignal::new(false);
    let error = RwSignal::new(Option::<String>::None);
    // 单调递增：每次 query 变化 +1。Effect 拿到自己触发时的快照值，
    // 触发 search 前再读一次当前 version；不一致就放弃——后到的请求不覆盖先到的结果。
    let version = RwSignal::new(0u64);

    // 把 selected 跟 query 同步：外部把 selected 改成 None（如提交成功后清空）时，
    // 输入框的 query 也要跟着清。但 `on:input` 也会把 selected 写 None，于是
    // 这个 Effect 一律触发的旧实现会把用户正在打的字立刻抹掉——这里加一道
    // 「只有 Some → None 才同步」的过滤，user 打字 / clear 按钮触发的 None
    // 都是「已知语义」不需要回灌。
    let prev_selected_some = RwSignal::new(false);
    Effect::new(move |_| {
        let cur = selected.get().is_some();
        let prev = prev_selected_some.get_untracked();
        prev_selected_some.set(cur);
        if cur {
            // pick() 内部已把 query 写成 title，外部基本不会写 selected=Some，
            // 漏掉 Some → Some 的 case 不会出问题。
            return;
        }
        if prev {
            // Some → None：外部 reset。输入框同步清空。
            query.set(String::new());
        }
        // None → None：用户打字 / clear 按钮触发的本地清空，query 由 on:input
        // 直接管，这里不掺和。
    });

    // 防抖触发搜索。
    Effect::new(move |_| {
        let v = query.get();
        // 已选中且 query 与选中 title 一致：用户在「选中后未再编辑」，别重搜。
        if let Some(s) = selected.get() {
            if v == s.title {
                return;
            }
        }
        version.update(|n| *n += 1);
        let my_v = version.get();
        let ws = workspace_id.get();
        let exclude = exclude_code.get();
        if v.trim().is_empty() || ws.is_empty() {
            results.set(Vec::new());
            loading.set(false);
            error.set(None);
            return;
        }
        loading.set(true);
        let q = v.clone();
        after_ms(DEBOUNCE_MS, move || {
            spawn_local(async move {
                // 进入闭包时再确认一次版本号，被中间新输入挤掉的请求就不发出去。
                if version.get_untracked() != my_v {
                    return;
                }
                let r = search_entries(&ws, &q, SEARCH_LIMIT).await;
                if version.get_untracked() != my_v {
                    return;
                }
                loading.set(false);
                match r {
                    Ok(page) => {
                        let mut items = page.items;
                        if !exclude.is_empty() {
                            items.retain(|e| e.code != exclude);
                        }
                        results.set(items);
                        error.set(None);
                    }
                    Err(e) => {
                        error.set(Some(e));
                        results.set(Vec::new());
                    }
                }
            });
        });
    });

    let pick = move |e: Entry| {
        let title = e.title.clone();
        selected.set(Some(e));
        query.set(title);
        open.set(false);
        results.set(Vec::new());
    };

    let clear = move |_| {
        selected.set(None);
        query.set(String::new());
        results.set(Vec::new());
        open.set(true);
    };

    view! {
        <div class="entry-picker">
            <div class="entry-picker-input">
                <input
                    type="text"
                    class="inp"
                    placeholder="搜索目标条目（按标题或编码）"
                    prop:value=move || query.get()
                    on:input=move |ev| {
                        // 用户开始改输入框即视为放弃当前选择（避免「选中态但显示的标题与输入框文字不一致」）。
                        selected.set(None);
                        query.set(event_target_value(&ev));
                        open.set(true);
                    }
                    on:focus=move |_| open.set(true)
                    on:blur=move |_| {
                        // 给「点击命中项」留出事件窗口——on:mousedown 在 blur 之前触发。
                        let open_sig = open;
                        after_ms(150, move || { open_sig.set(false); });
                    }
                />
                {move || if selected.get().is_some() {
                    view! { <button class="ibtn" type="button" on:click=clear title="清除选择">"×"</button> }.into_any()
                } else {
                    view! { <span></span> }.into_any()
                }}
            </div>
            {move || {
                if !open.get() {
                    return view! { <span></span> }.into_any();
                }
                if loading.get() && results.get().is_empty() {
                    return view! { <div class="entry-picker-results"><div class="mut">"搜索中…"</div></div> }.into_any();
                }
                if let Some(err) = error.get() {
                    return view! { <div class="entry-picker-results"><div class="hint">{"⚠ "}{err}</div></div> }.into_any();
                }
                let list = results.get();
                if list.is_empty() {
                    return view! { <div class="entry-picker-results"><div class="mut">"无匹配条目"</div></div> }.into_any();
                }
                view! {
                    <ul class="entry-picker-results">
                        {list.into_iter().map(|e| {
                            let e_for_click = e.clone();
                            let e_display = e.clone();
                            view! {
                                <li
                                    class="entry-picker-item"
                                    on:mousedown=move |_| pick(e_for_click.clone())
                                >
                                    <span class="ep-title">{e_display.title.clone()}</span>
                                    <span class="ep-code">{e_display.code.clone()}</span>
                                </li>
                            }
                        }).collect::<Vec<_>>()}
                    </ul>
                }.into_any()
            }}
        </div>
    }
}
