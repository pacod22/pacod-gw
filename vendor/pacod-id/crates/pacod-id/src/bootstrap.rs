//! What the service reads from its environment (concept 16.5). Secrets may come from files:
//! `NAME_FILE`, or the file `NAME` in `PID_SECRETS_DIR`, as in pacod-gw.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};

use anyhow::Context;
use base64::Engine;

use crate::config::Config;
use crate::crypto::Pepper;
use crate::crypto::envelope::Kek;
use crate::crypto::password::Cost;
use crate::net::TrustedProxies;

/// Variables that hold secrets and may come from files instead.
const SECRET_VARS: &[&str] = &[
    "PID_DATABASE_URL",
    "PID_KEK",
    "PID_KEK_OLD",
    "PID_TOKEN_PEPPER",
    "PID_LEGACY_PEPPERS",
    "PID_SMTP_PASSWORD",
    "PID_BOOTSTRAP_ADMIN_PASSWORD",
];

/// Everything the service needs to start, besides the configuration file's realms and clients.
#[derive(Debug, Clone)]
pub struct Bootstrap {
    pub database_url: String,
    pub public_listen: SocketAddr,
    pub internal_listen: SocketAddr,
    pub metrics_listen: SocketAddr,
    /// The current key-encryption key first, then older ones still needed to read records.
    pub keks: Vec<Kek>,
    pub token_pepper: Pepper,
    /// Peppers of products whose tokens were imported, by product (`gw` for pgwa_ tokens).
    pub legacy_peppers: BTreeMap<String, Pepper>,
    pub trusted_proxies: TrustedProxies,
    pub password_cost: Cost,
    /// Client secrets named by `secret_env` in the configuration, by client id.
    pub client_secrets: BTreeMap<String, String>,
    /// A random id for this process, for leases and the instances table.
    pub instance_id: String,
    pub log_json: bool,
    /// Where mail goes: SMTP from PID_SMTP_*, a list in memory in tests, or nowhere.
    pub mail: Option<crate::mail::Transport>,
    /// The first admin, made at start when no account has the address (PID_BOOTSTRAP_ADMIN_*).
    pub bootstrap_admin: Option<BootstrapAdmin>,
    /// Google client secrets (`google.secret_env` of a realm), by realm.
    pub google_secrets: BTreeMap<String, String>,
    /// The hosted pages' build (PID_UI_DIR; `ui/build` of the repository by default).
    pub ui_dir: std::path::PathBuf,
}

/// PID_BOOTSTRAP_ADMIN_EMAIL, _PASSWORD (or _FILE), _ROLES (default `id.admin`) and _REALM
/// (needed when there is more than one realm).
#[derive(Clone)]
pub struct BootstrapAdmin {
    pub email: String,
    pub password: Option<String>,
    pub roles: Vec<String>,
    pub realm: Option<String>,
}

impl std::fmt::Debug for BootstrapAdmin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootstrapAdmin")
            .field("email", &self.email)
            .field("password", &self.password.as_ref().map(|_| "(set)"))
            .field("roles", &self.roles)
            .field("realm", &self.realm)
            .finish()
    }
}

/// A secret from a file, when `NAME` itself is unset. Setting both `NAME` and `NAME_FILE` is
/// refused, so that it is clear which one counts. The content never appears in an error.
fn secret_file(name: &str, env: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Option<String>> {
    let var = format!("{name}_FILE");
    let raw = match env(&var) {
        Some(path) => {
            anyhow::ensure!(env(name).is_none(), "{name} and {var} are both set; set one of them");
            std::fs::read_to_string(&path).with_context(|| format!("{var}: reading {path}"))?
        }
        None => {
            let Some(dir) = env("PID_SECRETS_DIR").filter(|_| env(name).is_none()) else { return Ok(None) };
            let path = std::path::Path::new(&dir).join(name);
            match std::fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e).with_context(|| format!("PID_SECRETS_DIR: reading {}", path.display())),
            }
        }
    };
    let value = raw.trim();
    Ok((!value.is_empty()).then(|| value.to_owned()))
}

fn listen(env: &impl Fn(&str) -> Option<String>, name: &str, default: &str) -> anyhow::Result<SocketAddr> {
    let raw = env(name).unwrap_or_else(|| default.to_owned());
    raw.parse().with_context(|| format!("{name}: {raw:?} is not an address like 0.0.0.0:8070"))
}

impl Bootstrap {
    /// From the process environment.
    pub fn from_env(config: &Config) -> anyhow::Result<Self> {
        Self::resolve(config, &|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
    }

    /// From any source of variables (tests pass a map).
    pub fn resolve(config: &Config, raw: &impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut secrets = BTreeMap::new();
        let mut names: Vec<String> = SECRET_VARS.iter().map(|s| (*s).to_owned()).collect();
        names.extend(config.clients.values().filter_map(|c| c.secret_env.clone()));
        names.extend(config.realms.values().filter_map(|r| r.google.as_ref().map(|g| g.secret_env.clone())));
        for name in &names {
            if let Some(v) = secret_file(name, raw)? {
                secrets.insert(name.clone(), v);
            }
        }
        let env = |name: &str| raw(name).or_else(|| secrets.get(name).cloned());

        let database_url = env("PID_DATABASE_URL").context("PID_DATABASE_URL is not set")?;
        let kek = env("PID_KEK").context(
            "PID_KEK is not set. Generate one with `openssl rand -base64 32` and keep it outside the database backup",
        )?;
        let mut keks =
            vec![Kek::from_base64(env("PID_KEK_ID").unwrap_or_else(|| "default".into()), &kek).context("PID_KEK")?];
        if let Some(old) = env("PID_KEK_OLD") {
            for pair in old.split(',').filter(|p| !p.trim().is_empty()) {
                let (id, key) = pair.trim().split_once('=').context("PID_KEK_OLD entries look like id=base64key")?;
                keks.push(Kek::from_base64(id, key).with_context(|| format!("PID_KEK_OLD entry {id}"))?);
            }
        }
        let token_pepper = Pepper::new(
            env("PID_TOKEN_PEPPER")
                .context("PID_TOKEN_PEPPER is not set. Generate one with `openssl rand -base64 32`")?
                .into_bytes(),
        )
        .context("PID_TOKEN_PEPPER")?;
        let mut legacy_peppers = BTreeMap::new();
        if let Some(list) = env("PID_LEGACY_PEPPERS") {
            for pair in list.split(',').filter(|p| !p.trim().is_empty()) {
                let (product, pepper) = pair
                    .trim()
                    .split_once(':')
                    .context("PID_LEGACY_PEPPERS entries look like gw:<the product's pepper>")?;
                legacy_peppers.insert(
                    product.to_owned(),
                    Pepper::new(pepper.as_bytes().to_vec())
                        .with_context(|| format!("PID_LEGACY_PEPPERS entry {product}"))?,
                );
            }
        }
        let trusted_proxies = match env("PID_TRUSTED_PROXIES") {
            None => TrustedProxies::default(),
            Some(list) => TrustedProxies::parse(&list).map_err(|e| anyhow::anyhow!("PID_TRUSTED_PROXIES: {e}"))?,
        };
        let password_cost = match env("PID_ARGON2_M_KIB") {
            None => Cost::default(),
            Some(v) => Cost { m_kib: v.parse().context("PID_ARGON2_M_KIB is a number of KiB")? },
        };
        let mut client_secrets = BTreeMap::new();
        for c in config.clients.values() {
            if let Some(name) = &c.secret_env {
                let secret = env(name).with_context(|| format!("client {}: {name} (its secret) is not set", c.id))?;
                anyhow::ensure!(
                    secret.len() >= 32,
                    "client {}: the secret in {name} must have at least 32 characters",
                    c.id
                );
                client_secrets.insert(c.id.clone(), secret);
            }
        }
        let mut google_secrets = BTreeMap::new();
        for r in config.realms.values() {
            if let Some(g) = &r.google {
                let secret = env(&g.secret_env).with_context(|| {
                    format!("realm {}: {} (its Google client secret) is not set", r.name, g.secret_env)
                })?;
                google_secrets.insert(r.name.clone(), secret);
            }
        }
        let bootstrap_admin = match env("PID_BOOTSTRAP_ADMIN_EMAIL") {
            Some(email) => Some(BootstrapAdmin {
                email,
                password: env("PID_BOOTSTRAP_ADMIN_PASSWORD"),
                roles: env("PID_BOOTSTRAP_ADMIN_ROLES")
                    .unwrap_or_else(|| "id.admin".into())
                    .split(',')
                    .map(|r| r.trim().to_owned())
                    .filter(|r| !r.is_empty())
                    .collect(),
                realm: env("PID_BOOTSTRAP_ADMIN_REALM"),
            }),
            None => {
                anyhow::ensure!(
                    env("PID_BOOTSTRAP_ADMIN_PASSWORD").is_none(),
                    "PID_BOOTSTRAP_ADMIN_PASSWORD is set without PID_BOOTSTRAP_ADMIN_EMAIL"
                );
                None
            }
        };
        let mail = match env("PID_SMTP_HOST") {
            None => None,
            Some(host) => Some(crate::mail::Transport::smtp(&crate::mail::Smtp {
                host,
                port: env("PID_SMTP_PORT").map(|p| p.parse()).transpose().context("PID_SMTP_PORT is a port number")?,
                tls: match env("PID_SMTP_TLS").as_deref() {
                    None => "starttls".into(),
                    Some(t @ ("starttls" | "tls" | "none")) => t.to_owned(),
                    Some(other) => anyhow::bail!("PID_SMTP_TLS is starttls, tls or none, not {other:?}"),
                },
                username: env("PID_SMTP_USERNAME"),
                password: {
                    // A user without a password would send without signing in, and fail later.
                    anyhow::ensure!(
                        env("PID_SMTP_USERNAME").is_none() || env("PID_SMTP_PASSWORD").is_some(),
                        "PID_SMTP_USERNAME is set without PID_SMTP_PASSWORD"
                    );
                    env("PID_SMTP_PASSWORD")
                },
                from_address: env("PID_EMAIL_FROM_ADDRESS")
                    .context("PID_EMAIL_FROM_ADDRESS is needed with PID_SMTP_HOST")?,
                from_name: env("PID_EMAIL_FROM_NAME").unwrap_or_else(|| "PACOD".into()),
            })?),
        };
        Ok(Bootstrap {
            database_url,
            public_listen: listen(&env, "PID_PUBLIC_LISTEN", "0.0.0.0:8070")?,
            internal_listen: listen(&env, "PID_INTERNAL_LISTEN", "0.0.0.0:8071")?,
            metrics_listen: listen(&env, "PID_METRICS_LISTEN", "0.0.0.0:9170")?,
            keks,
            token_pepper,
            legacy_peppers,
            trusted_proxies,
            password_cost,
            client_secrets,
            instance_id: crate::crypto::tokens::new_id("ins"),
            log_json: env("PID_LOG_FORMAT").is_none_or(|f| f != "text"),
            mail,
            bootstrap_admin,
            google_secrets,
            ui_dir: env("PID_UI_DIR").unwrap_or_else(|| "ui/build".into()).into(),
        })
    }

    /// For tests: fixed secrets, listeners on any free port of 127.0.0.1.
    pub fn for_tests(database_url: &str) -> Self {
        let any = SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 0);
        Bootstrap {
            database_url: database_url.to_owned(),
            public_listen: any,
            internal_listen: any,
            metrics_listen: any,
            keks: vec![Kek::new("test", [7; 32])],
            token_pepper: Pepper::new(b"test pepper, sixteen bytes or more".to_vec()).expect("long enough"),
            legacy_peppers: BTreeMap::new(),
            trusted_proxies: TrustedProxies::default(),
            password_cost: Cost::default(),
            client_secrets: BTreeMap::new(),
            instance_id: crate::crypto::tokens::new_id("ins"),
            log_json: false,
            mail: None,
            bootstrap_admin: None,
            google_secrets: BTreeMap::new(),
            ui_dir: std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../ui/build")),
        }
    }

    /// The current key-encryption key.
    pub fn kek(&self) -> &Kek {
        &self.keks[0]
    }
}

/// A random secret for a new client, base64url: what `pacod-id clients secret` prints.
pub fn new_client_secret() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(crate::crypto::random_bytes::<32>())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEK: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn config() -> Config {
        Config::from_toml(
            r#"
[[realm]]
name = "gateway"
issuer = "http://127.0.0.1:13070"
brand = "gateway"

[[client]]
id = "console"
realm = "gateway"
kind = "confidential"
secret_env = "CONSOLE_SECRET"
redirect_uris = ["http://127.0.0.1:13000/auth/callback"]
audience = "pacod-gw"
"#,
        )
        .unwrap()
    }

    fn resolve(vars: &[(&str, &str)]) -> anyhow::Result<Bootstrap> {
        let map: BTreeMap<String, String> = vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        Bootstrap::resolve(&config(), &|n| map.get(n).cloned())
    }

    const BASE: [(&str, &str); 4] = [
        ("PID_DATABASE_URL", "postgres://x/y"),
        ("PID_KEK", KEK),
        ("PID_TOKEN_PEPPER", "0123456789abcdef0123"),
        ("CONSOLE_SECRET", "a-console-secret-of-at-least-32-characters"),
    ];

    #[test]
    fn reads_what_it_needs() {
        let b = resolve(&BASE).unwrap();
        assert_eq!(b.kek().id, "default");
        assert_eq!(b.public_listen.port(), 8070);
        assert_eq!(b.client_secrets["console"].len(), 42);
        assert!(b.log_json);
    }

    #[test]
    fn says_what_is_missing() {
        for missing in ["PID_DATABASE_URL", "PID_KEK", "PID_TOKEN_PEPPER", "CONSOLE_SECRET"] {
            let vars: Vec<_> = BASE.iter().copied().filter(|(k, _)| *k != missing).collect();
            let err = resolve(&vars).unwrap_err().to_string();
            assert!(err.contains(missing), "{missing}: {err}");
        }
    }

    #[test]
    fn mail_settings_are_checked() {
        let smtp = [("PID_SMTP_HOST", "smtp.example.com"), ("PID_EMAIL_FROM_ADDRESS", "id@example.com")];
        for (extra, needle) in [
            (vec![("PID_SMTP_TLS", "startls")], "PID_SMTP_TLS"),
            (vec![("PID_SMTP_USERNAME", "id")], "without PID_SMTP_PASSWORD"),
        ] {
            let vars: Vec<_> = BASE.iter().copied().chain(smtp).chain(extra).collect();
            let err = resolve(&vars).unwrap_err().to_string();
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }

    #[test]
    fn old_keys_and_legacy_peppers() {
        let mut vars = BASE.to_vec();
        vars.push(("PID_KEK_OLD", "2025=AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="));
        vars.push(("PID_LEGACY_PEPPERS", "gw:the-gateway-pepper-value"));
        let b = resolve(&vars).unwrap();
        assert_eq!(b.keks.iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["default", "2025"]);
        assert!(b.legacy_peppers.contains_key("gw"));
    }

    #[test]
    fn secrets_come_from_files() {
        let dir = std::env::temp_dir().join(format!("pid-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("PID_TOKEN_PEPPER"), "a pepper read from a file\n").unwrap();
        let dir_s = dir.to_string_lossy().into_owned();
        let vars: Vec<_> = BASE
            .iter()
            .copied()
            .filter(|(k, _)| *k != "PID_TOKEN_PEPPER")
            .chain([("PID_SECRETS_DIR", dir_s.as_str())])
            .collect();
        resolve(&vars).unwrap();
        let both: Vec<_> = BASE.iter().copied().chain([("PID_KEK_FILE", "/nonexistent")]).collect();
        assert!(resolve(&both).unwrap_err().to_string().contains("both set"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn short_client_secrets_are_refused() {
        let vars: Vec<_> =
            BASE.iter().map(|(k, v)| if *k == "CONSOLE_SECRET" { (*k, "short") } else { (*k, *v) }).collect();
        assert!(resolve(&vars).unwrap_err().to_string().contains("at least 32"));
    }
}
