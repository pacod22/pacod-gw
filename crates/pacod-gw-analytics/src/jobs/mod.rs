//! Batch jobs. They run here and not in the core because they fetch external pages: the
//! process that holds every provider key should not be the one that reads arbitrary HTML
//! (concept section 13.3).
//!
//! A job runs under a lease from the core's internal job API (`pacod_gw_events::jobs`), so
//! that one analytics instance runs it at a time. The core decides when a run is due, hands
//! out the work, and decides what happens with the result; this side only finds things out.

pub mod prices;
pub mod retention;
pub mod rollups;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pacod_gw_events::egress::EgressPolicy;
use pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER;
use pacod_gw_events::jobs::{JOB_PRICES, JOB_RETENTION, JOB_ROLLUPS, JobResult, LeaseReply, LeaseRequest};

use crate::storage::ClickHouse;

/// How often an instance asks the core for work.
const POLL: Duration = Duration::from_secs(30);

/// How long a lease is held. Long enough for a run; a crashed instance frees it by not renewing.
const LEASE_SECONDS: u32 = 600;

/// How many redirects a fetched source may take.
const MAX_REDIRECTS: usize = 5;

/// Resolves host names and drops the addresses the egress policy refuses, so that a name
/// which resolves to an internal address is not connected to.
struct GuardedResolver(Arc<EgressPolicy>);

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.0.clone();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed: Vec<SocketAddr> = resolved.iter().copied().filter(|a| policy.allows(&host, a.ip())).collect();
            if allowed.is_empty() && !resolved.is_empty() {
                return Err(format!("{host} resolves to an address the egress policy does not allow").into());
            }
            let addrs: reqwest::dns::Addrs = Box::new(allowed.into_iter());
            Ok(addrs)
        })
    }
}

/// The client that fetches what an administrator named: price feeds and pricing pages. It
/// connects only where the egress policy allows, on the first request and on every redirect.
fn fetch_client(policy: &Arc<EgressPolicy>) -> reqwest::Result<reqwest::Client> {
    let on_redirect = policy.clone();
    reqwest::Client::builder()
        // Pricing pages are public documents; a slow one must not hold a run for long.
        .timeout(Duration::from_secs(45))
        .user_agent(concat!("pacod-gw-analytics/", env!("CARGO_PKG_VERSION")))
        .dns_resolver(Arc::new(GuardedResolver(policy.clone())))
        .redirect(reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            // The resolver sees names; an address written into the URL is checked here.
            match on_redirect.check_url(attempt.url().as_str()) {
                Ok(_) => attempt.follow(),
                Err(e) => attempt.error(e),
            }
        }))
        .build()
}

/// What the job runner needs to reach the core and the sources.
#[derive(Clone)]
pub struct Runner {
    /// The core's management API, for example `http://core:8081`.
    pub core: String,
    pub internal_token: String,
    /// Names this instance as the holder of a lease.
    pub holder: String,
    /// For the core, which is on the internal network by design.
    pub http: reqwest::Client,
    /// For everything else. See [`fetch_client`].
    pub fetch: reqwest::Client,
    pub egress: Arc<EgressPolicy>,
    /// For the jobs that work on the stored data: retention and rollups.
    pub clickhouse: Option<ClickHouse>,
}

impl Runner {
    pub fn new(core: &str, internal_token: &str, egress: EgressPolicy) -> anyhow::Result<Self> {
        let egress = Arc::new(egress);
        Ok(Self {
            core: core.trim_end_matches('/').to_owned(),
            internal_token: internal_token.to_owned(),
            holder: format!("analytics-{}", uuid::Uuid::now_v7().simple()),
            http: reqwest::Client::builder()
                .user_agent(concat!("pacod-gw-analytics/", env!("CARGO_PKG_VERSION")))
                .build()?,
            fetch: fetch_client(&egress)?,
            egress,
            clickhouse: None,
        })
    }

    /// Lets the runner work on the stored data too.
    pub fn with_clickhouse(mut self, clickhouse: ClickHouse) -> Self {
        self.clickhouse = Some(clickhouse);
        self
    }

    /// Fetches a document an administrator named, at most `max_bytes` long.
    pub async fn fetch(&self, url: &str, max_bytes: usize) -> anyhow::Result<Vec<u8>> {
        self.egress.check_url(url)?;
        let mut resp = self.fetch.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("{url} answered {status}");
        }
        // Read with the limit in hand: a source that never ends must not fill the memory.
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            if body.len() + chunk.len() > max_bytes {
                anyhow::bail!("{url} is larger than {max_bytes} bytes");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> anyhow::Result<T> {
        let resp = self
            .http
            .post(format!("{}/api/v1/internal/jobs{path}", self.core))
            .header(INTERNAL_TOKEN_HEADER, &self.internal_token)
            .json(body)
            // An extraction is an LLM call and may take a while.
            .timeout(Duration::from_secs(300))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("the core answered {status}: {}", text.chars().take(300).collect::<String>());
        }
        Ok(resp.json().await?)
    }

    /// Asks for the retention job once, and runs it when it is due. Returns whether a run
    /// happened.
    pub async fn tick_retention(&self) -> anyhow::Result<bool> {
        let Some(clickhouse) = &self.clickhouse else { return Ok(false) };
        let lease: LeaseReply = self
            .post(
                "/lease",
                &LeaseRequest { job: JOB_RETENTION.into(), holder: self.holder.clone(), ttl_seconds: LEASE_SECONDS },
            )
            .await?;
        let (true, true, Some(run_id)) = (lease.granted, lease.due, lease.run_id) else { return Ok(false) };
        tracing::info!(run_id, "retention run started");
        let outcome = retention::run(clickhouse, &lease.work, chrono::Utc::now()).await;
        let (ok, message, result) = match outcome {
            Ok(done) => (true, String::new(), done),
            Err(e) => (false, format!("{e:#}"), serde_json::json!({})),
        };
        let report = JobResult {
            run_id: run_id.clone(),
            job: JOB_RETENTION.into(),
            finished_at: chrono::Utc::now(),
            ok,
            message,
            result,
        };
        let _: serde_json::Value = self.post(&format!("/{run_id}/result"), &report).await?;
        tracing::info!(run_id, ok, "retention run finished");
        Ok(true)
    }

    /// Asks for the rollup job once, and rebuilds the rollups when it is due.
    pub async fn tick_rollups(&self) -> anyhow::Result<bool> {
        let Some(clickhouse) = &self.clickhouse else { return Ok(false) };
        let lease: LeaseReply = self
            .post(
                "/lease",
                &LeaseRequest { job: JOB_ROLLUPS.into(), holder: self.holder.clone(), ttl_seconds: LEASE_SECONDS },
            )
            .await?;
        if !(lease.granted && lease.due) {
            return Ok(false);
        }
        let done = rollups::run(clickhouse, chrono::Utc::now()).await?;
        tracing::debug!(%done, "rollups rebuilt");
        Ok(true)
    }

    /// Asks for the price job once, and runs it when it is due. Returns whether a run happened.
    pub async fn tick(&self) -> anyhow::Result<bool> {
        let lease: LeaseReply = self
            .post(
                "/lease",
                &LeaseRequest { job: JOB_PRICES.into(), holder: self.holder.clone(), ttl_seconds: LEASE_SECONDS },
            )
            .await?;
        let (true, true, Some(run_id)) = (lease.granted, lease.due, lease.run_id) else { return Ok(false) };
        tracing::info!(run_id, "price tracker run started");
        let outcome = prices::run(self, &run_id, &lease.work).await;
        let (ok, message, result) = match outcome {
            Ok(found) => (true, String::new(), found),
            Err(e) => (false, format!("{e:#}"), serde_json::json!({ "candidates": [] })),
        };
        let report = JobResult {
            run_id: run_id.clone(),
            job: JOB_PRICES.into(),
            finished_at: chrono::Utc::now(),
            ok,
            message,
            result,
        };
        let decided: serde_json::Value = self.post(&format!("/{run_id}/result"), &report).await?;
        tracing::info!(run_id, ok, counts = %decided["counts"], "price tracker run finished");
        Ok(true)
    }

    /// Runs until the task is aborted.
    pub async fn run_forever(self) {
        loop {
            if let Err(error) = self.tick().await {
                // The core being away is not this service's failure: try again later.
                tracing::debug!(%error, "no job work this round");
            }
            if let Err(error) = self.tick_retention().await {
                tracing::debug!(%error, "no retention work this round");
            }
            if let Err(error) = self.tick_rollups().await {
                tracing::warn!(%error, "rebuilding the rollups failed; reports read the events meanwhile");
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    fn runner(allow: &[&str]) -> Runner {
        let allow: Vec<String> = allow.iter().map(|s| s.to_string()).collect();
        Runner::new("http://core.invalid:8081", "0123456789abcdef", EgressPolicy::parse(&allow).unwrap()).unwrap()
    }

    /// A server that answers every connection with `response`, on a loopback port.
    async fn serve(response: String) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { break };
                let response = response.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let _ = socket.read(&mut buf).await;
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn ok(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len())
    }

    #[tokio::test]
    async fn internal_addresses_are_not_fetched() {
        let r = runner(&[]);
        for url in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:9/prices.json",
            "http://10.0.0.5/prices.json",
            "http://[::1]/prices.json",
            "http://localhost/prices.json",
            "file:///etc/passwd",
        ] {
            let e = r.fetch(url, 1024).await.expect_err(url).to_string();
            assert!(e.contains("private, loopback or link-local") || e.contains("only http and https"), "{url}: {e}");
        }
        // The same server is fetched once the installation allows its address.
        let base = serve(ok("{}")).await;
        assert!(r.fetch(&format!("{base}/prices.json"), 1024).await.is_err());
        assert_eq!(runner(&["127.0.0.1"]).fetch(&format!("{base}/prices.json"), 1024).await.unwrap(), b"{}");
    }

    #[tokio::test]
    async fn a_name_that_resolves_to_an_internal_address_is_not_connected_to() {
        use reqwest::dns::Resolve;
        let name = || "localhost".parse::<reqwest::dns::Name>().unwrap();
        let refused = GuardedResolver(Arc::new(EgressPolicy::default())).resolve(name()).await;
        assert!(refused.err().unwrap().to_string().contains("does not allow"));
        let allowed = GuardedResolver(Arc::new(EgressPolicy::parse(&["localhost".into()]).unwrap()));
        assert!(allowed.resolve(name()).await.unwrap().count() > 0);
    }

    #[tokio::test]
    async fn a_redirect_to_an_internal_address_is_not_followed() {
        let away = "HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/latest/meta-data/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        let base = serve(away.to_owned()).await;
        // The first address is allowed; where it sends the fetch is not.
        let e = runner(&["127.0.0.1"]).fetch(&format!("{base}/prices.json"), 1024).await.unwrap_err();
        assert!(format!("{e:#}").contains("private, loopback or link-local"), "{e:#}");
    }

    #[tokio::test]
    async fn a_source_larger_than_the_limit_is_not_read_to_its_end() {
        let base = serve(ok(&"x".repeat(5000))).await;
        let r = runner(&["127.0.0.1"]);
        let e = r.fetch(&base, 1000).await.unwrap_err().to_string();
        assert!(e.contains("larger than 1000 bytes"), "{e}");
        assert_eq!(r.fetch(&base, 5000).await.unwrap().len(), 5000);
    }
}
