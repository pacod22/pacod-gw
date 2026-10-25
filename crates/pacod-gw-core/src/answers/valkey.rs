//! Answers kept in Valkey, shared by every instance of the gateway.
//!
//! One connection, multiplexed and reconnected by the client. Valkey on the request path must
//! never slow calls down when it is in trouble: every operation is bounded, and after a few
//! failures in a row the store is skipped for a while (a breaker), so that calls go straight to
//! their provider instead of waiting for a timeout each.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use bytes::Bytes;
use futures_util::future::BoxFuture;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::{AsyncCommands, Client, IntoConnectionInfo};
use tokio::sync::OnceCell;

use super::{AnswerStore, Forget, KEY_PREFIX, SharedFigures, StoreError, StoreFigures};

/// How long one operation, or connecting, may take.
const OP_TIMEOUT: Duration = Duration::from_millis(500);
/// Failures in a row that open the breaker.
const TRIP_AFTER: u32 = 3;
/// How long the store is skipped once the breaker is open.
const OPEN_FOR: Duration = Duration::from_secs(5);
/// How long one operation of management work may take: it waits for no call.
const ADMIN_OP_TIMEOUT: Duration = Duration::from_secs(2);
/// How long forgetting may take in all; what is left then expires in its time.
const FORGET_FOR: Duration = Duration::from_secs(60);
/// Keys asked for, and removed, per round of a scan.
const SCAN_COUNT: usize = 500;

pub struct ValkeyStore {
    client: Client,
    conn: OnceCell<ConnectionManager>,
    /// Failures in a row.
    failures: AtomicU32,
    /// Until when the store is skipped, in milliseconds since `epoch`; 0 when it is not.
    open_until_ms: AtomicU64,
    epoch: Instant,
}

impl ValkeyStore {
    /// Prepares the store. It connects on first use, so that a gateway starts while Valkey
    /// does not answer yet.
    pub fn new(url: &str, password: Option<&str>) -> anyhow::Result<Self> {
        let mut info = url.into_connection_info().context("PGW_VALKEY_URL is not a Valkey address")?;
        if let Some(p) = password {
            let settings = info.redis_settings().clone().set_password(p);
            info = info.set_redis_settings(settings);
        }
        let client = Client::open(info).context("PGW_VALKEY_URL")?;
        Ok(Self {
            client,
            conn: OnceCell::new(),
            failures: AtomicU32::new(0),
            open_until_ms: AtomicU64::new(0),
            epoch: Instant::now(),
        })
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn check_breaker(&self) -> Result<(), StoreError> {
        let until = self.open_until_ms.load(Ordering::Relaxed);
        if until != 0 && self.now_ms() < until {
            return Err(StoreError::Unavailable);
        }
        Ok(())
    }

    fn succeeded(&self) {
        if self.failures.swap(0, Ordering::Relaxed) >= TRIP_AFTER {
            tracing::info!("the response cache's Valkey answers again");
        }
        self.open_until_ms.store(0, Ordering::Relaxed);
    }

    fn failed(&self, e: &str) -> StoreError {
        let n = self.failures.fetch_add(1, Ordering::Relaxed) + 1;
        if n >= TRIP_AFTER {
            self.open_until_ms.store(self.now_ms() + OPEN_FOR.as_millis() as u64, Ordering::Relaxed);
            if n == TRIP_AFTER {
                tracing::warn!(error = %e, "the response cache's Valkey is failing; calls skip it for a while");
            }
        }
        StoreError::Failed(e.to_owned())
    }

    async fn conn(&self) -> Result<ConnectionManager, String> {
        let conn = self
            .conn
            .get_or_try_init(|| async {
                let config = ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(OP_TIMEOUT))
                    .set_response_timeout(Some(OP_TIMEOUT))
                    .set_number_of_retries(1);
                self.client.get_connection_manager_with_config(config).await
            })
            .await
            .map_err(|e| e.to_string())?;
        Ok(conn.clone())
    }

    async fn get_bytes(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        let mut conn = self.conn().await?;
        conn.get::<_, Option<Vec<u8>>>(key).await.map_err(|e| e.to_string())
    }

    async fn shared_figures(&self) -> Result<SharedFigures, String> {
        let mut conn = self.conn().await?;
        let keys: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await.map_err(|e| e.to_string())?;
        let info: String = redis::cmd("INFO").arg("memory").query_async(&mut conn).await.map_err(|e| e.to_string())?;
        Ok(SharedFigures { keys, ..parse_info(&info) })
    }

    async fn forget_keys(&self, which: Forget<'_>) -> Result<u64, String> {
        let pattern = match which {
            Forget::Workspaces([one]) => format!("{KEY_PREFIX}*:{}:*", glob_escape(one)),
            Forget::Workspaces([]) => return Ok(0),
            _ => format!("{KEY_PREFIX}*"),
        };
        let started = Instant::now();
        let mut conn = self.conn().await?;
        let (mut cursor, mut removed) = (0u64, 0u64);
        loop {
            let mut scan = redis::cmd("SCAN");
            scan.arg(cursor).arg("MATCH").arg(&pattern).arg("COUNT").arg(SCAN_COUNT);
            let (next, keys): (u64, Vec<String>) = admin_op(scan.query_async(&mut conn)).await?;
            let keys: Vec<String> = keys.into_iter().filter(|k| which.covers(k)).collect();
            if !keys.is_empty() {
                let n: u64 = admin_op(redis::cmd("UNLINK").arg(&keys).query_async(&mut conn)).await?;
                removed += n;
            }
            cursor = next;
            if cursor == 0 {
                return Ok(removed);
            }
            if started.elapsed() > FORGET_FOR {
                tracing::warn!(removed, "forgetting kept answers took too long; the rest expire in their time");
                return Ok(removed);
            }
            // Leaves room for the calls that use the store meanwhile.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn put_bytes(&self, key: &str, value: &[u8], ttl: Duration) -> Result<(), String> {
        let mut conn = self.conn().await?;
        conn.set_ex::<_, _, ()>(key, value, ttl.as_secs().max(1)).await.map_err(|e| e.to_string())
    }
}

impl AnswerStore for ValkeyStore {
    fn kind(&self) -> &'static str {
        "valkey"
    }

    fn shared(&self) -> bool {
        true
    }

    fn get<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<Option<Bytes>, StoreError>> {
        Box::pin(async move {
            self.check_breaker()?;
            match tokio::time::timeout(OP_TIMEOUT, self.get_bytes(key)).await {
                Ok(Ok(v)) => {
                    self.succeeded();
                    Ok(v.map(Bytes::from))
                }
                Ok(Err(e)) => Err(self.failed(&e)),
                Err(_) => Err(self.failed("no answer in time")),
            }
        })
    }

    fn put<'a>(&'a self, key: &'a str, value: Bytes, ttl: Duration) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            self.check_breaker()?;
            match tokio::time::timeout(OP_TIMEOUT, self.put_bytes(key, &value, ttl)).await {
                Ok(Ok(())) => {
                    self.succeeded();
                    Ok(())
                }
                Ok(Err(e)) => Err(self.failed(&e)),
                Err(_) => Err(self.failed("no answer in time")),
            }
        })
    }

    fn figures(&self) -> StoreFigures {
        StoreFigures {
            store: "valkey",
            breaker_open: self.check_breaker().is_err(),
            failures_in_a_row: self.failures.load(Ordering::Relaxed),
            ..Default::default()
        }
    }

    // Neither of these goes through `succeeded` or `failed`: management work must not open or
    // close the breaker the calls go by.
    fn inspect(&self) -> BoxFuture<'_, Result<Option<SharedFigures>, StoreError>> {
        Box::pin(async move { admin_op(self.shared_figures()).await.map(Some).map_err(StoreError::Failed) })
    }

    fn forget<'a>(&'a self, which: Forget<'a>) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move { self.forget_keys(which).await.map_err(StoreError::Failed) })
    }
}

/// One operation of management work, bounded.
async fn admin_op<T, E: std::fmt::Display>(op: impl std::future::Future<Output = Result<T, E>>) -> Result<T, String> {
    match tokio::time::timeout(ADMIN_OP_TIMEOUT, op).await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(_) => Err("no answer in time".into()),
    }
}

/// The memory figures of `INFO memory`; lines it does not know are passed over.
fn parse_info(info: &str) -> SharedFigures {
    let mut f = SharedFigures::default();
    for line in info.lines() {
        let Some((name, value)) = line.trim().split_once(':') else { continue };
        match name {
            "used_memory" => f.used_memory = value.parse().unwrap_or(0),
            "maxmemory" => f.max_memory = value.parse().unwrap_or(0),
            "maxmemory_policy" => f.eviction_policy = value.to_owned(),
            _ => {}
        }
    }
    f
}

/// A workspace id as a literal in a SCAN pattern.
fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_checked_and_the_password_stays_out_of_the_url() {
        assert!(ValkeyStore::new("redis://gw-valkey:6379/0", Some("secret")).is_ok());
        assert!(ValkeyStore::new("not a url", None).is_err());
    }

    #[tokio::test]
    async fn a_store_that_does_not_answer_is_skipped_for_a_while() {
        // Nothing listens on port 1: every attempt fails, and fast.
        let s = ValkeyStore::new("redis://127.0.0.1:1/0", None).unwrap();
        for _ in 0..TRIP_AFTER {
            assert!(matches!(s.get("k").await, Err(StoreError::Failed(_))));
        }
        assert_eq!(s.get("k").await, Err(StoreError::Unavailable), "the breaker is open");
        assert_eq!(s.put("k", Bytes::new(), Duration::from_secs(1)).await, Err(StoreError::Unavailable));
        let f = s.figures();
        assert!(f.breaker_open && f.failures_in_a_row >= TRIP_AFTER);
    }

    #[tokio::test]
    async fn management_work_on_a_failing_store_leaves_the_breaker_alone() {
        let s = ValkeyStore::new("redis://127.0.0.1:1/0", None).unwrap();
        assert!(s.inspect().await.is_err());
        assert!(s.forget(Forget::All).await.is_err());
        assert!(s.inspect().await.is_err());
        assert_eq!(s.figures(), StoreFigures { store: "valkey", ..Default::default() });
    }

    #[test]
    fn the_memory_figures_are_read_from_info() {
        let info = "# Memory\r\nused_memory:1048576\r\nused_memory_human:1.00M\r\nmaxmemory:402653184\r\n\
                    maxmemory_policy:allkeys-lru\r\n";
        let f = parse_info(info);
        assert_eq!((f.used_memory, f.max_memory, f.eviction_policy.as_str()), (1048576, 402653184, "allkeys-lru"));
        assert_eq!(glob_escape("ws_a*"), "ws_a\\*");
    }
}
