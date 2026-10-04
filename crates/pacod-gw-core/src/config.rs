//! Gateway configuration, loaded from `config/gateway.toml` (override with `PACOD_GW_CONFIG`).

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub server: ServerConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub listen: String,
}

impl Config {
    pub fn load() -> anyhow::Result<Self> {
        let path =
            std::env::var("PACOD_GW_CONFIG").unwrap_or_else(|_| "config/gateway.toml".into());
        let raw = std::fs::read_to_string(&path)?;
        Ok(toml::from_str(&raw)?)
    }
}
