//! Starts and stops the minimal gateway: the full gateway's data plane without its database,
//! analytics service, management API or metrics.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use pacod_gw_core::bootstrap::Bootstrap;
use pacod_gw_core::dataplane::{self, Gw, client_ip::TrustedProxies};
use pacod_gw_core::relay::CallSink;
use pacod_gw_core::store::objects::ObjectStore;
use pacod_gw_core::upstream::{EgressPolicy, Upstream};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::Config;

/// How often the batches made through this gateway are looked at.
const SETTLE_EVERY: Duration = Duration::from_secs(10);
/// How long the provider is not asked again about one batch.
const SETTLE_MIN_SECONDS: i64 = 30;

pub struct Server {
    pub addr: SocketAddr,
    pub gw: Arc<Gw>,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    drain: Duration,
    drain_delay: Duration,
}

impl Server {
    /// Builds the configuration and starts serving. `sink` receives every call; the binary
    /// passes the log ([`crate::log::LogSink`]).
    pub async fn start(cfg: &Config, sink: Arc<dyn CallSink>) -> anyhow::Result<Self> {
        pacod_gw_core::app::install_crypto_provider();
        let prepared = crate::prepare(cfg)?;

        let mut boot = Bootstrap::standalone(cfg.listen, prepared.pepper);
        boot.instance_id = "lite".into();
        boot.max_body_bytes = cfg.max_body_bytes;
        boot.max_in_flight = cfg.max_in_flight;
        boot.stream_idle_seconds = cfg.stream_idle_seconds;
        boot.drain_seconds = cfg.drain_seconds;
        boot.drain_delay_seconds = cfg.drain_delay_seconds;
        boot.egress_allow_private = cfg.egress_allow_private.clone();
        boot.trusted_proxies =
            TrustedProxies::parse(&cfg.trusted_proxies).map_err(|e| anyhow::anyhow!("PGW_TRUSTED_PROXIES: {e}"))?;
        boot.google_token_url = cfg.google_token_url.clone();
        boot.cache.memory_bytes = cfg.cache_memory_bytes;
        let egress = EgressPolicy::parse(&boot.egress_allow_private)
            .map_err(|e| anyhow::anyhow!("PGW_EGRESS_ALLOW_PRIVATE: {e}"))?;
        let upstream = Upstream::new(Arc::new(egress))?;
        let objects: Option<Arc<dyn ObjectStore>> = match &cfg.objects_dir {
            Some(dir) => Some(Arc::new(crate::objects::FileObjects::open(dir)?)),
            None => None,
        };
        let gw = Arc::new(Gw::standalone(Arc::new(boot), prepared.config, upstream, Some(sink), objects));

        let (shutdown, _) = watch::channel(false);
        let mut tasks = Vec::new();
        tasks.push(tokio::spawn(gw.limits.clone().run_local(shutdown.subscribe())));
        // The batches made through this gateway are asked about and accounted here, as the
        // full gateway's upkeep does.
        if gw.objects.is_some() {
            let (gw, mut stop) = (gw.clone(), shutdown.subscribe());
            tasks.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(SETTLE_EVERY);
                loop {
                    tokio::select! {
                        _ = tick.tick() => {
                            if let Err(e) = dataplane::objects::settle_batches(&gw, 20, SETTLE_MIN_SECONDS).await {
                                tracing::warn!(error = %e, "the batches could not be settled; tried again later");
                            }
                        }
                        _ = stop.changed() => break,
                    }
                }
            }));
        }

        let app = dataplane::router(gw.clone())
            .layer(axum::middleware::from_fn_with_state(gw.clone(), crate::log::requests))
            .into_make_service_with_connect_info::<SocketAddr>();
        let listener = TcpListener::bind(cfg.listen).await.with_context(|| format!("listening on {}", cfg.listen))?;
        let addr = listener.local_addr()?;
        let mut stop = shutdown.subscribe();
        tasks.push(tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stop.changed().await;
                })
                .await;
        }));

        tracing::info!(
            listen = %addr,
            providers = cfg.providers.len(),
            models = cfg.models.len(),
            pools = cfg.pools.len(),
            clients = cfg.clients.len(),
            version = %env!("CARGO_PKG_VERSION"),
            "pacod-gw-lite started"
        );
        Ok(Self {
            addr,
            gw,
            shutdown,
            tasks,
            drain: Duration::from_secs(cfg.drain_seconds),
            drain_delay: Duration::from_secs(cfg.drain_delay_seconds),
        })
    }

    /// Reports "not ready" for the drain delay, stops accepting requests and lets running
    /// calls finish within the drain time.
    pub async fn shutdown(self) {
        if !self.drain_delay.is_zero() {
            self.gw.draining.store(true, Ordering::Relaxed);
            tracing::info!(seconds = self.drain_delay.as_secs(), "not ready any more; serving on for the drain delay");
            tokio::time::sleep(self.drain_delay).await;
        }
        self.gw.sessions.drain(self.drain).await;
        let _ = self.shutdown.send(true);
        let all = async {
            for t in self.tasks {
                let _ = t.await;
            }
        };
        if tokio::time::timeout(self.drain + Duration::from_secs(1), all).await.is_err() {
            tracing::warn!("some calls did not finish within the drain time");
        }
    }
}
