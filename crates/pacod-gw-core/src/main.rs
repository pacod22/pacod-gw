use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use pacod_gw_core::app::App;
use pacod_gw_core::bootstrap::{Bootstrap, Role};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser)]
#[command(name = "pacod-gw-core", version, about = "AI gateway core: data plane and management API")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Run the gateway (the default).
    Serve {
        /// Which listeners to serve. Overrides PGW_ROLE and the config file.
        #[arg(long, value_enum)]
        role: Option<Role>,
    },
    /// Apply the database migrations and exit.
    Migrate,
    /// Print the OpenAPI document of the management API and exit. Needs no configuration.
    Openapi,
    /// Wrap every stored provider credential under the current key-encryption key (PGW_KEK),
    /// after a rotation. Needs the old key in PGW_KEK_OLD. Safe to run again.
    RewrapSecrets,
    /// Check the email settings: connect to the SMTP server, secure the connection and sign
    /// in, without sending anything. Needs only the PGW_SMTP_* and PGW_EMAIL_* settings.
    EmailCheck {
        /// Also send a test message to this address.
        #[arg(long)]
        send_to: Option<String>,
    },
    /// Moving the gateway's people to pacod-id (docs/runbook.md): export them, check that the
    /// gateway's own sign-in can come back, or delete the gateway's copy for good.
    Identity {
        #[command(subcommand)]
        command: IdentityCommand,
    },
}

#[derive(Subcommand)]
enum IdentityCommand {
    /// Write the users, admin tokens, role bindings and invitations (with their password and
    /// token hashes), the organisations and workspaces to a new file for `pacod-id import gw`.
    /// Keep the file secret, and delete it after the import.
    Export {
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Say whether PGW_AUTH=local is safe again: only while pacod-id recorded no identity change
    /// since the import. Run it with the core's OIDC settings (PGW_AUTH=oidc, PGW_ID_*).
    Rollback,
    /// Delete the gateway's own users, admin tokens, role bindings and invitations, which
    /// pacod-id holds now. Afterwards the core refuses PGW_AUTH=local. Needs PGW_AUTH=oidc.
    Finalize {
        /// Confirms that this is meant: it cannot be undone.
        #[arg(long)]
        yes: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Serve { role: None }) {
        Command::Openapi => {
            println!("{}", serde_json::to_string_pretty(&pacod_gw_core::mgmt::openapi_document())?);
            Ok(())
        }
        Command::EmailCheck { send_to } => {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let Some(cfg) = pacod_gw_core::bootstrap::load_email()? else {
                anyhow::bail!("email is not configured: PGW_SMTP_HOST is not set");
            };
            println!(
                "SMTP server {}:{} ({:?}), signing in as {}, sending as {}, links to {}",
                cfg.smtp_host,
                cfg.smtp_port,
                cfg.smtp_tls,
                cfg.smtp_username.as_deref().unwrap_or("nobody"),
                cfg.from_address,
                cfg.console_url
            );
            runtime(1)?.block_on(async {
                let mailer = pacod_gw_core::mail::Mailer::new(&cfg)?;
                mailer.check().await?;
                println!("ok: the server accepted the connection and the sign-in");
                if let Some(to) = send_to {
                    mailer.send_test(&to).await?;
                    println!("ok: a test message was sent to {to}");
                }
                Ok(())
            })
        }
        Command::Identity { command } => {
            pacod_gw_core::telemetry::init();
            let boot = Bootstrap::load()?;
            runtime(2)?.block_on(async {
                let store = pacod_gw_core::store::Store::connect(&boot.database_url, 2).await?;
                store.migrate().await?;
                let client = || match &boot.auth {
                    pacod_gw_core::bootstrap::AuthMode::Oidc(s) => Ok(pacod_gw_core::identity::client(s)),
                    pacod_gw_core::bootstrap::AuthMode::Local => Err(anyhow::anyhow!(
                        "this asks pacod-id: run it with the core's OIDC settings (PGW_AUTH=oidc and PGW_ID_*)"
                    )),
                };
                let said = match command {
                    IdentityCommand::Export { out } => {
                        pacod_gw_core::identity_migration::write_export(&store, &out).await?
                    }
                    IdentityCommand::Rollback => {
                        pacod_gw_core::identity_migration::rollback(&store, &client()?).await?
                    }
                    IdentityCommand::Finalize { yes } => {
                        anyhow::ensure!(yes, "finalize deletes the gateway's own users for good: add --yes to confirm");
                        pacod_gw_core::identity_migration::finalize(&store, &client()?).await?
                    }
                };
                println!("{said}");
                Ok(())
            })
        }
        Command::Migrate => {
            pacod_gw_core::telemetry::init();
            let boot = Bootstrap::load()?;
            runtime(2)?.block_on(async {
                let store = pacod_gw_core::store::Store::connect(&boot.database_url, 2).await?;
                store.migrate().await?;
                tracing::info!("migrations applied");
                Ok(())
            })
        }
        Command::RewrapSecrets => {
            pacod_gw_core::telemetry::init();
            let boot = Bootstrap::load()?;
            runtime(2)?.block_on(async {
                let store = pacod_gw_core::store::Store::connect(&boot.database_url, 2).await?;
                let current = boot.keks.first().ok_or_else(|| anyhow::anyhow!("no key-encryption key"))?;
                let behind = pacod_gw_core::mgmt::resources::provider::instances_without(&store, current).await?;
                if !behind.is_empty() {
                    anyhow::bail!(
                        "these running instances do not confirm the key-encryption key \"{}\": {}. Restart every instance with the new PGW_KEK first; they could not read the credentials otherwise",
                        current.id,
                        behind.join(", ")
                    );
                }
                let mut tx = store.begin().await?;
                let actor = pacod_gw_core::store::AuditEntry::system("rewrap-secrets");
                let report =
                    pacod_gw_core::mgmt::resources::provider::rewrap_credentials(&mut tx, &boot.keks, &actor).await?;
                tx.commit().await?;
                println!(
                    "{} credentials wrapped under key-encryption key \"{}\", {} already were",
                    report.rewrapped, report.kek_id, report.unchanged
                );
                for f in &report.failed {
                    println!("not readable: {} (key \"{}\"): {}", f.credential_id, f.kek_id, f.problem);
                }
                if report.failed.is_empty() {
                    Ok(())
                } else {
                    anyhow::bail!("{} credentials could not be read; is their key in PGW_KEK_OLD?", report.failed.len())
                }
            })
        }
        Command::Serve { role } => {
            pacod_gw_core::telemetry::init();
            let mut boot = Bootstrap::load()?;
            if let Some(r) = role {
                boot.role = r;
            }
            // The data plane has its own runtime. The management API, the database pool and
            // all background work share a small second one.
            let data = runtime(std::thread::available_parallelism().map_or(2, usize::from))?;
            let control = runtime(2)?;
            let data_handle = data.handle().clone();
            control.block_on(async move {
                let app = App::start(boot, Some(data_handle)).await?;
                wait_for_signal().await;
                tracing::info!("shutting down");
                app.shutdown().await;
                Ok::<_, anyhow::Error>(())
            })?;
            data.shutdown_timeout(std::time::Duration::from_secs(5));
            Ok(())
        }
    }
}

fn runtime(workers: usize) -> std::io::Result<tokio::runtime::Runtime> {
    // TOKIO_WORKER_THREADS, when set, still takes precedence for the data runtime in tests
    // that pin the worker count.
    let workers = std::env::var("TOKIO_WORKER_THREADS").ok().and_then(|v| v.parse().ok()).unwrap_or(workers);
    tokio::runtime::Builder::new_multi_thread().worker_threads(workers.max(1)).enable_all().build()
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
