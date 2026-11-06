//! Fetching a source of model data. An address an administrator named in the console
//! (OpenRouter's or LiteLLM's list) is fetched only where the egress policy allows, on the first
//! request and on every redirect, and a name that resolves to an internal address is not
//! connected to. pacod-aggregator's address comes from the operator (`PGW_AGGREGATOR_URL`), is
//! usually on the gateway's own network, and is the only one the token goes to.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pacod_gw_events::egress::EgressPolicy;

/// The most a source may send: LiteLLM's list is about 3.5 MB today.
pub const MAX_BYTES: usize = 32 << 20;

/// How long a fetch may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How many redirects a fetch may take.
const MAX_REDIRECTS: usize = 5;

/// Resolves host names and drops the addresses the egress policy refuses.
struct GuardedResolver(Arc<EgressPolicy>);

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let policy = self.0.clone();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let addrs = resolve_allowed(&policy, &host, 0).await?;
            let addrs: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs)
        })
    }
}

/// The addresses `host` resolves to that the egress policy allows. An error when it resolves
/// only to refused ones.
async fn resolve_allowed(
    policy: &EgressPolicy,
    host: &str,
    port: u16,
) -> Result<Vec<SocketAddr>, Box<dyn std::error::Error + Send + Sync>> {
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port)).await?.collect();
    let allowed: Vec<SocketAddr> = resolved.iter().copied().filter(|a| policy.allows(host, a.ip())).collect();
    if allowed.is_empty() && !resolved.is_empty() {
        return Err(format!("{host} resolves to an address the egress policy does not allow").into());
    }
    Ok(allowed)
}

fn user_agent() -> String {
    format!("pacod-gw/{}", env!("CARGO_PKG_VERSION"))
}

/// The clients a sync fetches with.
#[derive(Clone)]
pub struct Fetcher {
    guarded: reqwest::Client,
    trusted: reqwest::Client,
    policy: Arc<EgressPolicy>,
}

impl Fetcher {
    pub fn new(policy: Arc<EgressPolicy>) -> reqwest::Result<Self> {
        let on_redirect = policy.clone();
        let guarded = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(user_agent())
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
            .build()?;
        // The token is not to follow a redirect to another address.
        let trusted = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .user_agent(user_agent())
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self { guarded, trusted, policy })
    }

    /// Fetches an address an administrator named, within the egress policy.
    pub async fn get_public(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        self.policy.check_url(url)?;
        read(self.guarded.get(url).send().await?, url).await
    }

    /// Fetches from pacod-aggregator, with its token.
    pub async fn get_aggregator(&self, url: &str, token: Option<&str>) -> anyhow::Result<Vec<u8>> {
        let mut req = self.trusted.get(url);
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        read(req.send().await?, url).await
    }
}

/// The body of a successful answer, at most [`MAX_BYTES`] of it.
async fn read(mut resp: reqwest::Response, url: &str) -> anyhow::Result<Vec<u8>> {
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("{url} answered {status}");
    }
    // Read with the limit in hand: a document that never ends must not fill the memory.
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if body.len() + chunk.len() > MAX_BYTES {
            anyhow::bail!("{url} sent more than {} MiB", MAX_BYTES >> 20);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
