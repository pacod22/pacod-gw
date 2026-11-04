//! Settings: environment variables for where things are, a YAML file for what is offered.
//!
//! | Variable | Default | What |
//! |---|---|---|
//! | `GWB_GW_URL` | `http://pacod-gw-admin:8081` | The gateway's management API |
//! | `GWB_GW_TOKEN_FILE` / `GWB_GW_TOKEN` | — | A token that reads the platform: a `pgwa_` admin token with `viewer` at the platform, or a pacod-id service token with `gw.viewer` there |
//! | `GWB_BROKER_URL` | `http://pacod-broker:8060` | The broker's public API |
//! | `GWB_BROKER_AUTH` | `oidc` | `oidc`: each organisation announces with its own token (the file); `open`: a broker in open mode |
//! | `GWB_CONSUMER_TOKEN_FILE` / `GWB_CONSUMER_TOKEN` | — | The bridge's own broker token (`broker.consumer`, and `broker.witness` from phase 2), for evidence |
//! | `GWB_PUBLIC_URL` | — (required) | The gateway's public address, which the announced interfaces point at |
//! | `GWB_OBTAIN_URL` | the public URL | Where a consumer asks for a workspace key |
//! | `GWB_CONFIG_FILE` | — (required) | The offerings, the organisations' broker tokens and the evidence links |
//! | `GWB_LISTEN` | `0.0.0.0:8097` | `/readyz` and `/metrics` |
//! | `GWB_REFRESH_SECONDS` | `600` | How often offerings are rebuilt besides on config changes |
//! | `GWB_LIVE_SECONDS` | `10` | How often live state is read from the gateway |
//! | `GWB_EVIDENCE_SECONDS` | `300` | How often calls are read and reported |
//! | `GWB_SAMPLE_PER_HOUR` | `100` | Calls reported per resource and hour, at most (the broker counts 120) |
//! | `GWB_TTL_SECONDS` | `60` | The leases' TTL |
//! | `GWB_LOG_FORMAT` | `json` | `json` or `text` (pacod-demo/docs/LOGGING.md) |

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub gw_url: String,
    pub gw_token: String,
    pub broker_url: String,
    pub broker_open: bool,
    pub consumer_token: Option<String>,
    pub public_url: String,
    pub obtain_url: String,
    pub listen: SocketAddr,
    pub refresh: Duration,
    pub live_every: Duration,
    pub evidence_every: Duration,
    pub sample_per_hour: u32,
    pub ttl_s: u64,
    pub file: BridgeFile,
}

/// The bridge's file (`GWB_CONFIG_FILE`).
///
/// ```yaml
/// organisations:
///   - organisation: acme                 # the gateway organisation's name or id
///     token_file: /run/secrets/acme-broker   # pacod-id token with broker.provider in acme
///     offer:
///       - { workspace: prod, template: legal, visibility: platform, summary: "Contract review in German" }
/// links:                                  # gw model name → broker resource id, for evidence
///   openai/gpt-4o: res_0123…
/// ```
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeFile {
    #[serde(default)]
    pub organisations: Vec<OrgOffer>,
    #[serde(default)]
    pub links: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrgOffer {
    pub organisation: String,
    /// The organisation's broker token, or a file holding it. Not needed in open mode.
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    #[serde(default)]
    pub offer: Vec<Offering>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offering {
    pub workspace: String,
    pub template: String,
    /// `organisation` (the default: the organisation's own members), `platform` or `public`.
    #[serde(default = "own")]
    pub visibility: String,
    /// The resource's name on the broker; the template's name when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// One line for consumers; the template's description when absent.
    #[serde(default)]
    pub summary: Option<String>,
}

fn own() -> String {
    "organisation".into()
}

impl OrgOffer {
    /// The organisation's broker token, read now (a mounted secret may be rotated).
    pub fn token(&self) -> anyhow::Result<Option<String>> {
        match (&self.token, &self.token_file) {
            (Some(t), _) => Ok(Some(t.trim().to_string())),
            (None, Some(f)) => Ok(Some(read_secret(f)?)),
            (None, None) => Ok(None),
        }
    }
}

impl BridgeFile {
    pub fn read(path: &Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?;
        let f: Self = serde_saphyr::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        f.check()?;
        Ok(f)
    }

    fn check(&self) -> anyhow::Result<()> {
        for o in &self.organisations {
            for x in &o.offer {
                if !matches!(x.visibility.as_str(), "organisation" | "platform" | "public") {
                    bail!(
                        "{}: {}/{}: visibility is organisation, platform or public",
                        o.organisation,
                        x.workspace,
                        x.template
                    );
                }
            }
        }
        Ok(())
    }
}

fn read_secret(path: &Path) -> anyhow::Result<String> {
    Ok(std::fs::read_to_string(path).with_context(|| format!("{}", path.display()))?.trim().to_string())
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// A secret from `NAME_FILE` or `NAME`.
fn secret(name: &str) -> anyhow::Result<Option<String>> {
    if let Some(f) = var(&format!("{name}_FILE")) {
        return Ok(Some(read_secret(Path::new(&f))?));
    }
    Ok(var(name))
}

fn seconds(name: &str, default: u64, min: u64) -> anyhow::Result<Duration> {
    let v = match var(name) {
        Some(s) => s.parse::<u64>().with_context(|| format!("{name}: a number of seconds"))?,
        None => default,
    };
    if v < min {
        bail!("{name}: at least {min}");
    }
    Ok(Duration::from_secs(v))
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let gw_token = secret("GWB_GW_TOKEN")?.context("GWB_GW_TOKEN_FILE or GWB_GW_TOKEN is needed")?;
        let broker_open = match var("GWB_BROKER_AUTH").as_deref() {
            None | Some("oidc") => false,
            Some("open") => true,
            Some(other) => bail!("GWB_BROKER_AUTH: oidc or open, not {other:?}"),
        };
        let public_url = var("GWB_PUBLIC_URL").context("GWB_PUBLIC_URL is needed")?;
        url::Url::parse(&public_url).context("GWB_PUBLIC_URL")?;
        let file_path = var("GWB_CONFIG_FILE").context("GWB_CONFIG_FILE is needed")?;
        let file = BridgeFile::read(Path::new(&file_path))?;
        if !broker_open {
            for o in &file.organisations {
                if o.token.is_none() && o.token_file.is_none() {
                    bail!("{}: a broker token is needed in oidc mode (token or token_file)", o.organisation);
                }
            }
        }
        Ok(Self {
            gw_url: var("GWB_GW_URL").unwrap_or_else(|| "http://pacod-gw-admin:8081".into()),
            gw_token,
            broker_url: var("GWB_BROKER_URL").unwrap_or_else(|| "http://pacod-broker:8060".into()),
            broker_open,
            consumer_token: secret("GWB_CONSUMER_TOKEN")?,
            obtain_url: var("GWB_OBTAIN_URL").unwrap_or_else(|| public_url.clone()),
            public_url: public_url.trim_end_matches('/').to_string(),
            listen: var("GWB_LISTEN").unwrap_or_else(|| "0.0.0.0:8097".into()).parse().context("GWB_LISTEN")?,
            refresh: seconds("GWB_REFRESH_SECONDS", 600, 10)?,
            live_every: seconds("GWB_LIVE_SECONDS", 10, 1)?,
            evidence_every: seconds("GWB_EVIDENCE_SECONDS", 300, 10)?,
            sample_per_hour: var("GWB_SAMPLE_PER_HOUR")
                .map(|v| v.parse())
                .transpose()
                .context("GWB_SAMPLE_PER_HOUR")?
                .unwrap_or(100),
            ttl_s: seconds("GWB_TTL_SECONDS", 60, 5)?.as_secs(),
            file,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_reads_and_checks_visibility() {
        let f: BridgeFile = serde_saphyr::from_str(
            "organisations:\n  - organisation: acme\n    token: pidt_x\n    offer:\n      - { workspace: prod, template: legal, visibility: platform }\nlinks:\n  openai/gpt-4o: res_1\n",
        )
        .unwrap();
        f.check().unwrap();
        assert_eq!(f.organisations[0].offer[0].visibility, "platform");
        assert_eq!(f.organisations[0].token().unwrap().as_deref(), Some("pidt_x"));
        assert_eq!(f.links["openai/gpt-4o"], "res_1");
        let bad: BridgeFile = serde_saphyr::from_str(
            "organisations:\n  - organisation: acme\n    offer:\n      - { workspace: prod, template: legal, visibility: everyone }\n",
        )
        .unwrap();
        assert!(bad.check().is_err());
    }
}
