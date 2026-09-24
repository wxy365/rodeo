use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::graphql_client::{
    mark_all_messages_read, mark_message_read, messages as fetch_messages, GqlMessage,
};
use crate::frontend::icons::ic_close;

/// 站内消息列表。点开气泡时挂载，关掉时卸载。
/// 列表项点击：单条标已读（不强制跳转 Entry——spec 没要求，跳转需要 workspace slug
/// 信息；当前 `GqlMessage` 不携带，留作后续增强）。
#[component]
pub fn MessageList(visible: Signal<bool>, on_close: Callback<()>) -> impl IntoView {
    let items = RwSignal::new(None::<Result<Vec<GqlMessage>, String>>);
    let close = move |_| on_close.run(());

    let reload = move || {
        if !visible.get() {
            return;
        }
        spawn_local(async move {
            match fetch_messages(Some(100)).await {
                Ok(list) => items.set(Some(Ok(list))),
                Err(e) => items.set(Some(Err(e))),
            }
        });
    };

    Effect::new(move |_| {
        let v = visible.get();
        if v {
            reload();
        }
    });

    view! {
        <div class="msg-panel" class:hidden=move || !visible.get()>
            <div class="msg-head">
                <span>"消息"</span>
                <button class="btn sm" on:click=move |_| {
                    spawn_local(async move {
                        let _ = mark_all_messages_read().await;
                        reload();
                    });
                }>"全部已读"</button>
                <button class="btn sm icon-only" on:click=close>{ic_close()}</button>
            </div>
            <div class="msg-body">
                {move || match items.get() {
                    None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            view! { <div class="mut">"暂无消息"</div> }.into_any()
                        } else {
                            list.into_iter().map(|m| {
                                let id = m.id.clone();
                                let cls = if m.read { "msg-item" } else { "msg-item unread" };
                                view! {
                                    <div class=cls on:click={
                                        let id = id.clone();
                                        move |_| {
                                            let id = id.clone();
                                            spawn_local(async move {
                                                let _ = mark_message_read(&id).await;
                                            });
                                        }
                                    }>
                                        <div class="msg-meta">
                                            <span class="msg-actor">{m.actor_name}</span>
                                            <span class="mut">" 在 "</span>
                                            <span class="msg-ws">{m.workspace_name}</span>
                                            <span class="mut">" 中提到你"</span>
                                        </div>
                                        <div class="msg-preview">{m.preview}</div>
                                    </div>
                                }
                            }).collect::<Vec<_>>().into_any()
                        }
                    }
                }}
            </div>
        </div>
    }
}