//! Tantivy-backed full-text search index with a CJK ngram tokenizer.

use std::path::Path;
use std::sync::Mutex;

use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::{NgramTokenizer, TextAnalyzer};
use tantivy::{doc, Index, IndexReader, IndexWriter, Term};
use ulid::Ulid;

use crate::domain::{Entry, Labeling};
use crate::error::AppError;
use crate::storage::{cf, DocStore};

pub const TEXT_CANDIDATE_LIMIT: usize = 1000;
const TOKENIZER: &str = "cjk";

fn srch_err<E: std::fmt::Display>(e: E) -> AppError {
    AppError::Storage(format!("检索索引错误: {e}"))
}

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

pub struct SearchIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    f_code: Field,
    f_ws: Field,
    f_title: Field,
    f_content: Field,
    f_labels: Field,
}

impl SearchIndex {
    pub fn open(dir: &str) -> Result<Self, AppError> {
        std::fs::create_dir_all(dir).map_err(|e| AppError::Storage(e.to_string()))?;
        let mut b = Schema::builder();
        let text = |b: &mut tantivy::schema::SchemaBuilder, name: &str| {
            let indexing = TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions);
            b.add_text_field(name, TextOptions::default().set_indexing_options(indexing))
        };
        let f_code = b.add_text_field("entry_code", STORED | STRING);
        let f_ws = b.add_text_field("workspace_id", STRING);
        let f_title = text(&mut b, "title");
        let f_content = text(&mut b, "content");
        let f_labels = text(&mut b, "labels");
        let schema = b.build();

        let index = match Index::open_in_dir(dir) {
            Ok(i) => i,
            Err(_) => Index::create_in_dir(Path::new(dir), schema.clone()).map_err(srch_err)?,
        };
        let analyzer = TextAnalyzer::builder(NgramTokenizer::new(1, 2, false).map_err(srch_err)?)
            .build();
        index.tokenizers().register(TOKENIZER, analyzer);

        let writer = index.writer_with_num_threads(1, 50_000_000).map_err(srch_err)?;
        let reader = index.reader().map_err(srch_err)?;

        Ok(Self {
            index,
            writer: Mutex::new(writer),
            reader,
            f_code,
            f_ws,
            f_title,
            f_content,
            f_labels,
        })
    }

    pub fn index_entry(&self, entry: &Entry, labels: &[Labeling]) -> Result<(), AppError> {
        let label_text = labels
            .iter()
            .map(|l| {
                let v = match l.value.to_json() {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                format!("{} {}", l.label_name, v)
            })
            .collect::<Vec<_>>()
            .join(" ");

        let mut writer = self
            .writer
            .lock()
            .map_err(|_| AppError::Internal("索引写锁中毒".into()))?;
        writer.delete_term(Term::from_field_text(self.f_code, &entry.code));
        writer
            .add_document(doc!(
                self.f_code => entry.code.clone(),
                self.f_ws => entry.workspace_id.to_string(),
                self.f_title => entry.title.clone(),
                self.f_content => strip_rich_text(&entry.detail),
                self.f_labels => label_text,
            ))
            .map_err(srch_err)?;
        writer.commit().map_err(srch_err)?;
        drop(writer);
        self.reader.reload().map_err(srch_err)?;
        Ok(())
    }

    pub fn remove_entry(&self, code: &str) -> Result<(), AppError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| AppError::Internal("索引写锁中毒".into()))?;
        writer.delete_term(Term::from_field_text(self.f_code, code));
        writer.commit().map_err(srch_err)?;
        drop(writer);
        self.reader.reload().map_err(srch_err)?;
        Ok(())
    }

    pub fn search(&self, ws: Ulid, keyword: &str, limit: usize) -> Result<Vec<String>, AppError> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Ok(Vec::new());
        }
        let parser = QueryParser::for_index(&self.index, vec![self.f_title, self.f_content, self.f_labels]);
        let parsed = parser.parse_query(keyword).map_err(srch_err)?;
        let ws_query = TermQuery::new(
            Term::from_field_text(self.f_ws, &ws.to_string()),
            IndexRecordOption::Basic,
        );
        let combined = BooleanQuery::new(vec![
            (Occur::Must, parsed),
            (Occur::Must, Box::new(ws_query)),
        ]);

        let searcher = self.reader.searcher();
        let top = searcher
            .search(&combined, &TopDocs::with_limit(limit).order_by_score())
            .map_err(srch_err)?;
        let mut out = Vec::new();
        for (_score, addr) in top {
            let doc = searcher
                .doc::<tantivy::TantivyDocument>(addr)
                .map_err(srch_err)?;
            if let Some(code) = doc.get_first(self.f_code).and_then(|v| v.as_str()) {
                out.push(code.to_string());
            }
        }
        Ok(out)
    }

    pub fn num_docs(&self) -> u64 {
        self.reader.searcher().num_docs()
    }

    /// 索引为空时从 RocksDB 回填；已有文档则跳过（幂等）。
    pub fn backfill(&self, store: &DocStore) -> Result<usize, AppError> {
        if self.num_docs() > 0 {
            return Ok(0);
        }
        let rows = store.scan_prefix(cf::ENTRIES, b"")?;
        let mut count = 0;
        for (_, v) in rows {
            let entry: Entry = bincode::deserialize(&v)?;
            if entry.is_deleted() {
                continue;
            }
            let labels = store
                .scan_prefix(cf::LABELINGS, entry.code.as_bytes())?
                .into_iter()
                .map(|(_, lv)| bincode::deserialize::<Labeling>(&lv))
                .collect::<Result<Vec<_>, _>>()?;
            self.index_entry(&entry, &labels)?;
            count += 1;
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{LabelValue, Labeling};
    use crate::storage::DocStore;
    use ulid::Ulid;

    fn temp_dir(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("rodeo-search-{name}-{}", Ulid::new()));
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn strip_rich_text_extracts_delta_ops() {
        let delta = r#"{"ops":[{"insert":"复现步骤：\n"},{"insert":"1. 打标签"}]}"#;
        assert_eq!(strip_rich_text(delta), "复现步骤：\n1. 打标签");
        assert_eq!(strip_rich_text("纯文本"), "纯文本");
        assert_eq!(strip_rich_text(""), "");
    }

    #[test]
    fn indexes_and_searches_chinese_substring() {
        let dir = temp_dir("cjk");
        let idx = SearchIndex::open(&dir).unwrap();
        let ws = Ulid::new();
        let mut e = Entry::new(ws, "找回密码失败".to_string(), Ulid::new());
        e.detail = r#"{"ops":[{"insert":"用户反馈邮箱收不到验证码"}]}"#.to_string();
        idx.index_entry(&e, &[]).unwrap();

        let hits = idx.search(ws, "密码", 10).unwrap();
        assert_eq!(hits, vec![e.code.clone()], "中文子串必须命中");

        let hits = idx.search(ws, "验证码", 10).unwrap();
        assert_eq!(hits, vec![e.code.clone()], "详情正文必须可检索");
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn search_is_scoped_to_workspace_and_drops_deleted() {
        let dir = temp_dir("scope");
        let idx = SearchIndex::open(&dir).unwrap();
        let ws_a = Ulid::new();
        let ws_b = Ulid::new();
        let ea = Entry::new(ws_a, "共享词".to_string(), Ulid::new());
        let eb = Entry::new(ws_b, "共享词".to_string(), Ulid::new());
        idx.index_entry(&ea, &[]).unwrap();
        idx.index_entry(&eb, &[]).unwrap();

        assert_eq!(idx.search(ws_a, "共享", 10).unwrap(), vec![ea.code.clone()]);

        idx.remove_entry(&ea.code).unwrap();
        assert!(idx.search(ws_a, "共享", 10).unwrap().is_empty());
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn label_values_are_searchable() {
        let dir = temp_dir("labels");
        let idx = SearchIndex::open(&dir).unwrap();
        let ws = Ulid::new();
        let e = Entry::new(ws, "t".to_string(), Ulid::new());
        let l = Labeling::new(e.code.clone(), "Owner".to_string(), LabelValue::Enum("陈晨".to_string()), Ulid::new());
        idx.index_entry(&e, &[l]).unwrap();
        assert_eq!(idx.search(ws, "陈晨", 10).unwrap(), vec![e.code.clone()]);
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn backfill_is_idempotent() {
        let dir = temp_dir("backfill");
        let store = DocStore::open(&dir).unwrap();
        let idx_dir = format!("{dir}/search");
        let idx = SearchIndex::open(&idx_dir).unwrap();
        let ws = Ulid::new();
        let mut e = Entry::new(ws, "回填目标".to_string(), Ulid::new());
        e.detail = String::new();
        store.put(cf::ENTRIES, e.code.as_bytes(), &e).unwrap();

        assert_eq!(idx.backfill(&store).unwrap(), 1);
        assert_eq!(idx.search(ws, "回填", 10).unwrap(), vec![e.code.clone()]);
        assert_eq!(idx.backfill(&store).unwrap(), 0, "已有文档时不再回填");
        drop(idx);
        drop(store);
        std::fs::remove_dir_all(&dir).ok();
    }
}
