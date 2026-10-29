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
