//! A provider that keeps itself announced.
//!
//! [`Provider::start`] announces and keeps the lease alive in a background task:
//! - a heartbeat every `heartbeat_every_s` the broker asked for, carrying the live state the
//!   provider sets through its [`LiveHandle`] (status, capacity in use, observed latency);
//! - when the broker no longer knows the lease (it restarted, or the lease expired while the
//!   network was down), the provider announces again on its own;
//! - when the broker cannot be reached, it tries again with back-off and keeps the last answer;
//! - [`Running::stop`] says goodbye, which counts in the provider's favour in reputation.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pacod_broker_model::LiveStatus;
use tokio::sync::{RwLock, watch};
use tokio::task::JoinHandle;

use crate::client::{Auth, BrokerClient, Error, Result};
use crate::types::{Announced, Announcement, Heartbeat, HeartbeatOne};

/// What the provider reports with its next heartbeat.
#[derive(Debug, Clone, Default)]
struct LiveState {
    all: HeartbeatOne,
    per_key: HashMap<String, HeartbeatOne>,
}

/// Sets the live state the next heartbeat reports. Cheap to clone; share it with the code
/// that serves the resources.
#[derive(Debug, Clone, Default)]
pub struct LiveHandle(Arc<Mutex<LiveState>>);

impl LiveHandle {
    pub fn set_status(&self, s: LiveStatus) {
        self.0.lock().expect("live state").all.status = Some(s);
    }

    pub fn set_in_use(&self, n: u32) {
        self.0.lock().expect("live state").all.in_use = Some(n);
    }

    pub fn set_ttft_ms(&self, ms: f64) {
        self.0.lock().expect("live state").all.ttft_ms = Some(ms);
    }

    /// The state of one resource, by its key, over the provider-wide one.
    pub fn set_resource(
        &self,
        key: impl Into<String>,
        status: Option<LiveStatus>,
        in_use: Option<u32>,
        ttft_ms: Option<f64>,
    ) {
        self.0.lock().expect("live state").per_key.insert(key.into(), HeartbeatOne { status, in_use, ttft_ms });
    }

    fn heartbeat(&self) -> Heartbeat {
        let s = self.0.lock().expect("live state");
        Heartbeat { status: s.all.status, in_use: s.all.in_use, ttft_ms: s.all.ttft_ms, resources: s.per_key.clone() }
    }
}

/// What a running provider tells the code that started it.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderEvent {
    Announced(Announced),
    /// The broker did not know the lease any more; the provider announced again.
    Reannounced(Announced),
    /// A heartbeat or an announcement failed; the provider tries again.
    Retrying(String),
    /// The provider waits for an admin's approval.
    Pending,
}

pub struct Provider {
    client: BrokerClient,
    announcement: Announcement,
    live: LiveHandle,
}

impl Provider {
    pub fn new(client: BrokerClient, announcement: Announcement) -> Self {
        Self { client, announcement, live: LiveHandle::default() }
    }

    pub fn live(&self) -> LiveHandle {
        self.live.clone()
    }

    /// The provider name open-mode brokers identify the lease's owner by.
    fn owner(&self) -> Option<String> {
        matches!(self.client.auth(), Auth::None | Auth::OpenClient(_)).then(|| self.announcement.provider.name.clone())
    }

    /// Announces, then keeps the lease alive until [`Running::stop`].
    pub async fn start(self) -> Result<Running> {
        self.start_with(|_| {}).await
    }

    /// The same, calling `on_event` for each announcement, re-announcement and retry.
    pub async fn start_with(self, on_event: impl Fn(ProviderEvent) + Send + Sync + 'static) -> Result<Running> {
        let first = self.client.announce(&self.announcement).await?;
        on_event(ProviderEvent::Announced(first.clone()));
        let announced = Arc::new(RwLock::new(first));
        let (stop, mut stopped) = watch::channel(false);
        let state = announced.clone();
        let owner = self.owner();
        let Provider { client, announcement, live } = self;
        // Shared with `Running::update`, so a re-announcement after a lost lease sends the latest.
        let announcement = Arc::new(RwLock::new(announcement));
        let task_announcement = announcement.clone();
        let task_client = client.clone();
        let task_owner = owner.clone();
        let task = tokio::spawn(async move {
            let owner = task_owner;
            let client = task_client;
            let announcement = task_announcement;
            let mut failures: u32 = 0;
            loop {
                let (lease, every) = {
                    let a = state.read().await;
                    (a.lease_id.clone(), a.heartbeat_every_s.max(1))
                };
                // After a failure, try again sooner, but never more often than every second.
                let wait = if failures == 0 {
                    Duration::from_secs(every)
                } else {
                    Duration::from_secs((1u64 << failures.min(5)).min(every))
                };
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stopped.changed() => return,
                }
                match client.heartbeat(&lease, &live.heartbeat(), owner.as_deref()).await {
                    Ok(()) => failures = 0,
                    Err(e) if e.is_not_found() => match client.announce(&announcement.read().await.clone()).await {
                        Ok(a) => {
                            failures = 0;
                            *state.write().await = a.clone();
                            on_event(ProviderEvent::Reannounced(a));
                        }
                        Err(Error::Pending(_)) => {
                            failures = 5;
                            on_event(ProviderEvent::Pending);
                        }
                        Err(e) => {
                            failures += 1;
                            on_event(ProviderEvent::Retrying(e.to_string()));
                        }
                    },
                    Err(e) => {
                        failures += 1;
                        on_event(ProviderEvent::Retrying(e.to_string()));
                    }
                }
            }
        });
        Ok(Running { client, announcement, announced, stop, task, owner })
    }
}

/// A provider that keeps its lease alive.
pub struct Running {
    client: BrokerClient,
    announcement: Arc<RwLock<Announcement>>,
    announced: Arc<RwLock<Announced>>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    owner: Option<String>,
}

impl Running {
    /// The latest answer to an announcement: lease, resource ids by key.
    pub async fn announced(&self) -> Announced {
        self.announced.read().await.clone()
    }

    /// Announces a changed announcement under the same lease: resources added, changed or gone,
    /// new prices. Ids stay the same for the same keys. When the broker no longer knows the
    /// lease, it announces afresh. Later re-announcements send this version.
    pub async fn update(&self, mut announcement: Announcement) -> Result<Announced> {
        announcement.lease_id = None;
        *self.announcement.write().await = announcement.clone();
        announcement.lease_id = Some(self.announced.read().await.lease_id.clone());
        let a = match self.client.announce(&announcement).await {
            Err(e) if e.is_not_found() => {
                announcement.lease_id = None;
                self.client.announce(&announcement).await?
            }
            other => other?,
        };
        *self.announced.write().await = a.clone();
        Ok(a)
    }

    /// Stops the heartbeats and ends the lease cleanly.
    pub async fn stop(self) -> Result<()> {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        let lease = self.announced.read().await.lease_id.clone();
        match self.client.deregister(&lease, self.owner.as_deref()).await {
            Err(e) if e.is_not_found() => Ok(()),
            other => other,
        }
    }

    /// Stops the heartbeats without a goodbye, as a crash would: the lease runs out after its
    /// TTL and counts against the provider's lease discipline. For handing a lease to another
    /// process, and for tests.
    pub async fn abandon(self) -> Announced {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        self.announced.read().await.clone()
    }
}
