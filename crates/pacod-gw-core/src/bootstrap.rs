//! Bootstrap values: the only configuration that is not in the database (concept section 11.1).
//!
//! Values come from an optional TOML file (`PGW_CONFIG`, default `config/gateway.toml`) and are
//! overridden by environment variables. Secrets are accepted from the environment only.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

use crate::crypto::envelope::Kek;
use crate::crypto::keys::Pepper;

/// Which listeners this process serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Data plane and management API.
    All,
    /// Data plane only.
    Gateway,
    /// Management API only.
    Admin,
}

impl Role {
    pub fn serves_data_plane(self) -> bool {
        matches!(self, Role::All | Role::Gateway)
    }

    pub fn serves_management(self) -> bool {
        matches!(self, Role::All | Role::Admin)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::All => "all",
            Role::Gateway => "gateway",
            Role::Admin => "admin",
        }
    }
}

/// What to do when the event spool is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpoolFullPolicy {
    /// Keep serving; drop and count new events.
    KeepServing,
    /// Refuse new requests until the spool drains.
    FailClosed,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileConfig {
    role: Role,
    server: ServerSection,
    database: DatabaseSection,
    analytics: AnalyticsSection,
    spool: SpoolSection,
    telemetry: TelemetrySection,
    limits: LimitsSection,
    egress: EgressSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerSection {
    listen: String,
    admin_listen: String,
    metrics_listen: String,
    drain_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DatabaseSection {
    url: String,
    max_connections: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
struct AnalyticsSection {
    url: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SpoolSection {
    dir: String,
    max_bytes: u64,
    audit_max_bytes: u64,
    full_policy: SpoolFullPolicy,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
struct TelemetrySection {
    otlp_endpoint: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LimitsSection {
    max_body_bytes: usize,
    max_in_flight: usize,
    admin_concurrency: usize,
    stream_idle_seconds: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
struct EgressSection {
    /// Hosts or CIDR ranges on private networks that providers may live on, for example a
    /// local model server. Everything else that is private, link-local or loopback is refused.
    allow_private: Vec<String>,
}

impl Default for FileConfig {
    fn default() -> Self {
        Self {
            role: Role::All,
            server: ServerSection::default(),
            database: DatabaseSection::default(),
            analytics: AnalyticsSection::default(),
            spool: SpoolSection::default(),
            telemetry: TelemetrySection::default(),
            limits: LimitsSection::default(),
            egress: EgressSection::default(),
        }
    }
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".into(),
            admin_listen: "0.0.0.0:8081".into(),
            metrics_listen: "0.0.0.0:9100".into(),
            drain_seconds: 30,
        }
    }
}

impl Default for DatabaseSection {
    fn default() -> Self {
        Self { url: String::new(), max_connections: 10 }
    }
}

impl Default for SpoolSection {
    fn default() -> Self {
        Self {
            dir: "data/spool".into(),
            max_bytes: 1 << 30,
            audit_max_bytes: 4 << 30,
            full_policy: SpoolFullPolicy::KeepServing,
        }
    }
}

impl Default for LimitsSection {
    fn default() -> Self {
        Self { max_body_bytes: 32 << 20, max_in_flight: 10_000, admin_concurrency: 64, stream_idle_seconds: 120 }
    }
}

/// The resolved bootstrap configuration.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub role: Role,
    pub data_listen: SocketAddr,
    pub admin_listen: SocketAddr,
    pub metrics_listen: SocketAddr,
    /// How long running streams may finish on shutdown.
    pub drain_seconds: u64,

    pub database_url: String,
    pub db_max_connections: u32,

    /// Base URL of the analytics service. `None` runs the minimal setup: no event shipping,
    /// no reports, no audit mode.
    pub analytics_url: Option<String>,
    /// Shared secret between core and analytics.
    pub internal_token: String,

    pub spool_dir: PathBuf,
    pub spool_max_bytes: u64,
    pub audit_spool_max_bytes: u64,
    pub spool_full_policy: SpoolFullPolicy,

    /// Key-encryption keys: the current one first, older ones after it.
    pub keks: Vec<Kek>,
    pub pepper: Pepper,

    /// The first platform admin, created when no user exists yet.
    pub admin_email: Option<String>,
    pub admin_password: Option<String>,

    pub otlp_endpoint: Option<String>,

    pub max_body_bytes: usize,
    pub max_in_flight: usize,
    pub admin_concurrency: usize,
    pub stream_idle_seconds: u64,
    pub egress_allow_private: Vec<String>,

    /// Identifies this process in events, metrics and the instance list.
    pub instance_id: String,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

impl Bootstrap {
    /// Loads the file (when present) and applies environment overrides.
    pub fn load() -> anyhow::Result<Self> {
        let path = env("PGW_CONFIG").unwrap_or_else(|| "config/gateway.toml".into());
        let file: FileConfig = match std::fs::read_to_string(&path) {
            Ok(raw) => toml::from_str(&raw).with_context(|| format!("parsing {path}"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && env("PGW_CONFIG").is_none() => FileConfig::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {path}")),
        };
        Self::resolve(file, env)
    }

    fn resolve(file: FileConfig, env: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let role = match env("PGW_ROLE") {
            Some(v) => match v.as_str() {
                "all" => Role::All,
                "gateway" => Role::Gateway,
                "admin" => Role::Admin,
                other => anyhow::bail!("PGW_ROLE must be all, gateway or admin, not \"{other}\""),
            },
            None => file.role,
        };
        let addr = |var: &str, fallback: &str| -> anyhow::Result<SocketAddr> {
            let v = env(var).unwrap_or_else(|| fallback.to_owned());
            v.parse().with_context(|| format!("{var}: \"{v}\" is not an address like 0.0.0.0:8080"))
        };

        let database_url = env("DATABASE_URL")
            .or_else(|| (!file.database.url.is_empty()).then(|| file.database.url.clone()))
            .context("DATABASE_URL is not set")?;

        let kek_current = env("PGW_KEK").context(
            "PGW_KEK is not set. Generate one with `openssl rand -base64 32` and keep it outside the database backup",
        )?;
        let mut keks = vec![
            Kek::from_base64(env("PGW_KEK_ID").unwrap_or_else(|| "default".into()), &kek_current).context("PGW_KEK")?,
        ];
        // Older keys, so that records encrypted before a rotation stay readable: "id1=base64,id2=base64".
        if let Some(old) = env("PGW_KEK_OLD") {
            for pair in old.split(',').filter(|p| !p.trim().is_empty()) {
                let (id, key) = pair.trim().split_once('=').context("PGW_KEK_OLD entries look like id=base64key")?;
                keks.push(Kek::from_base64(id, key).with_context(|| format!("PGW_KEK_OLD entry {id}"))?);
            }
        }

        let pepper = Pepper::new(
            env("PGW_KEY_PEPPER")
                .context("PGW_KEY_PEPPER is not set. Generate one with `openssl rand -base64 32`")?
                .into_bytes(),
        )
        .context("PGW_KEY_PEPPER")?;

        let analytics_url = env("PGW_ANALYTICS_URL")
            .or_else(|| (!file.analytics.url.is_empty()).then(|| file.analytics.url.clone()))
            .map(|u| u.trim_end_matches('/').to_owned());
        let internal_token = env("PGW_INTERNAL_TOKEN").unwrap_or_default();
        if analytics_url.is_some() {
            anyhow::ensure!(
                internal_token.len() >= 16,
                "PGW_INTERNAL_TOKEN must be set (at least 16 characters) when PGW_ANALYTICS_URL is set"
            );
        }

        let host = env("HOSTNAME").unwrap_or_else(|| "gw".into());
        let instance_id = env("PGW_INSTANCE_ID")
            .unwrap_or_else(|| format!("{host}-{}", hex::encode(crate::crypto::random_bytes::<3>())));

        Ok(Self {
            role,
            data_listen: addr("PGW_LISTEN", &file.server.listen)?,
            admin_listen: addr("PGW_ADMIN_LISTEN", &file.server.admin_listen)?,
            metrics_listen: addr("PGW_METRICS_LISTEN", &file.server.metrics_listen)?,
            drain_seconds: file.server.drain_seconds,
            database_url,
            db_max_connections: file.database.max_connections,
            analytics_url,
            internal_token,
            spool_dir: PathBuf::from(env("PGW_SPOOL_DIR").unwrap_or(file.spool.dir)),
            spool_max_bytes: file.spool.max_bytes,
            audit_spool_max_bytes: file.spool.audit_max_bytes,
            spool_full_policy: file.spool.full_policy,
            keks,
            pepper,
            admin_email: env("PGW_ADMIN_EMAIL"),
            admin_password: env("PGW_ADMIN_PASSWORD"),
            otlp_endpoint: env("PGW_OTLP_ENDPOINT")
                .or_else(|| (!file.telemetry.otlp_endpoint.is_empty()).then(|| file.telemetry.otlp_endpoint.clone())),
            max_body_bytes: file.limits.max_body_bytes,
            max_in_flight: file.limits.max_in_flight,
            admin_concurrency: file.limits.admin_concurrency,
            stream_idle_seconds: file.limits.stream_idle_seconds,
            egress_allow_private: env("PGW_EGRESS_ALLOW_PRIVATE")
                .map(|v| v.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect())
                .unwrap_or(file.egress.allow_private),
            instance_id,
        })
    }
}

impl Bootstrap {
    /// A configuration for tests and local tools: listeners on free loopback ports, fixed
    /// secrets, loopback providers allowed, no analytics.
    #[doc(hidden)]
    pub fn for_tests(database_url: impl Into<String>, spool_dir: impl Into<PathBuf>) -> Self {
        Self {
            role: Role::All,
            data_listen: "127.0.0.1:0".parse().expect("address"),
            admin_listen: "127.0.0.1:0".parse().expect("address"),
            metrics_listen: "127.0.0.1:0".parse().expect("address"),
            drain_seconds: 1,
            database_url: database_url.into(),
            db_max_connections: 5,
            analytics_url: None,
            internal_token: "test-internal-token-0123456789".into(),
            spool_dir: spool_dir.into(),
            spool_max_bytes: 64 << 20,
            audit_spool_max_bytes: 64 << 20,
            spool_full_policy: SpoolFullPolicy::KeepServing,
            keks: vec![Kek::new("test", [7u8; 32])],
            pepper: Pepper::new(b"test-pepper-0123456789abcdef".to_vec()).expect("long enough"),
            admin_email: Some("admin@example.test".into()),
            admin_password: Some("correct horse battery".into()),
            otlp_endpoint: None,
            max_body_bytes: 8 << 20,
            max_in_flight: 10_000,
            admin_concurrency: 64,
            stream_idle_seconds: 5,
            egress_allow_private: vec!["127.0.0.0/8".into(), "localhost".into()],
            instance_id: format!("test-{}", hex::encode(crate::crypto::random_bytes::<4>())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn resolve(file: &str, vars: &[(&str, &str)]) -> anyhow::Result<Bootstrap> {
        let file: FileConfig = toml::from_str(file)?;
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Bootstrap::resolve(file, |name| map.get(name).cloned())
    }

    const KEK: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    const BASE: &[(&str, &str)] =
        &[("DATABASE_URL", "postgres://x/y"), ("PGW_KEK", KEK), ("PGW_KEY_PEPPER", "0123456789abcdef0123")];

    #[test]
    fn defaults_match_the_documented_ports() {
        let b = resolve("", BASE).unwrap();
        assert_eq!(b.role, Role::All);
        assert_eq!(b.data_listen.port(), 8080);
        assert_eq!(b.admin_listen.port(), 8081);
        assert_eq!(b.metrics_listen.port(), 9100);
        assert!(b.analytics_url.is_none(), "the minimal setup has no analytics");
        assert_eq!(b.keks.len(), 1);
    }

    #[test]
    fn environment_overrides_the_file() {
        let mut vars = BASE.to_vec();
        vars.extend([("PGW_ROLE", "gateway"), ("PGW_LISTEN", "127.0.0.1:18080")]);
        let b = resolve("role = \"admin\"\n[server]\nlisten = \"0.0.0.0:1\"", &vars).unwrap();
        assert_eq!(b.role, Role::Gateway);
        assert_eq!(b.data_listen.to_string(), "127.0.0.1:18080");
    }

    #[test]
    fn missing_secrets_are_reported_by_name() {
        let err = resolve("", &[("DATABASE_URL", "postgres://x/y")]).unwrap_err().to_string();
        assert!(err.contains("PGW_KEK"), "{err}");
        let err = resolve("", &[("DATABASE_URL", "postgres://x/y"), ("PGW_KEK", KEK)]).unwrap_err().to_string();
        assert!(err.contains("PGW_KEY_PEPPER"), "{err}");
        let err = resolve("", &[("PGW_KEK", KEK), ("PGW_KEY_PEPPER", "0123456789abcdef0123")]).unwrap_err().to_string();
        assert!(err.contains("DATABASE_URL"), "{err}");
    }

    #[test]
    fn analytics_requires_the_internal_token() {
        let mut vars = BASE.to_vec();
        vars.push(("PGW_ANALYTICS_URL", "http://analytics:8090/"));
        assert!(resolve("", &vars).unwrap_err().to_string().contains("PGW_INTERNAL_TOKEN"));
        vars.push(("PGW_INTERNAL_TOKEN", "0123456789abcdef"));
        assert_eq!(resolve("", &vars).unwrap().analytics_url.as_deref(), Some("http://analytics:8090"));
    }

    #[test]
    fn old_keks_are_kept_for_decryption() {
        let mut vars = BASE.to_vec();
        let old = format!("2026-01={KEK}");
        vars.push(("PGW_KEK_OLD", &old));
        vars.push(("PGW_KEK_ID", "2026-10"));
        let b = resolve("", &vars).unwrap();
        assert_eq!(b.keks.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["2026-10", "2026-01"]);
    }

    #[test]
    fn unknown_file_keys_are_rejected() {
        assert!(toml::from_str::<FileConfig>("[[providers]]\nname = \"x\"").is_err());
    }
}
