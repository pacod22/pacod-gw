//! Bootstrap values: the only configuration that is not in the database (concept section 11.1).
//!
//! Values come from an optional TOML file (`PGW_CONFIG`, default `config/gateway.toml`) and are
//! overridden by environment variables. Secrets are accepted from the environment only.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

use crate::crypto::envelope::Kek;
use crate::crypto::keys::Pepper;
use crate::dataplane::client_ip::TrustedProxies;

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

/// What to do when the event spool is full. The installation's default is set here; the
/// global settings, an organisation or a workspace may override it.
pub use crate::model::settings::SpoolFullPolicy;

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
    email: EmailSection,
    cache: CacheSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ServerSection {
    listen: String,
    admin_listen: String,
    metrics_listen: String,
    drain_seconds: u64,
    /// How long an instance that is stopping keeps serving while it already reports itself
    /// as not ready, so that the load balancer stops sending it requests first.
    drain_delay_seconds: u64,
    /// Load balancers whose `X-Forwarded-For` is believed: addresses, CIDR ranges or
    /// `private_ranges`.
    trusted_proxies: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DatabaseSection {
    url: String,
    /// Default: 4 for the gateway role, 10 otherwise.
    max_connections: Option<u32>,
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

/// Where the response cache keeps answers (`docs/plugin-concept.md`, section 15). The Valkey
/// password is accepted from the environment only (`PGW_VALKEY_PASSWORD`, or a file).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct CacheSection {
    /// `redis://host:6379/0`, or `rediss://` for TLS. Empty: this instance's memory.
    valkey_url: String,
    /// The most the memory store holds, in MiB.
    memory_mb: usize,
}

impl Default for CacheSection {
    fn default() -> Self {
        Self { valkey_url: String::new(), memory_mb: 64 }
    }
}

/// The response cache's store, resolved.
#[derive(Clone, PartialEq, Eq)]
pub struct CacheConfig {
    /// Valkey, shared by every instance. `None`: each instance keeps answers in its memory.
    pub valkey_url: Option<String>,
    /// Kept out of the URL, so that the URL can be logged.
    pub valkey_password: Option<String>,
    /// The most the memory store holds, in bytes.
    pub memory_bytes: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self { valkey_url: None, valkey_password: None, memory_bytes: 64 << 20 }
    }
}

impl std::fmt::Debug for CacheConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheConfig")
            .field("valkey_url", &self.valkey_url)
            .field("valkey_password", &self.valkey_password.as_ref().map(|_| "<set>"))
            .field("memory_bytes", &self.memory_bytes)
            .finish()
    }
}

/// Outgoing email for account messages: email verification and password reset. The SMTP
/// password is accepted from the environment only (`PGW_SMTP_PASSWORD`, or a file named by
/// `PGW_SMTP_PASSWORD_FILE`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
struct EmailSection {
    smtp_host: String,
    /// Default: 587 for STARTTLS, 465 for implicit TLS, 25 without TLS.
    smtp_port: Option<u16>,
    smtp_username: String,
    smtp_tls: Option<SmtpTls>,
    smtp_timeout_seconds: Option<u64>,
    /// Default: the SMTP username.
    from_address: String,
    from_name: String,
    /// The address of the console as users reach it, for the links in the emails.
    console_url: String,
}

/// How the connection to the SMTP server is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpTls {
    /// Plain connection upgraded with STARTTLS (port 587). The upgrade is required.
    Starttls,
    /// TLS from the first byte (port 465).
    Tls,
    /// No encryption. Only for a relay on the same host or a development mail catcher.
    None,
}

impl SmtpTls {
    fn default_port(self) -> u16 {
        match self {
            SmtpTls::Starttls => 587,
            SmtpTls::Tls => 465,
            SmtpTls::None => 25,
        }
    }
}

/// The resolved email settings. Present when an SMTP host is configured.
#[derive(Clone)]
pub struct EmailConfig {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub smtp_tls: SmtpTls,
    pub smtp_timeout_seconds: u64,
    pub from_address: String,
    pub from_name: String,
    /// The console's address without a trailing slash, for example `https://console.example.com`.
    pub console_url: String,
}

impl std::fmt::Debug for EmailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailConfig")
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &self.smtp_username)
            .field("smtp_password", &self.smtp_password.as_ref().map(|_| "<set>"))
            .field("smtp_tls", &self.smtp_tls)
            .field("from_address", &self.from_address)
            .field("console_url", &self.console_url)
            .finish_non_exhaustive()
    }
}

/// Reads the email settings from the file section and the environment. `None` when no SMTP
/// host is set: the installation then sends no email.
fn resolve_email(file: EmailSection, env: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<EmailConfig>> {
    let pick = |var: &str, from_file: String| env(var).or_else(|| (!from_file.is_empty()).then_some(from_file));
    let Some(smtp_host) = pick("PGW_SMTP_HOST", file.smtp_host) else {
        return Ok(None);
    };
    let smtp_tls = match env("PGW_SMTP_TLS") {
        Some(v) => match v.to_lowercase().as_str() {
            "starttls" => SmtpTls::Starttls,
            "tls" => SmtpTls::Tls,
            "none" => SmtpTls::None,
            other => anyhow::bail!("PGW_SMTP_TLS must be starttls, tls or none, not \"{other}\""),
        },
        None => file.smtp_tls.unwrap_or(SmtpTls::Starttls),
    };
    let smtp_port = match env("PGW_SMTP_PORT") {
        Some(v) => v.parse().with_context(|| format!("PGW_SMTP_PORT: \"{v}\" is not a port number"))?,
        None => file.smtp_port.unwrap_or_else(|| smtp_tls.default_port()),
    };
    let smtp_username = pick("PGW_SMTP_USERNAME", file.smtp_username);
    let smtp_password = env("PGW_SMTP_PASSWORD");
    anyhow::ensure!(
        smtp_password.is_none() || smtp_username.is_some(),
        "PGW_SMTP_PASSWORD is set but PGW_SMTP_USERNAME is not"
    );
    let from_address = pick("PGW_EMAIL_FROM_ADDRESS", file.from_address)
        .or_else(|| smtp_username.clone().filter(|u| u.contains('@')))
        .context("PGW_EMAIL_FROM_ADDRESS is not set, and the SMTP username is not an email address")?;
    let from_name = pick("PGW_EMAIL_FROM_NAME", file.from_name).unwrap_or_else(|| "pacod-gw".into());
    // Links are built from this setting only, never from a request's Host header.
    let console_url = pick("PGW_CONSOLE_URL", file.console_url)
        .context("PGW_CONSOLE_URL must be set when email is configured: the emails link to the console")?;
    let console_url = console_url.trim_end_matches('/').to_owned();
    anyhow::ensure!(
        url::Url::parse(&console_url).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host().is_some()),
        "PGW_CONSOLE_URL must be an address such as https://console.example.com, not \"{console_url}\""
    );
    Ok(Some(EmailConfig {
        smtp_host,
        smtp_port,
        smtp_username,
        smtp_password,
        smtp_tls,
        smtp_timeout_seconds: file.smtp_timeout_seconds.unwrap_or(15).clamp(1, 120),
        from_address,
        from_name,
        console_url,
    }))
}

/// Only the email settings, for the `email-check` command, which needs no database or keys.
pub fn load_email() -> anyhow::Result<Option<EmailConfig>> {
    let path = env("PGW_CONFIG").unwrap_or_else(|| "config/gateway.toml".into());
    let file: FileConfig = match std::fs::read_to_string(&path) {
        Ok(raw) => toml::from_str(&raw).with_context(|| format!("parsing {path}"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && env("PGW_CONFIG").is_none() => FileConfig::default(),
        Err(e) => return Err(e).with_context(|| format!("reading {path}")),
    };
    resolve_email(file.email, &env)
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
            email: EmailSection::default(),
            cache: CacheSection::default(),
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
            drain_delay_seconds: 6,
            trusted_proxies: Vec::new(),
        }
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
    /// How often a management instance looks for housekeeping work (sunsets, retiring
    /// credentials, scheduled discovery). Not configurable; tests shorten it.
    /// Model discovery on its schedule. Tests switch it off so that mock providers see only
    /// the calls a test makes; a run that is asked for still happens.
    pub scheduled_discovery: bool,
    pub upkeep_seconds: u64,
    pub drain_seconds: u64,
    /// How long a stopping instance keeps serving while `/readyz` already fails.
    pub drain_delay_seconds: u64,
    /// Load balancers whose `X-Forwarded-For` is believed.
    pub trusted_proxies: TrustedProxies,

    pub database_url: String,
    /// `None`: chosen by role.
    pub db_max_connections: Option<u32>,

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
    /// Google's token endpoint for service accounts, replaced only for tests
    /// (`PGW_GOOGLE_TOKEN_URL`). `None` is <https://oauth2.googleapis.com/token>.
    pub google_token_url: Option<String>,

    /// Outgoing email for account messages. `None`: no email is sent; accounts created by an
    /// admin need no verification and forgotten passwords are reset by an admin.
    pub email: Option<EmailConfig>,

    /// Where the response cache keeps answers.
    pub cache: CacheConfig,

    /// Identifies this instance in events, metrics and the instance list. Empty until the
    /// instance starts when `PGW_INSTANCE_ID` is not set: the id kept in the spool directory
    /// is used then (`cluster::identity`).
    pub instance_id: String,
    /// The instance records its heartbeat every this many configuration polls (one second).
    pub heartbeat_every_polls: u32,

    /// Who signs people in to the management API: the core itself (`local`, sessions and
    /// passwords in its own database) or pacod-id (`oidc`).
    pub auth: AuthMode,
}

/// `PGW_AUTH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMode {
    /// The core's own accounts and sessions: the default, and the way back until
    /// `identity finalize`.
    Local,
    /// pacod-id: tokens it signs, roles from its feed.
    Oidc(IdSettings),
}

/// pacod-id as the core reaches it (`PGW_ID_*`).
#[derive(Clone, PartialEq, Eq)]
pub struct IdSettings {
    /// The realm's public issuer (`PGW_ID_ISSUER`, comma-separated for several realms): what
    /// tokens carry as `iss`.
    pub issuers: Vec<String>,
    /// pacod-id's internal listener (`PGW_ID_INTERNAL_URL`, `http://pacod-id:8071`).
    pub internal_url: String,
    /// The core's client (`PGW_ID_CLIENT_ID`, default `pacod-gw-core`) and its secret
    /// (`PGW_ID_CLIENT_SECRET`, or `_FILE`).
    pub client_id: String,
    pub client_secret: String,
    /// The audience of the tokens the management API takes (`PGW_ID_AUDIENCE`, `pacod-gw`).
    pub audience: String,
    /// The feed's age after which management calls answer 503 (`PGW_ID_MAX_STALENESS`,
    /// seconds, 60).
    pub max_staleness_seconds: u64,
}

impl std::fmt::Debug for IdSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdSettings")
            .field("issuers", &self.issuers)
            .field("internal_url", &self.internal_url)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<hidden>")
            .field("audience", &self.audience)
            .field("max_staleness_seconds", &self.max_staleness_seconds)
            .finish()
    }
}

/// `role`: an instance that serves only the data plane never calls pacod-id, so it needs no
/// client secret (the scale layout gives it none).
fn resolve_auth(env: &impl Fn(&str) -> Option<String>, role: Role) -> anyhow::Result<AuthMode> {
    match env("PGW_AUTH").as_deref() {
        None | Some("local") => Ok(AuthMode::Local),
        Some("oidc") => {
            let need = |name: &str| env(name).with_context(|| format!("PGW_AUTH=oidc needs {name}"));
            let issuers: Vec<String> = need("PGW_ID_ISSUER")?
                .split(',')
                .map(|s| s.trim().trim_end_matches('/').to_owned())
                .filter(|s| !s.is_empty())
                .collect();
            let max_staleness_seconds = match env("PGW_ID_MAX_STALENESS") {
                None => 60,
                Some(v) => v.parse().context("PGW_ID_MAX_STALENESS is a number of seconds")?,
            };
            Ok(AuthMode::Oidc(IdSettings {
                issuers,
                internal_url: need("PGW_ID_INTERNAL_URL")?.trim_end_matches('/').to_owned(),
                client_id: env("PGW_ID_CLIENT_ID").unwrap_or_else(|| "pacod-gw-core".into()),
                client_secret: if role.serves_management() {
                    need("PGW_ID_CLIENT_SECRET")?
                } else {
                    env("PGW_ID_CLIENT_SECRET").unwrap_or_default()
                },
                audience: env("PGW_ID_AUDIENCE").unwrap_or_else(|| "pacod-gw".into()),
                max_staleness_seconds,
            }))
        }
        Some(other) => anyhow::bail!("PGW_AUTH must be local or oidc, not \"{other}\""),
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// The variables that hold secrets. Each may instead come from a file, for secrets mounted as
/// files (Kubernetes, Docker): the file `NAME_FILE` names, else the file `NAME` in the
/// directory `PGW_SECRETS_DIR` names. The value is the file's content without surrounding
/// whitespace; an empty or (in the directory) missing file counts as unset.
const SECRET_VARS: &[&str] = &[
    "DATABASE_URL",
    "PGW_KEK",
    "PGW_KEK_OLD",
    "PGW_KEY_PEPPER",
    "PGW_INTERNAL_TOKEN",
    "PGW_ADMIN_PASSWORD",
    "PGW_SMTP_PASSWORD",
    "PGW_ID_CLIENT_SECRET",
    "PGW_VALKEY_PASSWORD",
];

/// A secret from a file, when `NAME` itself is unset; see [`SECRET_VARS`]. Setting both
/// `NAME` and `NAME_FILE` is refused, so that it is clear which one is used. The content never
/// appears in an error.
fn secret_file(name: &str, env: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<String>> {
    let var = format!("{name}_FILE");
    let raw = match env(&var) {
        Some(path) => {
            anyhow::ensure!(env(name).is_none(), "{name} and {var} are both set; set one of them");
            std::fs::read_to_string(&path).with_context(|| format!("{var}: reading {path}"))?
        }
        None => {
            let Some(dir) = env("PGW_SECRETS_DIR").filter(|_| env(name).is_none()) else { return Ok(None) };
            let path = std::path::Path::new(&dir).join(name);
            match std::fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e).with_context(|| format!("PGW_SECRETS_DIR: reading {}", path.display())),
            }
        }
    };
    let value = raw.trim();
    Ok((!value.is_empty()).then(|| value.to_owned()))
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

    fn resolve(file: FileConfig, raw_env: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut from_files = HashMap::new();
        for name in SECRET_VARS {
            if let Some(v) = secret_file(name, &raw_env)? {
                from_files.insert(*name, v);
            }
        }
        let env = |name: &str| raw_env(name).or_else(|| from_files.get(name).cloned());
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
        // Older keys, so that records encrypted before a rotation stay readable:
        // "id1=base64,id2=base64" (in a file, one per line also works).
        if let Some(old) = env("PGW_KEK_OLD") {
            for pair in old.split([',', '\n']).filter(|p| !p.trim().is_empty()) {
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

        // Only the management API sends email: a gateway ignores the settings, so that it can
        // share an environment file with the management instance.
        let email = if role == Role::Gateway { None } else { resolve_email(file.email, &env)? };

        let instance_id = env("PGW_INSTANCE_ID").unwrap_or_default();
        anyhow::ensure!(
            instance_id.is_empty() || crate::cluster::identity::valid_id(&instance_id),
            "PGW_INSTANCE_ID \"{instance_id}\" is not usable: use up to 64 letters, digits, '.', '_' or '-', starting with a letter or digit"
        );
        let db_max_connections = match env("PGW_DB_MAX_CONNECTIONS") {
            Some(v) => Some(
                v.parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .with_context(|| format!("PGW_DB_MAX_CONNECTIONS: \"{v}\" is not a positive number"))?,
            ),
            None => file.database.max_connections,
        };
        let drain_delay_seconds = match env("PGW_DRAIN_DELAY_SECONDS") {
            Some(v) => {
                v.parse().with_context(|| format!("PGW_DRAIN_DELAY_SECONDS: \"{v}\" is not a number of seconds"))?
            }
            None => file.server.drain_delay_seconds,
        };
        let trusted_proxies = match env("PGW_TRUSTED_PROXIES") {
            Some(v) => TrustedProxies::parse(
                &v.split(|c: char| c == ',' || c.is_whitespace()).map(str::to_owned).collect::<Vec<_>>(),
            )
            .map_err(|e| anyhow::anyhow!("PGW_TRUSTED_PROXIES: {e}"))?,
            None => TrustedProxies::parse(&file.server.trusted_proxies)
                .map_err(|e| anyhow::anyhow!("[server] trusted_proxies: {e}"))?,
        };

        let memory_mb = match env("PGW_CACHE_MEMORY_MB") {
            Some(v) => v.parse::<usize>().with_context(|| format!("PGW_CACHE_MEMORY_MB: \"{v}\" is not a number"))?,
            None => file.cache.memory_mb,
        };
        let valkey_url =
            env("PGW_VALKEY_URL").or_else(|| (!file.cache.valkey_url.is_empty()).then_some(file.cache.valkey_url));
        if let Some(u) = &valkey_url {
            anyhow::ensure!(
                u.starts_with("redis://") || u.starts_with("rediss://"),
                "PGW_VALKEY_URL must start with redis:// or rediss://"
            );
        }
        let cache =
            CacheConfig { valkey_url, valkey_password: env("PGW_VALKEY_PASSWORD"), memory_bytes: memory_mb << 20 };

        Ok(Self {
            role,
            data_listen: addr("PGW_LISTEN", &file.server.listen)?,
            admin_listen: addr("PGW_ADMIN_LISTEN", &file.server.admin_listen)?,
            metrics_listen: addr("PGW_METRICS_LISTEN", &file.server.metrics_listen)?,
            drain_seconds: file.server.drain_seconds,
            upkeep_seconds: 30,
            scheduled_discovery: true,
            drain_delay_seconds,
            trusted_proxies,
            database_url,
            db_max_connections,
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
            google_token_url: env("PGW_GOOGLE_TOKEN_URL"),
            email,
            cache,
            instance_id,
            heartbeat_every_polls: 5,
            auth: resolve_auth(&env, role)?,
        })
    }
}

impl Bootstrap {
    /// A data plane without a database (`pacod-gw-lite`): the defaults of the bootstrap file,
    /// no analytics, no traces, no email, no key-encryption keys (the caller seals its own
    /// credentials). The caller changes the public fields its own configuration sets.
    pub fn standalone(data_listen: SocketAddr, pepper: Pepper) -> Self {
        let file = FileConfig::default();
        let unused = |a: &str| a.parse().unwrap_or(data_listen);
        Self {
            role: Role::Gateway,
            data_listen,
            admin_listen: unused(&file.server.admin_listen),
            metrics_listen: unused(&file.server.metrics_listen),
            drain_seconds: file.server.drain_seconds,
            upkeep_seconds: 30,
            scheduled_discovery: false,
            drain_delay_seconds: file.server.drain_delay_seconds,
            trusted_proxies: TrustedProxies::default(),
            database_url: String::new(),
            db_max_connections: None,
            analytics_url: None,
            internal_token: String::new(),
            spool_dir: PathBuf::new(),
            spool_max_bytes: file.spool.max_bytes,
            audit_spool_max_bytes: file.spool.audit_max_bytes,
            spool_full_policy: SpoolFullPolicy::KeepServing,
            keks: Vec::new(),
            pepper,
            admin_email: None,
            admin_password: None,
            otlp_endpoint: None,
            max_body_bytes: file.limits.max_body_bytes,
            max_in_flight: file.limits.max_in_flight,
            admin_concurrency: file.limits.admin_concurrency,
            stream_idle_seconds: file.limits.stream_idle_seconds,
            egress_allow_private: file.egress.allow_private,
            google_token_url: None,
            email: None,
            cache: CacheConfig { memory_bytes: file.cache.memory_mb << 20, ..Default::default() },
            instance_id: "standalone".into(),
            heartbeat_every_polls: 5,
            auth: AuthMode::Local,
        }
    }

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
            upkeep_seconds: 1,
            scheduled_discovery: false,
            drain_delay_seconds: 0,
            trusted_proxies: TrustedProxies::default(),
            database_url: database_url.into(),
            db_max_connections: Some(5),
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
            email: None,
            cache: CacheConfig::default(),
            instance_id: format!("test-{}", hex::encode(crate::crypto::random_bytes::<4>())),
            google_token_url: None,
            heartbeat_every_polls: 1,
            auth: AuthMode::Local,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn email_is_off_without_an_smtp_host() {
        assert!(resolve("", BASE).unwrap().email.is_none());
    }

    #[test]
    fn email_needs_the_console_address_and_a_sender() {
        let mut vars = BASE.to_vec();
        vars.push(("PGW_SMTP_HOST", "smtp.example.com"));
        let err = resolve("", &vars).unwrap_err().to_string();
        assert!(err.contains("PGW_EMAIL_FROM_ADDRESS"), "{err}");
        vars.push(("PGW_SMTP_USERNAME", "mailer@example.com"));
        let err = resolve("", &vars).unwrap_err().to_string();
        assert!(err.contains("PGW_CONSOLE_URL"), "{err}");
        vars.push(("PGW_CONSOLE_URL", "console.example.com"));
        assert!(resolve("", &vars).unwrap_err().to_string().contains("PGW_CONSOLE_URL"));
        vars.pop();
        vars.extend([("PGW_CONSOLE_URL", "https://console.example.com/"), ("PGW_SMTP_PASSWORD", "secret value")]);
        let e = resolve("", &vars).unwrap().email.unwrap();
        assert_eq!(e.console_url, "https://console.example.com");
        assert_eq!((e.smtp_tls, e.smtp_port), (SmtpTls::Starttls, 587));
        assert_eq!(e.from_address, "mailer@example.com", "the username stands in for the sender");
        assert!(!format!("{e:?}").contains("secret value"), "the password is not printed");
    }

    #[test]
    fn a_gateway_ignores_the_email_settings() {
        let mut vars = BASE.to_vec();
        vars.extend([("PGW_SMTP_HOST", "smtp.example.com"), ("PGW_ROLE", "gateway")]);
        assert!(resolve("", &vars).unwrap().email.is_none(), "incomplete email settings do not stop a gateway");
        vars.pop();
        assert!(resolve("", &vars).is_err(), "they stop the management API");
    }

    #[test]
    fn email_settings_come_from_the_file_and_the_environment() {
        let file = "[email]\nsmtp_host = \"mail.internal\"\nsmtp_tls = \"tls\"\nfrom_address = \"gw@example.com\"\nconsole_url = \"http://localhost:13000\"";
        let e = resolve(file, BASE).unwrap().email.unwrap();
        assert_eq!((e.smtp_host.as_str(), e.smtp_port, e.smtp_tls), ("mail.internal", 465, SmtpTls::Tls));
        let mut vars = BASE.to_vec();
        vars.extend([("PGW_SMTP_TLS", "none"), ("PGW_SMTP_PORT", "2525")]);
        let e = resolve(file, &vars).unwrap().email.unwrap();
        assert_eq!((e.smtp_port, e.smtp_tls), (2525, SmtpTls::None));
        vars.push(("PGW_SMTP_PASSWORD", "x"));
        assert!(resolve(file, &vars).unwrap_err().to_string().contains("PGW_SMTP_USERNAME"));
    }

    #[test]
    fn cluster_settings() {
        let b = resolve("", BASE).unwrap();
        assert_eq!(b.db_max_connections, None, "chosen by role at start");
        assert_eq!(b.instance_id, "", "taken from the spool directory at start");
        assert_eq!(b.drain_delay_seconds, 6);
        assert_eq!(
            b.trusted_proxies.client_ip("10.0.0.1".parse().unwrap(), &Default::default()).to_string(),
            "10.0.0.1"
        );

        let mut vars = BASE.to_vec();
        vars.extend([
            ("PGW_DB_MAX_CONNECTIONS", "7"),
            ("PGW_INSTANCE_ID", "gateway-1"),
            ("PGW_TRUSTED_PROXIES", "private_ranges, 203.0.113.0/24"),
            ("PGW_DRAIN_DELAY_SECONDS", "0"),
        ]);
        let b = resolve("", &vars).unwrap();
        assert_eq!((b.db_max_connections, b.instance_id.as_str(), b.drain_delay_seconds), (Some(7), "gateway-1", 0));
        let mut xff = axum::http::HeaderMap::new();
        xff.insert("x-forwarded-for", "198.51.100.1".parse().unwrap());
        assert_eq!(b.trusted_proxies.client_ip("203.0.113.5".parse().unwrap(), &xff).to_string(), "198.51.100.1");

        let file = "[server]\ntrusted_proxies = [\"10.1.2.3\"]\n[database]\nmax_connections = 3";
        let b = resolve(file, BASE).unwrap();
        assert_eq!(b.db_max_connections, Some(3));
        assert_eq!(b.trusted_proxies.client_ip("10.1.2.3".parse().unwrap(), &xff).to_string(), "198.51.100.1");

        for (var, value) in
            [("PGW_DB_MAX_CONNECTIONS", "0"), ("PGW_INSTANCE_ID", "two words"), ("PGW_TRUSTED_PROXIES", "caddy")]
        {
            let mut vars = BASE.to_vec();
            vars.push((var, value));
            let err = resolve("", &vars).unwrap_err().to_string();
            assert!(err.contains(var), "{err}");
        }
    }

    #[test]
    fn unknown_file_keys_are_rejected() {
        assert!(toml::from_str::<FileConfig>("[[providers]]\nname = \"x\"").is_err());
    }

    #[test]
    fn the_identity_provider_is_chosen_with_pgw_auth() {
        assert_eq!(resolve("", BASE).unwrap().auth, AuthMode::Local);
        let oidc: Vec<(&str, &str)> = BASE
            .iter()
            .copied()
            .chain([
                ("PGW_AUTH", "oidc"),
                ("PGW_ID_ISSUER", "https://id.example.com/, https://id.other.example"),
                ("PGW_ID_INTERNAL_URL", "http://id:8071/"),
                ("PGW_ID_CLIENT_SECRET", "a-core-secret-of-the-gateway-0123456789"),
            ])
            .collect();
        let AuthMode::Oidc(s) = resolve("", &oidc).unwrap().auth else { panic!("oidc") };
        assert_eq!(s.issuers, ["https://id.example.com", "https://id.other.example"]);
        assert_eq!(
            (s.internal_url.as_str(), s.client_id.as_str(), s.audience.as_str()),
            ("http://id:8071", "pacod-gw-core", "pacod-gw")
        );
        assert_eq!(s.max_staleness_seconds, 60);
        assert!(!format!("{s:?}").contains("0123456789"), "the secret stays out of Debug");
        let missing: Vec<_> = oidc.iter().copied().filter(|(k, _)| *k != "PGW_ID_CLIENT_SECRET").collect();
        assert!(resolve("", &missing).unwrap_err().to_string().contains("PGW_ID_CLIENT_SECRET"));
        let data_plane: Vec<_> = missing.iter().copied().chain([("PGW_ROLE", "gateway")]).collect();
        assert!(matches!(resolve("", &data_plane).unwrap().auth, AuthMode::Oidc(_)), "the data plane needs no secret");
        let wrong: Vec<_> = BASE.iter().copied().chain([("PGW_AUTH", "ldap")]).collect();
        assert!(resolve("", &wrong).unwrap_err().to_string().contains("local or oidc"));
    }

    #[test]
    fn secrets_can_come_from_files() {
        let dir = std::env::temp_dir().join(format!("pgw-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, content: &str| {
            let p = dir.join(name);
            std::fs::write(&p, content).unwrap();
            p.to_string_lossy().into_owned()
        };
        let kek = write("kek", &format!("{KEK}\n"));
        let pepper = write("pepper", "0123456789abcdef0123\n");
        let old = write("old", &format!("2026-01={KEK}\n2025-01={KEK}\n"));
        let b = resolve(
            "",
            &[
                ("DATABASE_URL", "postgres://x/y"),
                ("PGW_KEK_FILE", &kek),
                ("PGW_KEY_PEPPER_FILE", &pepper),
                ("PGW_KEK_OLD_FILE", &old),
            ],
        )
        .unwrap();
        assert_eq!(b.keks.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["default", "2026-01", "2025-01"]);

        let both = resolve("", &[("DATABASE_URL", "postgres://x/y"), ("PGW_KEK", KEK), ("PGW_KEK_FILE", &kek)]);
        assert!(both.unwrap_err().to_string().contains("PGW_KEK and PGW_KEK_FILE are both set"));
        let empty = write("empty", "\n");
        let err = resolve("", &[("DATABASE_URL", "postgres://x/y"), ("PGW_KEK_FILE", &empty)]).unwrap_err().to_string();
        assert!(err.contains("PGW_KEK is not set"), "an empty file is no key: {err}");
        let missing = dir.join("missing").to_string_lossy().into_owned();
        let err = resolve("", &[("DATABASE_URL", "postgres://x/y"), ("PGW_KEK_FILE", &missing)]).unwrap_err();
        assert!(format!("{err:#}").contains("PGW_KEK_FILE: reading"), "{err:#}");

        // A mounted Secret: one file per key, optional ones may be missing.
        let mounted = dir.join("mounted");
        std::fs::create_dir_all(&mounted).unwrap();
        std::fs::write(mounted.join("PGW_KEK"), KEK).unwrap();
        std::fs::write(mounted.join("PGW_KEY_PEPPER"), "0123456789abcdef0123").unwrap();
        std::fs::write(mounted.join("DATABASE_URL"), "postgres://from/file").unwrap();
        let mounted = mounted.to_string_lossy().into_owned();
        let b = resolve("", &[("PGW_SECRETS_DIR", &mounted)]).unwrap();
        assert_eq!((b.database_url.as_str(), b.keks.len()), ("postgres://from/file", 1));
        let b = resolve("", &[("PGW_SECRETS_DIR", &mounted), ("DATABASE_URL", "postgres://x/y")]).unwrap();
        assert_eq!(b.database_url, "postgres://x/y", "the environment comes first");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
