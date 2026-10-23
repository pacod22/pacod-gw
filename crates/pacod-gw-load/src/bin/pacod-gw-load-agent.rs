//! `pacod-gw-load-agent`: makes the load of a load test for the tester, in a container of its
//! own, in the stack or on another machine. See `pacod_gw_load::agent` and the crate README.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use mimalloc::MiMalloc;
use pacod_gw_load::agent::config::{AgentConfig, Settings, token_from_env};
use pacod_gw_load::agent::{Agent, router, spawn_lease_keeper};
use pacod_gw_load::contract::Caps;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

/// How long a run stopped at shutdown may take to wind down.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(10);

/// Makes load for pacod-gw's tester. The token is read from LOAD_AGENT_TOKEN or the file
/// LOAD_AGENT_TOKEN_FILE names, never from the command line.
#[derive(Parser)]
#[command(name = "pacod-gw-load-agent", version)]
struct Args {
    /// The address to listen on for the tester.
    #[arg(long, env = "LOAD_AGENT_LISTEN", default_value = "0.0.0.0:8096")]
    listen: SocketAddr,
    /// The name the console shows.
    #[arg(long, env = "LOAD_AGENT_NAME", default_value = "agent")]
    name: String,
    /// The gateway as this agent reaches it, for runs against "this gateway".
    #[arg(long, env = "LOAD_AGENT_TARGET_URL")]
    target: url::Url,
    #[arg(long, env = "LOAD_AGENT_MAX_CONCURRENCY", default_value_t = 10_000)]
    max_concurrency: u32,
    /// Calls a second, in rate mode.
    #[arg(long, env = "LOAD_AGENT_MAX_RATE", default_value_t = 5_000.0)]
    max_rate: f64,
    /// Seconds.
    #[arg(long, env = "LOAD_AGENT_MAX_DURATION", default_value_t = 3_600)]
    max_duration: u64,
    #[arg(long, env = "LOAD_AGENT_MAX_BODY_BYTES", default_value_t = 1_048_576)]
    max_body_bytes: u64,
    /// PEM certificates to trust besides the system's (a gateway behind a private CA). Empty:
    /// none; Compose passes an unset variable as an empty one.
    #[arg(long, env = "LOAD_AGENT_CA_FILE", default_value = "")]
    ca_file: String,
    /// json (the default) or text, in any case; anything else is json.
    #[arg(long, env = "LOAD_AGENT_LOG_FORMAT")]
    log_format: Option<String>,
}

fn main() -> std::process::ExitCode {
    match run(Args::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pacod-gw-load-agent: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> anyhow::Result<()> {
    let token = token_from_env(&|name| std::env::var(name).ok())?;
    let settings = Settings {
        listen: args.listen,
        name: args.name,
        target: args.target,
        caps: Caps {
            max_concurrency: args.max_concurrency,
            max_rate: args.max_rate,
            max_duration_s: args.max_duration,
            max_body_bytes: args.max_body_bytes,
        },
        ca_file: (!args.ca_file.is_empty()).then(|| PathBuf::from(args.ca_file)),
    };
    let cfg = AgentConfig::new(settings, &token)?;
    drop(token);
    init_log(args.log_format.as_deref());
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(serve(cfg))
}

/// The log of the pacod logging convention: RUST_LOG sets the level, the format comes from
/// LOAD_AGENT_LOG_FORMAT or --log-format, SERVICE_NAME names the agent's service.
fn init_log(format: Option<&str>) {
    let var = |name: &str| std::env::var(name).ok();
    let settings = pacod_gw_logging::Settings::new("gw-load-agent", "info")
        .service(var(pacod_gw_logging::SERVICE_VAR).as_deref())
        .level(var(pacod_gw_logging::LEVEL_VAR).as_deref())
        .format("LOAD_AGENT_LOG_FORMAT", format);
    pacod_gw_logging::install(settings, std::io::stdout);
}

async fn serve(cfg: AgentConfig) -> anyhow::Result<()> {
    let s = &cfg.settings;
    tracing::info!(
        name = %s.name,
        listen = %s.listen,
        target = %s.target,
        max_concurrency = s.caps.max_concurrency,
        max_rate = s.caps.max_rate,
        max_duration_s = s.caps.max_duration_s,
        cpus = cfg.cpus,
        "load agent starting"
    );
    let listener = tokio::net::TcpListener::bind(s.listen).await.with_context(|| format!("binding {}", s.listen))?;
    let agent = Agent::new(cfg);
    let keeper = spawn_lease_keeper(agent.clone());
    axum::serve(listener, router(agent.clone())).with_graceful_shutdown(signal()).await?;
    keeper.abort();
    // A run still going is stopped, and given a moment so its last second is counted.
    if let Some(live) = agent.stop_all() {
        let until = tokio::time::Instant::now() + SHUTDOWN_WAIT;
        while !live.state().0.finished() && tokio::time::Instant::now() < until {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    tracing::info!("load agent stopped");
    Ok(())
}

async fn signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = term => {}
    }
}
