//! 全文检索的共享辅助：纯文本剥离、评论拼接。
//!
//! 两个后端（Tantivy / Meilisearch）都会用到——Tantivy 直接拿字符串喂给 schema，
//! Meilisearch 走预分词后再喂，但「先抽纯文本、再拼评论」这段对两者一致。

use crate::error::AppError;
use crate::storage::{cf, DocStore};

/// 从 Delta JSON 提取纯文本；非 Delta（历史纯文本）原样返回。
pub fn strip_rich_text(detail: &str) -> String {
    let d = detail.trim();
    if d.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(d) {
        let ops = v
            .get("ops")
            .and_then(|o| o.as_array())
            .cloned()
            .or_else(|| v.as_array().cloned());
        if let Some(ops) = ops {
            let mut out = String::new();
            for op in ops {
                if let Some(s) = op.get("insert").and_then(|i| i.as_str()) {
                    out.push_str(s);
                }
            }
            return out;
        }
    }
    d.to_string()
}

/// 把一条条目的全部评论正文抽成纯文本，供检索索引拼接。
pub fn comments_text(store: &DocStore, entry_code: &str) -> Result<String, AppError> {
    let mut out = String::new();
    for (_, v) in store.scan_prefix(cf::COMMENTS, entry_code.as_bytes())? {
        let c: crate::domain::Comment = bincode::deserialize(&v)?;
        let t = strip_rich_text(&c.body);
        if !t.trim().is_empty() {
            out.push_str(&t);
            out.push('\n');
        }
    }
    Ok(out)
}

/// 共享错误包装：所有「检索后端」失败都收成 `AppError::Storage`，
/// 字符串前缀固定，便于日志聚合时过滤。
pub(crate) fn srch_err<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Storage(format!("检索索引错误: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_rich_text_extracts_delta_ops() {
        let delta = r#"{"ops":[{"insert":"复现步骤：\n"},{"insert":"1. 打标签"}]}"#;
        assert_eq!(strip_rich_text(delta), "复现步骤：\n1. 打标签");
        assert_eq!(strip_rich_text("纯文本"), "纯文本");
        assert_eq!(strip_rich_text(""), "");
    }
}
