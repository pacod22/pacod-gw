use std::path::PathBuf;

use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use std::sync::Arc;

use pacod_gw_lite::config;
use pacod_gw_lite::env::Vars;
use pacod_gw_lite::log::LogSink;
use pacod_gw_lite::server::Server;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser)]
#[command(
    name = "pacod-gw-lite",
    version,
    about = "The gateway's data plane on its own: no database, configured by environment variables, every call logged"
)]
struct Cli {
    /// An env file to read; variables of the process win over it.
    #[arg(long, global = true, env = "PGW_ENV_FILE")]
    env_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the gateway (the default).
    Serve,
    /// Read the configuration and say what it holds, without any secret. Exits with 1 when
    /// the gateway would refuse to start.
    Check,
    /// Print a new client key for PGW_CLIENT_<NAME>_KEY.
    Keygen {
        /// The client's name: the label of its variables in lower case, for example `pacod`.
        name: String,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli.command.unwrap_or(Command::Serve), cli.env_file) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(command: Command, env_file: Option<PathBuf>) -> anyhow::Result<()> {
    match command {
        Command::Serve => {
            let vars = Vars::load(env_file.as_deref())?;
            let cfg = match config::read(&vars) {
                Ok(cfg) => cfg,
                // Said before logging is set up: the log format may be among the mistakes.
                Err(e) => anyhow::bail!("{e}"),
            };
            pacod_gw_lite::log::init(cfg.log_format, vars.get("RUST_LOG"));
            for w in &cfg.warnings {
                tracing::warn!("{w}");
            }
            let workers = std::thread::available_parallelism().map_or(2, usize::from);
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(workers).enable_all().build()?;
            runtime.block_on(async {
                let server = Server::start(&cfg, Arc::new(LogSink)).await?;
                wait_for_signal().await;
                tracing::info!("shutting down");
                server.shutdown().await;
                Ok::<_, anyhow::Error>(())
            })?;
            runtime.shutdown_timeout(std::time::Duration::from_secs(5));
            Ok(())
        }
        Command::Check => {
            let vars = Vars::load(env_file.as_deref())?;
            let cfg = config::read(&vars)?;
            // Building the snapshot finds what the variables alone do not show.
            pacod_gw_core::app::install_crypto_provider();
            pacod_gw_lite::prepare(&cfg)?;
            for line in config::summary(&cfg) {
                println!("{line}");
            }
            println!("ok");
            Ok(())
        }
        Command::Keygen { name } => {
            anyhow::ensure!(
                !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
                "the name is the client's label in lower case: letters and digits only"
            );
            let key = pacod_gw_core::crypto::keys::generate(pacod_gw_core::crypto::keys::API_KEY_PREFIX, &name);
            println!("PGW_CLIENT_{}_KEY={}", name.to_ascii_uppercase(), key.full);
            Ok(())
        }
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
