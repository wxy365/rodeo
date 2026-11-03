use leptos::html::Div;
use leptos::prelude::*;

use crate::frontend::graphql_client::Member;

/// 富文本编辑器组件（Tiptap v3）。
///
/// `initial` / `on_change` 都是 **HTML**：
/// - Tiptap 原生吃 HTML，服务端存的也是 HTML（`service::richtext` 消毒后落库），
///   边界不用翻译；
/// - 旧的 Quill Delta 在服务端由 `richtext::delta_to_html` 转成 HTML 后才落库，
///   浏览器这一侧只见 HTML。
///
/// `readonly = true` 时不挂工具栏、不绑上传、不调 `on_change` / `on_uploaded`。
/// 匿名分享页的条目正文走这条路。
#[component]
pub fn RichEditor(
    /// 初始内容（HTML）。
    #[prop(into)]
    initial: String,
    /// 所属条目编码。粘贴图片时上传到这条目下；空串表示无归属时上传会失败。
    entry_code: Signal<String>,
    /// 内容变化时回调，参数是新的 HTML。
    on_change: Callback<String>,
    /// 粘贴图片上传成功后调用。上传会推进条目的 `updated_at`（见
    /// `AttachmentService::save`），宿主页面握着的乐观并发版本号随之作废，
    /// 必须借这个回调重新取一次条目 —— 否则紧接着的「保存」会被判成冲突，
    /// 冲突处理会把编辑器连同刚插入的图片一起回滚掉。
    on_uploaded: Callback<()>,
    /// 当前工作空间成员列表，作为 `@` 提及下拉的候选项。
    #[prop(default = Vec::new())]
    members: Vec<Member>,
    /// 只读模式：匿名分享页用来展示 Entry 正文。
    #[prop(default = false)]
    readonly: bool,
) -> impl IntoView {
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
    // 不要在这里写 class：Leptos 重渲染会把节点的 class 属性整体重写成模板里的
    // 值。编辑器的类名由 Tiptap 自己加（editorProps.attributes.class）。
    view! { <div node_ref=el class="tiptap-editor"></div> }
}

/// 在节点挂载后初始化编辑器（仅 wasm；SSR 下为空操作）。
///
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
        // 存量 Quill Delta（启动期的 repair 会转掉；转不掉时至少别把 JSON 原文
        // 显示给用户，退化成纯文本）。
        return crate::richtext::delta_to_text(t);
    }
    body.to_string()
}

/// 章节目录里的一项：标题层级（1-3）与纯文本。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineItem {
    pub level: u8,
    pub text: String,
}

/// 从正文 HTML 里抽取 h1 / h2 / h3，供全屏页左侧的章节目录使用。
///
/// 正文在库里是消毒后的 HTML，标题只可能是 `<h1>` - `<h3>`（编辑器只开了
/// 这三级）。这里做的是一个**够用的**扫描器：按出现顺序找开始标签，取到
/// 对应结束标签之间的内容、剥掉行内标签与实体。不引 HTML 解析库——为目录
/// 拉一整棵 DOM 树不值得，而且这个函数在每次编辑器 update 时都会跑。
///
/// 空标题返回空串（渲染侧兜底成省略号），不会从目录里被吞掉：标题在正文里
/// 占一行，目录里也该留一行。
pub fn outline(html: &str) -> Vec<OutlineItem> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = lower[cursor..].find("<h") {
        let start = cursor + rel;
        let after_level = start + 2; // 越过 "<h"，指向层级数字
        // 层级：<h1> - <h3>，且后面必须紧跟 `>` 或空白（排除 <hr>、<html>）。
        let Some(level) = lower[after_level..].chars().next().and_then(|c| c.to_digit(10)) else {
            cursor = after_level;
            continue;
        };
        if !(1..=3).contains(&level) {
            cursor = after_level;
            continue;
        }
        let next = lower[after_level + 1..].chars().next();
        let tag_ok = next.is_some_and(|c| c == '>' || c.is_ascii_whitespace());
        if !tag_ok {
            cursor = after_level + 1;
            continue;
        }
        let Some(gt_rel) = lower[start..].find('>') else {
            break;
        };
        let text_start = start + gt_rel + 1;
        let close = format!("</h{level}>");
        let Some(text_end_rel) = lower[text_start..].find(&close) else {
            break;
        };
        let text_end = text_start + text_end_rel;
        out.push(OutlineItem {
            level: level as u8,
            text: heading_text(&html[text_start..text_end]),
        });
        cursor = text_end + close.len();
    }
    out
}

/// 标题内联内容 → 纯文本：剥标签、解常见实体、折叠空白。
fn heading_text(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                // 跳到标签结束；未闭合的半截标签按标签处理（内容不泄漏进目录）。
                for n in chars.by_ref() {
                    if n == '>' {
                        break;
                    }
                }
            }
            '&' => {
                let mut entity = String::new();
                while let Some(&n) = chars.peek() {
                    if n == ';' || entity.len() >= 10 {
                        break;
                    }
                    entity.push(n);
                    chars.next();
                }
                if chars.peek() == Some(&';') {
                    chars.next();
                }
                out.push_str(&decode_entity(&entity));
            }
            _ => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 常见 HTML 实体。解不出来的保留原文，目录里显示成 `&xxx;` 总比吞掉好。
fn decode_entity(entity: &str) -> String {
    match entity {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" | "#39" => "'".into(),
        "nbsp" => " ".into(),
        _ => {
            let code = if let Some(hex) = entity.strip_prefix("#x").or_else(|| entity.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                entity.strip_prefix('#').and_then(|d| d.parse::<u32>().ok())
            };
            match code.and_then(char::from_u32) {
                Some(c) => c.to_string(),
                None => format!("&{entity};"),
            }
        }
    }
}

/// 正文归一化：空的归一成空串，其余原样透传。
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
    let Some(bridge) = Reflect::get(&global, &JsValue::from_str("__rodeo_editor__")).ok() else {
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

    let opts = js_sys::Object::new();
    let _ = Reflect::set(&opts, &JsValue::from_str("readOnly"), &JsValue::from(readonly));

    if !readonly {
        // 图片上传：把异步上传包成 Promise 交给 glue.js。
        // 闭包在「调用时」才读 entry_code，而不是挂载时快照 —— 详情面板切换条目后
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
                            // 上传推进了 entry.updated_at，宿主的版本号已经过期，
                            // 先让它重新取一次条目。
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

        // @提及候选：只要 id 与显示名。
        //
        // 传的是 **JSON 字符串**（wasm → JS 的边界只能过基本类型），glue.js 那边
        // 用 `normalizeMembers` 解析它 —— 见 public/tiptap/glue.js 的注释。
        let members_json: Vec<serde_json::Value> = members
            .iter()
            .map(|m| serde_json::json!({ "id": m.account_id, "name": m.name }))
            .collect();
        let members_js =
            JsValue::from_str(&serde_json::to_string(&members_json).unwrap_or_else(|_| "[]".into()));
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
    // update 绑定，避免在节点上挂永远不响应的回调。
    if readonly {
        let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_editor"), &editor);
        return;
    }

    // editor.on('update', ({ editor }) => on_change(editor.getHTML()))
    let editor_for_cb = editor.clone();
    let closure = Closure::wrap(Box::new(move |payload: JsValue| {
        let ed = Reflect::get(&payload, &JsValue::from_str("editor"))
            .unwrap_or_else(|_| editor_for_cb.clone());
        let html = Reflect::get(&ed, &JsValue::from_str("getHTML"))
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
            .and_then(|f| Reflect::apply(&f, &ed, Array::new().as_ref()).ok())
            .and_then(|v| v.as_string())
            .unwrap_or_default();
        on_change.run(html);
    }) as Box<dyn FnMut(JsValue)>);
    let closure_js = closure.into_js_value();

    if let Some(on) = Reflect::get(&editor, &JsValue::from_str("on"))
        .ok()
        .and_then(|f| f.dyn_into::<Function>().ok())
    {
        let a = Array::new();
        a.push(&JsValue::from_str("update"));
        a.push(&closure_js);
        let _ = Reflect::apply(&on, &editor, a.as_ref());
    }

    // 将 editor 与回调引用挂在节点上，避免被 GC 回收。
    let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_editor"), &editor);
    let _ = Reflect::set(&node_js, &JsValue::from_str("__rodeo_onchange"), &closure_js);
}
