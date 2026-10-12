//! 全文检索抽象。两个后端：`TantivyBackend`（本地嵌入式，单机）与
//! `MeilisearchBackend`（外部 HTTP 服务，多实例可共享）。
//!
//! 选择 `SearchBackend` trait 而不是 enum 派发，是为了让 `Arc<dyn SearchBackend>`
//! 直接挂在 `Services` 上，不必把每个 resolver 的命中分支再写成两层 match。
//! 两个实现都按 sync trait 实现——`MeilisearchBackend` 内部持有一个独立的
//! 多线程 `tokio::runtime::Runtime`，HTTP 走 `block_on`，与 rodeo 自己的
//! `current_thread` runtime 完全隔离，不会让 axum 请求线程被外部服务的网络
//! 往返阻塞。

use ulid::Ulid;

use crate::domain::{Entry, Labeling};
use crate::error::AppError;
use crate::storage::DocStore;

pub mod helpers;
pub mod meilisearch;
pub mod tantivy;

pub use helpers::{comments_text, strip_rich_text};
pub use meilisearch::MeilisearchBackend;
pub use tantivy::{TantivyBackend, TEXT_CANDIDATE_LIMIT};

/// 全文检索后端的统一接口。所有实现必须能在多线程上下文中共享
///（axum 的 State 是 `Arc`，后端实例也是 `Arc<dyn SearchBackend>`）。
pub trait SearchBackend: Send + Sync {
    /// 写一条文档；upsert 语义（已存在则覆盖）。
    fn index_entry(
        &self,
        entry: &Entry,
        labels: &[Labeling],
        comments: &str,
    ) -> Result<(), AppError>;

    /// 从索引中移除一条文档。
    fn remove_entry(&self, code: &str) -> Result<(), AppError>;

    /// 在工作空间内做关键词检索，返回条目 code 列表（按相关度降序）。
    fn search(
        &self,
        ws: Ulid,
        keyword: &str,
        limit: usize,
    ) -> Result<Vec<String>, AppError>;

    /// 启动期回填：扫 `DocStore` 的 `cf::ENTRIES`，把每条（非删除/非归档）写入索引。
    /// 已有文档时跳过（幂等）。
    fn backfill(&self, store: &DocStore) -> Result<usize, AppError>;

    /// 后端名称（启动日志 / `/api/health`）。
    fn name(&self) -> &'static str;
}
