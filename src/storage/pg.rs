//! PostgreSQL 文档后端：把「列族 + 二进制键」这层 K/V 模型原样落在一张
//! `kv(cf, key, value)` 表上，从而与 RocksDB 后端共享全部上层代码。
//!
//! 这不是关系建模（每个实体一张表）——那意味着把服务层约 15k 行同步 CRUD
//! 全部改写成 SQL，而收益只是「看起来更正统」。换来的好处是实打实的：
//! `BatchOp` 的多列族原子写天然映射成一次事务；键的字节序与 Postgres `bytea`
//! 的比较语义一致（都是无符号逐字节 memcmp），所以 `scan_prefix` 的
//! 「按 key 升序、在第一个不匹配处收尾」逐字保留。

use postgres::NoTls;
use r2d2::Pool;
use r2d2_postgres::PostgresConnectionManager;

use crate::error::AppError;

use super::doc::BatchOp;

const UPSERT_KV: &str = "\
INSERT INTO kv (cf, key, value) VALUES ($1, $2, $3)
ON CONFLICT (cf, key) DO UPDATE SET value = EXCLUDED.value";

const DELETE_KV: &str = "DELETE FROM kv WHERE cf = $1 AND key = $2";

pub(crate) struct PgDoc {
    pool: Pool<PostgresConnectionManager<NoTls>>,
}

impl PgDoc {
    pub fn open(url: &str) -> Result<Self, AppError> {
        let cfg: postgres::Config = url
            .parse()
            .map_err(|e| AppError::Storage(format!("PostgreSQL 连接串无法解析: {e}")))?;
        // NoTls：仓库全树禁 openssl / native-tls（见 Cargo.toml 里 rustls 被钉成只开 ring），
        // 所以这里不启用任何 TLS 特性。数据库请放内网，或由隧道 / 代理终结 TLS。
        let manager = PostgresConnectionManager::new(cfg, NoTls);
        // `build` 会先真的取一条连接，比 `build_unchecked` 的懒加载更早暴露
        // 「地址写错 / 库不存在 / 密码不对」这类配置问题。
        let pool = Pool::builder()
            .max_size(8)
            .build(manager)
            .map_err(|e| AppError::Storage(format!("连接 PostgreSQL 失败: {e}")))?;
        let doc = Self { pool };
        doc.init_schema()?;
        Ok(doc)
    }

    /// 幂等建表。`(cf, key)` 主键同时就是 `scan_prefix` 依赖的有序索引：
    /// `cf` 等值 + `key` 范围 + `ORDER BY key` 全部由这一个 btree 满足。
    fn init_schema(&self) -> Result<(), AppError> {
        let mut conn = self.pool.get()?;
        conn.batch_execute(
            "CREATE TABLE IF NOT EXISTS kv (
                 cf    TEXT  NOT NULL,
                 key   BYTEA NOT NULL,
                 value BYTEA NOT NULL,
                 PRIMARY KEY (cf, key)
             )",
        )?;
        Ok(())
    }

    pub fn get_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, AppError> {
        let mut conn = self.pool.get()?;
        let row = conn.query_opt("SELECT value FROM kv WHERE cf = $1 AND key = $2", &[&cf, &key])?;
        Ok(row.map(|r| r.get(0)))
    }

    pub fn put_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), AppError> {
        let mut conn = self.pool.get()?;
        conn.execute(UPSERT_KV, &[&cf, &key, &value])?;
        Ok(())
    }

    pub fn delete(&self, cf: &str, key: &[u8]) -> Result<(), AppError> {
        let mut conn = self.pool.get()?;
        conn.execute(DELETE_KV, &[&cf, &key])?;
        Ok(())
    }

    pub fn scan_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        let mut conn = self.pool.get()?;
        // 有上界就用它收窄范围；没有（前缀全为 0xFF）则退化成 `key >= prefix`，
        // 由下面那个 `starts_with` 循环与 RocksDB 一样在第一个不匹配处收尾。
        let rows = match prefix_upper_bound(prefix) {
            Some(upper) => conn.query(
                "SELECT key, value FROM kv WHERE cf = $1 AND key >= $2 AND key < $3 ORDER BY key",
                &[&cf, &prefix, &upper],
            )?,
            None => conn.query(
                "SELECT key, value FROM kv WHERE cf = $1 AND key >= $2 ORDER BY key",
                &[&cf, &prefix],
            )?,
        };
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let key: Vec<u8> = row.get(0);
            if !key.starts_with(prefix) {
                break;
            }
            out.push((key, row.get(1)));
        }
        Ok(out)
    }

    /// 多列族原子写 → 单事务。RocksDB `WriteBatch` 的语义（按序应用、要么全成要么全不成）
    /// 由事务的提交粒度保证；同一键先删后写也按语句顺序生效。
    pub fn write_batch(&self, ops: Vec<BatchOp>) -> Result<(), AppError> {
        let mut conn = self.pool.get()?;
        let mut tx = conn.transaction()?;
        for op in &ops {
            match op {
                BatchOp::Put { cf, key, value } => {
                    tx.execute(UPSERT_KV, &[cf, key, value])?;
                }
                BatchOp::Delete { cf, key } => {
                    tx.execute(DELETE_KV, &[cf, key])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }
}

/// 前缀扫描的上界：把最后一个非 `0xFF` 字节 +1，其后截断。
/// 全部为 `0xFF`（或空前缀）时返回 `None`。
///
/// 例：`[b'a', 0xFF]` → `[b'b']`；`[b'a']` → `[b'b']`；`[0xFF]` → `None`。
fn prefix_upper_bound(prefix: &[u8]) -> Option<Vec<u8>> {
    let pos = prefix.iter().rposition(|b| *b != 0xFF)?;
    let mut upper = prefix[..=pos].to_vec();
    upper[pos] += 1;
    Some(upper)
}
