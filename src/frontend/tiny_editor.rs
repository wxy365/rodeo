use leptos::html::Div;
use leptos::prelude::*;

/// 富文本编辑器组件（@opentiny/fluent-editor，基于 Quill 2.0）。
///
/// `initial` 为初始内容：既可以是 Quill Delta JSON，也可以是旧版纯文本
/// （纯文本会自动包装成 Delta）。用户每次编辑经 `text-change` 事件，
/// 把当前 Delta JSON 写回 `on_change`。
#[component]
pub fn TinyEditor(
    #[prop(into)] initial: String,
    /// 所属条目编码。粘贴图片时上传到这条目下；空串表示无归属时上传会失败。
    entry_code: Signal<String>,
    on_change: Callback<String>,
    /// 粘贴图片上传成功后调用。上传会推进条目的 `updated_at`（见
    /// `AttachmentService::save`），宿主页面握着的乐观并发版本号随之作废，
    /// 必须借这个回调重新取一次条目——否则紧接着的「保存」会被判成冲突，
    /// 冲突处理会把编辑器连同刚插入的图片一起回滚掉。
    /// 与页面上手动上传附件的路径（`AttachmentList` 的 `on_changed`）同一个补救。
    on_uploaded: Callback<()>,
) -> impl IntoView {
    let el: NodeRef<Div> = NodeRef::new();
    let delta = normalize_delta(&initial);

    mount_when_ready(el, delta, entry_code, on_change, on_uploaded);

    view! { <div node_ref=el class="tiny-editor"></div> }
}

/// 在节点挂载后初始化编辑器（仅 wasm；SSR 下为空操作）。
/// 用 `NodeRef::on_load`（内部 `f.take()`）保证每个节点只挂载一次，
/// 避免响应式 effect 在重渲染时重复 new 编辑器导致工具栏堆叠。
fn mount_when_ready(
    el: NodeRef<Div>,
    delta: String,
    entry_code: Signal<String>,
    on_change: Callback<String>,
    on_uploaded: Callback<()>,
) {
    #[cfg(target_arch = "wasm32")]
    el.on_load(move |node| {
        mount_editor(node, delta, entry_code, on_change, on_uploaded);
    });

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (el, delta, entry_code, on_change, on_uploaded);
    }
}

/// 将历史纯文本或 Delta JSON 归一化为 Delta JSON 字符串。
/// 空内容返回空串；`{"ops":[...]}` 或 `[...]` 原样返回；其余视为纯文本包装成 Delta。
fn normalize_delta(detail: &str) -> String {
    let d = detail.trim();
    if d.is_empty() {
        return String::new();
    }
    match serde_json::from_str::<serde_json::Value>(d) {
        Ok(serde_json::Value::Array(_)) => d.to_string(),
        Ok(serde_json::Value::Object(ref m)) if m.contains_key("ops") => d.to_string(),
        _ => {
            let text = if d.ends_with('\n') {
                d.to_string()
            } else {
                format!("{d}\n")
            };
            serde_json::json!({ "ops": [{ "insert": text }] }).to_string()
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn mount_editor<N: wasm_bindgen::JsCast>(
    node: N,
    delta_json: String,
    entry_code: Signal<String>,
    on_change: Callback<String>,
    on_uploaded: Callback<()>,
) {
    use js_sys::{Array, Function, Reflect};
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};

    let global = js_sys::global();
    let Some(bridge) = Reflect::get(&global, &JsValue::from_str("__rodeo_tiny_editor__")).ok()
    else {
        return;
    };
    let Some(create) = Reflect::get(&bridge, &JsValue::from_str("create"))
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok())
    else {
        return;
    };

    let node_js = node.unchecked_into::<JsValue>();

    // 防止重复挂载：组件因 prop 变化重跑时，节点上已存在编辑器实例。
    if Reflect::get(&node_js, &JsValue::from_str("__rodeo_editor"))
        .ok()
        .is_some_and(|v| !v.is_undefined())
    {
        return;
    }

    // 第三个参数：把异步上传包成 Promise 交给 glue.js。
    // 闭包在「调用时」才读 entry_code，而不是挂载时快照——详情面板切换条目后
    // 编辑器节点可能被复用，此时上传必须落在当前条目上。
    let opts = js_sys::Object::new();
    let upload_fn = {
        let closure = Closure::wrap(Box::new(move |file: JsValue| -> JsValue {
            let code = entry_code.get_untracked();
            let fut = async move {
                let file = file
                    .dyn_into::<web_sys::File>()
                    .map_err(|_| JsValue::from_str("粘贴的内容不是文件"))?;
                match crate::frontend::graphql_client::upload_attachment(&code, &file).await {
                    Ok(a) => {
                        // 上传推进了 entry.updated_at，宿主的版本号已经过期，先让它重新取一次。
                        on_uploaded.run(());
                        Ok(JsValue::from_str(&a.url))
                    }
                    Err(e) => Err(JsValue::from_str(&e)),
                }
            };
            wasm_bindgen_futures::future_to_promise(fut).into()
        }) as Box<dyn FnMut(JsValue) -> JsValue>);
        // into_js_value 会泄漏这个闭包，但也正因此它不会被回收；
        // 再把引用挂到节点上，与 __rodeo_editor / __rodeo_onchange 同一套保活方式。
        let js = closure.into_js_value();
        let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_upload"), &js);
        js
    };
    let _ = Reflect::set(&opts, &JsValue::from_str("upload"), &upload_fn);

    let args = Array::new();
    args.push(&node_js);
    args.push(&JsValue::from_str(&delta_json));
    args.push(&opts);

    let Some(editor) = Reflect::apply(&create, &bridge, args.as_ref()).ok() else {
        return;
    };

    // text-change -> on_change(JSON.stringify(getContents()))
    let editor_for_cb = editor.clone();
    let closure = Closure::wrap(Box::new(
        move |_delta: JsValue, _old: JsValue, _source: JsValue| {
            let contents = Reflect::get(&editor_for_cb, &JsValue::from_str("getContents"))
                .ok()
                .and_then(|f| f.dyn_into::<Function>().ok())
                .and_then(|f| Reflect::apply(&f, &editor_for_cb, Array::new().as_ref()).ok());
            let Some(contents) = contents else { return };
            let json = js_sys::JSON::stringify(&contents)
                .ok()
                .and_then(|j| j.as_string())
                .unwrap_or_default();
            on_change.run(json);
        },
    ) as Box<dyn FnMut(JsValue, JsValue, JsValue)>);
    let closure_js = closure.into_js_value();

    if let Some(on) = Reflect::get(&editor, &JsValue::from_str("on"))
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok())
    {
        let a = Array::new();
        a.push(&JsValue::from_str("text-change"));
        a.push(&closure_js);
        let _ = Reflect::apply(&on, &editor, a.as_ref());
    }

    // 将 editor 与回调引用挂在节点上，避免被 GC 回收。
    let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_editor"), &editor);
    let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_onchange"), &closure_js);
}

/// 把 Delta JSON 渲染成 HTML。非 wasm 目标或桥接未加载时返回空串
/// （与 `TinyEditor` 一样，渲染只发生在浏览器里）。
pub fn delta_to_html(delta: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        use js_sys::{Array, Function, Reflect};
        use wasm_bindgen::{JsCast, JsValue};

        if delta.trim().is_empty() {
            return String::new();
        }
        let global = js_sys::global();
        let Ok(bridge) = Reflect::get(&global, &JsValue::from_str("__rodeo_tiny_editor__")) else {
            return String::new();
        };
        let Some(to_html) = Reflect::get(&bridge, &JsValue::from_str("toHtml"))
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
        else {
            return String::new();
        };
        let args = Array::new();
        args.push(&JsValue::from_str(delta));
        Reflect::apply(&to_html, &bridge, args.as_ref())
            .ok()
            .and_then(|v| v.as_string())
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = delta;
        String::new()
    }
}
