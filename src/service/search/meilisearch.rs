//! Meilisearch 后端。
//!
//! 与 `TantivyBackend` 共同实现 `SearchBackend` trait；选择 meili 的部署
//! 适合多实例共享同一索引。
//!
//! 实现要点：
//! - 内部持有一个独立的**多线程** `tokio::runtime::Runtime`（worker=2）。
//!   rodeo 主进程是 `current_thread` runtime——meili 的 HTTP 调用走自己的
//!   runtime 上的 worker，与 axum 请求线程完全隔离，不阻塞。
//! - 预分词：rodeo 侧用 `NgramTokenizer::new(1, 2)` 跑一遍，写入前空格 join；
//!   `entry_code_text` 字段**不**预分词（结构化 ID 让 Meilisearch 内置分词更合适）。
//! - 启动期探活：`health()` + `get_index()` / `create_index()` + 设置
//!   `filterableAttributes` / `searchableAttributes`——任一步失败在启动期 panic。

use meilisearch_sdk::client::Client;
use serde::{Deserialize, Serialize};
use tantivy::tokenizer::{NgramTokenizer, Tokenizer, TokenStream};
use ulid::Ulid;

use crate::config::MeilisearchSearchConfig;
use crate::domain::{Entry, Labeling};
use crate::error::AppError;
use crate::storage::DocStore;

use super::helpers::{comments_text, strip_rich_text};
use super::SearchBackend;

const PRIMARY_KEY: &str = "entry_code";
const FILTERABLE: &[&str] = &["workspace_id"];
const SEARCHABLE: &[&str] = &[
    "title",
    "content",
    "labels",
    "code_text",
    "comments",
];

/// Meilisearch 索引里的文档形态。字段名必须与 `SEARCHABLE` / `FILTERABLE` 字面量一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct MeiliDoc {
    entry_code: String,
    workspace_id: String,
    title: String,
    content: String,
    labels: String,
    code_text: String,
    comments: String,
}

pub struct MeilisearchBackend {
    rt: tokio::runtime::Runtime,
    client: Client,
    index_name: String,
    pre_segment: bool,
}

impl MeilisearchBackend {
    pub fn new(cfg: &MeilisearchSearchConfig) -> Result<Self, AppError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("rodeo-meili")
            .build()
            .map_err(|e| AppError::Storage(format!("创建 meili tokio runtime 失败: {e}")))?;
        let client = Client::new(cfg.url.trim_end_matches('/'), Some(&cfg.api_key)).map_err(|e| {
            AppError::Storage(format!(
                "初始化 meilisearch client 失败 (url={}): {e}",
                cfg.url
            ))
        })?;
        Ok(Self {
            rt,
            client,
            index_name: cfg.index_name.clone(),
            pre_segment: cfg.pre_segment,
        })
    }

    /// 启动期探活：health + index 存在则设置 settings，否则创建并设置。
    /// 在 `Services::new` 里同步调用——`health_check` 是同步入口，
    /// 内部 `block_on` 在 meili 自管的 runtime 上跑。
    pub fn health_check(&self) -> Result<(), AppError> {
        self.rt.block_on(async {
            self.client
                .health()
                .await
                .map_err(|e| AppError::Storage(format!(
                    "meilisearch 不可达 (url-host={}): {e}",
                    self.index_name
                )))?;
            match self.client.get_index(&self.index_name).await {
                Ok(idx) => {
                    apply_settings(&idx).await?;
                }
                Err(_) => {
                    let task = self
                        .client
                        .create_index(&self.index_name, Some(PRIMARY_KEY))
                        .await
                        .map_err(|e| AppError::Storage(format!(
                            "创建 meili 索引失败 (index={}): {e}",
                            self.index_name
                        )))?;
                    task.wait_for_completion(&self.client, None, None)
                        .await
                        .map_err(|e| AppError::Storage(format!(
                            "等待 meili 索引创建失败: {e}"
                        )))?;
                    let idx = self.client.index(&self.index_name);
                    apply_settings(&idx).await?;
                }
            }
            Ok::<_, AppError>(())
        })
    }

    fn build_doc(
        &self,
        entry: &Entry,
        labels: &[Labeling],
        comments: &str,
    ) -> MeiliDoc {
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
        let detail = strip_rich_text(&entry.detail);
        let seg = |s: &str| -> String {
            if !self.pre_segment {
                return s.to_string();
            }
            // ngram tokenizer 不会失败——构造用 unwrap_e 是 new() 的固定签名
            let mut tokenizer = NgramTokenizer::new(1, 2, false)
                .expect("NgramTokenizer::new(1, 2, false) 是固定签名，不应失败");
            let mut stream: Box<dyn TokenStream> = Box::new(tokenizer.token_stream(s));
            let mut out = String::new();
            while let Some(token) = stream.next() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&token.text);
            }
            out
        };
        MeiliDoc {
            entry_code: entry.code.clone(),
            workspace_id: entry.workspace_id.to_string(),
            title: seg(&entry.title),
            content: seg(&detail),
            labels: seg(&label_text),
            code_text: entry.code.clone(), // 不预分词
            comments: seg(comments),
        }
    }
}

async fn apply_settings(
    idx: &meilisearch_sdk::indexes::Index,
) -> Result<(), AppError> {
    idx.set_filterable_attributes(FILTERABLE)
        .await
        .map_err(|e| AppError::Storage(format!("设置 meili filterable 失败: {e}")))?;
    idx.set_searchable_attributes(SEARCHABLE)
        .await
        .map_err(|e| AppError::Storage(format!("设置 meili searchable 失败: {e}")))?;
    Ok(())
}

impl SearchBackend for MeilisearchBackend {
    fn index_entry(
        &self,
        entry: &Entry,
        labels: &[Labeling],
        comments: &str,
    ) -> Result<(), AppError> {
        let doc = self.build_doc(entry, labels, comments);
        let idx = self.client.index(&self.index_name);
        self.rt.block_on(async {
            idx.add_or_replace(&[doc], Some(PRIMARY_KEY))
                .await
                .map_err(|e| AppError::Storage(format!("meili index_entry 失败: {e}")))?;
            Ok::<_, AppError>(())
        })
    }

    fn remove_entry(&self, code: &str) -> Result<(), AppError> {
        let idx = self.client.index(&self.index_name);
        self.rt.block_on(async {
            idx.delete_document(code)
                .await
                .map_err(|e| AppError::Storage(format!("meili remove_entry 失败: {e}")))?;
            Ok::<_, AppError>(())
        })
    }

    fn search(
        &self,
        ws: Ulid,
        keyword: &str,
        limit: usize,
    ) -> Result<Vec<String>, AppError> {
        let keyword = keyword.trim();
        if keyword.is_empty() {
            return Ok(Vec::new());
        }
        let q = if self.pre_segment {
            let mut tokenizer = NgramTokenizer::new(1, 2, false)
                .map_err(|e| AppError::Storage(format!("ngram tokenizer 构造失败: {e}")))?;
            let mut stream: Box<dyn TokenStream> = Box::new(tokenizer.token_stream(keyword));
            let mut out = String::new();
            while let Some(token) = stream.next() {
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&token.text);
            }
            out
        } else {
            keyword.to_string()
        };
        let filter = format!("workspace_id = \"{}\"", ws);
        let idx = self.client.index(&self.index_name);
        self.rt.block_on(async {
            let mut search_query = meilisearch_sdk::search::SearchQuery::new(&idx);
            search_query
                .with_query(&q)
                .with_limit(limit)
                .with_filter(&filter);
            let results: meilisearch_sdk::search::SearchResults<MeiliDoc> = search_query
                .execute::<MeiliDoc>()
                .await
                .map_err(|e| AppError::Storage(format!("meili search 失败: {e}")))?;
            Ok(results
                .hits
                .into_iter()
                .map(|h| h.result.entry_code)
                .collect::<Vec<_>>())
        })
    }

    fn backfill(&self, store: &DocStore) -> Result<usize, AppError> {
        let rows = store.scan_prefix(crate::storage::cf::ENTRIES, b"")?;
        let mut docs: Vec<MeiliDoc> = Vec::with_capacity(rows.len());
        for (_, v) in rows {
            let entry: Entry = bincode::deserialize(&v)?;
            if entry.is_deleted()
                || store.exists(
                    crate::storage::cf::ENTRIES_ARCHIVED,
                    entry.code.as_bytes(),
                )?
            {
                continue;
            }
            let labels: Vec<Labeling> = store
                .scan_prefix(crate::storage::cf::LABELINGS, entry.code.as_bytes())?
                .into_iter()
                .map(|(_, lv)| bincode::deserialize::<Labeling>(&lv))
                .collect::<Result<Vec<_>, _>>()?;
            let comments = comments_text(store, &entry.code)?;
            docs.push(self.build_doc(&entry, &labels, &comments));
        }
        let n = docs.len();
        if n == 0 {
            return Ok(0);
        }
        let idx = self.client.index(&self.index_name);
        self.rt.block_on(async {
            let task = idx
                .add_or_replace(&docs, Some(PRIMARY_KEY))
                .await
                .map_err(|e| AppError::Storage(format!("meili backfill add 失败: {e}")))?;
            task.wait_for_completion(&self.client, None, None)
                .await
                .map_err(|e| AppError::Storage(format!("meili backfill wait 失败: {e}")))?;
            Ok::<_, AppError>(n)
        })
    }

    fn name(&self) -> &'static str {
        "meilisearch"
    }
}
