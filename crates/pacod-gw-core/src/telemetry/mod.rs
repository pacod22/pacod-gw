//! Logging, metrics, live counters and traces (concept sections 14.7 and 14.8).

pub mod live;
pub mod metrics;
pub mod trace;

use tracing_subscriber::EnvFilter;

/// Sets up structured logging. `RUST_LOG` selects the level; `PGW_LOG_FORMAT=json` selects
/// JSON lines. Request bodies and authorization headers are never logged anywhere in the core.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let json = std::env::var("PGW_LOG_FORMAT").is_ok_and(|v| v.eq_ignore_ascii_case("json"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    // A second call (tests start several gateways in one process) keeps the first subscriber.
    let _ = if json { builder.json().try_init() } else { builder.try_init() };
}
