use pacod_gw_scraper::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // RUST_LOG sets the level, SCRAPER_LOG_FORMAT the format (json, the default, or text),
    // SERVICE_NAME the service key (pacod-demo/docs/LOGGING.md).
    pacod_gw_logging::init("gw-scraper", "info", "SCRAPER_LOG_FORMAT");
    let config = Config::from_env()?;
    pacod_gw_scraper::run(config).await
}
