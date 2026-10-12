use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::frontend::graphql_client::{
    mark_all_messages_read, mark_message_read, messages as fetch_messages, GqlMessage,
};
use crate::frontend::i18n::{use_i18n, I18nController};
use crate::frontend::icons::ic_close;

/// 站内消息列表。点开气泡时挂载，关掉时卸载。
/// 列表项点击：标已读 + 跳转 Entry 全屏详情。
/// 跳转走 Router 的 `/{workspace_slug}/entry/{entry_code}` 路由，与左侧列表
/// 点条目同一条 URL——回退、重新加载、历史栈行为一致。`workspace_slug` 为
/// 空的兜底（工作空间已被硬删的极端情况）退化为只标已读。
#[component]
pub fn MessageList(visible: Signal<bool>, on_close: Callback<()>) -> impl IntoView {
    let i18n: I18nController = use_i18n();
    let navigate = use_navigate();
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
                <span>{move || i18n.t("message.title")}</span>
                <button class="btn sm" on:click=move |_| {
                    spawn_local(async move {
                        let _ = mark_all_messages_read().await;
                        reload();
                    });
                }>{move || i18n.t("message.mark_all_read")}</button>
                <button class="btn sm icon-only" on:click=close>{ic_close()}</button>
            </div>
            <div class="msg-body">
                {move || match items.get() {
                    None => view! { <div class="mut">{move || i18n.t("common.loading")}</div> }.into_any(),
                    Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                    Some(Ok(list)) => {
                        if list.is_empty() {
                            view! { <div class="mut">{move || i18n.t("message.empty")}</div> }.into_any()
                        } else {
                            list.into_iter().map(|m| {
                                let id = m.id.clone();
                                let workspace_slug = m.workspace_slug.clone();
                                let entry_code = m.entry_code.clone();
                                let cls = if m.read { "msg-item" } else { "msg-item unread" };
                                // 有 slug 才走可点击样式 + 跳转；空 slug（工作空间已被
                                // 硬删的极端情况）退化为只读行，仍能标已读避免无限残留。
                                let clickable = !workspace_slug.is_empty() && !entry_code.is_empty();
                                let cls = if clickable {
                                    format!("{cls} clickable")
                                } else {
                                    cls.to_string()
                                };
                                // `navigate` 是 `impl Fn(...) + Clone`，但点击闭包被
                                // `move` 捕获后只能取一次；外层再克隆一份给闭包用。
                                // 同款模式见 app.rs:OAuthCallback。
                                let nav = navigate.clone();
                                view! {
                                    <div class=cls on:click={
                                        let id = id.clone();
                                        let workspace_slug = workspace_slug.clone();
                                        let entry_code = entry_code.clone();
                                        move |_| {
                                            let id = id.clone();
                                            let workspace_slug = workspace_slug.clone();
                                            let entry_code = entry_code.clone();
                                            // 乐观更新：先在本地把这一行标成已读再发请求，
                                            // 否则等后端确认期间用户还能看到这条未读的高亮，
                                            // 会觉得"点了没反应"。
                                            spawn_local(async move {
                                                let _ = mark_message_read(&id).await;
                                                items.update(|slot| {
                                                    if let Some(Ok(list)) = slot.as_mut() {
                                                        if let Some(m) =
                                                            list.iter_mut().find(|m| m.id == id)
                                                        {
                                                            m.read = true;
                                                        }
                                                    }
                                                });
                                            });
                                            // 跳 Entry 全屏详情。关掉面板再跳，避免浮层
                                            // 盖住目标页；空 slug（工作空间已硬删）跳过跳转，
                                            // 退化为只标已读。
                                            if !workspace_slug.is_empty() && !entry_code.is_empty() {
                                                on_close.run(());
                                                let path = format!("/{}/entry/{}", workspace_slug, entry_code);
                                                nav(&path, Default::default());
                                            }
                                        }
                                    }>
                                        <div class="msg-meta">
                                            <span class="msg-actor">{m.actor_name}</span>
                                            <span class="mut">{move || i18n.t("message.mentioned_in")}</span>
                                            <span class="msg-ws">{m.workspace_name}</span>
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
