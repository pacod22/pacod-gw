//! Starting and stopping the service: database, keys, listeners, background work.

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::bootstrap::{Bootstrap, Listen};
use crate::config::Config;
use crate::http::{self, AppState, Shared};
use crate::keys::{self, KeyRing};
use crate::store::{AuditEntry, Store};

/// The maintenance job's lease: one instance rotates keys and prunes old rows.
const MAINTENANCE: &str = "maintenance";
const MAINTENANCE_EVERY: Duration = Duration::from_secs(60);
const KEYS_RELOAD_EVERY: Duration = Duration::from_secs(30);
/// Another instance's catalogue push reaches this one within this.
const CATALOG_RELOAD_EVERY: Duration = Duration::from_secs(10);
const HEARTBEAT_EVERY: Duration = Duration::from_secs(15);
/// Every instance sends what is due; the outbox's row locks keep a mail from going twice.
const MAIL_EVERY: Duration = Duration::from_secs(3);
/// The audit log's retention.
const AUDIT_DAYS: u32 = 400;

/// One metrics recorder per process; tests start many services in one process.
fn metrics_handle() -> PrometheusHandle {
    static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();
    HANDLE
        .get_or_init(|| {
            let recorder = PrometheusBuilder::new().build_recorder();
            let handle = recorder.handle();
            // Another recorder may already be installed (a test harness): then only this one's
            // handle is lost, not the service.
            let _ = metrics::set_global_recorder(recorder);
            handle
        })
        .clone()
}

/// The listeners, bound before start so that tests can know their ports in advance.
pub struct Listeners {
    pub public: TcpListener,
    pub internal: TcpListener,
    pub metrics: TcpListener,
}

impl Listeners {
    pub fn bind(boot: &Bootstrap) -> anyhow::Result<Self> {
        let bind = |listen: &Listen, what: &str| -> anyhow::Result<TcpListener> {
            let l = listen.bind().with_context(|| format!("binding the {what} listener to {listen}"))?;
            l.set_nonblocking(true)?;
            Ok(l)
        };
        Ok(Listeners {
            public: bind(&boot.public_listen, "public")?,
            internal: bind(&boot.internal_listen, "internal")?,
            metrics: bind(&boot.metrics_listen, "metrics")?,
        })
    }
}

/// A running service.
pub struct Running {
    pub public: SocketAddr,
    pub internal: SocketAddr,
    pub metrics: SocketAddr,
    pub state: Shared,
    shutdown: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
}

impl Running {
    /// Stops accepting requests, finishes the ones in flight, gives up the leases.
    pub async fn stop(self) {
        self.state.ready.store(false, Ordering::Relaxed);
        let _ = self.shutdown.send(true);
        for t in self.tasks {
            let _ = t.await;
        }
        let _ = self.state.store.job_release(MAINTENANCE, &self.state.boot.instance_id).await;
        let _ = self.state.store.instance_gone(&self.state.boot.instance_id).await;
    }
}

/// Records the secrets of clients the configuration gives one (`secret_env`), as HMACs.
async fn store_client_secrets(store: &Store, boot: &Bootstrap, config: &Config) -> anyhow::Result<()> {
    for (client, secret) in &boot.client_secrets {
        let realm = &config.clients[client].realm;
        store.client_set_secret(client, realm, &boot.token_pepper.hash(secret)).await?;
    }
    Ok(())
}

pub async fn start(boot: Bootstrap, config: Config, listeners: Listeners) -> anyhow::Result<Running> {
    let store = Store::connect(&boot.database_url).await.context("connecting to PostgreSQL")?;
    store.migrate().await.context("migrating the database")?;
    store_client_secrets(&store, &boot, &config).await?;
    let keys = Arc::new(KeyRing::load(&store, &boot.keks).await.context("loading the signing keys")?);
    store.instance_heartbeat(&boot.instance_id, &boot.kek().id, &boot.kek().check_value()).await?;
    for alg in config.clients.values().flat_map(|c| [c.id_token_alg, c.access_token_alg]) {
        keys::ensure_active(&store, boot.kek(), alg).await.context("making a signing key")?;
    }
    keys.reload(&store, &boot.keks).await?;
    let cost = boot.password_cost;
    let dummy_hash = tokio::task::spawn_blocking(move || crate::crypto::password::dummy_hash(cost)).await?;
    let hashers = std::thread::available_parallelism().map_or(2, |n| n.get() * 2).clamp(2, 16);
    let catalog = crate::authz::CatalogCell::default();
    catalog.reload(&store).await.context("loading the catalogue")?;
    let feed = crate::authz::feed::Feed::new(store.authz_head().await?);
    let (shutdown, rx) = watch::channel(false);
    let boot_ui_dir = boot.ui_dir.clone();
    let state: Shared = Arc::new(AppState {
        mail: boot.mail.clone(),
        boot: Arc::new(boot),
        config: Arc::new(config),
        store,
        keys,
        metrics: metrics_handle(),
        ready: false.into(),
        hashing: tokio::sync::Semaphore::new(hashers),
        dummy_hash,
        catalog,
        feed,
        stopping: rx.clone(),
        upstreams: crate::federation::Upstreams::default(),
        ui: crate::ui::Ui::load(&boot_ui_dir),
    });
    crate::authz::bootstrap::ensure_admin(&state).await.context("the bootstrap admin")?;

    let mut tasks = Vec::new();
    let mut addrs = Vec::new();
    for (listener, router) in [
        (listeners.public, http::public(state.clone())),
        (listeners.internal, http::internal(state.clone())),
        (listeners.metrics, http::metrics(state.clone())),
    ] {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        addrs.push(listener.local_addr()?);
        let mut rx = rx.clone();
        let app = router.into_make_service_with_connect_info::<SocketAddr>();
        tasks.push(tokio::spawn(async move {
            let stopped = async move {
                let _ = rx.wait_for(|stop| *stop).await;
            };
            if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(stopped).await {
                tracing::error!(error = %e, "a listener stopped");
            }
        }));
    }
    tasks.push(tokio::spawn(crate::authz::feed::listen(state.clone(), rx.clone())));
    tasks.push(tokio::spawn(background(state.clone(), rx)));
    state.ready.store(true, Ordering::Relaxed);
    let _ = state.store.audit(&AuditEntry::new("instance_started").subject(&state.boot.instance_id)).await;
    tracing::info!(public = %addrs[0], internal = %addrs[1], metrics = %addrs[2], "pacod-id is up");
    Ok(Running { public: addrs[0], internal: addrs[1], metrics: addrs[2], state, shutdown, tasks })
}

/// Heartbeat and key reload on every instance; maintenance on the lease holder.
async fn background(st: Shared, mut stop: watch::Receiver<bool>) {
    let mut heartbeat = tokio::time::interval(HEARTBEAT_EVERY);
    let mut reload = tokio::time::interval(KEYS_RELOAD_EVERY);
    let mut maintenance = tokio::time::interval(MAINTENANCE_EVERY);
    let mut mail = tokio::time::interval(MAIL_EVERY);
    let mut catalog = tokio::time::interval(CATALOG_RELOAD_EVERY);
    loop {
        tokio::select! {
            _ = stop.changed() => return,
            _ = mail.tick() => {
                if st.mail.is_some() && let Err(e) = crate::mail::deliver_due(&st).await {
                    tracing::warn!(error = %e, "sending mail failed");
                }
                if let Err(e) = crate::oidc::logout::deliver(&st).await {
                    tracing::warn!(error = %e, "back-channel logout failed");
                }
            }
            _ = heartbeat.tick() => {
                let kek = st.boot.kek();
                if let Err(e) = st.store.instance_heartbeat(&st.boot.instance_id, &kek.id, &kek.check_value()).await {
                    tracing::warn!(error = %e, "heartbeat failed");
                }
            }
            _ = reload.tick() => {
                if let Err(e) = st.keys.reload(&st.store, &st.boot.keks).await {
                    tracing::warn!(error = %e, "reloading the signing keys failed");
                }
            }
            _ = catalog.tick() => {
                if let Err(e) = st.catalog.reload(&st.store).await {
                    tracing::warn!(error = %e, "reloading the catalogue failed");
                }
            }
            _ = maintenance.tick() => {
                if let Err(e) = maintain(&st).await {
                    tracing::warn!(error = %e, "maintenance failed");
                }
            }
        }
    }
}

/// One maintenance pass, when this instance holds the lease.
pub async fn maintain(st: &Shared) -> anyhow::Result<()> {
    if !st.store.job_lease(MAINTENANCE, &st.boot.instance_id, 3 * MAINTENANCE_EVERY.as_secs() as u32).await? {
        return Ok(());
    }
    let rotation = keys::rotate(&st.store, st.boot.kek(), chrono::Utc::now()).await?;
    if rotation != keys::Rotation::default() {
        tracing::info!(?rotation, "signing keys rotated");
        let _ = st
            .store
            .audit(&AuditEntry::new("keys_rotated").detail(serde_json::json!({
                "published": rotation.published, "promoted": rotation.promoted, "deleted": rotation.deleted
            })))
            .await;
        st.keys.reload(&st.store, &st.boot.keks).await?;
    }
    st.store.throttle_prune(24 * 3600).await?;
    st.store.oidc_prune().await?;
    st.store.mail_prune().await?;
    st.store.backchannel_prune().await?;
    st.store.authz_changes_prune(crate::authz::feed::KEEP_DAYS).await?;
    st.store.audit_prune(AUDIT_DAYS).await?;
    st.store.instances_prune(24).await?;
    st.store.job_ran(MAINTENANCE).await?;
    Ok(())
}

/// The key check values of the running instances that do not have the current key: rewrap
/// waits until there are none.
pub async fn instances_without(store: &Store, boot: &Bootstrap) -> anyhow::Result<Vec<String>> {
    let current = boot.kek().check_value();
    Ok(store
        .instances_alive(3 * HEARTBEAT_EVERY.as_secs() as u32)
        .await?
        .into_iter()
        .filter(|i| i.id != boot.instance_id && (i.kek_id != boot.kek().id || i.kek_check != current))
        .map(|i| i.id)
        .collect())
}
