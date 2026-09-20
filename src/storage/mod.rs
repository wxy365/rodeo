//! 存储层。对外只暴露后端无关的东西：列族名、批量写操作、门面 [`DocStore`]，
//! 以及附件后端 [`BlobStore`]。RocksDB / PostgreSQL 的实现细节不出这个模块。

mod doc;
mod pg;
mod rocksdb;

pub mod keys;

pub use doc::{cf, BatchOp, DocStore};
