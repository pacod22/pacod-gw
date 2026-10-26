//! The pacod-id command: the service and its operations.

use anyhow::Context;
use clap::{Parser, Subcommand};
use pacod_id::{Bootstrap, Config, Listeners, app, bootstrap, keys, store::Store};
use pacod_id_jose::Alg;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Parser)]
#[command(name = "pacod-id", version, about = "Sign-in and access for every product that uses it")]
struct Cli {
    /// The realms and clients (TOML).
    #[arg(long, env = "PID_CONFIG", default_value = "config/pacod-id.toml", global = true)]
    config: String,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Runs the service (the default).
    Serve,
    /// Brings the database schema up to date and exits.
    Migrate,
    /// Reads and checks the configuration file, then exits.
    CheckConfig,
    /// Signing keys.
    Keys {
        #[command(subcommand)]
        command: KeysCommand,
    },
    /// Client secrets.
    Clients {
        #[command(subcommand)]
        command: ClientsCommand,
    },
    /// Moves every sealed key under the current key-encryption key (PID_KEK), once every running
    /// instance has it. Then PID_KEK_OLD can go.
    Rewrap,
    /// Takes over a product's own people, tokens, roles and invitations (docs/runbook.md).
    Import {
        #[command(subcommand)]
        command: ImportCommand,
    },
}

#[derive(Subcommand)]
enum ImportCommand {
    /// The gateway's `pacod-gw-core identity export`. Prints what it did (or, with --dry-run,
    /// what it would do) as JSON; refused while there are conflicts.
    Gw {
        /// The export file.
        file: String,
        /// Report only: change nothing.
        #[arg(long)]
        dry_run: bool,
        /// The gateway's realm in PID_CONFIG.
        #[arg(long, default_value = "gateway")]
        realm: String,
    },
}

#[derive(Subcommand)]
enum KeysCommand {
    /// Lists the signing keys.
    List,
    /// Replaces the active key at once (an emergency). --revoke deletes a key from the JWKS, so
    /// tokens it signed stop verifying.
    Rotate {
        #[arg(long, default_value = "RS256", value_parser = ["RS256", "ES256"])]
        alg: String,
        #[arg(long)]
        revoke: Option<String>,
    },
}

#[derive(Subcommand)]
enum ClientsCommand {
    /// Makes a new secret for a confidential client, stores its hash and prints it once.
    Secret { client: String },
}

fn read_config(path: &str) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| format!("reading PID_CONFIG {path}"))?;
    let text = pacod_id::config::expand_env(&text, |name| std::env::var(name).ok())
        .with_context(|| format!("PID_CONFIG {path}"))?;
    Config::from_toml(&text).with_context(|| format!("PID_CONFIG {path}"))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = read_config(&cli.config)?;
    if let Some(Command::CheckConfig) = cli.command {
        println!("{}: {} realms, {} clients", cli.config, config.realms.len(), config.clients.len());
        return Ok(());
    }
    let boot = Bootstrap::from_env(&config)?;
    pacod_id::logging::init(boot.log_json);
    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let listeners = Listeners::bind(&boot)?;
            let running = app::start(boot, config, listeners).await?;
            shutdown_signal().await;
            tracing::info!("shutting down");
            running.stop().await;
        }
        Command::Migrate => {
            Store::connect(&boot.database_url).await?.migrate().await?;
            println!("the database is up to date");
        }
        Command::CheckConfig => unreachable!("handled above"),
        Command::Keys { command } => {
            let store = Store::connect(&boot.database_url).await?;
            store.migrate().await?;
            match command {
                KeysCommand::List => {
                    for k in store.keys().await? {
                        println!(
                            "{}\t{}\t{}\tactivates {}\t{}",
                            k.kid,
                            k.alg,
                            k.state,
                            k.activates_at.format("%Y-%m-%d %H:%M"),
                            k.retires_at.map(|t| format!("retires {}", t.format("%Y-%m-%d %H:%M"))).unwrap_or_default()
                        );
                    }
                }
                KeysCommand::Rotate { alg, revoke } => {
                    let alg = if alg == "ES256" { Alg::ES256 } else { Alg::RS256 };
                    let kid = keys::rotate_now(&store, boot.kek(), alg, revoke.as_deref()).await?;
                    println!("{kid} signs now; running instances take it within 30 seconds");
                }
            }
        }
        Command::Clients { command: ClientsCommand::Secret { client } } => {
            let c = config.clients.get(&client).with_context(|| format!("no client {client} in the configuration"))?;
            anyhow::ensure!(
                c.kind == pacod_id::config::ClientKind::Confidential || c.kind == pacod_id::config::ClientKind::Service,
                "client {client} is public: it has no secret"
            );
            anyhow::ensure!(
                c.secret_env.is_none(),
                "client {client} takes its secret from {}; change it there",
                c.secret_env.as_deref().unwrap_or_default()
            );
            let store = Store::connect(&boot.database_url).await?;
            store.migrate().await?;
            let secret = bootstrap::new_client_secret();
            store.client_set_secret(&client, &c.realm, &boot.token_pepper.hash(&secret)).await?;
            println!("{secret}");
            eprintln!("the secret of {client}; shown once, store it where the client reads it");
        }
        Command::Import { command: ImportCommand::Gw { file, dry_run, realm } } => {
            anyhow::ensure!(config.realms.contains_key(&realm), "PID_CONFIG has no realm {realm:?}");
            let text = std::fs::read_to_string(&file).with_context(|| format!("reading {file}"))?;
            let export: pacod_id::import::GwExport =
                serde_json::from_str(&text).with_context(|| format!("{file} is not a gateway identity export"))?;
            let store = Store::connect(&boot.database_url).await?;
            // Not migrated from here (a dry run least of all): the running service does that.
            anyhow::ensure!(
                !store.migrations_pending().await?,
                "the database is not at this version's schema yet: start the service, or run `pacod-id migrate`"
            );
            match pacod_id::import::gw(&store, &config, &realm, &export, dry_run).await {
                Ok(report) => println!("{}", serde_json::to_string_pretty(&report)?),
                Err(pacod_id::import::ImportError::Conflicts(report)) => {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                    anyhow::bail!("{} conflicts: nothing was imported", report.conflicts.len());
                }
                Err(e) => return Err(e.into()),
            }
        }
        Command::Rewrap => {
            let store = Store::connect(&boot.database_url).await?;
            let behind = app::instances_without(&store, &boot).await?;
            anyhow::ensure!(
                behind.is_empty(),
                "{} running instances do not have the current PID_KEK yet ({}); restart them with it first",
                behind.len(),
                behind.join(", ")
            );
            let moved = keys::rewrap(&store, &boot.keks).await?;
            println!("{moved} keys moved under {}", boot.kek().id);
        }
    }
    Ok(())
}
