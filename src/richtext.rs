//! 前后端共用的富文本辅助。
//!
//! **`service` 模块是 `#[cfg(feature = "ssr")]` 门控的** —— 前端（hydrate/wasm）
//! 引用不到它。所以两边都要用的东西（markdown 表格的识别与渲染）放这里。

//
// 识别规则（JS 侧 public/tiny-editor/glue.js 与前端 agent_panel.rs 曾各有一份
// 同样规则的实现；现在统一到 Rust 这份，JS 侧靠服务端转换、不再自己认表）：
//
//   表格   := 表头行 分隔行 (数据行)*
//   表头行 := 含 '|' 的行，切出 N 个单元格，N >= 2
//   分隔行 := 每格匹配 /^:?-+:?$/，且格数与表头行相同
//   数据行 := 紧跟分隔行之后的连续「含 '|' 的行」
//
// 切格：按 '|' 切（'\|' 是字面竖线），首尾 trim；行内代码 span 里的 '|' 不算分隔符。
// 分隔行的 ':' 对齐标记不实现；各行格数不一致时按最大列数补空。

/// 一张 markdown 表格。
#[derive(Debug, Clone)]
pub struct MdTable {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
    /// 表头 + 分隔行 + 数据行一共占掉的行数（扫描时用来跳到下一段）。
    pub end: usize,
}

/// 按 `|` 切一行成单元格。
pub fn split_md_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let mut cells: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_code = false;
    let mut chars = t.chars();
    while let Some(c) = chars.next() {
        if c == '`' {
            in_code = !in_code;
            cur.push(c);
            continue;
        }
        if c == '\\' {
            // 转义竖线：把 `\|` 还原成字面 `|`。
            if matches!(chars.clone().next(), Some('|')) {
                cur.push('|');
                chars.next();
                continue;
            }
            cur.push(c);
            continue;
        }
        if c == '|' && !in_code {
            cells.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(c);
    }
    cells.push(cur);
    // 行首/行尾的竖线是 markdown 装饰，对应的空 cell 去掉。
    if t.starts_with('|') && !cells.is_empty() && cells[0].is_empty() {
        cells.remove(0);
    }
    if t.ends_with('|') && !t.ends_with("\\|") && !cells.is_empty() {
        let last_empty = cells.last().map(|c| c.is_empty()).unwrap_or(false);
        if last_empty {
            cells.pop();
        }
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

/// 分隔行的单元格：匹配 `/^:?-+:?$/`。
fn is_md_separator_cell(c: &str) -> bool {
    let core = c.strip_prefix(':').unwrap_or(c);
    let core = core.strip_suffix(':').unwrap_or(core);
    !core.is_empty() && core.chars().all(|ch| ch == '-')
}

/// 从第 `i` 行开始试解析一张 markdown 表格。
pub fn parse_md_table_at(lines: &[&str], i: usize) -> Option<MdTable> {
    if i + 1 >= lines.len() {
        return None;
    }
    let header_line = lines[i];
    if !header_line.contains('|') {
        return None;
    }
    let mut header = split_md_row(header_line);
    if header.len() < 2 {
        return None;
    }
    let sep = split_md_row(lines[i + 1]);
    if sep.len() != header.len() {
        return None;
    }
    if !sep.iter().all(|c| is_md_separator_cell(c)) {
        return None;
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut j = i + 2;
    while j < lines.len() && lines[j].contains('|') {
        rows.push(split_md_row(lines[j]));
        j += 1;
    }
    let cols = rows.iter().fold(header.len(), |m, r| m.max(r.len()));
    while header.len() < cols {
        header.push(String::new());
    }
    for r in rows.iter_mut() {
        while r.len() < cols {
            r.push(String::new());
        }
    }
    let end = 2 + rows.len();
    Some(MdTable {
        header,
        rows,
        end,
    })
}

/// 表格 → `<table>`。表头行进 `<thead>`，数据行进 `<tbody>`。
///
/// 表头行用行内 `<strong>` 加粗，不用「表头单元格」格式 —— 那个在只读导出时
/// 会变成 `<h1>`，套在 `<td>` 里很难看。
pub fn md_table_to_html(t: &MdTable) -> String {
    let mut out = String::from("<table><thead><tr>");
    for c in &t.header {
        out.push_str(&format!("<th><strong>{}</strong></th>", escape_html(c)));
    }
    out.push_str("</tr></thead><tbody>");
    for row in &t.rows {
        out.push_str("<tr>");
        for c in row {
            out.push_str(&format!("<td>{}</td>", escape_html(c)));
        }
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
    out
}

/// 把一段文本切成若干块。返回 `(块内容, 是不是表格)`。
///
/// 表格块是整块识别的：一张表必须从头到尾都在同一段里（表头行 + 分隔行 +
/// 数据行），夹在中间的散文会打断它。
pub fn split_md_tables(text: &str) -> Vec<(String, bool)> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;
    while i < lines.len() {
        if let Some(t) = parse_md_table_at(&lines, i) {
            // 表头 + 分隔 + 数据行占掉的行数
            let consumed = 2 + t.rows.len();
            if !plain.is_empty() {
                out.push((std::mem::take(&mut plain), false));
            }
            out.push((md_table_to_html(&t), true));
            i += consumed;
            continue;
        }
        plain.push_str(lines[i]);
        plain.push('\n');
        i += 1;
    }
    if !plain.is_empty() {
        out.push((plain, false));
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

/// Delta → 纯文本（剥掉所有属性，只留文字）。
///
/// 前端也要用：`service` 模块是 ssr 门控的，frontend 引用不到
/// `service::richtext::delta_to_text`，而「库里万一还留着没迁移的 Delta」
/// 这个兜底路径得有人兜。
pub fn delta_to_text(delta: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(delta.trim()) else {
        return delta.to_string();
    };
    let Some(ops) = v
        .get("ops")
        .and_then(|o| o.as_array())
        .cloned()
        .or_else(|| v.as_array().cloned())
    else {
        return delta.to_string();
    };
    let mut out = String::new();
    for op in ops {
        match op.get("insert") {
            Some(serde_json::Value::String(s)) => out.push_str(&s),
            // mention 嵌入：把 `@姓名` 补回去，下游的提及解析才认得出
            Some(serde_json::Value::Object(m)) if m.get("mention").is_some() => {
                let mention = m.get("mention").cloned().unwrap_or_default();
                let text = mention.get("text").and_then(|t| t.as_str()).unwrap_or("");
                out.push('@');
                out.push_str(text);
            }
            _ => {}
        }
    }
    out
}
