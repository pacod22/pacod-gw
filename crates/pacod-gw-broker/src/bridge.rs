//! The bridge's loop: offerings refreshed on the gateway's config changes and every
//! `GWB_REFRESH_SECONDS`, live state every `GWB_LIVE_SECONDS`, evidence every
//! `GWB_EVIDENCE_SECONDS`, and a clean goodbye for every lease on shutdown.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::Utc;
use futures_util::StreamExt;
use pacod_broker_client::{Announcement, BrokerClient, LiveHandle, Provider, ProviderEvent, Running};
use tokio::sync::{Mutex, Notify};

use crate::config::{Config, OrgOffer};
use crate::evidence::{Held, Tickets};
use crate::gw::{Gw, Model, Named, Price};
use crate::metrics::Metrics;
use crate::offer::{self, Offered, Site};

/// One organisation's lease on the broker.
struct Org {
    running: Running,
    live: LiveHandle,
    announced: serde_json::Value,
    /// The pool's model names, by resource key, for live state.
    pools: HashMap<String, Vec<String>>,
}

pub struct Bridge {
    cfg: Config,
    gw: Gw,
    consumer: Option<BrokerClient>,
    orgs: Mutex<HashMap<String, Org>>,
    tickets: Mutex<Tickets>,
    pub metrics: Arc<Metrics>,
    changed: Notify,
}

impl Bridge {
    pub fn new(cfg: Config, metrics: Arc<Metrics>) -> anyhow::Result<Arc<Self>> {
        let gw = Gw::new(&cfg.gw_url, &cfg.gw_token)?;
        let consumer = match &cfg.consumer_token {
            Some(t) => Some(BrokerClient::new(&cfg.broker_url)?.with_token(t)),
            // In open mode the bridge names itself, so its tickets are its own.
            None if cfg.broker_open => Some(BrokerClient::new(&cfg.broker_url)?.with_client_name("gw-broker")),
            None => None,
        };
        Ok(Arc::new(Self {
            cfg,
            gw,
            consumer,
            orgs: Mutex::new(HashMap::new()),
            tickets: Mutex::new(HashMap::new()),
            metrics,
            changed: Notify::new(),
        }))
    }

    fn site(&self) -> Site {
        Site { public_url: self.cfg.public_url.clone(), obtain_url: self.cfg.obtain_url.clone(), ttl_s: self.cfg.ttl_s }
    }

    fn client_for(&self, o: &OrgOffer) -> anyhow::Result<BrokerClient> {
        let c = BrokerClient::new(&self.cfg.broker_url)?;
        Ok(match o.token()? {
            Some(t) if !self.cfg.broker_open => c.with_token(t),
            _ => c,
        })
    }

    /// What each organisation in the file offers now, read from the gateway.
    async fn offerings(&self) -> anyhow::Result<Vec<(OrgOffer, Named, Vec<Offered>)>> {
        let orgs = self.gw.organisations().await?;
        let models: HashMap<String, Model> = self.gw.models().await?.into_iter().map(|m| (m.name.clone(), m)).collect();
        let mut prices: HashMap<String, Vec<Price>> = HashMap::new();
        let mut out = Vec::new();
        for o in &self.cfg.file.organisations {
            let Some(org) = orgs.iter().find(|x| x.name == o.organisation || x.id == o.organisation) else {
                tracing::warn!(organisation = %o.organisation, "the gateway has no such organisation; nothing offered for it");
                continue;
            };
            let workspaces = self.gw.workspaces(&org.id).await?;
            let mut offered = Vec::new();
            for x in &o.offer {
                let Some(ws) = workspaces.iter().find(|w| w.name == x.workspace || w.id == x.workspace) else {
                    tracing::warn!(organisation = %org.name, workspace = %x.workspace, "no such workspace");
                    continue;
                };
                let templates = self.gw.templates(&org.id, &ws.id).await?;
                let Some(t) = templates.iter().find(|t| t.name == x.template || t.id == x.template) else {
                    tracing::warn!(organisation = %org.name, workspace = %ws.name, template = %x.template, "no such template");
                    continue;
                };
                if !t.status.is_empty() && t.status != "active" {
                    continue;
                }
                let model = t.spec.pool().first().and_then(|n| models.get(n)).cloned();
                let p = match &model {
                    Some(m) => match prices.get(&m.id) {
                        Some(p) => p.clone(),
                        None => {
                            let p = self.gw.prices(&m.id).await.unwrap_or_default();
                            prices.insert(m.id.clone(), p.clone());
                            p
                        }
                    },
                    None => Vec::new(),
                };
                offered.push(Offered {
                    offering: x.clone(),
                    workspace: ws.clone(),
                    template: t.clone(),
                    model,
                    prices: p,
                });
            }
            out.push((o.clone(), org.clone(), offered));
        }
        Ok(out)
    }

    /// Brings every organisation's lease in line with what it offers now.
    pub async fn refresh(&self) -> anyhow::Result<()> {
        let site = self.site();
        let mut orgs = self.orgs.lock().await;
        let mut count = 0;
        for (o, org, offered) in self.offerings().await? {
            let name = org.name.clone();
            if offered.is_empty() {
                if let Some(old) = orgs.remove(&name) {
                    let _ = old.running.stop().await;
                    tracing::info!(organisation = %name, "nothing offered any more; the lease ended");
                }
                continue;
            }
            count += offered.len();
            let a = match offer::announcement(&org, &offered, &site) {
                Ok(a) => a,
                Err(e) => {
                    tracing::warn!(organisation = %name, error = %format!("{e:#}"), "offerings left as they were");
                    continue;
                }
            };
            let pools = offered.iter().map(|x| (offer::key(x), x.template.spec.pool())).collect();
            let value = serde_json::to_value(&a)?;
            match orgs.get_mut(&name) {
                Some(cur) if cur.announced == value => cur.pools = pools,
                Some(cur) => match cur.running.update(a).await {
                    Ok(_) => {
                        self.metrics.announces.fetch_add(1, Ordering::Relaxed);
                        cur.announced = value;
                        cur.pools = pools;
                        tracing::info!(organisation = %name, resources = offered.len(), "offerings updated on the broker");
                    }
                    Err(e) => tracing::warn!(organisation = %name, error = %e, "the broker refused the update"),
                },
                None => match self.start(&o, a).await {
                    Ok((running, live)) => {
                        tracing::info!(organisation = %name, resources = offered.len(), "offerings announced on the broker");
                        orgs.insert(name, Org { running, live, announced: value, pools });
                    }
                    Err(e) => tracing::warn!(organisation = %name, error = %e, "the broker refused the announcement"),
                },
            }
        }
        self.metrics.offerings.store(count as u64, Ordering::Relaxed);
        self.metrics.ready.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn start(&self, o: &OrgOffer, a: Announcement) -> anyhow::Result<(Running, LiveHandle)> {
        let provider = Provider::new(self.client_for(o)?, a);
        let live = provider.live();
        let m = self.metrics.clone();
        let org = o.organisation.clone();
        let running = provider
            .start_with(move |e| match e {
                ProviderEvent::Announced(_) | ProviderEvent::Reannounced(_) => {
                    m.announces.fetch_add(1, Ordering::Relaxed);
                }
                ProviderEvent::Retrying(why) => {
                    m.heartbeat_failures.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(organisation = %org, error = %why, "heartbeat failed; trying again");
                }
                ProviderEvent::Pending => tracing::info!(organisation = %org, "waiting for a broker admin's approval"),
            })
            .await?;
        self.metrics.announces.fetch_add(1, Ordering::Relaxed);
        Ok((running, live))
    }

    /// The offerings' live state, from the gateway's routing view.
    pub async fn live(&self) -> anyhow::Result<()> {
        let targets = self.gw.routing().await?;
        for org in self.orgs.lock().await.values() {
            for (key, pool) in &org.pools {
                let (status, in_use, ttft) = offer::live(pool, &targets);
                org.live.set_resource(key.clone(), status, in_use, ttft);
            }
        }
        Ok(())
    }

    /// Reports a sample of the calls to each linked upstream (R3a).
    pub async fn evidence(&self) -> anyhow::Result<()> {
        let Some(consumer) = &self.consumer else { return Ok(()) };
        let mut links: HashMap<String, String> = self.cfg.file.links.clone().into_iter().collect();
        // From phase 2 the gateway's repository says which model is which broker resource.
        for m in self.gw.models().await.unwrap_or_default() {
            if let Some(r) = m.broker_resource_id.filter(|r| !r.is_empty()) {
                links.entry(m.name).or_insert(r);
            }
        }
        let now = Utc::now();
        let mut tickets = self.tickets.lock().await;
        for (model, rid) in links {
            if tickets.get(&rid).is_none_or(|h| h.stale(now)) {
                match consumer.resource(&rid).await {
                    Ok(v) => match v.ticket {
                        Some(t) => {
                            tickets.insert(rid.clone(), Held::new(t, Utc::now()));
                        }
                        None => continue,
                    },
                    Err(e) => {
                        tracing::warn!(resource = %rid, error = %e, "no ticket for a linked resource");
                        continue;
                    }
                }
            }
            let Some(held) = tickets.get_mut(&rid) else { continue };
            let mut events = Vec::new();
            let mut cursor: Option<String> = None;
            // A few pages at most per round: the budget is small and the rest waits.
            for _ in 0..5 {
                let page = self.gw.requests(&model, held.reported_to, cursor.as_deref()).await?;
                events.extend(page.requests);
                match page.next_cursor {
                    Some(c) if !c.is_empty() => cursor = Some(c),
                    _ => break,
                }
            }
            let reports = held.take(events, self.cfg.sample_per_hour, Utc::now());
            if reports.is_empty() {
                continue;
            }
            match consumer.report_usage(&reports).await {
                Ok(a) => {
                    self.metrics.reports_sent.fetch_add(reports.len() as u64, Ordering::Relaxed);
                    self.metrics.reports_counted.fetch_add(a.accepted as u64, Ordering::Relaxed);
                }
                Err(e) => tracing::warn!(resource = %rid, error = %e, "usage reports refused"),
            }
        }
        Ok(())
    }

    /// Follows the gateway's config changes; each wakes a refresh. Reconnects with back-off.
    pub async fn follow_changes(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(1);
        loop {
            match self.gw.config_events().await {
                Ok(stream) => {
                    backoff = Duration::from_secs(1);
                    let mut stream = Box::pin(stream);
                    while stream.next().await.is_some() {
                        self.changed.notify_one();
                    }
                }
                Err(e) => tracing::debug!(error = %e, "the gateway's event stream is not available"),
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(60));
        }
    }

    pub async fn run(self: Arc<Self>, mut stop: tokio::sync::watch::Receiver<bool>) {
        tokio::spawn(self.clone().follow_changes());
        let mut refresh = tokio::time::interval(self.cfg.refresh);
        let mut live = tokio::time::interval(self.cfg.live_every);
        let mut evidence = tokio::time::interval(self.cfg.evidence_every);
        for t in [&mut refresh, &mut live, &mut evidence] {
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        }
        loop {
            tokio::select! {
                _ = refresh.tick() => self.logged("refresh", self.refresh().await),
                _ = self.changed.notified() => {
                    // Several changes in a row become one refresh.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    self.logged("refresh", self.refresh().await);
                }
                _ = live.tick() => self.logged("live state", self.live().await),
                _ = evidence.tick() => self.logged("evidence", self.evidence().await),
                _ = stop.changed() => break,
            }
        }
        for (name, org) in self.orgs.lock().await.drain() {
            match org.running.stop().await {
                Ok(()) => tracing::info!(organisation = %name, "lease ended"),
                Err(e) => tracing::warn!(organisation = %name, error = %e, "the goodbye failed; the lease runs out"),
            }
        }
    }

    fn logged(&self, what: &str, r: anyhow::Result<()>) {
        if let Err(e) = r {
            self.metrics.failures.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(error = %format!("{e:#}"), "{what} failed");
        }
    }
}
