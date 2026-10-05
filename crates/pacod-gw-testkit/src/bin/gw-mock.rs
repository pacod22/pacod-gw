//! `gw-mock`: the scripted mock providers as a standalone server, for the development stack.

use std::net::SocketAddr;

use anyhow::Context;
use clap::Parser;
use pacod_gw_testkit::mock::MockProvider;

/// Scripted mock of the OpenAI-compatible, Anthropic, Gemini and Ollama APIs.
#[derive(Parser)]
#[command(name = "gw-mock", version)]
struct Args {
    /// The address to listen on.
    #[arg(long, env = "GW_MOCK_LISTEN", default_value = "0.0.0.0:9000")]
    listen: SocketAddr,

    /// How many captured requests to keep for `GET /__mock/requests`; older ones are dropped,
    /// and 0 captures nothing (for load tests).
    #[arg(long, env = "GW_MOCK_MAX_CAPTURED", default_value_t = 10_000)]
    max_captured: usize,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    // Logs go to stderr; stdout only carries the line that announces the address. Requests
    // are logged at debug level (`RUST_LOG=debug`).
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let mock = MockProvider::bind(args.listen).await.with_context(|| format!("could not listen on {}", args.listen))?;
    mock.set_capture_limit(args.max_captured);
    // With port 0 this line is how a caller learns the port.
    println!("gw-mock listening on {}", mock.base_url());

    shutdown_signal().await;
    tracing::info!("shutting down");
    mock.shutdown().await;
    Ok(())
}

/// Resolves on Ctrl-C or, on Unix, SIGTERM (what a container runtime sends).
async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => tokio::select! {
                () = interrupt => {}
                _ = terminate.recv() => {}
            },
            Err(_) => interrupt.await,
        }
    }
    #[cfg(not(unix))]
    interrupt.await;
}
