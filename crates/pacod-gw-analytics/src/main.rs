use pacod_gw_analytics::Config;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_logging();
    let config = Config::from_env()?;
    pacod_gw_analytics::run(config).await
}

/// `RUST_LOG` sets the level (default `info`). `ANALYTICS_LOG_FORMAT=json` writes one JSON
/// object per line instead of text.
fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if std::env::var("ANALYTICS_LOG_FORMAT").is_ok_and(|format| format == "json") {
        builder.json().init();
    } else {
        builder.init();
    }
}
