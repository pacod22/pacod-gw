//! The HTTP client of the broker's public API.

use std::time::Duration;

use futures_util::{Stream, StreamExt};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::types::*;

/// How the client identifies itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// Nothing: anonymous discovery where the broker allows it.
    None,
    /// A pacod-id access token or a service account's `pidt_…` token.
    Bearer(String),
    /// Open mode: the name the client gives itself (`X-Broker-Client`), so that its tickets and
    /// reports are its own.
    OpenClient(String),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the broker cannot be reached: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("the broker refused: {0}")]
    Problem(Problem),
    #[error("the announcement waits for an admin's approval")]
    Pending(Problem),
    #[error("the answer could not be read: {0}")]
    Decode(String),
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// The HTTP status of a refusal.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Problem(p) | Self::Pending(p) => Some(p.status),
            Self::Transport(e) => e.status().map(|s| s.as_u16()),
            _ => None,
        }
    }

    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// A client of one broker. Cheap to clone: clones share the connection pool.
#[derive(Debug, Clone)]
pub struct BrokerClient {
    http: reqwest::Client,
    base: String,
    auth: Auth,
}

impl BrokerClient {
    /// A client of the broker at `base` (`http://127.0.0.1:8060`), anonymous.
    pub fn new(base: impl Into<String>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("pacod-broker-client/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .build()?;
        Ok(Self { http, base: base.into().trim_end_matches('/').to_string(), auth: Auth::None })
    }

    /// From `BROKER_URL` (default `http://127.0.0.1:8060`), `BROKER_TOKEN` and `BROKER_CLIENT`.
    pub fn from_env() -> Result<Self> {
        let c = Self::new(std::env::var("BROKER_URL").unwrap_or_else(|_| "http://127.0.0.1:8060".into()))?;
        Ok(
            match (
                std::env::var("BROKER_TOKEN").ok().filter(|t| !t.is_empty()),
                std::env::var("BROKER_CLIENT").ok().filter(|t| !t.is_empty()),
            ) {
                (Some(t), _) => c.with_token(t),
                (None, Some(n)) => c.with_client_name(n),
                (None, None) => c,
            },
        )
    }

    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.auth = Auth::Bearer(token.into());
        self
    }

    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        self.auth = Auth::OpenClient(name.into());
        self
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        let r = self.http.request(method, format!("{}{path}", self.base)).timeout(Duration::from_secs(30));
        match &self.auth {
            Auth::None => r,
            Auth::Bearer(t) => r.bearer_auth(t),
            Auth::OpenClient(n) => r.header("x-broker-client", n),
        }
    }

    async fn read<T: DeserializeOwned>(res: Response) -> Result<T> {
        let status = res.status();
        let bytes = res.bytes().await?;
        if status.is_success() && status != StatusCode::ACCEPTED {
            return serde_json::from_slice(&bytes).map_err(|e| {
                Error::Decode(format!("{e}: {}", String::from_utf8_lossy(&bytes[..bytes.len().min(300)])))
            });
        }
        let mut p: Problem = serde_json::from_slice(&bytes).unwrap_or_else(|_| Problem {
            status: status.as_u16(),
            title: status.canonical_reason().unwrap_or("error").into(),
            detail: Some(String::from_utf8_lossy(&bytes[..bytes.len().min(300)]).into_owned()),
            ..Problem::default()
        });
        p.status = status.as_u16();
        Err(if status == StatusCode::ACCEPTED { Error::Pending(p) } else { Error::Problem(p) })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        Self::read(self.request(Method::GET, path).send().await?).await
    }

    async fn send<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<T> {
        Self::read(self.request(method, path).json(body).send().await?).await
    }

    async fn empty(&self, method: Method, path: &str) -> Result<()> {
        let res = self.request(method, path).send().await?;
        if res.status().is_success() {
            return Ok(());
        }
        Self::read::<serde_json::Value>(res).await.map(|_| ())
    }

    // --- Consumers ------------------------------------------------------------------------

    /// Runs a discovery query.
    pub async fn discover(&self, q: &Query) -> Result<DiscoverResponse> {
        self.send(Method::POST, "/v1/discover", q).await
    }

    /// Every page of a query, following the cursor, up to `max` results.
    pub async fn discover_all(&self, q: &Query, max: usize) -> Result<Vec<Hit>> {
        let mut q = q.clone();
        let mut out = Vec::new();
        loop {
            let page = self.discover(&q).await?;
            out.extend(page.hits);
            match page.next_cursor {
                Some(c) if out.len() < max => q.cursor = Some(c),
                _ => break,
            }
        }
        out.truncate(max);
        Ok(out)
    }

    /// The simple form: `GET /v1/resources` with parameters (`kind`, `q`, `tag`, …).
    pub async fn resources(&self, params: &[(&str, &str)]) -> Result<DiscoverResponse> {
        Self::read(self.request(Method::GET, "/v1/resources").query(params).send().await?).await
    }

    pub async fn resource(&self, id: &str) -> Result<ResourceView> {
        self.get(&format!("/v1/resources/{}", enc(id))).await
    }

    /// Prices in force in pacod-gw's price feed format (`GET /v1/feeds/prices`), with the
    /// parameters `provider`, `kind`, `currency` and `key` (`name`, `key` or `id`).
    pub async fn price_feed(&self, params: &[(&str, &str)]) -> Result<serde_json::Value> {
        Self::read(self.request(Method::GET, "/v1/feeds/prices").query(params).send().await?).await
    }

    pub async fn provider(&self, id: &str) -> Result<ProviderView> {
        self.get(&format!("/v1/providers/{}", enc(id))).await
    }

    /// Changes as they happen. The stream ends when the connection does; watch again with the
    /// last generation seen to resume (a `Resync` then says to read the catalogue again).
    pub async fn watch(&self, after: Option<u64>) -> Result<impl Stream<Item = Result<WatchEvent>> + use<>> {
        let mut r = self.http.get(format!("{}/v1/watch", self.base)).header("accept", "text/event-stream");
        r = match &self.auth {
            Auth::None => r,
            Auth::Bearer(t) => r.bearer_auth(t),
            Auth::OpenClient(n) => r.header("x-broker-client", n),
        };
        if let Some(a) = after {
            r = r.query(&[("after", a)]);
        }
        let res = r.send().await?;
        if !res.status().is_success() {
            return Err(Self::read::<serde_json::Value>(res)
                .await
                .err()
                .unwrap_or(Error::Decode("watch refused".into())));
        }
        let mut parser = crate::sse::Parser::default();
        let stream = res.bytes_stream().flat_map(move |chunk| {
            let events: Vec<Result<WatchEvent>> = match chunk {
                Ok(b) => parser
                    .push(&b)
                    .into_iter()
                    .filter_map(|(event, data)| crate::sse::to_event(&event, &data))
                    .collect(),
                Err(e) => vec![Err(Error::Transport(e))],
            };
            futures_util::stream::iter(events)
        });
        Ok(stream)
    }

    // --- Reports --------------------------------------------------------------------------

    /// Reports how calls went, each with the ticket of the result it called.
    pub async fn report_usage(&self, reports: &[UsageReport]) -> Result<Accepted> {
        self.send(Method::POST, "/v1/reports/usage", &serde_json::json!({ "reports": reports })).await
    }

    /// Rates a resource, 1 to 5, with the ticket of the result it rates.
    pub async fn rate(&self, ticket: &str, rating: u8, comment: Option<&str>) -> Result<Accepted> {
        if !(1..=5).contains(&rating) {
            return Err(Error::Invalid("a rating is 1 to 5".into()));
        }
        self.send(
            Method::POST,
            "/v1/reports/feedback",
            &serde_json::json!({ "ticket": ticket, "rating": rating, "comment": comment }),
        )
        .await
    }

    /// The caller's own reputation.
    pub async fn me(&self) -> Result<Me> {
        self.get("/v1/reputation/me").await
    }

    // --- Providers ------------------------------------------------------------------------

    /// Announces once. [`crate::Provider`] keeps the lease alive.
    pub async fn announce(&self, a: &Announcement) -> Result<Announced> {
        self.send(Method::PUT, "/v1/announce", a).await
    }

    /// A heartbeat for a lease. `provider_name` is needed in open mode.
    pub async fn heartbeat(&self, lease_id: &str, hb: &Heartbeat, provider_name: Option<&str>) -> Result<()> {
        let path = with_provider(&format!("/v1/leases/{}/heartbeat", enc(lease_id)), provider_name);
        self.send::<_, serde_json::Value>(Method::POST, &path, hb).await.map(|_| ())
    }

    /// Ends a lease cleanly.
    pub async fn deregister(&self, lease_id: &str, provider_name: Option<&str>) -> Result<()> {
        self.empty(Method::DELETE, &with_provider(&format!("/v1/leases/{}", enc(lease_id)), provider_name)).await
    }

    /// Who presented a ticket, and how reputable they are (the ticket's resource must be yours).
    pub async fn verify_ticket(&self, ticket: &str) -> Result<TicketInfo> {
        self.send(Method::POST, "/v1/tickets/verify", &serde_json::json!({ "ticket": ticket })).await
    }

    /// Reports about a client that presented `ticket` to you.
    pub async fn report_client(
        &self,
        ticket: &str,
        kind: ClientReportKind,
        details: Option<&str>,
    ) -> Result<ClientReported> {
        self.send(
            Method::POST,
            "/v1/reports/client",
            &serde_json::json!({ "ticket": ticket, "kind": kind, "details": details }),
        )
        .await
    }

    /// The JSON Schemas of what the API takes.
    pub async fn schema(&self) -> Result<serde_json::Value> {
        self.get("/v1/schema").await
    }

    /// Whether the broker answers `/readyz`.
    pub async fn ready(&self) -> bool {
        matches!(self.http.get(format!("{}/readyz", self.base)).send().await, Ok(r) if r.status().is_success())
    }
}

fn with_provider(path: &str, name: Option<&str>) -> String {
    match name {
        Some(n) => format!("{path}?provider={}", enc(n)),
        None => path.to_string(),
    }
}

/// Percent-encodes a path segment or query value.
fn enc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
