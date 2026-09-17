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
/// 承载 Entry Code 的可检索副本；`f_code` 是未分词的 STRING 字段，只能精确匹配，
/// 供删除/取回用，检索需要走这里的分词字段。
const CODE_TEXT_FIELD: &str = "entry_code_text";
/// 评论正文的检索副本。单开一个字段而不是拼进 `content`，
/// 是为了不让标题/详情/评论混在一起影响相关度。
const COMMENTS_FIELD: &str = "comments";

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

pub struct SearchIndex {
    index: Index,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    f_code: Field,
    f_ws: Field,
    f_title: Field,
    f_content: Field,
    f_labels: Field,
    f_code_text: Field,
    f_comments: Field,
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
        let f_code_text = text(&mut b, CODE_TEXT_FIELD);
        let f_comments = text(&mut b, COMMENTS_FIELD);
        let schema = b.build();

        let index = match Index::open_in_dir(dir) {
            // 旧索引缺少 code 或评论检索字段：删掉重建。索引是纯派生物，
            // 启动时的 `backfill` 会从 RocksDB 重新灌满。
            Ok(existing)
                if existing.schema().get_field(CODE_TEXT_FIELD).is_err()
                    || existing.schema().get_field(COMMENTS_FIELD).is_err() =>
            {
                drop(existing);
                std::fs::remove_dir_all(dir).map_err(srch_err)?;
                // create_in_dir 要求父目录存在，重建刚被删掉的目录。
                std::fs::create_dir_all(dir).map_err(srch_err)?;
                Index::create_in_dir(Path::new(dir), schema.clone()).map_err(srch_err)?
            }
            Ok(existing) => existing,
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
            f_code_text,
            f_comments,
        })
    }

    /// 将一条文档写入给定 writer（不 commit），供单条索引与批量回填复用。
    fn add_entry_doc(
        &self,
        writer: &IndexWriter,
        entry: &Entry,
        labels: &[Labeling],
        comments: &str,
    ) -> Result<(), AppError> {
        let label_text = labels
            .iter()
            .map(|l| {
                let v = match &l.value {
                    crate::domain::LabelValue::EnumList(v) => v.join(" "),
                    other => match other.to_json() {
                        serde_json::Value::String(s) => s,
                        o => o.to_string(),
                    },
                };
                format!("{} {}", l.label_name, v)
            })
            .collect::<Vec<_>>()
            .join(" ");
        writer.delete_term(Term::from_field_text(self.f_code, &entry.code));
        writer
            .add_document(doc!(
                self.f_code => entry.code.clone(),
                self.f_ws => entry.workspace_id.to_string(),
                self.f_title => entry.title.clone(),
                self.f_content => strip_rich_text(&entry.detail),
                self.f_labels => label_text,
                self.f_code_text => entry.code.clone(),
                self.f_comments => comments.to_string(),
            ))
            .map_err(srch_err)?;
        Ok(())
    }

    pub fn index_entry(
        &self,
        entry: &Entry,
        labels: &[Labeling],
        comments: &str,
    ) -> Result<(), AppError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| AppError::Internal("索引写锁中毒".into()))?;
        self.add_entry_doc(&writer, entry, labels, comments)?;
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
        let parser = QueryParser::for_index(
            &self.index,
            vec![
                self.f_title,
                self.f_content,
                self.f_labels,
                self.f_code_text,
                self.f_comments,
            ],
        );
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
    /// 全部文档在同一个 writer 会话内写入，末尾只 commit / reload 一次。
    pub fn backfill(&self, store: &DocStore) -> Result<usize, AppError> {
        if self.num_docs() > 0 {
            return Ok(0);
        }
        let rows = store.scan_prefix(cf::ENTRIES, b"")?;
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| AppError::Internal("索引写锁中毒".into()))?;
        let mut count = 0;
        for (_, v) in rows {
            let entry: Entry = bincode::deserialize(&v)?;
            // 归档条目与已删除一样不进检索索引。
            if entry.is_deleted() || store.exists(cf::ENTRIES_ARCHIVED, entry.code.as_bytes())? {
                continue;
            }
            let labels = store
                .scan_prefix(cf::LABELINGS, entry.code.as_bytes())?
                .into_iter()
                .map(|(_, lv)| bincode::deserialize::<Labeling>(&lv))
                .collect::<Result<Vec<_>, _>>()?;
            let comments = comments_text(store, &entry.code)?;
            self.add_entry_doc(&writer, &entry, &labels, &comments)?;
            count += 1;
        }
        writer.commit().map_err(srch_err)?;
        drop(writer);
        self.reader.reload().map_err(srch_err)?;
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
        idx.index_entry(&e, &[], "评论区补充：验证码有过期时间").unwrap();

        let hits = idx.search(ws, "密码", 10).unwrap();
        assert_eq!(hits, vec![e.code.clone()], "中文子串必须命中");

        let hits = idx.search(ws, "验证码", 10).unwrap();
        assert_eq!(hits, vec![e.code.clone()], "详情正文必须可检索");

        let hits = idx.search(ws, "过期时间", 10).unwrap();
        assert_eq!(hits, vec![e.code.clone()], "评论正文必须可检索");
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn entry_code_is_searchable() {
        let dir = temp_dir("code");
        let idx = SearchIndex::open(&dir).unwrap();
        let ws = Ulid::new();
        let mut e = Entry::new(ws, "无关标题".to_string(), Ulid::new());
        e.code = "RD-kM3vB7dR".to_string();
        idx.index_entry(&e, &[], "").unwrap();

        // 整段 Code 与其中一段子串都应命中。
        assert_eq!(idx.search(ws, "RD-kM3vB7dR", 10).unwrap(), vec![e.code.clone()]);
        assert_eq!(idx.search(ws, "kM3v", 10).unwrap(), vec![e.code.clone()]);
        drop(idx);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_index_without_code_field_is_rebuilt() {
        let dir = temp_dir("legacy");
        std::fs::create_dir_all(&dir).unwrap();
        // 用缺字段的旧 schema 造一个索引，模拟升级前的残留。
        let mut b = Schema::builder();
        let f_code = b.add_text_field("entry_code", STORED | STRING);
        let f_ws = b.add_text_field("workspace_id", STRING);
        b.add_text_field("title", STORED | STRING);
        b.add_text_field("content", STORED | STRING);
        b.add_text_field("labels", STORED | STRING);
        let old = b.build();
        {
            let index = Index::create_in_dir(Path::new(&dir), old).unwrap();
            let mut w = index.writer_with_num_threads(1, 15_000_000).unwrap();
            w.add_document(doc!(f_code => "RD-old", f_ws => Ulid::new().to_string())).unwrap();
            w.commit().unwrap();
        }

        // 打开时检测到缺字段 → 重建为空索引，旧文档不再可见。
        let idx = SearchIndex::open(&dir).unwrap();
        assert_eq!(idx.num_docs(), 0, "旧索引应被重建");
        let ws = Ulid::new();
        let e = Entry::new(ws, "重建后".to_string(), Ulid::new());
        idx.index_entry(&e, &[], "").unwrap();
        assert_eq!(idx.search(ws, e.code.as_str(), 10).unwrap(), vec![e.code.clone()]);
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
        idx.index_entry(&ea, &[], "").unwrap();
        idx.index_entry(&eb, &[], "").unwrap();

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
        idx.index_entry(&e, &[l], "").unwrap();
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
