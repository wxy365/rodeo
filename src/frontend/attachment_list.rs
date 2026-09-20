use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::frontend::components::{human_size, logged_out, role_at_least};
use crate::frontend::graphql_client::{attachments, delete_attachment, me, my_role, Attachment};
// `upload_attachment` 只存在于 wasm 目标（`web_sys::File` 参数），单独按 cfg 引进来。
#[cfg(target_arch = "wasm32")]
use crate::frontend::graphql_client::upload_attachment;
use crate::frontend::icons::{ic_folder, ic_upload};

/// 把选中的文件传上去。`upload_attachment` 与 `web_sys` 都只在 wasm 目标下存在，
/// 而事件闭包在 SSR 期同样要过类型检查，于是照 `tiny_editor::mount_when_ready` 的
/// 双 `cfg` 写法把上传整段抽成两个同签名的自由函数：wasm 下真传，其余目标空实现。
#[cfg(target_arch = "wasm32")]
fn spawn_upload(
    ev: leptos::ev::Event,
    code: String,
    uploading: RwSignal<bool>,
    error: RwSignal<Option<String>>,
    reload: Callback<()>,
    on_changed: Callback<()>,
) {
    use js_sys::Reflect;
    use wasm_bindgen::{JsCast, JsValue};

    let Some(input) = ev
        .target()
        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
    else {
        return;
    };
    // `HtmlInputElement::files()` 要 web-sys 的 `FileList` 特性，而本仓库没开、Cargo.toml
    // 又不该由本任务改，于是照 `tiny_editor` 取粘贴文件的办法用 Reflect 读 `files[0]`
    // （没选文件时是 `undefined`，`dyn_into` 自然失败）。
    let Ok(files) = Reflect::get(AsRef::<JsValue>::as_ref(&input), &JsValue::from_str("files"))
    else {
        return;
    };
    let Ok(first) = Reflect::get(&files, &JsValue::from_f64(0.0)) else {
        return;
    };
    let Ok(file) = first.dyn_into::<web_sys::File>() else {
        return;
    };
    // 选完即传；input 立刻清空，同一个文件重选也能再触发 change。
    input.set_value("");
    if code.is_empty() {
        return;
    }
    uploading.set(true);
    spawn_local(async move {
        match upload_attachment(&code, &file, false).await {
            Ok(_) => {
                error.set(None);
                reload.run(());
                on_changed.run(());
            }
            Err(e) => error.set(Some(e)),
        }
        uploading.set(false);
    });
}

/// 非 wasm（SSR）下的空实现：服务端只渲染骨架，不会有人选文件。
#[cfg(not(target_arch = "wasm32"))]
fn spawn_upload(
    _ev: leptos::ev::Event,
    _code: String,
    _uploading: RwSignal<bool>,
    _error: RwSignal<Option<String>>,
    _reload: Callback<()>,
    _on_changed: Callback<()>,
) {
}

#[component]
pub fn AttachmentList(
    /// 条目编码；空串时不取数也不渲染。
    code: Signal<String>,
    /// 所属工作空间，用来查当前用户角色。
    workspace_id: Signal<String>,
    /// 增删后通知外层：条目的「更新时间」要跟着刷新。
    on_changed: Callback<()>,
) -> impl IntoView {
    let items = RwSignal::new(None::<Result<Vec<Attachment>, String>>);
    let role = RwSignal::new(String::new());
    let my_id = RwSignal::new(String::new());
    let error = RwSignal::new(None::<String>);
    let confirm_del = RwSignal::new(None::<String>);
    let uploading = RwSignal::new(false);

    let load = move || {
        // 组件可能已被卸载（双击行会跳转条目全屏页，整个工作空间页卸下），
        // 此后读 `code` 会 panic。
        if items.is_disposed() {
            return;
        }
        let c = code.get();
        let ws = workspace_id.get();
        if c.is_empty() || ws.is_empty() {
            return;
        }
        if cfg!(target_arch = "wasm32") {
            spawn_local(async move {
                let r = async {
                    let list = attachments(&c).await?;
                    let user = me().await?;
                    let r = my_role(&ws).await?;
                    Ok::<_, String>((list, user, r))
                }
                .await;
                if items.is_disposed() || code.get_untracked() != c {
                    return;
                }
                match r {
                    Ok((list, user, r)) => {
                        my_id.set(user.map(|u| u.id).unwrap_or_default());
                        role.set(r);
                        items.set(Some(Ok(list)));
                    }
                    Err(e) => items.set(Some(Err(e))),
                }
            });
        }
    };
    // 上传完成后要重新取数。让外层直接调 `load()` 会把不可名状的闭包类型塞进
    // `spawn_upload`，所以包一层 `Callback`（signal 都是 `Copy`，`load` 也是 `Copy`）。
    let reload = Callback::new(move |_| load());

    Effect::new(move |_| {
        code.get();
        workspace_id.get();
        if logged_out() {
            return;
        }
        load();
    });

    let do_delete = move |id: String| {
        spawn_local(async move {
            match delete_attachment(&id).await {
                Ok(_) => {
                    confirm_del.set(None);
                    error.set(None);
                    reload.run(());
                    on_changed.run(());
                }
                Err(e) => error.set(Some(e)),
            }
        });
    };

    let on_pick = move |ev: leptos::ev::Event| {
        spawn_upload(ev, code.get(), uploading, error, reload, on_changed);
    };

    view! {
        <div class="grp-h">
            {ic_upload()}
            {move || format!(
                "附件（{}）",
                items.get().and_then(|r| r.ok()).map(|v| v.len()).unwrap_or(0),
            )}
        </div>

        {move || error.get().map(|e| view! { <div class="hint">{"⚠ "}{e}</div> })}

        {move || {
            let can_moderate = role_at_least(&role.get(), "maintainer");
            let mine = my_id.get();
            match items.get() {
                None => view! { <div class="mut">"加载中…"</div> }.into_any(),
                Some(Err(e)) => view! { <div class="mut">{e}</div> }.into_any(),
                Some(Ok(list)) => {
                    if list.is_empty() {
                        view! { <div class="mut">"暂无附件"</div> }.into_any()
                    } else {
                        view! {
                            {list.into_iter().map(|a| {
                                let id = a.id.clone();
                                let id_for_confirm = id.clone();
                                let can_delete = a.created_by == mine || can_moderate;
                                let confirming = confirm_del.get().as_deref() == Some(id.as_str());
                                view! {
                                    <div class="att">
                                        <span class="ic">{ic_folder()}</span>
                                        <a href=a.url.clone() download=a.filename.clone()>{a.filename.clone()}</a>
                                        <span class="sz">{human_size(a.size)}</span>
                                        {can_delete.then(|| view! {
                                            <button
                                                class="btn sm danger"
                                                on:click={
                                                    let sid = id.clone();
                                                    move |_| confirm_del.set(Some(sid.clone()))
                                                }
                                            >"删除"</button>
                                        })}
                                    </div>
                                    {confirming.then(|| view! {
                                        <div class="c-confirm">
                                            <span class="mut">"删除后不可恢复，确定？"</span>
                                            <button class="btn danger sm" on:click={
                                                let sid = id_for_confirm.clone();
                                                move |_| do_delete(sid.clone())
                                            }>"确认删除"</button>
                                            <button class="btn sm" on:click=move |_| confirm_del.set(None)>"取消"</button>
                                        </div>
                                    })}
                                }
                            }).collect::<Vec<_>>()}
                        }.into_any()
                    }
                }
            }
        }}

        {move || {
            if !role_at_least(&role.get(), "worker") {
                return view! { <div></div> }.into_any();
            }
            view! {
                <label class="btn sm" style="margin-top:8px">
                    {move || if uploading.get() { "上传中…" } else { "上传附件" }}
                    <input
                        type="file"
                        style="display:none"
                        disabled=move || uploading.get()
                        on:change=on_pick
                    />
                </label>
            }.into_any()
        }}
    }
}
