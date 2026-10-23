//! Logging, metrics, live counters and traces (concept sections 14.7 and 14.8).

pub mod live;
pub mod metrics;
pub mod trace;

/// Sets up the log, after the pacod logging convention (`pacod_gw_logging`): `RUST_LOG` sets the
/// level, `PGW_LOG_FORMAT` the format (`json`, the default, or `text`), `SERVICE_NAME` the
/// `service` key (`pacod-gw`). Request bodies and authorization headers are never logged
/// anywhere in the core.
pub fn init() {
    // A second call (tests start several gateways in one process) keeps the first subscriber.
    pacod_gw_logging::init("pacod-gw", "info", "PGW_LOG_FORMAT");
}
