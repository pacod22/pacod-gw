//! The pooled HTTP client used for every provider call.
//!
//! It relays bodies as bytes in both directions, keeps connections alive per provider, speaks
//! HTTP/2 where the provider does, refuses destinations the egress policy does not allow, and
//! reports when a connection was ready so that connection time can be shown separately from
//! the gateway's own delay.

use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use http::{Request, Response};
use hyper::body::Incoming;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::dns::Name;
use hyper_util::client::legacy::connect::{HttpConnector, capture_connection};
use hyper_util::rt::TokioExecutor;

use super::egress::EgressPolicy;

/// Resolves host names and drops addresses the egress policy refuses.
#[derive(Clone)]
pub struct GuardedResolver {
    policy: Arc<EgressPolicy>,
}

impl tower_service::Service<Name> for GuardedResolver {
    type Response = std::vec::IntoIter<SocketAddr>;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, name: Name) -> Self::Future {
        let policy = self.policy.clone();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let allowed: Vec<SocketAddr> = resolved.iter().copied().filter(|a| policy.allows(&host, a.ip())).collect();
            if allowed.is_empty() && !resolved.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("{host} resolves to an address the egress policy does not allow"),
                ));
            }
            Ok(allowed.into_iter())
        })
    }
}

type Connector = HttpsConnector<HttpConnector<GuardedResolver>>;

#[derive(Clone)]
pub struct Upstream {
    client: Client<Connector, Body>,
    /// HTTP/1.1 only, unpooled: the client of WebSocket handshakes. An HTTP/2 connection, which
    /// the pooled client negotiates where it can, cannot be upgraded.
    upgrades: Client<Connector, Body>,
    policy: Arc<EgressPolicy>,
}

/// What became of a request to upgrade a connection.
pub enum Upgrade {
    /// The provider switched protocols: its handshake head, and the connection.
    Switched { head: http::response::Parts, io: hyper::upgrade::Upgraded },
    /// The provider answered otherwise; its answer is passed on as it is.
    Refused(Response<Incoming>),
}

/// Why a provider call produced no response head.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// The destination is not allowed. Nothing was sent.
    #[error("{0}")]
    Egress(String),
    /// No connection could be established. Nothing was sent, so the call may be retried.
    #[error("could not connect to the provider: {0}")]
    Connect(String),
    /// No response head within the time limit. The provider may have processed the request.
    #[error("the provider did not answer within {0:?}")]
    Timeout(Duration),
    /// The connection failed after the request may have been sent.
    #[error("the provider connection failed: {0}")]
    Other(String),
}

impl SendError {
    /// True when the request certainly did not reach the provider.
    pub fn nothing_sent(&self) -> bool {
        matches!(self, SendError::Egress(_) | SendError::Connect(_))
    }
}

/// A response head together with the timing of the attempt.
pub struct Sent {
    pub response: Response<Incoming>,
    /// When a connection, pooled or new, was ready for the request.
    pub connected_at: Option<Instant>,
    /// When the response head arrived.
    pub head_at: Instant,
}

impl Upstream {
    pub fn new(policy: Arc<EgressPolicy>) -> anyhow::Result<Self> {
        let mut http = HttpConnector::new_with_resolver(GuardedResolver { policy: policy.clone() });
        http.enforce_http(false);
        http.set_nodelay(true);
        http.set_connect_timeout(Some(Duration::from_secs(10)));
        http.set_keepalive(Some(Duration::from_secs(60)));

        // The platform's trust store when it has one (this also honours private CAs installed
        // on the host), the bundled web roots otherwise.
        let builder = hyper_rustls::HttpsConnectorBuilder::new();
        let builder = match builder.with_native_roots() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "no platform trust store; using the bundled web roots");
                hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots()
            }
        };
        let upgrade_connector = builder_with_roots().https_or_http().enable_http1().wrap_connector(http.clone());
        let connector = builder.https_or_http().enable_all_versions().wrap_connector(http);
        let client = Client::builder(TokioExecutor::new())
            .pool_idle_timeout(Duration::from_secs(90))
            .pool_max_idle_per_host(512)
            .build(connector);
        let upgrades = Client::builder(TokioExecutor::new()).pool_max_idle_per_host(0).build(upgrade_connector);
        Ok(Self { client, upgrades, policy })
    }

    /// Address literals bypass the resolver, so they are checked here.
    fn check_literal(&self, uri: &http::Uri) -> Result<(), SendError> {
        if let Some(host) = uri.host() {
            let literal = host.trim_start_matches('[').trim_end_matches(']');
            if let Ok(ip) = literal.parse::<IpAddr>()
                && !self.policy.allows(literal, ip)
            {
                return Err(SendError::Egress(format!("{ip} is not an allowed destination")));
            }
        }
        Ok(())
    }

    /// Asks the provider to switch the connection to another protocol (a WebSocket), through
    /// the same egress policy as every other call, and waits at most `timeout` for its answer
    /// and the switch.
    pub async fn upgrade(&self, req: Request<Body>, timeout: Duration) -> Result<Upgrade, SendError> {
        self.check_literal(req.uri())?;
        let switch = async {
            let mut response = self.upgrades.request(req).await.map_err(|e| {
                let text = error_chain(&e);
                match (e.is_connect(), text.contains("egress policy")) {
                    (true, true) => SendError::Egress(text),
                    (true, false) => SendError::Connect(text),
                    _ => SendError::Other(text),
                }
            })?;
            if response.status() != http::StatusCode::SWITCHING_PROTOCOLS {
                return Ok(Upgrade::Refused(response));
            }
            let io = hyper::upgrade::on(&mut response).await.map_err(|e| SendError::Other(error_chain(&e)))?;
            let (head, _) = response.into_parts();
            Ok(Upgrade::Switched { head, io })
        };
        tokio::time::timeout(timeout, switch).await.map_err(|_| SendError::Timeout(timeout))?
    }

    /// Sends a request and waits for the response head, at most `timeout`.
    pub async fn send(&self, mut req: Request<Body>, timeout: Duration) -> Result<Sent, SendError> {
        self.check_literal(req.uri())?;

        let mut captured = capture_connection(&mut req);
        let fut = self.client.request(req);
        tokio::pin!(fut);
        let deadline = tokio::time::sleep(timeout);
        tokio::pin!(deadline);
        let mut connected_at = None;

        let result = loop {
            tokio::select! {
                r = &mut fut => break r,
                _ = captured.wait_for_connection_metadata(), if connected_at.is_none() => {
                    connected_at = Some(Instant::now());
                }
                _ = &mut deadline => return Err(SendError::Timeout(timeout)),
            }
        };
        match result {
            Ok(response) => Ok(Sent { response, connected_at, head_at: Instant::now() }),
            Err(e) if e.is_connect() => {
                let text = error_chain(&e);
                if text.contains("egress policy") {
                    Err(SendError::Egress(text))
                } else {
                    Err(SendError::Connect(text))
                }
            }
            Err(e) => Err(SendError::Other(error_chain(&e))),
        }
    }
}

/// The TLS connector's trust roots, as for the pooled client.
fn builder_with_roots() -> hyper_rustls::HttpsConnectorBuilder<hyper_rustls::builderstates::WantsSchemes> {
    match hyper_rustls::HttpsConnectorBuilder::new().with_native_roots() {
        Ok(b) => b,
        Err(_) => hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots(),
    }
}

/// The error with its causes, for the event and the log. Contains no request data.
fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(": ");
        out.push_str(&s.to_string());
        src = s.source();
    }
    out
}
