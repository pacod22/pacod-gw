use std::path::PathBuf;

use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use pacod_gw_lite::config;
use pacod_gw_lite::env::Vars;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser)]
#[command(
    name = "pacod-gw-lite",
    version,
    about = "The gateway's data plane on its own: no database, configured by environment variables, every call logged"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Read the configuration and say what it holds, without any secret. Exits with 1 when
    /// the gateway would refuse to start.
    Check {
        /// An env file to read; variables of the process win over it.
        #[arg(long, env = "PGW_ENV_FILE")]
        env_file: Option<PathBuf>,
    },
    /// Print a new client key for PGW_CLIENT_<NAME>_KEY.
    Keygen {
        /// The client's name: the label of its variables in lower case, for example `pacod`.
        name: String,
    },
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    match run(cli.command) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Check { env_file } => {
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
