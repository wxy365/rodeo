use leptos::html::Div;
use leptos::prelude::*;

use crate::frontend::graphql_client::Member;

/// 富文本编辑器组件（@opentiny/fluent-editor，基于 Quill 2.0）。
///
/// `initial` 为初始内容（HTML）。用户每次编辑经 `text-change` 事件，
/// 把当前的语义化 HTML 写回 `on_change`。
///
/// `readonly = true` 时只展示内容：工具栏不挂、粘贴图片拦截不绑、`on_change`
/// 不会被调用。匿名分享页的 Entry 全屏详情走这条路径。
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
    /// 当前工作空间成员列表，作为 `@` 提及下拉的候选项。
    /// 后端 `service/mention.rs::extract_mention_names` 已能从纯文本里抓
    /// `@姓名`，所以这份名单只用来填前端下拉；插入的 chip 里仍是裸名字，
    /// 服务端按名字精确匹配（见 resolve_mentions 的同邮箱冲突回退）。
    /// 默认空——空时 glue.js 不挂 mention 模块，编辑器走纯文本路径。
    /// 关键：fluent-editor 的 mention 模块只接受 `search(term)`，不接受
    /// `mentionList` 字段，所以 Rust 侧只能传原始名单，由 JS 侧自己闭包搜索。
    #[prop(default = Vec::new())]
    members: Vec<Member>,
    /// 只读模式：匿名分享页用来展示 Entry 正文，不让读者改、不让上传图片、
    /// 不调 `on_change` / `on_uploaded`。`readonly` 下 `members` 与
    /// `entry_code` 仍接受，但 JS 端会直接跳过工具栏与粘贴上传监听。
    #[prop(default = false)]
    readonly: bool,
) -> impl IntoView {
    // 诊断日志：body 跑一次就输出一条。在侧栏 Entry 详情粘贴图片的复现里
    // 看这一行的次数就知道 Leptos 是否在「上传完成 → 父级重渲染」时把
    // 评论输入框这个组件也带着重跑了。修复后应为：评论输入框的 body
    // 只跑一次（挂载），上传完成时不再跑。
    #[cfg(target_arch = "wasm32")]
    {
        let initial_preview: String = initial.chars().take(20).collect();
        leptos::web_sys::console::log_1(
            &format!(
                "[TinyEditor] body run, initial={:?}\u{2026}, readonly={}",
                initial_preview, readonly
            )
            .into(),
        );
    }
    let el: NodeRef<Div> = NodeRef::new();
    let html = normalize_html(&initial);

    mount_when_ready(
        el,
        html,
        entry_code,
        on_change,
        on_uploaded,
        members,
        readonly,
    );

    // 不要在这里写 class="tiny-editor"：Leptos 重渲染会把节点的 class 属性
    // 整体重写成模板里的值，把 Quill 挂上来的 `ql-container ql-snow` 一并
    // 抹掉（评论一次之后正文编辑器边框消失、暗色下背景变黑就是这么来的）。
    // `.tiny-editor` 改由 glue.js 的 create() 添加，Quill 的类没人再动。
    view! { <div node_ref=el></div> }
}

/// 在节点挂载后初始化编辑器（仅 wasm；SSR 下为空操作）。
/// 用 `NodeRef::on_load`（内部 `f.take()`）保证每个节点只挂载一次，
/// 避免响应式 effect 在重渲染时重复 new 编辑器导致工具栏堆叠。
fn mount_when_ready(
    el: NodeRef<Div>,
    html: String,
    entry_code: Signal<String>,
    on_change: Callback<String>,
    on_uploaded: Callback<()>,
    members: Vec<Member>,
    readonly: bool,
) {
    #[cfg(target_arch = "wasm32")]
    el.on_load(move |node| {
        mount_editor(
            node,
            html,
            entry_code,
            on_change,
            on_uploaded,
            members,
            readonly,
        );
    });

    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (
            el,
            html,
            entry_code,
            on_change,
            on_uploaded,
            members,
            readonly,
        );
    }
}

/// 正文归一化。正文在库里存 **HTML**（服务端的 richtext 消毒后落库）。
///
/// 这里只把空内容归一成空串，其余原样透传 —— 库里可能还有没迁移的
/// Quill Delta，JS 侧两种都认（见 glue.js 的 setStoredContent）。
fn normalize_html(content: &str) -> String {
    if content.trim().is_empty() {
        return String::new();
    }
    content.to_string()
}

#[cfg(target_arch = "wasm32")]
fn mount_editor<N: wasm_bindgen::JsCast>(
    node: N,
    html: String,
    entry_code: Signal<String>,
    on_change: Callback<String>,
    on_uploaded: Callback<()>,
    members: Vec<Member>,
    readonly: bool,
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
        // 诊断日志：与 body 跑的日志配合，确认挂载是否被跳过。
        leptos::web_sys::console::log_1(
            &"[tiny-editor] skip remount (node already has editor)".into(),
        );
        return;
    }
    // 诊断日志：若出现说明 DOM 节点被换了，编辑器内容随之丢失。
    leptos::web_sys::console::log_1(&"[tiny-editor] MOUNT new editor".into());

    // 把 readOnly / members 等开关一次性塞给 glue.js；只有非 readOnly 路径
    // 才会注册 on_change / upload 闭包，避免泄漏与无效绑定。
    let opts = js_sys::Object::new();
    let _ = Reflect::set(
        &opts,
        &JsValue::from_str("readOnly"),
        &JsValue::from(readonly),
    );

    if !readonly {
        // 第三个参数：把异步上传包成 Promise 交给 glue.js。
        // 闭包在「调用时」才读 entry_code，而不是挂载时快照——详情面板切换条目后
        // 编辑器节点可能被复用，此时上传必须落在当前条目上。
        let upload_fn = {
            let closure = Closure::wrap(Box::new(move |file: JsValue| -> JsValue {
                let code = entry_code.get_untracked();
                let fut = async move {
                    let file = file
                        .dyn_into::<web_sys::File>()
                        .map_err(|_| JsValue::from_str("粘贴的内容不是文件"))?;
                    // 编辑器里的图片是正文的一部分：不列进附件，随正文删除而回收。
                    match crate::frontend::graphql_client::upload_attachment(
                        &code, &file, true, None,
                    )
                    .await
                    {
                        Ok(a) => {
                            // 诊断日志：标记上传完成 → on_uploaded → 父级可能重渲染的时间点。
                            leptos::web_sys::console::log_1(
                                &"[tiny-editor] upload ok, calling on_uploaded".into(),
                            );
                            // 上传推进了 entry.updated_at，宿主的版本号已经过期，先让它重新取一次。
                            on_uploaded.run(());
                            leptos::web_sys::console::log_1(
                                &"[tiny-editor] on_uploaded returned".into(),
                            );
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

        // @ 提及：把成员名单 JSON 化交给 glue.js，由它在 modules.mention 里
        // 当 mentionList（id / name / avatar）。空列表时 JS 端按 enabled=false
        // 处理——不要在这里就裁掉，否则热点条目详情页的编辑器会因 Signal
        // 还未填好而拿不到空名单，dropdown 反而闪。
        let members_js = serde_json::to_string(&members).unwrap_or_else(|_| "[]".into());
        let members_js = JsValue::from_str(&members_js);
        let _ = Reflect::set(&opts, &JsValue::from_str("members"), &members_js);
    }

    let args = Array::new();
    args.push(&node_js);
    args.push(&JsValue::from_str(&html));
    args.push(&opts);

    let Some(editor) = Reflect::apply(&create, &bridge, args.as_ref()).ok() else {
        return;
    };

    // 只读模式下：on_change / on_uploaded 永远不被调用，跳过 closure 构造与
    // text-change 绑定，避免在节点上挂永远不响应的回调。
    if readonly {
        let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_editor"), &editor);
        return;
    }

    // text-change -> on_change(JSON.stringify(getContents()))
    let editor_for_cb = editor.clone();
    let closure = Closure::wrap(
        Box::new(move |_delta: JsValue, _old: JsValue, _source: JsValue| {
            // 正文在库里存 **HTML**：Quill 的语义化导出（getSemanticHTML）。
            // 不用 getContents —— 那是 Delta JSON，服务端已经不再收它了。
            let html = Reflect::get(&editor_for_cb, &JsValue::from_str("getSemanticHTML"))
                .ok()
                .and_then(|f| f.dyn_into::<Function>().ok())
                .and_then(|f| Reflect::apply(&f, &editor_for_cb, Array::new().as_ref()).ok())
                .and_then(|v| v.as_string())
                .unwrap_or_default();
            on_change.run(html);
        }) as Box<dyn FnMut(JsValue, JsValue, JsValue)>,
    );
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
    let _ = Reflect::set(
        &node_js,
        &JsValue::from_str("__rodeo_onchange"),
        &closure_js,
    );
}

/// 把正文渲染成 HTML。
///
/// 正文在库里存的就是**消毒过的 HTML**（服务端写入时消毒），所以绝大多数情况
/// 直接返回即可 —— **SSR 阶段就能渲染出评论正文**，不用等 hydrate。
///
/// 存量里可能还有没迁移的 Quill Delta（启动期的 repair 会转掉，转不掉的保留
/// 原文）：那种走 JS 侧的转换，而它是 wasm-only 的，SSR 阶段会是空串。
pub fn content_to_html(body: &str) -> String {
    let t = body.trim_start();
    if t.starts_with('{') || t.starts_with('[') {
        return delta_to_html(body);
    }
    body.to_string()
}

/// 把 Delta JSON 渲染成 HTML。非 wasm 目标或桥接未加载时返回空串
/// （与 `content_to_html` 一样，渲染只发生在浏览器里）。
fn delta_to_html(delta: &str) -> String {
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
