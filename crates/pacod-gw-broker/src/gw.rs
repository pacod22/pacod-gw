//! The gateway's management API, as far as the bridge reads it.
//!
//! The views take only the fields the bridge uses, each with a default, so a newer gateway with
//! more fields (or an older one with fewer) still reads.

use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, Utc};
use futures_util::{Stream, StreamExt};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

#[derive(Clone)]
pub struct Gw {
    http: reqwest::Client,
    base: String,
    token: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Named {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub description: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub current_version: u32,
    pub spec: TemplateSpec,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TemplateSpec {
    pub model: String,
    pub models: Vec<PoolEntry>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PoolEntry {
    pub model: String,
}

impl TemplateSpec {
    /// The pool's model names, in order.
    pub fn pool(&self) -> Vec<String> {
        if self.models.is_empty() {
            (!self.model.is_empty()).then(|| self.model.clone()).into_iter().collect()
        } else {
            self.models.iter().map(|m| m.model.clone()).collect()
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub state: String,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// From phase 2: the broker resource this model is.
    pub broker_resource_id: Option<String>,
}

/// One price version (`/models/{id}/prices`): the rates flattened, plus variants, tiers, windows.
#[derive(Debug, Clone, Deserialize)]
pub struct Price {
    pub version: u32,
    pub currency: String,
    pub effective_from: DateTime<Utc>,
    #[serde(default)]
    pub status: String,
    #[serde(flatten)]
    pub rest: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RoutingTarget {
    pub model: String,
    pub breaker: String,
    pub in_flight: u32,
    pub first_byte_ms: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct Page<T> {
    items: Vec<T>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RequestPage {
    pub requests: Vec<Value>,
    pub next_cursor: Option<String>,
}

impl Gw {
    pub fn new(base: &str, token: &str) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("gw-broker/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(60))
            .build()?;
        Ok(Self { http, base: base.trim_end_matches('/').to_string(), token: token.to_string() })
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> anyhow::Result<T> {
        let url = format!("{}/api/v1{path}", self.base);
        let r = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .query(query)
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = r.status();
        if !status.is_success() {
            let body = r.text().await.unwrap_or_default();
            anyhow::bail!("GET {path}: {status}: {}", body.chars().take(300).collect::<String>());
        }
        r.json().await.with_context(|| format!("GET {path}: the answer does not read"))
    }

    /// Every item of a listing, page by page.
    async fn all<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<Vec<T>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q = vec![("limit", "500".to_string())];
            if let Some(c) = &cursor {
                q.push(("cursor", c.clone()));
            }
            let page: Page<T> = self.get(path, &q).await?;
            out.extend(page.items);
            match page.next_cursor {
                Some(c) if !c.is_empty() => cursor = Some(c),
                _ => return Ok(out),
            }
        }
    }

    pub async fn organisations(&self) -> anyhow::Result<Vec<Named>> {
        self.all("/orgs").await
    }

    pub async fn workspaces(&self, org: &str) -> anyhow::Result<Vec<Named>> {
        self.all(&format!("/orgs/{org}/workspaces")).await
    }

    pub async fn templates(&self, org: &str, ws: &str) -> anyhow::Result<Vec<Template>> {
        self.all(&format!("/orgs/{org}/workspaces/{ws}/templates")).await
    }

    pub async fn models(&self) -> anyhow::Result<Vec<Model>> {
        self.all("/models").await
    }

    /// Every version of a model's price, newest first.
    pub async fn prices(&self, model_id: &str) -> anyhow::Result<Vec<Price>> {
        self.get(&format!("/models/{model_id}/prices"), &[]).await
    }

    pub async fn routing(&self) -> anyhow::Result<Vec<RoutingTarget>> {
        self.get("/monitoring/routing", &[]).await
    }

    /// Calls to `model` (a gw model name) from `from` on, newest first, a page at a time.
    pub async fn requests(
        &self,
        model: &str,
        from: DateTime<Utc>,
        cursor: Option<&str>,
    ) -> anyhow::Result<RequestPage> {
        let mut q = vec![
            ("model", model.to_string()),
            ("from", from.to_rfc3339()),
            ("to", Utc::now().to_rfc3339()),
            ("limit", "1000".to_string()),
        ];
        if let Some(c) = cursor {
            q.push(("cursor", c.to_string()));
        }
        self.get("/requests", &q).await
    }

    /// The calls that quoted a pacod-broker ticket, ticket included, from `from` on, newest
    /// first (`/broker/calls`, the broker_bridge role).
    pub async fn broker_calls(&self, from: DateTime<Utc>, cursor: Option<&str>) -> anyhow::Result<RequestPage> {
        let mut q = vec![("from", from.to_rfc3339()), ("to", Utc::now().to_rfc3339()), ("limit", "1000".to_string())];
        if let Some(c) = cursor {
            q.push(("cursor", c.to_string()));
        }
        self.get("/broker/calls", &q).await
    }

    /// The gateway's change events (`GET /api/v1/events`): yields once per `config` event. The
    /// stream ends when the connection does; the caller connects again.
    pub async fn config_events(&self) -> anyhow::Result<impl Stream<Item = ()> + use<>> {
        let url = format!("{}/api/v1/events", self.base);
        let r = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .header("accept", "text/event-stream")
            .timeout(Duration::from_secs(24 * 3600))
            .send()
            .await?
            .error_for_status()?;
        let mut buf = String::new();
        Ok(r.bytes_stream().flat_map(move |chunk| {
            let mut hits = 0usize;
            if let Ok(bytes) = chunk {
                buf.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(end) = buf.find("\n\n") {
                    let frame: String = buf.drain(..end + 2).collect();
                    if frame.lines().any(|l| l.trim() == "event: config" || l.trim() == "event:config") {
                        hits += 1;
                    }
                }
            }
            futures_util::stream::iter(std::iter::repeat_n((), hits))
        }))
    }
}
