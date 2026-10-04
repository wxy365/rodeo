//! OAuth 流程的两类短期 token：
//! - csrf state：`/start` 写到 store，`/callback` 取出来比对（防 CSRF）。
//!   TTL 10 分钟。
//! - bind session：`/callback` 在未绑定时写到 store，绑定页用它把 openid 关联到 Rodeo 账号。
//!   TTL 30 分钟。
//!
//! 都存在进程内 `Mutex<HashMap>`；**懒清理**（`insert` 时顺手扫一遍过期键），
//! 不挂 tokio 定时器 —— current_thread runtime 上定时任务会卡其它请求。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::domain::OAuthProvider;

pub struct CsrfEntry {
    /// 防跨 provider 重放：拿微信 csrf 走 Google callback 必须被拒。
    /// 与 `redirect_uri` 一起记下，callback 处比对供方回跳的 URL 是否与当时
    /// 我们跳出去时一致；不一致说明被中间人改写或客户端错配（spec §14）。
    pub provider: String,
    pub redirect_uri: String,
    pub return_to: String,
    pub created_at: DateTime<Utc>,
}

pub struct BindEntry {
    pub provider: OAuthProvider,
    pub external_id: String,
    pub access_token: String,
    pub return_to: String,
    pub created_at: DateTime<Utc>,
}

pub struct OAuthStateStore {
    csrf: Mutex<HashMap<String, CsrfEntry>>,
    bind: Mutex<HashMap<String, BindEntry>>,
    csrf_ttl: Duration,
    bind_ttl: Duration,
}

impl OAuthStateStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            csrf: Mutex::new(HashMap::new()),
            bind: Mutex::new(HashMap::new()),
            csrf_ttl: Duration::from_secs(10 * 60),
            bind_ttl: Duration::from_secs(30 * 60),
        })
    }

    /// 生成 32 字节随机 token，hex 编码（64 字符）。
    /// 用 `rand_core` 的 `OsRng` 实例（getrandom feature 已开）。
    pub fn new_token() -> String {
        use rand_core::RngCore;
        let mut bytes = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut bytes);
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn is_expired(when: DateTime<Utc>, ttl: Duration) -> bool {
        let age = Utc::now().signed_duration_since(when);
        // chrono::Duration → std::time::Duration 的转换；负数（时钟回拨）按未过期处理。
        age.to_std().unwrap_or(Duration::ZERO) > ttl
    }

    fn sweep_csrf(&self, map: &mut HashMap<String, CsrfEntry>) {
        map.retain(|_, v| !Self::is_expired(v.created_at, self.csrf_ttl));
    }

    fn sweep_bind(&self, map: &mut HashMap<String, BindEntry>) {
        map.retain(|_, v| !Self::is_expired(v.created_at, self.bind_ttl));
    }

    pub fn put_csrf(self: &Arc<Self>, state: String, mut entry: CsrfEntry) {
        entry.created_at = Self::now();
        let mut map = self.csrf.lock();
        self.sweep_csrf(&mut map);
        map.insert(state, entry);
    }

    /// 取出即删：state 不能被复用。
    pub fn take_csrf(self: &Arc<Self>, state: &str) -> Option<CsrfEntry> {
        let mut map = self.csrf.lock();
        self.sweep_csrf(&mut map);
        map.remove(state)
    }

    pub fn put_bind(self: &Arc<Self>, token: String, mut entry: BindEntry) {
        entry.created_at = Self::now();
        let mut map = self.bind.lock();
        self.sweep_bind(&mut map);
        map.insert(token, entry);
    }

    pub fn take_bind(self: &Arc<Self>, token: &str) -> Option<BindEntry> {
        let mut map = self.bind.lock();
        self.sweep_bind(&mut map);
        map.remove(token)
    }
}
