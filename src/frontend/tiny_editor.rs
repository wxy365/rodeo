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
    on_change: Callback<String>,
) -> impl IntoView {
    let el: NodeRef<Div> = NodeRef::new();
    let delta = normalize_delta(&initial);

    mount_when_ready(el, delta, on_change);

    view! { <div node_ref=el class="tiny-editor"></div> }
}

/// 在节点挂载后初始化编辑器（仅 wasm；SSR 下为空操作）。
/// 用 `NodeRef::on_load`（内部 `f.take()`）保证每个节点只挂载一次，
/// 避免响应式 effect 在重渲染时重复 new 编辑器导致工具栏堆叠。
fn mount_when_ready(el: NodeRef<Div>, delta: String, on_change: Callback<String>) {
    #[cfg(target_arch = "wasm32")]
    el.on_load(move |node| {
        mount_editor(node, delta, on_change);
    });

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (el, delta, on_change);
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
    on_change: Callback<String>,
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

    let args = Array::new();
    args.push(&node_js);
    args.push(&JsValue::from_str(&delta_json));

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
