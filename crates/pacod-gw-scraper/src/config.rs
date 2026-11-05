//! The scraper's configuration, read from the environment.

use std::fmt;
use std::net::SocketAddr;

use anyhow::{Context, bail};

/// Shortest internal token the scraper accepts, as the core and the analytics service do.
pub const MIN_TOKEN_CHARS: usize = 16;

#[derive(Clone)]
pub struct Config {
    /// `PGW_CORE_ADMIN_URL`: the core's management API, for example `http://pacod-gw-admin:8081`.
    /// Required: the scraper does nothing without the core.
    pub core_admin_url: String,
    /// `PGW_INTERNAL_TOKEN`: the secret shared with the core. Required.
    pub internal_token: String,
    /// `PGW_EGRESS_ALLOW_PRIVATE`: hosts, addresses and ranges that are not public and may still
    /// be fetched, separated by commas. The same list the core reads.
    pub egress_allow_private: Vec<String>,
    /// `SCRAPER_BROWSER_URL`: the browser's DevTools endpoint, default `http://gw-browser:9222`.
    pub browser_url: String,
    /// `SCRAPER_PROXY_LISTEN`: where the egress proxy the browser goes out through listens, as
    /// `host:port`, default `0.0.0.0:3128`. Compose names the alias the scraper has on the
    /// browser's network alone (`gw-scraper-proxy:3128`), so that nothing else reaches it.
    pub proxy_listen: String,
    /// `SCRAPER_PROXY_URL`: the proxy as the browser reaches it, default `http://gw-scraper-proxy:3128`.
    pub proxy_url: String,
    /// `SCRAPER_LISTEN`: `/healthz` and `/readyz`, default `0.0.0.0:8098`.
    pub listen: SocketAddr,
    /// `SCRAPER_USER_AGENT`: the user agent pages are fetched with. Default: the browser's own,
    /// with `HeadlessChrome` written as `Chrome`, and `pacod-gw-scraper/<version>` after it.
    pub user_agent: Option<String>,
    /// `SCRAPER_PAGE_TIMEOUT_SECONDS`: how long a page may take to render, default 45.
    pub page_timeout_seconds: u64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Builds the configuration from any source of variables. Empty values count as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let get = |name: &str| lookup(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let addr = |name: &str, default: &str| -> anyhow::Result<SocketAddr> {
            let v = get(name).unwrap_or_else(|| default.to_owned());
            v.parse().with_context(|| format!("{name} is not a socket address: {v}"))
        };
        let page_timeout_seconds = match get("SCRAPER_PAGE_TIMEOUT_SECONDS") {
            Some(v) => v.parse().with_context(|| format!("SCRAPER_PAGE_TIMEOUT_SECONDS is not a number: {v}"))?,
            None => 45,
        };
        let config = Self {
            core_admin_url: get("PGW_CORE_ADMIN_URL").context("PGW_CORE_ADMIN_URL is required")?,
            internal_token: get("PGW_INTERNAL_TOKEN").context("PGW_INTERNAL_TOKEN is required")?,
            egress_allow_private: get("PGW_EGRESS_ALLOW_PRIVATE")
                .map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())
                .unwrap_or_default(),
            browser_url: get("SCRAPER_BROWSER_URL").unwrap_or_else(|| "http://gw-browser:9222".to_owned()),
            proxy_listen: get("SCRAPER_PROXY_LISTEN").unwrap_or_else(|| "0.0.0.0:3128".to_owned()),
            proxy_url: get("SCRAPER_PROXY_URL").unwrap_or_else(|| "http://gw-scraper-proxy:3128".to_owned()),
            listen: addr("SCRAPER_LISTEN", "0.0.0.0:8098")?,
            user_agent: get("SCRAPER_USER_AGENT"),
            page_timeout_seconds,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.internal_token.chars().count() < MIN_TOKEN_CHARS {
            bail!("PGW_INTERNAL_TOKEN must be at least {MIN_TOKEN_CHARS} characters");
        }
        if let Err(why) = pacod_gw_events::egress::EgressPolicy::parse(&self.egress_allow_private) {
            bail!("PGW_EGRESS_ALLOW_PRIVATE: {why}");
        }
        for (name, value) in [("PGW_CORE_ADMIN_URL", &self.core_admin_url), ("SCRAPER_BROWSER_URL", &self.browser_url)]
        {
            let url = url::Url::parse(value).with_context(|| format!("{name} is not a URL: {value}"))?;
            if url.scheme() != "http" {
                bail!("{name} is an http:// address on the gateway's own networks: {value}");
            }
        }
        if self.proxy_listen.rsplit_once(':').is_none_or(|(h, p)| h.is_empty() || p.parse::<u16>().is_err()) {
            bail!("SCRAPER_PROXY_LISTEN is host:port: {}", self.proxy_listen);
        }
        if !(5..=600).contains(&self.page_timeout_seconds) {
            bail!("SCRAPER_PAGE_TIMEOUT_SECONDS must be between 5 and 600");
        }
        Ok(())
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("core_admin_url", &self.core_admin_url)
            .field("internal_token", &"<redacted>")
            .field("egress_allow_private", &self.egress_allow_private)
            .field("browser_url", &self.browser_url)
            .field("proxy_listen", &self.proxy_listen)
            .field("proxy_url", &self.proxy_url)
            .field("listen", &self.listen)
            .field("user_agent", &self.user_agent)
            .field("page_timeout_seconds", &self.page_timeout_seconds)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from(vars: &[(&str, &str)]) -> anyhow::Result<Config> {
        Config::from_lookup(|name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned()))
    }

    #[test]
    fn the_core_and_its_token_are_required_and_the_rest_has_defaults() {
        let base = [("PGW_CORE_ADMIN_URL", "http://pacod-gw-admin:8081"), ("PGW_INTERNAL_TOKEN", "0123456789abcdef")];
        let c = from(&base).unwrap();
        assert_eq!(
            (c.browser_url.as_str(), c.listen.port(), c.proxy_listen.as_str()),
            ("http://gw-browser:9222", 8098, "0.0.0.0:3128")
        );
        assert_eq!(c.page_timeout_seconds, 45);
        assert!(!format!("{c:?}").contains("0123456789abcdef"), "the token is not logged");
        assert!(from(&base[..1]).is_err());
        assert!(from(&[base[0], ("PGW_INTERNAL_TOKEN", "short")]).is_err());
        for bad in [
            ("PGW_EGRESS_ALLOW_PRIVATE", "10.0.0.0/99"),
            ("SCRAPER_BROWSER_URL", "https://gw-browser:9222"),
            ("SCRAPER_PAGE_TIMEOUT_SECONDS", "1"),
            ("SCRAPER_LISTEN", "nowhere"),
            ("SCRAPER_PROXY_LISTEN", "gw-scraper-proxy"),
        ] {
            assert!(from(&[base[0], base[1], bad]).is_err(), "{bad:?}");
        }
        let c = from(&[base[0], base[1], ("PGW_EGRESS_ALLOW_PRIVATE", "gw-mock, 10.0.0.0/8")]).unwrap();
        assert_eq!(c.egress_allow_private, ["gw-mock", "10.0.0.0/8"]);
    }
}
