//! Fetching what an administrator named, without the browser: price lists, `robots.txt` and
//! sitemaps. Every connection goes only where the egress policy allows, on the first request and
//! on every redirect, and a name that resolves to an internal address is not connected to.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pacod_gw_events::egress::EgressPolicy;

/// How many redirects a fetch may take.
const MAX_REDIRECTS: usize = 5;

/// Resolves host names and drops the addresses the egress policy refuses.
pub struct GuardedResolver(pub Arc<EgressPolicy>);

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
pub async fn resolve_allowed(
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

/// What a fetch answered.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub status: u16,
    /// Where the fetch ended up after redirects.
    pub final_url: String,
    pub body: Vec<u8>,
}

/// The client for everything the scraper fetches itself.
#[derive(Clone)]
pub struct Fetcher {
    client: reqwest::Client,
    policy: Arc<EgressPolicy>,
}

impl Fetcher {
    pub fn new(policy: Arc<EgressPolicy>, user_agent: &str) -> reqwest::Result<Self> {
        let on_redirect = policy.clone();
        let client = reqwest::Client::builder()
            // A public document; a slow one must not hold a run for long.
            .timeout(Duration::from_secs(45))
            .user_agent(user_agent)
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
        Ok(Self { client, policy })
    }

    pub fn policy(&self) -> &Arc<EgressPolicy> {
        &self.policy
    }

    /// Fetches `url`, at most `max_bytes` of it. Any status is an answer; only a failure to
    /// connect, or a body that is too long, is an error.
    pub async fn get(&self, url: &str, max_bytes: usize) -> anyhow::Result<Fetched> {
        self.policy.check_url(url)?;
        let mut resp = self.client.get(url).send().await?;
        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        // Read with the limit in hand: a document that never ends must not fill the memory.
        let mut body = Vec::new();
        while let Some(chunk) = resp.chunk().await? {
            if body.len() + chunk.len() > max_bytes {
                anyhow::bail!("{url} is larger than {max_bytes} bytes");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Fetched { status, final_url, body })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_allowed_addresses_are_fetched() {
        let fetcher = Fetcher::new(Arc::new(EgressPolicy::default()), "test").unwrap();
        for url in ["http://127.0.0.1:9/x", "http://169.254.169.254/latest/meta-data", "ftp://example.com/x"] {
            assert!(fetcher.get(url, 1024).await.is_err(), "{url}");
        }
        // localhost is loopback, which the policy refuses by name and by address.
        let e = format!("{:#}", fetcher.get("http://localhost:9/x", 1024).await.unwrap_err());
        assert!(e.contains("PGW_EGRESS_ALLOW_PRIVATE") || e.contains("egress policy"), "{e}");
    }

    #[tokio::test]
    async fn a_body_is_read_within_its_limit() {
        let app = axum::Router::new().route("/big", axum::routing::get(|| async { "x".repeat(4096) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let policy = EgressPolicy::parse(&["127.0.0.1".to_owned()]).unwrap();
        let fetcher = Fetcher::new(Arc::new(policy), "test").unwrap();
        let ok = fetcher.get(&format!("http://{addr}/big"), 8192).await.unwrap();
        assert_eq!((ok.status, ok.body.len()), (200, 4096));
        assert!(fetcher.get(&format!("http://{addr}/big"), 1024).await.is_err());
        assert_eq!(fetcher.get(&format!("http://{addr}/nothing"), 1024).await.unwrap().status, 404);
    }
}
