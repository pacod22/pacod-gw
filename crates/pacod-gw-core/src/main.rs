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
    /// Check the email settings: connect to the SMTP server, secure the connection and sign
    /// in, without sending anything. Needs only the PGW_SMTP_* and PGW_EMAIL_* settings.
    EmailCheck {
        /// Also send a test message to this address.
        #[arg(long)]
        send_to: Option<String>,
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
