#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG sets the level, GWB_LOG_FORMAT the format (json, the default, or text),
    // SERVICE_NAME the service key (pacod-demo/docs/LOGGING.md).
    pacod_gw_logging::init("gw-broker", "info", "GWB_LOG_FORMAT");
    let config = pacod_gw_broker::Config::from_env()?;
    pacod_gw_broker::run(config).await
}
