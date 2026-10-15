//! The agent's settings: where it listens, the gateway it calls by default, the token the
//! tester must present, and the most it accepts.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use ring::digest;

use crate::contract::Caps;

/// A shorter token is refused: it is the only thing that keeps others from making load.
pub const MIN_TOKEN_LEN: usize = 32;

/// Settings that are not secret, from the command line or the environment.
#[derive(Debug, Clone)]
pub struct Settings {
    pub listen: SocketAddr,
    pub name: String,
    /// The gateway calls go to when a plan names no `base_url`.
    pub target: url::Url,
    pub caps: Caps,
    /// PEM certificates to trust besides the system's, for a gateway behind a private CA.
    pub ca_file: Option<PathBuf>,
}

/// Everything the agent runs with.
pub struct AgentConfig {
    pub settings: Settings,
    /// The CA file's content.
    pub ca_pem: Option<Vec<u8>>,
    /// Only the token's digest is kept: requests are checked against it.
    pub token_digest: [u8; 32],
    pub cpus: u32,
}

impl AgentConfig {
    pub fn new(settings: Settings, token: &str) -> anyhow::Result<AgentConfig> {
        anyhow::ensure!(
            token.chars().count() >= MIN_TOKEN_LEN,
            "LOAD_AGENT_TOKEN must be at least {MIN_TOKEN_LEN} characters"
        );
        check_target(&settings.target).map_err(|e| anyhow::anyhow!("LOAD_AGENT_TARGET_URL: {e}"))?;
        let caps = &settings.caps;
        anyhow::ensure!(caps.max_concurrency > 0, "LOAD_AGENT_MAX_CONCURRENCY must be at least 1");
        anyhow::ensure!(caps.max_rate.is_finite() && caps.max_rate > 0.0, "LOAD_AGENT_MAX_RATE must be above 0");
        anyhow::ensure!(caps.max_duration_s > 0, "LOAD_AGENT_MAX_DURATION must be at least 1");
        anyhow::ensure!(caps.max_body_bytes > 0, "LOAD_AGENT_MAX_BODY_BYTES must be at least 1");
        let ca_pem = match &settings.ca_file {
            Some(path) => {
                let pem =
                    std::fs::read(path).with_context(|| format!("LOAD_AGENT_CA_FILE: reading {}", path.display()))?;
                reqwest::Certificate::from_pem_bundle(&pem).context("LOAD_AGENT_CA_FILE holds no PEM certificate")?;
                Some(pem)
            }
            None => None,
        };
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get() as u32);
        Ok(AgentConfig { settings, ca_pem, token_digest: token_digest(token), cpus })
    }
}

pub fn token_digest(token: &str) -> [u8; 32] {
    let d = digest::digest(&digest::SHA256, token.as_bytes());
    let mut out = [0; 32];
    out.copy_from_slice(d.as_ref());
    out
}

/// The token from `LOAD_AGENT_TOKEN`, or from the file `LOAD_AGENT_TOKEN_FILE` names (a
/// mounted secret). Both set is refused, so that it is clear which one is used. The token is
/// never a command-line argument, where `ps` would show it.
pub fn token_from_env(env: &impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let set = |name: &str| env(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
    let token = match (set("LOAD_AGENT_TOKEN"), set("LOAD_AGENT_TOKEN_FILE")) {
        (Some(_), Some(_)) => anyhow::bail!("LOAD_AGENT_TOKEN and LOAD_AGENT_TOKEN_FILE are both set; set one of them"),
        (Some(token), None) => token,
        (None, Some(path)) => std::fs::read_to_string(&path)
            .with_context(|| format!("LOAD_AGENT_TOKEN_FILE: reading {path}"))?
            .trim()
            .to_owned(),
        (None, None) => anyhow::bail!("LOAD_AGENT_TOKEN (or LOAD_AGENT_TOKEN_FILE) is not set"),
    };
    anyhow::ensure!(!token.is_empty(), "the token file is empty");
    Ok(token)
}

/// A gateway URL: http(s), a host, no user name or password (they would end up in logs), no
/// query or fragment (the plan's path brings its own). A path is kept: calls go below it.
pub fn check_target(url: &url::Url) -> Result<(), String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{url} is not an http or https URL"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(format!("{url} has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the URL may not hold a user name or password".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("the URL may not hold a query or a fragment".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            listen: "127.0.0.1:0".parse().unwrap(),
            name: "local".into(),
            target: url::Url::parse("http://core:8080").unwrap(),
            caps: Caps { max_concurrency: 100, max_rate: 100.0, max_duration_s: 60, max_body_bytes: 1024 },
            ca_file: None,
        }
    }

    #[test]
    fn a_short_token_is_refused() {
        assert!(AgentConfig::new(settings(), "short").is_err());
        assert!(AgentConfig::new(settings(), &"x".repeat(32)).is_ok());
    }

    #[test]
    fn the_token_comes_from_one_place() {
        let dir = std::env::temp_dir().join(format!("pgw-load-token-{}", std::process::id()));
        std::fs::write(&dir, "  from-a-file-0123456789012345678901\n").unwrap();
        let path = dir.to_string_lossy().into_owned();
        let only_file = |name: &str| (name == "LOAD_AGENT_TOKEN_FILE").then(|| path.clone());
        assert_eq!(token_from_env(&only_file).unwrap(), "from-a-file-0123456789012345678901");
        let both = |name: &str| Some(if name.ends_with("_FILE") { path.clone() } else { "x".into() });
        assert!(token_from_env(&both).unwrap_err().to_string().contains("both set"));
        assert!(token_from_env(&|_: &str| None).is_err());
        std::fs::remove_file(dir).unwrap();
    }

    #[test]
    fn targets_are_plain_http_urls() {
        let ok = |s: &str| check_target(&url::Url::parse(s).unwrap());
        assert!(ok("https://api.example.com").is_ok());
        assert!(ok("http://10.0.0.7:14080/base").is_ok());
        assert!(ok("ftp://example.com").is_err());
        assert!(ok("http://user:pw@example.com").is_err());
        assert!(ok("http://example.com/?a=1").is_err());
        assert!(ok("http://example.com/#x").is_err());
    }
}
