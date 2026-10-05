//! Service configuration, read from the environment.

use std::fmt;
use std::net::SocketAddr;

use anyhow::{Context, bail};

/// Shortest internal token the service accepts.
pub const MIN_TOKEN_CHARS: usize = 16;

/// Default cap on one ingest batch body.
pub const DEFAULT_MAX_BATCH_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub struct Config {
    /// `ANALYTICS_LISTEN`, default `0.0.0.0:8090`.
    pub listen: SocketAddr,
    /// `CLICKHOUSE_URL`: the HTTP interface, default `http://localhost:8123`.
    pub clickhouse_url: String,
    /// `CLICKHOUSE_DATABASE`, default `pacod_gw`. Created when missing.
    pub clickhouse_database: String,
    /// `CLICKHOUSE_USER`. When unset, ClickHouse's default user is used.
    pub clickhouse_user: Option<String>,
    /// `CLICKHOUSE_PASSWORD`.
    pub clickhouse_password: Option<String>,
    /// `PGW_INTERNAL_TOKEN`: the secret shared with the core. Required.
    pub internal_token: String,
    /// `ANALYTICS_MAX_BATCH_BYTES`: largest accepted ingest body, default 64 MiB.
    pub max_batch_bytes: usize,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Builds the configuration from any source of variables. Empty values count as unset.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let get = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());

        let listen = get("ANALYTICS_LISTEN").unwrap_or_else(|| "0.0.0.0:8090".to_owned());
        let listen = listen.parse().with_context(|| format!("ANALYTICS_LISTEN is not a socket address: {listen}"))?;

        let max_batch_bytes = match get("ANALYTICS_MAX_BATCH_BYTES") {
            Some(value) => {
                value.parse().with_context(|| format!("ANALYTICS_MAX_BATCH_BYTES is not a number: {value}"))?
            }
            None => DEFAULT_MAX_BATCH_BYTES,
        };

        let config = Self {
            listen,
            clickhouse_url: get("CLICKHOUSE_URL").unwrap_or_else(|| "http://localhost:8123".to_owned()),
            clickhouse_database: get("CLICKHOUSE_DATABASE").unwrap_or_else(|| "pacod_gw".to_owned()),
            clickhouse_user: get("CLICKHOUSE_USER"),
            clickhouse_password: get("CLICKHOUSE_PASSWORD"),
            internal_token: get("PGW_INTERNAL_TOKEN").context("PGW_INTERNAL_TOKEN is required")?,
            max_batch_bytes,
        };
        config.validate()?;
        Ok(config)
    }

    /// The ClickHouse URL without any user name or password it may carry, for logs.
    pub fn clickhouse_url_for_log(&self) -> String {
        match url::Url::parse(&self.clickhouse_url) {
            Ok(mut url) if !url.username().is_empty() || url.password().is_some() => {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.to_string()
            }
            _ => self.clickhouse_url.clone(),
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.internal_token.chars().count() < MIN_TOKEN_CHARS {
            bail!("PGW_INTERNAL_TOKEN must be at least {MIN_TOKEN_CHARS} characters");
        }
        if !is_identifier(&self.clickhouse_database) {
            bail!(
                "CLICKHOUSE_DATABASE must start with a letter or underscore and contain only \
                 letters, digits and underscores (at most 63 characters)"
            );
        }
        if self.max_batch_bytes == 0 {
            bail!("ANALYTICS_MAX_BATCH_BYTES must be greater than zero");
        }
        Ok(())
    }
}

/// The database name is the one identifier that reaches SQL from configuration.
fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let starts_well = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_');
    starts_well && name.len() <= 63 && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("clickhouse_url", &self.clickhouse_url_for_log())
            .field("clickhouse_database", &self.clickhouse_database)
            .field("clickhouse_user", &self.clickhouse_user)
            .field("clickhouse_password", &self.clickhouse_password.as_ref().map(|_| "<redacted>"))
            .field("internal_token", &"<redacted>")
            .field("max_batch_bytes", &self.max_batch_bytes)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| vars.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned())
    }

    #[test]
    fn defaults_apply_when_only_the_token_is_set() {
        let config = Config::from_lookup(lookup(&[("PGW_INTERNAL_TOKEN", "0123456789abcdef")])).expect("valid");
        assert_eq!(config.listen.to_string(), "0.0.0.0:8090");
        assert_eq!(config.clickhouse_url, "http://localhost:8123");
        assert_eq!(config.clickhouse_database, "pacod_gw");
        assert_eq!(config.clickhouse_user, None);
        assert_eq!(config.clickhouse_password, None);
        assert_eq!(config.max_batch_bytes, DEFAULT_MAX_BATCH_BYTES);
    }

    #[test]
    fn the_token_is_required_and_must_be_long_enough() {
        let missing = Config::from_lookup(lookup(&[])).unwrap_err();
        assert!(missing.to_string().contains("PGW_INTERNAL_TOKEN is required"));

        let short = Config::from_lookup(lookup(&[("PGW_INTERNAL_TOKEN", "0123456789abcde")])).unwrap_err();
        assert!(short.to_string().contains("at least 16 characters"));

        let blank = Config::from_lookup(lookup(&[("PGW_INTERNAL_TOKEN", "   ")])).unwrap_err();
        assert!(blank.to_string().contains("PGW_INTERNAL_TOKEN is required"));
    }

    #[test]
    fn the_database_name_must_be_a_plain_identifier() {
        for bad in ["pacod-gw", "1abc", "a b", "a`; DROP DATABASE x", "a.b"] {
            let error = Config::from_lookup(lookup(&[
                ("PGW_INTERNAL_TOKEN", "0123456789abcdef"),
                ("CLICKHOUSE_DATABASE", bad),
            ]))
            .unwrap_err();
            assert!(error.to_string().contains("CLICKHOUSE_DATABASE"), "{bad}");
        }
        assert!(is_identifier("pacod_gw_2"));
        assert!(is_identifier("_x"));
    }

    #[test]
    fn debug_output_hides_the_secrets() {
        let config = Config::from_lookup(lookup(&[
            ("PGW_INTERNAL_TOKEN", "super-secret-token-value"),
            ("CLICKHOUSE_PASSWORD", "hunter2-hunter2"),
            ("CLICKHOUSE_URL", "https://writer:url-secret@ch.internal:8443/"),
        ]))
        .expect("valid");
        let printed = format!("{config:?}");
        assert!(!printed.contains("super-secret-token-value"));
        assert!(!printed.contains("hunter2-hunter2"));
        assert!(!printed.contains("url-secret"));
        assert_eq!(config.clickhouse_url_for_log(), "https://ch.internal:8443/");
    }
}
