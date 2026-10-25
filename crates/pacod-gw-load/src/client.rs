//! One call, timed: the HTTP client and what a call measured.

use std::time::{Duration, Instant};

use anyhow::Context;
use bytes::Bytes;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::contract::{AuthHeader, HttpVersion, RequestSpec};

/// How long a connection may take to open. Longer is not a slow gateway but a network fault,
/// and counts as `connect`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The client for one run. `pool` is how many idle connections per host it keeps: the run's
/// peak, so that calls reuse connections instead of opening new ones under load.
pub fn build(
    base: &url::Url,
    http: HttpVersion,
    pool: usize,
    ca_pem: Option<&[u8]>,
) -> anyhow::Result<reqwest::Client> {
    let mut b = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_nodelay(true)
        .pool_max_idle_per_host(pool.max(1))
        // A gateway answers a load test itself; a redirect would measure something else.
        .redirect(reqwest::redirect::Policy::none());
    b = match http {
        HttpVersion::Http1 => b.http1_only(),
        // Without TLS there is no negotiation, so HTTP/2 has to be assumed.
        HttpVersion::Http2 if base.scheme() == "http" => b.http2_prior_knowledge(),
        HttpVersion::Http2 => b,
    };
    if let Some(pem) = ca_pem {
        for cert in reqwest::Certificate::from_pem_bundle(pem).context("the CA file holds no PEM certificate")? {
            b = b.add_root_certificate(cert);
        }
    }
    Ok(b.build()?)
}

/// The request every call of a run sends, built once.
#[derive(Clone)]
pub struct Ready {
    pub method: reqwest::Method,
    pub url: url::Url,
    pub headers: HeaderMap,
    pub body: Bytes,
}

impl Ready {
    /// `base` plus the spec's path, with the key in its header.
    pub fn new(base: &url::Url, spec: &RequestSpec, auth: AuthHeader, key: &str) -> anyhow::Result<Ready> {
        let method = reqwest::Method::from_bytes(spec.method.as_bytes()).context("the method is not valid")?;
        let url = join(base, &spec.path)?;
        let mut headers = HeaderMap::new();
        for (name, value) in &spec.headers {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes())
                    .with_context(|| format!("header name {name:?} is not valid"))?,
                HeaderValue::from_str(value).with_context(|| format!("the value of header {name} is not valid"))?,
            );
        }
        // A load test measures the gateway and its providers, not the response cache, which
        // would answer every repetition of the same request. A spec can ask for it by name.
        if !headers.contains_key("x-gw-cache") {
            headers.insert("x-gw-cache", HeaderValue::from_static("off"));
        }
        if !key.is_empty() {
            let mut value = HeaderValue::from_str(&auth.value(key)).context("the key is not a valid header value")?;
            value.set_sensitive(true);
            headers.insert(auth.name(), value);
        }
        Ok(Ready { method, url, headers, body: Bytes::from(spec.body.clone()) })
    }
}

/// `base` (which may have a path) followed by `path` (which starts with `/` and may have a
/// query). `Url::join` would drop the base's path.
pub fn join(base: &url::Url, path: &str) -> anyhow::Result<url::Url> {
    let text = format!("{}{}", base.as_str().trim_end_matches('/'), path);
    url::Url::parse(&text).with_context(|| format!("{text} is not a valid URL"))
}

/// Why a call has no complete answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Connect,
    Timeout,
    Body,
    Other,
}

/// What one call measured.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CallOutcome {
    pub status: Option<u16>,
    /// Until the first byte of the body (the whole call for an empty body).
    pub ttfb_us: u64,
    pub total_us: u64,
    /// The gateway's own `x-gw-delay-ms`, in microseconds.
    pub gw_delay_us: Option<u64>,
    pub bytes: u64,
    pub error: Option<ErrorKind>,
    /// The start of the body, kept only when asked for (the probe).
    pub excerpt: Vec<u8>,
}

impl CallOutcome {
    /// An answer with a success status, read to its end.
    pub fn ok(&self) -> bool {
        self.error.is_none() && self.status.is_some_and(|s| (200..300).contains(&s))
    }
}

fn kind(e: &reqwest::Error) -> ErrorKind {
    if e.is_timeout() {
        ErrorKind::Timeout
    } else if e.is_connect() {
        ErrorKind::Connect
    } else if e.is_body() || e.is_decode() {
        ErrorKind::Body
    } else {
        ErrorKind::Other
    }
}

/// Makes one call and reads the answer to its end without keeping it. `excerpt` is how many
/// bytes of the body to keep.
pub async fn call(http: &reqwest::Client, ready: &Ready, timeout: Duration, excerpt: usize) -> CallOutcome {
    let started = Instant::now();
    let mut out = CallOutcome::default();
    // The call writes into `out` as it goes, so that what it learnt before a timeout (the
    // status, the first byte) is kept.
    let finished = tokio::time::timeout(timeout, exchange(http, ready, started, excerpt, &mut out)).await;
    out.total_us = started.elapsed().as_micros() as u64;
    match finished {
        Err(_) => out.error = Some(ErrorKind::Timeout),
        Ok(Err(kind)) => out.error = Some(kind),
        Ok(Ok(())) => {}
    }
    if out.ttfb_us == 0 {
        out.ttfb_us = out.total_us;
    }
    out
}

async fn exchange(
    http: &reqwest::Client,
    ready: &Ready,
    started: Instant,
    excerpt: usize,
    out: &mut CallOutcome,
) -> Result<(), ErrorKind> {
    let request = http.request(ready.method.clone(), ready.url.clone()).headers(ready.headers.clone());
    let request = if ready.body.is_empty() { request } else { request.body(ready.body.clone()) };
    let mut resp = request.send().await.map_err(|e| kind(&e))?;
    out.status = Some(resp.status().as_u16());
    out.gw_delay_us = resp
        .headers()
        .get("x-gw-delay-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
        .map(|ms| (ms * 1000.0).round() as u64);
    while let Some(chunk) = resp.chunk().await.map_err(|e| kind(&e))? {
        if out.bytes == 0 && !chunk.is_empty() {
            out.ttfb_us = started.elapsed().as_micros() as u64;
        }
        out.bytes += chunk.len() as u64;
        if out.excerpt.len() < excerpt {
            let take = (excerpt - out.excerpt.len()).min(chunk.len());
            out.excerpt.extend_from_slice(&chunk[..take]);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_keeps_the_base_path_and_the_query() {
        let base = url::Url::parse("http://lite:8080/prefix/").unwrap();
        assert_eq!(join(&base, "/v1/chat?x=1").unwrap().as_str(), "http://lite:8080/prefix/v1/chat?x=1");
        let bare = url::Url::parse("https://api.example.com").unwrap();
        assert_eq!(join(&bare, "/v1/messages").unwrap().as_str(), "https://api.example.com/v1/messages");
    }

    #[test]
    fn the_key_goes_in_the_chosen_header_and_is_sensitive() {
        let base = url::Url::parse("http://core:8080").unwrap();
        let spec = RequestSpec {
            method: "POST".into(),
            path: "/v1/messages".into(),
            headers: [("content-type".to_owned(), "application/json".to_owned())].into(),
            body: "{}".into(),
            stream: false,
        };
        let r = Ready::new(&base, &spec, AuthHeader::XApiKey, "pgw_k_secret").unwrap();
        assert!(r.headers["x-api-key"].is_sensitive());
        assert_eq!(r.headers["x-api-key"], "pgw_k_secret");
        let r = Ready::new(&base, &spec, AuthHeader::Authorization, "pgw_k_secret").unwrap();
        assert_eq!(r.headers["authorization"], "Bearer pgw_k_secret");
    }

    #[test]
    fn the_response_cache_is_bypassed_unless_the_spec_asks_for_it() {
        let base = url::Url::parse("http://core:8080").unwrap();
        let mut spec = RequestSpec {
            method: "POST".into(),
            path: "/v1/messages".into(),
            headers: Default::default(),
            body: "{}".into(),
            stream: false,
        };
        assert_eq!(Ready::new(&base, &spec, AuthHeader::XApiKey, "k").unwrap().headers["x-gw-cache"], "off");
        spec.headers.insert("x-gw-cache".into(), "on".into());
        assert_eq!(Ready::new(&base, &spec, AuthHeader::XApiKey, "k").unwrap().headers["x-gw-cache"], "on");
    }
}
