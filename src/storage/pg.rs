//! PostgreSQL 文档后端：把「列族 + 二进制键」这层 K/V 模型原样落在一张
//! `kv(cf, key, value)` 表上，从而与 RocksDB 后端共享全部上层代码。
//!
//! 这不是关系建模（每个实体一张表）——那意味着把服务层约 15k 行同步 CRUD
//! 全部改写成 SQL，而收益只是「看起来更正统」。换来的好处是实打实的：
//! `BatchOp` 的多列族原子写天然映射成一次事务；键的字节序与 Postgres `bytea`
//! 的比较语义一致（都是无符号逐字节 memcmp），所以 `scan_prefix` 的
//! 「按 key 升序、在第一个不匹配处收尾」逐字保留。
//!
//! 线程模型：同步 `postgres` 客户端每次调用都在它自己的 tokio Runtime 上
//! `block_on`（见 `postgres` 的 `Connection::runtime`），而本进程跑在
//! `#[tokio::main(flavor = "current_thread")]` 里——在运行时线程上再嵌一层
//! `block_on` 是死锁，不是「稍微阻塞一下」。所以这里不在调用线程上碰
//! `postgres`：`open` 起一条专用 OS 线程，连接、建表、所有查询都在那条线程上
//! 跑；调用方把请求经 channel 递过去，再阻塞等回包。`DocStore` 的同步 API 不变。
//!
//! 只有一条连接、没有连接池：应用是 current_thread 运行时，同一时刻只有一处
//! 在调存储，池永远给不出第二路并发，却会带来「启动时凑齐 8 条连接」的等待。

use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread;

use postgres::NoTls;

use crate::error::AppError;

use super::doc::BatchOp;

const UPSERT_KV: &str = "\
INSERT INTO kv (cf, key, value) VALUES ($1, $2, $3)
ON CONFLICT (cf, key) DO UPDATE SET value = EXCLUDED.value";

const DELETE_KV: &str = "DELETE FROM kv WHERE cf = $1 AND key = $2";

/// 在专用线程上执行的一件工作。闭包拿到那条线程独占的 `Client`，
/// 自行把结果塞进它捕获的应答 channel。
type Job = Box<dyn FnOnce(&mut postgres::Client) + Send>;

pub(crate) struct PgDoc {
    /// 投递口。`Sender` 不是 `Sync`，靠 `Mutex` 兜住——所以 `PgDoc` 是 `Sync`，
    /// 能待在 `Arc<DocStore>` 里被多个 handler 共享。
    tx: Mutex<Sender<Job>>,
}

impl PgDoc {
    pub fn open(url: &str) -> Result<Self, AppError> {
        let cfg: postgres::Config = url
            .parse()
            .map_err(|e| AppError::Storage(format!("PostgreSQL 连接串无法解析: {e}")))?;
        let (job_tx, job_rx) = mpsc::channel::<Job>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), AppError>>();
        // 这条线程不在任何 tokio 上下文中，所以 `postgres` 内部那层
        // `Runtime::block_on` 在这里合法。
        //
        // NoTls：仓库全树禁 openssl / native-tls（见 Cargo.toml 里 rustls 被钉成只开 ring），
        // 所以这里不启用任何 TLS 特性。数据库请放内网，或由隧道 / 代理终结 TLS。
        thread::Builder::new()
            .name("pg-doc".to_string())
            .spawn(move || {
                let mut client = match cfg.connect(NoTls) {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = ready_tx.send(Err(AppError::Storage(format!(
                            "连接 PostgreSQL 失败: {e}"
                        ))));
                        return;
                    }
                };
                // 幂等建表。`(cf, key)` 主键同时就是 `scan_prefix` 依赖的有序索引：
                // `cf` 等值 + `key` 范围 + `ORDER BY key` 全部由这一个 btree 满足。
                if let Err(e) = client.batch_execute(
                    "CREATE TABLE IF NOT EXISTS kv (
                         cf    TEXT  NOT NULL,
                         key   BYTEA NOT NULL,
                         value BYTEA NOT NULL,
                         PRIMARY KEY (cf, key)
                     )",
                ) {
                    let _ = ready_tx.send(Err(AppError::from(e)));
                    return;
                }
                let _ = ready_tx.send(Ok(()));
                // `recv` 返回 Err 说明所有 `Sender` 都没了，即 `PgDoc` 已 drop。
                while let Ok(job) = job_rx.recv() {
                    job(&mut client);
                }
            })
            .map_err(|e| AppError::Storage(format!("启动 PostgreSQL 工作线程失败: {e}")))?;
        // 阻塞到握手完成：连接失败 / 建表失败在这里就变成 `open` 的错误，
        // 而不是等第一次查询才发现。这样「配置写错」在启动期就清零。
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self { tx: Mutex::new(job_tx) }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AppError::Storage("PostgreSQL 工作线程提前退出".to_string())),
        }
    }

    /// 把闭包递到工作线程并等它的结果。借用参数必须由调用方 clone 成 owned
    /// 值再 move 进来——闭包要 `'static`。
    fn call<T, F>(&self, f: F) -> Result<T, AppError>
    where
        T: Send + 'static,
        F: FnOnce(&mut postgres::Client) -> Result<T, AppError> + Send + 'static,
    {
        let (reply_tx, reply_rx) = mpsc::channel::<Result<T, AppError>>();
        let job: Job = Box::new(move |client| {
            // 调用方已经走了，回包没人收也无所谓，不能因此 panic 掉工作线程。
            let _ = reply_tx.send(f(client));
        });
        // 只在投递期间持锁：工作线程执行期间不挡其他调用方投递。
        self.tx
            .lock()
            .map_err(|_| AppError::Storage("PostgreSQL 通道锁已中毒".to_string()))?
            .send(job)
            .map_err(|_| AppError::Storage("PostgreSQL 工作线程已退出".to_string()))?;
        reply_rx
            .recv()
            .map_err(|_| AppError::Storage("PostgreSQL 工作线程已退出".to_string()))?
    }

    pub fn get_raw(&self, cf: &str, key: &[u8]) -> Result<Option<Vec<u8>>, AppError> {
        let cf = cf.to_string();
        let key = key.to_vec();
        self.call(move |client| {
            let row = client.query_opt(
                "SELECT value FROM kv WHERE cf = $1 AND key = $2",
                &[&cf, &key],
            )?;
            Ok(row.map(|r| r.get(0)))
        })
    }

    pub fn put_raw(&self, cf: &str, key: &[u8], value: &[u8]) -> Result<(), AppError> {
        let cf = cf.to_string();
        let key = key.to_vec();
        let value = value.to_vec();
        self.call(move |client| {
            client.execute(UPSERT_KV, &[&cf, &key, &value])?;
            Ok(())
        })
    }

    pub fn delete(&self, cf: &str, key: &[u8]) -> Result<(), AppError> {
        let cf = cf.to_string();
        let key = key.to_vec();
        self.call(move |client| {
            client.execute(DELETE_KV, &[&cf, &key])?;
            Ok(())
        })
    }

    pub fn scan_prefix(&self, cf: &str, prefix: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>, AppError> {
        let cf = cf.to_string();
        let prefix = prefix.to_vec();
        self.call(move |client| {
            // 有上界就用它收窄范围；没有（前缀全为 0xFF）则退化成 `key >= prefix`，
            // 由下面那个 `starts_with` 循环与 RocksDB 一样在第一个不匹配处收尾。
            let rows = match prefix_upper_bound(&prefix) {
                Some(upper) => client.query(
                    "SELECT key, value FROM kv WHERE cf = $1 AND key >= $2 AND key < $3 ORDER BY key",
                    &[&cf, &prefix, &upper],
                )?,
                None => client.query(
                    "SELECT key, value FROM kv WHERE cf = $1 AND key >= $2 ORDER BY key",
                    &[&cf, &prefix],
                )?,
            };
            let mut out = Vec::with_capacity(rows.len());
            for row in &rows {
                let key: Vec<u8> = row.get(0);
                if !key.starts_with(&prefix) {
                    break;
                }
                out.push((key, row.get(1)));
            }
            Ok(out)
        })
    }

    /// 多列族原子写 → 单事务。RocksDB `WriteBatch` 的语义（按序应用、要么全成要么全不成）
    /// 由事务的提交粒度保证；同一键先删后写也按语句顺序生效。
    pub fn write_batch(&self, ops: Vec<BatchOp>) -> Result<(), AppError> {
        self.call(move |client| {
            let mut tx = client.transaction()?;
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
        })
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
