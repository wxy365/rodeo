//! 富文本正文处理：HTML 消毒与纯文本抽取。
//!
//! 正文在数据库里存**消毒后的 HTML**（不再是 Quill Delta）：
//!
//! - 只读渲染能在 SSR 里直接输出，不用等 hydrate —— 现在评论正文在 SSR 里是
//!   空白的（`frontend::tiny_editor::delta_to_html` 是 wasm-only 的）；
//! - 检索、AI 摘要、@提及解析都退化成「剥标签」，不必在 Rust 里复刻一套 Delta 语义；
//! - 编辑器换成 Tiptap 之后边界协议不用再动 —— Tiptap 原生吃 HTML。
//!
//! 过渡期库里可能同时存在两种形态：Quill 的 Delta JSON 与 HTML。
//! `detect_format` 判定形态，`html_to_text` 与 `normalize_for_storage` 两种都吃。

use ammonia::Builder;
use html_escape::decode_html_entities;

/// 构造消毒器。
///
/// ammonia 的默认白名单已经覆盖了我们要的绝大多数标签（table/thead/tbody/td/th/
/// tr/pre/code/span/u/s/img/blockquote/h1–h6/ul/ol/li/p/br/hr/a/mark…），这里只补
/// 默认没有的，并把 `data-*` 放行 —— @提及 chip（`data-type` / `data-id`）、
/// 任务项勾选状态（`data-checked`）、代码块语言标记都靠它。
///
/// 默认已经处理好的：`script` / `style` 的**内容**会被一起丢掉（`clean_content_tags`
/// 默认含这两个），只去掉标签的话内容会变成正文露出来；`javascript:` / `data:` 这类
/// URL scheme 不在默认白名单里；`rel="noopener noreferrer"` 自动补上。
fn builder() -> Builder<'static> {
    let mut b = Builder::default();
    b.add_tags(["figure", "figcaption"]);
    b.add_generic_attribute_prefixes(["data-"]);
    // class 只在 code/pre 上放行（代码块的语言标记），别的 class 没有意义。
    b.add_tag_attributes("code", ["class"]);
    b.add_tag_attributes("pre", ["class"]);
    // a 的 target（rel 由 link_rel 自动补）；img 的 title（alt/width/height/src 默认就有）。
    b.add_tag_attributes("a", ["target"]);
    b.add_tag_attributes("img", ["title"]);
    // attribute_filter 的回调返回 Option<Cow<str>>：None = 丢掉这个属性，
    // Some = 保留（可以改写）。
    b.attribute_filter(|_element, name, value| {
        if name == "class" {
            // 只放行语言标记：<code class="language-rust">
            if value.split_whitespace().all(|c| c.starts_with("language-")) {
                return Some(value.into());
            }
            return None;
        }
        Some(value.into())
    });
    b
}

/// 把 HTML 消毒成安全子集。
///
/// 输入不是 HTML（历史纯文本）时原样返回 —— 调用方负责先判定形态
/// （见 `detect_format`）。
pub fn sanitize_html(html: &str) -> String {
    builder().clean(html).to_string()
}

/// 剥标签取纯文本。
///
/// 规则（这几条同时决定 @提及解析、检索、AI 摘要的输入，必须写死）：
///
/// | 输入 | 输出 |
/// |---|---|
/// | `<p>你好</p><p>@张三</p>` | `你好\n@张三` |
/// | `<span data-type="mention" data-id="01H">@张三</span>` | `@张三`（标签剥掉，文字留下） |
/// | `<br>` | `\n` |
/// | `&amp;` `&lt;` `&gt;` `&quot;` `&#39;` | 解码成对应字符 |
/// | `<p><br></p>` | `\n`（trim 后为空 → 判空成立） |
/// | `<img src="...">` | 空（图片不贡献文字，但 `has_image_embed` 另行判定） |
///
/// **块边界必须是 `\n`，不能直接拼**：`mention::extract_mention_names` 要求 `@`
/// 前面是空白或特定标点，`你好@张三` 里的 `@` 前面是汉字 → 提及认不出来 →
/// 通知不触发。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        rest = &rest[lt..];
        // 注释
        if rest.starts_with("<!--") {
            match rest.find("-->") {
                Some(i) => {
                    rest = &rest[i + 3..];
                    continue;
                }
                None => break,
            }
        }
        let Some(gt) = rest.find('>') else { break };
        let raw = &rest[1..gt];
        rest = &rest[gt + 1..];
        let closing = raw.starts_with('/');
        let name = raw
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match name.as_str() {
            "br" => out.push('\n'),
            "script" | "style" => {
                // 内容整段跳过（大小写不敏感；to_ascii_lowercase 不改字节长度，
                // 索引仍然对得上原串）。
                if !closing {
                    let close = format!("</{name}");
                    if let Some(i) = rest.to_ascii_lowercase().find(&close) {
                        rest = &rest[i..];
                    }
                }
            }
            "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
            | "blockquote" | "pre" | "ul" | "ol" | "table" | "section" | "article" => {
                if closing {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    out.push_str(rest);
    decode_html_entities(&out).into_owned()
}

/// 内容形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RichFormat {
    /// Quill Delta JSON（`{"ops":[...]}` 或 `[...]`）。
    Delta,
    /// HTML。
    Html,
    /// 历史纯文本。
    Plain,
}

/// 判定内容形态。
///
/// Delta 的判据：trim 后以 `{` 或 `[` 开头，且能解析出 `ops` 数组。老正文可能
/// 恰好是 `123`、`"foo"`、`{"a":1}` —— `JSON.parse` 成功但不是 Delta 形状，
/// 那属于纯文本。
pub fn detect_format(content: &str) -> RichFormat {
    let c = content.trim();
    if c.is_empty() {
        return RichFormat::Plain;
    }
    if c.starts_with('<') {
        return RichFormat::Html;
    }
    if c.starts_with('{') || c.starts_with('[') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(c) {
            let has_ops = v.get("ops").and_then(|o| o.as_array()).is_some() || v.as_array().is_some();
            if has_ops {
                return RichFormat::Delta;
            }
        }
    }
    RichFormat::Plain
}

/// 历史纯文本 → HTML：按行切分，每行包一个 `<p>`，空行跳过。文本要 escape。
pub fn plain_to_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        out.push_str("<p>");
        out.push_str(&escape_html(line));
        out.push_str("</p>");
    }
    out
}

/// HTML 转义（用于把纯文本包进 HTML）。
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ── Quill Delta → HTML ────────────────────────────────────────────────
//
// 存量正文是 Quill Delta（`{"ops":[...]}`）。转成 HTML 才能把存储切到 HTML，
// 也才能把编辑器换成 Tiptap（它原生吃 HTML）。
//
// 渲染语义照抄 Quill 的 `getSemanticHTML()`：
//   · 行内属性（bold/italic/underline/strike/code/link/mention/code-token）
//     转成行内标签；
//   · 块的类型由**块末尾那个换行 op** 的 attributes 决定
//     （header/list/blockquote/code-block）；
//   · 连续的同类型列表行合并进同一个 `<ul>` / `<ol>`。
//
// 覆盖的形态（**漏一个就是静默丢内容**）：
//   · `code-block: "<语言>"` —— 取值是语言字符串，不是 `true`；
//   · `code-token` —— 阶段 0 起的语法高亮 token，落库时带在行内文本上；
//   · `table-col` / `table-cell-line` —— better-table 的表格结构。

/// Delta 里一个 op 的块级属性（决定它结束的��是什么）。
#[derive(Debug, Default, Clone)]
struct BlockAttrs {
    header: Option<u8>,
    list: Option<String>,
    blockquote: bool,
    code_block: Option<String>,
}

/// 一个 op 的行内属性。
#[derive(Debug, Default, Clone)]
struct InlineAttrs {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    code: bool,
    link: Option<String>,
    mention: Option<(String, String)>,
    /// 语法高亮 token（阶段 0 引入）：`{"code-token":"keyword"}`
    code_token: Option<String>,
}

/// Delta 的一个 op。
enum DeltaOp {
    Text {
        text: String,
        inline: InlineAttrs,
        block: BlockAttrs,
    },
    Embed {
        kind: String,
        value: serde_json::Value,
    },
}

/// 把 Delta JSON 渲染成 HTML。解析不出来就当纯文本（escape 后按行包 `<p>`）。
pub fn delta_to_html(delta: &str) -> String {
    let Some(ops) = parse_delta_ops(delta) else {
        return plain_to_html(delta);
    };
    let mut out = String::new();
    // 当前块的行内 HTML
    let mut inline_buf = String::new();
    // 当前块的块级属性
    let mut block = BlockAttrs::default();
    // 正在打开的列表标签（"ul" / "ol"），None 表示不在列表里
    let mut open_list: Option<&'static str> = None;
    // 任务列表需要单独的 <ul data-type="taskList">，和普通 <ul> 不能混
    let mut open_list_is_task = false;

    for op in &ops {
        match op {
            DeltaOp::Text {
                text,
                inline,
                block: op_block,
            } => {
                let mut parts = text.split('\n');
                let first = parts.next().unwrap_or("");
                inline_buf.push_str(&render_inline(first, inline));
                for part in parts {
                    // 换行 → 收束当前块；块的类型由带这个换行的 op 决定
                    block = op_block.clone();
                    out.push_str(&finish_block(
                        &mut inline_buf,
                        &mut block,
                        &mut open_list,
                        &mut open_list_is_task,
                    ));
                    inline_buf.push_str(&render_inline(part, inline));
                }
            }
            DeltaOp::Embed { kind, value } => {
                inline_buf.push_str(&render_embed(kind, value));
            }
        }
    }
    // 末尾没有换行 op 的残余内容。**空的不收束** —— 收束会多出一个空的 <p></p>。
    if !inline_buf.is_empty() {
        out.push_str(&finish_block(
            &mut inline_buf,
            &mut block,
            &mut open_list,
            &mut open_list_is_task,
        ));
    }
    // 列表可能还开着（最后一个 op 是列表项）
    out.push_str(&close_open_list(&mut open_list, &mut open_list_is_task));
    out
}

/// 收束一个块，产出 HTML。列表行在这里合并进同一个 `<ul>`/`<ol>`。
fn finish_block(
    inline_buf: &mut String,
    block: &mut BlockAttrs,
    open_list: &mut Option<&'static str>,
    open_list_is_task: &mut bool,
) -> String {
    let mut out = String::new();
    let (want_list, is_task) = match block.list.as_deref() {
        Some("ordered") => (Some("ol"), false),
        Some("bullet") => (Some("ul"), false),
        // 勾选列表：<ul data-type="taskList"><li data-checked="true">
        Some("checked") | Some("unchecked") => (Some("ul"), true),
        _ => (None, false),
    };

    if let Some(tag) = want_list {
        if open_list.is_none() || *open_list != Some(tag) || *open_list_is_task != is_task {
            out.push_str(&close_open_list(open_list, open_list_is_task));
            if is_task {
                out.push_str("<ul data-type=\"taskList\">");
            } else {
                out.push_str(&format!("<{tag}>"));
            }
            *open_list = Some(tag);
            *open_list_is_task = is_task;
        }
        let checked = match block.list.as_deref() {
            Some("checked") => "true",
            Some("unchecked") => "false",
            _ => "",
        };
        if checked.is_empty() {
            out.push_str(&format!("<li>{}</li>", inline_buf));
        } else {
            out.push_str(&format!("<li data-checked=\"{checked}\">{}</li>", inline_buf));
        }
    } else {
        out.push_str(&close_open_list(open_list, open_list_is_task));
        let content = std::mem::take(inline_buf);
        if let Some(h) = block.header {
            out.push_str(&format!("<h{h}>{content}</h{h}>"));
        } else if block.blockquote {
            out.push_str(&format!("<blockquote>{content}</blockquote>"));
        } else if let Some(lang) = &block.code_block {
            let cls = if lang.is_empty() || lang == "plain" {
                String::new()
            } else {
                format!(" class=\"language-{lang}\"")
            };
            out.push_str(&format!("<pre><code{cls}>{content}</code></pre>"));
        } else if content.trim().is_empty() {
            // 空行也要留一个 <p>，否则 Quill 的空段落占位语义会丢
            out.push_str("<p></p>");
        } else {
            out.push_str(&format!("<p>{content}</p>"));
        }
    }
    *block = BlockAttrs::default();
    inline_buf.clear();
    out
}

/// 关掉还开着的列表标签（如果开着）。返回闭合标签 HTML。
fn close_open_list(
    open_list: &mut Option<&'static str>,
    open_list_is_task: &mut bool,
) -> String {
    match open_list.take() {
        Some(tag) => {
            *open_list_is_task = false;
            format!("</{tag}>")
        }
        None => String::new(),
    }
}

/// 解析 op 的行内属性。
fn parse_inline_attrs(v: Option<&serde_json::Value>) -> InlineAttrs {
    let mut a = InlineAttrs::default();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return a;
    };
    a.bold = truthy(obj.get("bold"));
    a.italic = truthy(obj.get("italic"));
    a.underline = truthy(obj.get("underline"));
    a.strike = truthy(obj.get("strike"));
    a.code = truthy(obj.get("code"));
    if let Some(href) = obj.get("link").and_then(|v| v.as_str()) {
        a.link = Some(href.to_string());
    }
    if let Some(tok) = obj.get("code-token").and_then(|v| v.as_str()) {
        a.code_token = Some(tok.to_string());
    }
    // mention：fluent-editor 的 mention 是 embed（{"insert":{"mention":{...}}}），
    // 但历史数据里也可能以行内属性的形式出现，两种都认。
    if let Some(m) = obj.get("mention").and_then(|v| v.as_object()) {
        let id = m.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let text = m.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        a.mention = Some((id, text));
    }
    a
}

fn truthy(v: Option<&serde_json::Value>) -> bool {
    match v {
        Some(serde_json::Value::Bool(true)) => true,
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(serde_json::Value::String(s)) => !s.is_empty() && s != "false" && s != "0",
        _ => false,
    }
}

/// 行内文本 → HTML。属性的包裹顺序固定，避免同一份内容每次渲染结果不一样。
fn render_inline(text: &str, attrs: &InlineAttrs) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut s = escape_html(text);
    if let Some(tok) = &attrs.code_token {
        s = format!("<span class=\"hljs-{}\">{}</span>", escape_html(tok), s);
    }
    if let Some((id, name)) = &attrs.mention {
        s = format!(
            "<span data-type=\"mention\" data-id=\"{}\">@{}</span>",
            escape_html(id),
            escape_html(name)
        );
    }
    if attrs.code {
        s = format!("<code>{s}</code>");
    }
    if attrs.bold {
        s = format!("<strong>{s}</strong>");
    }
    if attrs.italic {
        s = format!("<em>{s}</em>");
    }
    if attrs.underline {
        s = format!("<u>{s}</u>");
    }
    if attrs.strike {
        s = format!("<s>{s}</s>");
    }
    if let Some(href) = &attrs.link {
        s = format!(
            "<a href=\"{}\" rel=\"noopener noreferrer\" target=\"_blank\">{}</a>",
            escape_html(href),
            s
        );
    }
    s
}

/// embed（图片 / 提及）→ HTML。
fn render_embed(kind: &str, value: &serde_json::Value) -> String {
    match kind {
        "image" => match value.as_str() {
            Some(url) => format!("<img src=\"{}\" alt=\"\">", escape_html(url)),
            None => String::new(),
        },
        "mention" => {
            let id = value.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let text = value.get("text").and_then(|v| v.as_str()).unwrap_or("");
            format!(
                "<span data-type=\"mention\" data-id=\"{}\">@{}</span>",
                escape_html(id),
                escape_html(text)
            )
        }
        _ => String::new(),
    }
}

/// 解析 Delta 的 ops。
fn parse_delta_ops(delta: &str) -> Option<Vec<DeltaOp>> {
    let v: serde_json::Value = serde_json::from_str(delta).ok()?;
    let arr = v
        .get("ops")
        .and_then(|o| o.as_array())
        .cloned()
        .or_else(|| v.as_array().cloned())?;
    let mut out = Vec::with_capacity(arr.len());
    for op in arr {
        let insert = op.get("insert")?;
        let attrs = op.get("attributes");
        match insert {
            serde_json::Value::String(text) => {
                out.push(DeltaOp::Text {
                    text: text.clone(),
                    inline: parse_inline_attrs(attrs),
                    block: parse_block_attrs(attrs),
                });
            }
            serde_json::Value::Object(map) => {
                let (kind, value) = map.iter().next()?;
                out.push(DeltaOp::Embed {
                    kind: kind.clone(),
                    value: value.clone(),
                });
            }
            _ => {}
        }
    }
    Some(out)
}

/// 解析 op 的块级属性。
fn parse_block_attrs(v: Option<&serde_json::Value>) -> BlockAttrs {
    let mut b = BlockAttrs::default();
    let Some(obj) = v.and_then(|v| v.as_object()) else {
        return b;
    };
    if let Some(h) = obj.get("header").and_then(|v| v.as_u64()) {
        b.header = Some(h.clamp(1, 6) as u8);
    }
    b.blockquote = truthy(obj.get("blockquote"));
    if let Some(cb) = obj.get("code-block") {
        b.code_block = Some(match cb {
            serde_json::Value::String(s) => s.clone(),
            _ => "plain".to_string(),
        });
    }
    if let Some(l) = obj.get("list") {
        // list 取值可能是字符串，也可能是 {"value": "..."}（fluent-editor 改过形状）
        b.list = match l {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Object(o) => o
                .get("value")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            _ => None,
        };
    }
    b
}
