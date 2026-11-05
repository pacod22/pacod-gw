//! gw-scraper, the price tracker's scraper. It asks the core for the work of the daily check
//! and the weekly scan, renders the providers' pages in a headless browser, fetches their price
//! lists, and reports what it found. The core decides what is read and what happens with it, and
//! has the pages read by its extraction model: this service never holds a provider key.

pub mod check;
pub mod config;
pub mod feeds;
pub mod fetch;
pub mod render;
pub mod runner;
pub mod scan;
pub mod text;

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use pacod_gw_events::egress::EgressPolicy;
use pacod_gw_events::jobs::BrowserStatus;
use tokio::sync::watch;

pub use config::Config;

use crate::render::{BoxFuture, Render, Rendered};

/// A renderer for a scraper without a browser: every page fails, and says why.
pub struct NoBrowser;

impl Render for NoBrowser {
    fn render<'a>(&'a self, _url: &'a str) -> BoxFuture<'a, anyhow::Result<Rendered>> {
        Box::pin(async { anyhow::bail!("this scraper has no browser to render pages in") })
    }

    fn status(&self) -> BoxFuture<'_, BrowserStatus> {
        Box::pin(async {
            BrowserStatus { ok: false, product: String::new(), message: "no browser is configured".into() }
        })
    }
}

/// The user agent the scraper fetches with when none is configured.
pub fn default_user_agent() -> String {
    concat!("pacod-gw-scraper/", env!("CARGO_PKG_VERSION")).to_owned()
}

async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Ready while the browser answers: without it the scraper can read feeds only.
async fn readyz(State(render): State<Arc<dyn Render>>) -> (StatusCode, String) {
    let status = render.status().await;
    if status.ok { (StatusCode::OK, status.product) } else { (StatusCode::SERVICE_UNAVAILABLE, status.message) }
}

/// Runs the scraper until SIGTERM or Ctrl-C.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let policy = EgressPolicy::parse(&config.egress_allow_private)
        .map_err(|why| anyhow::anyhow!("PGW_EGRESS_ALLOW_PRIVATE: {why}"))?;
    let policy = Arc::new(policy);
    let user_agent = config.user_agent.clone().unwrap_or_else(default_user_agent);
    let fetch = fetch::Fetcher::new(policy.clone(), &user_agent)?;
    let core = runner::Core::new(&config.core_admin_url, &config.internal_token)?;
    let render: Arc<dyn Render> = Arc::new(NoBrowser);
    let runner = Arc::new(runner::Runner::new(core, fetch, render.clone()));

    let (stop, stopped) = watch::channel(false);
    let health = axum::Router::new().route("/healthz", get(healthz)).route("/readyz", get(readyz)).with_state(render);
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(addr = %listener.local_addr()?, core = %config.core_admin_url, holder = %runner.holder, "pacod-gw-scraper listening");
    let mut health_stop = stopped.clone();
    let served = tokio::spawn(async move {
        axum::serve(listener, health)
            .with_graceful_shutdown(async move {
                let _ = health_stop.changed().await;
            })
            .await
    });
    let work = tokio::spawn(runner.run_forever(stopped));
    shutdown_signal().await;
    let _ = stop.send(true);
    let _ = work.await;
    served.await??;
    tracing::info!("pacod-gw-scraper stopped");
    Ok(())
}

async fn shutdown_signal() {
    let interrupt = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = interrupt => {}
        _ = terminate => {}
    }
}
