//! Assembles a running gateway from the bootstrap values: listeners, snapshot, spool and the
//! background tasks. Tests start a gateway through the same function.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use axum::routing::get;
use tokio::net::TcpListener;
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::bootstrap::{Bootstrap, SpoolFullPolicy};
use crate::dataplane::{self, Gw};
use crate::limits::LimitEngine;
use crate::mgmt::{self, AdminState};
use crate::snapshot::{ConfigState, InstanceInfo};
use crate::spool::{Shipper, ShipperConfig, Spool, SpoolConfig};
use crate::store::Store;
use crate::telemetry::live::LiveStats;
use crate::telemetry::metrics;
use crate::telemetry::trace::Tracer;
use crate::upstream::{EgressPolicy, Upstream};

/// A running gateway.
pub struct App {
    /// Where the data plane listens, when this role serves it.
    pub data_addr: Option<SocketAddr>,
    /// Where the management API listens, when this role serves it.
    pub admin_addr: Option<SocketAddr>,
    pub metrics_addr: SocketAddr,
    pub gw: Arc<Gw>,
    pub store: Store,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    drain: Duration,
}

/// Installs the process-wide TLS provider. Several libraries in the binary use rustls; naming
/// the provider once avoids an ambiguity error at the first TLS handshake.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn open_spool(
    boot: &Bootstrap,
    kind: &'static str,
    max_bytes: u64,
    path: &str,
    shutdown: &watch::Sender<bool>,
    tasks: &mut Vec<JoinHandle<()>>,
) -> anyhow::Result<Spool> {
    let Some(base) = &boot.analytics_url else { return Ok(Spool::disabled()) };
    let cfg =
        SpoolConfig::new(boot.spool_dir.join(kind), max_bytes, boot.spool_full_policy == SpoolFullPolicy::FailClosed);
    let (spool, reader) =
        Spool::open(cfg).with_context(|| format!("opening the {kind} spool in {}", boot.spool_dir.display()))?;
    let shipper = Shipper::new(
        ShipperConfig {
            url: format!("{base}{path}"),
            token: boot.internal_token.clone(),
            instance_id: boot.instance_id.clone(),
            batch_records: 1000,
            batch_bytes: 4 << 20,
        },
        reader,
    )?;
    tasks.push(tokio::spawn(shipper.run(shutdown.subscribe())));
    Ok(spool)
}

impl App {
    /// Starts a gateway. The management API and all background work run on the runtime this
    /// is called from; the data plane is served on `data_runtime` when one is given, so that
    /// an expensive admin query cannot slow request handling.
    pub async fn start(boot: Bootstrap, data_runtime: Option<Handle>) -> anyhow::Result<Self> {
        install_crypto_provider();
        let boot = Arc::new(boot);
        let (shutdown, _) = watch::channel(false);
        let mut tasks: Vec<JoinHandle<()>> = Vec::new();

        let store =
            Store::connect(&boot.database_url, boot.db_max_connections).await.context("connecting to PostgreSQL")?;
        store.migrate().await.context("applying database migrations")?;
        if boot.role.serves_management() {
            mgmt::ensure_first_admin(&store, &boot).await?;
        }

        let config = ConfigState::load(store.clone(), boot.keks.clone()).await.context("loading the configuration")?;
        let egress = Arc::new(EgressPolicy::parse(&boot.egress_allow_private).map_err(anyhow::Error::msg)?);
        let upstream = Upstream::new(egress.clone())?;
        let live = Arc::new(LiveStats::default());
        let limits = Arc::new(LimitEngine::new());
        tasks.push(tokio::spawn(limits.clone().run(store.clone(), shutdown.subscribe())));
        let tracer = match &boot.otlp_endpoint {
            Some(endpoint) => Tracer::spawn(endpoint, boot.instance_id.clone()),
            None => Tracer::disabled(),
        };

        let usage_spool = open_spool(
            &boot,
            "usage",
            boot.spool_max_bytes,
            pacod_gw_events::ingest::USAGE_PATH,
            &shutdown,
            &mut tasks,
        )?;
        let audit_spool = open_spool(
            &boot,
            "audit",
            boot.audit_spool_max_bytes,
            pacod_gw_events::ingest::AUDIT_PATH,
            &shutdown,
            &mut tasks,
        )?;

        let gw = Arc::new(Gw {
            boot: boot.clone(),
            config: config.clone(),
            upstream: upstream.clone(),
            usage_spool: usage_spool.clone(),
            audit_spool: audit_spool.clone(),
            live: live.clone(),
            limits: limits.clone(),
            tracer,
            auth_failures: Default::default(),
            key_slots: Default::default(),
        });

        // --- configuration poller and heartbeat ---
        let info = InstanceInfo {
            id: boot.instance_id.clone(),
            role: boot.role.as_str().to_owned(),
            hostname: std::env::var("HOSTNAME").unwrap_or_else(|_| "unknown".into()),
            started_at: chrono::Utc::now(),
        };
        tasks.push(tokio::spawn(config.clone().run(info, shutdown.subscribe())));

        // --- gauges that are read rather than pushed ---
        {
            let (config, usage, audit, id) =
                (config.clone(), usage_spool.clone(), audit_spool.clone(), boot.instance_id.clone());
            let (limits, live) = (limits.clone(), live.clone());
            let mut stop = shutdown.subscribe();
            tasks.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                loop {
                    tokio::select! { _ = tick.tick() => {} _ = stop.changed() => break }
                    metrics::set_config(&id, config.version(), config.lag());
                    metrics::set_spool(&id, "usage", usage.pending_bytes());
                    metrics::set_spool(&id, "audit", audit.pending_bytes());
                    metrics::set_streams_in_flight(live.in_flight());

                    // What the alert rules watch (concept section 14.9): limits, prices, providers.
                    let snap = config.snapshot();
                    let now = chrono::Utc::now();
                    for ws in snap.workspaces.values() {
                        let org = snap
                            .organisations
                            .get(&ws.organisation_id)
                            .map(|o| o.doc.name.as_str())
                            .unwrap_or_default();
                        for s in limits.scope_states(ws, now) {
                            let ratio = if s.amount > 0.0 { s.used / s.amount } else { 1.0 };
                            // A limit on the whole organisation is one series, not one per workspace.
                            let workspace = match s.subject {
                                crate::model::LimitSubject::Organisation => "",
                                _ => ws.doc.name.as_str(),
                            };
                            metrics::set_limit_used(org, workspace, &s.name, ratio);
                        }
                    }
                    for m in snap.models.values().filter(|m| m.doc.state.callable()) {
                        if let Some(p) = snap.price_at(&m.id, now) {
                            metrics::set_price_age(&m.doc.name, (now - p.checked_at()).num_seconds() as f64 / 86_400.0);
                        }
                    }
                    for h in live.health() {
                        metrics::set_upstream_health(&h.provider, &h.model, h.success_ratio);
                    }
                }
            }));
        }

        // --- metrics listener ---
        let handle = metrics::install();
        let metrics_listener = TcpListener::bind(boot.metrics_listen).await.context("binding the metrics listener")?;
        let metrics_addr = metrics_listener.local_addr()?;
        {
            let app = Router::new().route("/metrics", get(move || std::future::ready(handle.render())));
            let mut stop = shutdown.subscribe();
            tasks.push(tokio::spawn(async move {
                let _ = axum::serve(metrics_listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = stop.changed().await;
                    })
                    .await;
            }));
        }

        // --- management API ---
        let mut admin_addr = None;
        if boot.role.serves_management() {
            let (routes, openapi) = mgmt::build();
            let state = AdminState {
                boot: boot.clone(),
                config: config.clone(),
                store: store.clone(),
                egress: egress.clone(),
                http: reqwest::Client::builder().connect_timeout(Duration::from_secs(5)).build()?,
                logins: Default::default(),
                live: live.clone(),
                limits: limits.clone(),
                upstream: upstream.clone(),
                gw: gw.clone(),
                openapi: Arc::new(openapi),
                started_at: chrono::Utc::now(),
            };
            let app = routes
                .with_state(state)
                .layer(axum::extract::DefaultBodyLimit::max(4 << 20))
                .layer(tower::limit::ConcurrencyLimitLayer::new(boot.admin_concurrency));
            let listener = TcpListener::bind(boot.admin_listen).await.context("binding the management listener")?;
            admin_addr = Some(listener.local_addr()?);
            let mut stop = shutdown.subscribe();
            tasks.push(tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = stop.changed().await;
                    })
                    .await;
            }));

            // Expired sessions are removed once an hour.
            let (store, mut stop) = (store.clone(), shutdown.subscribe());
            tasks.push(tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    tokio::select! { _ = tick.tick() => {} _ = stop.changed() => break }
                    let _ = store.sessions_prune().await;
                }
            }));
        }

        // --- data plane ---
        let mut data_addr = None;
        if boot.role.serves_data_plane() {
            let app = dataplane::router(gw.clone()).into_make_service_with_connect_info::<SocketAddr>();
            // The listener is created on the runtime that will drive it.
            let listen = boot.data_listen;
            let mut stop = shutdown.subscribe();
            let rt = data_runtime.unwrap_or_else(Handle::current);
            let (addr_tx, addr_rx) = tokio::sync::oneshot::channel();
            tasks.push(rt.spawn(async move {
                let listener = match TcpListener::bind(listen).await {
                    Ok(l) => l,
                    Err(e) => {
                        let _ = addr_tx.send(Err(e));
                        return;
                    }
                };
                let _ = addr_tx.send(listener.local_addr());
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = stop.changed().await;
                    })
                    .await;
            }));
            data_addr =
                Some(addr_rx.await.context("data plane task ended early")?.context("binding the data plane listener")?);
        }

        tracing::info!(
            role = boot.role.as_str(),
            data = ?data_addr,
            admin = ?admin_addr,
            metrics = %metrics_addr,
            config_version = config.version(),
            instance = %boot.instance_id,
            "pacod-gw-core started"
        );
        Ok(Self {
            data_addr,
            admin_addr,
            metrics_addr,
            gw,
            store,
            shutdown,
            tasks,
            drain: Duration::from_secs(boot.drain_seconds),
        })
    }

    /// Stops accepting requests, lets running calls finish within the drain time, and flushes
    /// the spool.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let all = async {
            for t in self.tasks {
                let _ = t.await;
            }
        };
        if tokio::time::timeout(self.drain + Duration::from_secs(10), all).await.is_err() {
            tracing::warn!("some tasks did not finish within the drain time");
        }
    }
}
