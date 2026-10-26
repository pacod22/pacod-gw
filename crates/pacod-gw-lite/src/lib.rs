//! The minimal gateway: the data plane of `pacod-gw-core` in one process, without a database,
//! configured by environment variables, with every call logged (docs/lite.md).

pub mod config;
pub mod env;
pub mod log;
pub mod objects;
pub mod rows;
pub mod server;

use std::sync::Arc;

use pacod_gw_core::crypto::envelope::Kek;
use pacod_gw_core::crypto::keys::Pepper;
use pacod_gw_core::crypto::random_bytes;
use pacod_gw_core::snapshot::ConfigState;

/// The configuration in the form the data plane reads, with the pepper its client keys are
/// hashed with. Both secrets are new for every process.
pub struct Prepared {
    pub config: Arc<ConfigState>,
    pub pepper: Pepper,
}

/// Builds the snapshot. A configuration object the core cannot use stops the start: without a
/// database there is nobody to fix it at run time.
pub fn prepare(cfg: &config::Config) -> anyhow::Result<Prepared> {
    let kek = Kek::new("lite", random_bytes());
    let pepper = Pepper::new(random_bytes::<32>().to_vec())?;
    let rows = rows::rows(cfg, &kek, &pepper)?;
    let config = ConfigState::from_rows(rows, vec![kek]);
    let problems = &config.snapshot().problems;
    anyhow::ensure!(problems.is_empty(), "the configuration cannot be used:\n  - {}", problems.join("\n  - "));
    Ok(Prepared { config, pepper })
}
