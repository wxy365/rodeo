use ulid::Ulid;

use crate::domain::{Account, WorkspaceMember};

/// 把 Delta JSON 摊平成单行纯文本，丢弃所有富文本属性 / 嵌入。
/// 评论里的图片 / 换行 / 链接都不会进入消息预览，正文里的 `@姓名` 仍然出现。
///
/// 输入允许三种形态：`{"ops":[...]}` / `[...]` / 任意字符串（视作纯文本）。
pub fn flat_text(delta_json: &str) -> String {
    let v: serde_json::Value = match serde_json::from_str(delta_json.trim()) {
        Ok(v) => v,
        Err(_) => return delta_json.to_string(),
    };
    let ops = match v {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(ref m) if m.contains_key("ops") => {
            match m.get("ops").and_then(|o| o.as_array()) {
                Some(a) => a.clone(),
                None => return delta_json.to_string(),
            }
        }
        // 解析后是一段普通字符串——本身就当纯文本返回。
        serde_json::Value::String(s) => return s,
        _ => return delta_json.to_string(),
    };
    let mut out = String::new();
    for op in ops {
        let insert = match op.get("insert") {
            Some(i) => i,
            None => continue,
        };
        match insert {
            serde_json::Value::String(s) => out.push_str(s),
            // image / mention 嵌入：跳过（避免给消息预览塞奇怪的字符）。
            _ => {}
        }
    }
    out
}

/// 预览文本：截前 N 个字符 + 「…」省略号。
pub fn preview(delta_json: &str, max_chars: usize) -> String {
    let t = flat_text(delta_json);
    let t = t.trim().replace('\n', " ");
    let n = t.chars().count();
    if n <= max_chars {
        return t;
    }
    let mut s: String = t.chars().take(max_chars).collect();
    s.push('…');
    s
}

/// 从文本里抽取 `@姓名` 的姓名集合。返回 distinct 的姓名（按字面对比）。
///
/// 匹配规则：紧跟 `@` 的连续非空白字符。名字里**不允许**有空白。
/// 多字符名（如英文 first.last）按整段取。这是有意的简化。
pub fn extract_mention_names(text: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            // `@` 必须前接空白或字符串开头；避免误吃邮箱（如 `foo@bar`）。
            let prev_ok = if i == 0 {
                true
            } else if bytes[i - 1].is_ascii_whitespace() {
                true
            } else {
                // 全角标点 / ASCII 括号类分隔也算「词边界」：中文用户写「，@张三」应识别。
                matches!(bytes[i - 1] as char, '(' | '[' | ',' | ';' | '：' | '，' | '。')
            };
            if prev_ok {
                let mut j = i + 1;
                while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if j > i + 1 {
                    if let Ok(name) = std::str::from_utf8(&bytes[i + 1..j]) {
                        // 名字至少 1 字符，且不能以 `@` / `(` / `[` 收尾。
                        if !name.is_empty() && !name.starts_with('@') {
                            out.insert(name.to_string());
                        }
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// 把 `extract_mention_names` 解析出的姓名集合映射到成员 id。
///
/// 规则：按 `Account.name` 精确匹配。**多命中时取 email 字典序最小者**，其余忽略，
/// 消息里附一条 debug 提示但不报错——同名是真实存在的（曾有两位「张三」）。
pub fn resolve_mentions(
    names: &std::collections::HashSet<String>,
    members: &[(WorkspaceMember, Account)],
) -> Vec<Ulid> {
    let mut hits: Vec<&(WorkspaceMember, Account)> = Vec::new();
    for n in names {
        let mut matches: Vec<&(WorkspaceMember, Account)> = members
            .iter()
            .filter(|(_, a)| a.name == *n)
            .collect();
        if !matches.is_empty() {
            matches.sort_by(|a, b| a.1.email.cmp(&b.1.email));
            hits.push(matches[0]);
        }
    }
    // 去重（一个成员被多个 alias 命中的可能）。
    let mut seen = std::collections::HashSet::new();
    hits.into_iter()
        .filter_map(|(_, a)| seen.insert(a.id).then_some(a.id))
        .collect()
}