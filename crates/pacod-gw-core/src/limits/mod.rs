//! Quotas, budgets and rate limits (concept section 12).
//!
//! Admission happens before a request is forwarded: every enforced limit that applies must
//! have room. Settlement happens when the call ends: tokens and cost are added to every limit
//! that applies, enforced or advisory. Counters live in memory; a background task adds each
//! instance's increments to PostgreSQL about once a second and reads the combined totals back
//! (see `sync`), so several instances enforce one limit with a lag of about a second.

pub mod sync;
pub mod window;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use pacod_gw_events::Usage;
use parking_lot::{Mutex, RwLock};
use schemars::JsonSchema;
use serde::Serialize;
use tokio::sync::watch;

use crate::model::{Enforcement, LimitSubject, Metric, Window};
use crate::snapshot::{KeyRt, LimitRt, WorkspaceRt};

/// One counter: a limit, a subject, a window.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CellKey {
    pub limit_id: String,
    pub subject: String,
    /// Start of the window in unix seconds; 0 for lifetime limits.
    pub window_start: i64,
}

#[derive(Debug, Default)]
pub struct Cell {
    /// The total across all instances as of the last synchronisation.
    base: AtomicU64,
    /// What this instance has counted since then.
    pending: AtomicU64,
    /// What this instance has counted and is writing right now. It still counts until the
    /// write has landed and `base` holds it.
    flushing: AtomicU64,
    /// When the window ends, for clean-up.
    window_end: i64,
    /// The stored total has been read since the cell was created.
    loaded: AtomicBool,
}

impl Cell {
    fn used(&self) -> u64 {
        self.base.load(Ordering::Relaxed) + self.unwritten()
    }

    /// What this instance has counted and not yet seen in the stored total.
    fn unwritten(&self) -> u64 {
        self.pending.load(Ordering::Relaxed) + self.flushing.load(Ordering::Relaxed)
    }
}

/// A limit that applies to a request, with the subject and window worked out.
#[derive(Debug, Clone)]
pub struct Applicable {
    pub limit: Arc<LimitRt>,
    pub subject: String,
    /// The allowance in counter units, after any end-user override.
    pub allowance: u64,
    pub window_start: i64,
    pub window_end: i64,
    /// Seconds into the window, for the sliding per-minute calculation.
    elapsed: i64,
    /// True once admission has counted this request against a requests limit.
    request_counted: bool,
}

impl Applicable {
    fn key(&self) -> CellKey {
        CellKey { limit_id: self.limit.id.clone(), subject: self.subject.clone(), window_start: self.window_start }
    }

    /// The counters the limit's state is computed from, with the end of their windows: the
    /// current window, and for per-minute limits, which slide, the minute before.
    fn keys_needed(&self) -> Vec<(CellKey, i64)> {
        let key = self.key();
        if self.limit.spec.window != Window::Minute {
            return vec![(key, self.window_end)];
        }
        let previous = CellKey { window_start: self.window_start - 60, ..key.clone() };
        vec![(key, self.window_end), (previous, self.window_start)]
    }
}

/// Why admission refused a request.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub limit_name: String,
    pub metric: Metric,
    pub window: Window,
    pub subject_kind: LimitSubject,
    pub resets_at: Option<DateTime<Utc>>,
    /// Seconds until the window frees up; set for per-minute limits, which are throttles.
    pub retry_after_seconds: Option<u32>,
    /// The limit counts only certain models: another model may still serve the call.
    pub model_specific: bool,
}

/// The state of one limit as the quota endpoints report it.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
pub struct LimitState {
    pub limit_id: String,
    pub name: String,
    pub subject: LimitSubject,
    pub metric: Metric,
    pub window: Window,
    pub enforcement: Enforcement,
    /// The allowance for the window. For spend limits in the reporting currency.
    pub amount: f64,
    pub used: f64,
    pub remaining: f64,
    /// When the window ends. Absent for lifetime limits.
    pub resets_at: Option<DateTime<Utc>>,
}

/// Counter units per unit of the metric: spend is counted in millionths of the currency.
fn scale(metric: Metric) -> f64 {
    if metric == Metric::Spend { 1_000_000.0 } else { 1.0 }
}

fn to_units(metric: Metric, amount: f64) -> u64 {
    (amount.max(0.0) * scale(metric)).round() as u64
}

fn from_units(metric: Metric, units: u64) -> f64 {
    units as f64 / scale(metric)
}

pub struct LimitEngine {
    cells: RwLock<HashMap<CellKey, Arc<Cell>>>,
    /// Cells created since the last synchronisation, whose stored total is still to be read.
    to_load: Mutex<Vec<CellKey>>,
    /// Counts the synchronisation rounds, so that a caller can wait for the next one.
    rounds: watch::Sender<u64>,
    /// No database behind it (`pacod-gw-lite`): nothing stored to read, so every counter
    /// starts out read. [`run_local`](Self::run_local) keeps it tidy.
    local: bool,
}

impl Default for LimitEngine {
    fn default() -> Self {
        Self { cells: Default::default(), to_load: Default::default(), rounds: watch::channel(0).0, local: false }
    }
}

/// What a request looks like to the limits.
pub struct Subjects<'a> {
    pub workspace: &'a WorkspaceRt,
    pub key: &'a KeyRt,
    pub end_user: &'a str,
    pub model: &'a str,
    pub template: &'a str,
    pub tags: &'a std::collections::BTreeMap<String, String>,
    /// The intent an enforcing plugin found, as routing and limits act on it.
    pub intent: Option<&'a str>,
    /// A plugin's own call (`docs/plugin-concept.md`, section 3.8): limits that count requests,
    /// and those of a key, do not apply. Its tokens and spend count like any call's.
    pub internal: bool,
}

impl LimitEngine {
    pub fn new() -> Self {
        Self::default()
    }

    /// An engine for an instance without a database, which counts on its own.
    pub fn local() -> Self {
        Self { local: true, ..Self::default() }
    }

    fn cell(&self, key: &CellKey, window_end: i64) -> Arc<Cell> {
        if let Some(c) = self.cells.read().get(key) {
            return c.clone();
        }
        let mut map = self.cells.write();
        map.entry(key.clone())
            .or_insert_with(|| {
                if !self.local {
                    self.to_load.lock().push(key.clone());
                }
                Arc::new(Cell { window_end, loaded: AtomicBool::new(self.local), ..Default::default() })
            })
            .clone()
    }

    fn peek(&self, key: &CellKey) -> u64 {
        self.cells.read().get(key).map_or(0, |c| c.used())
    }

    /// What this instance has counted and not yet written.
    fn unwritten(&self, key: &CellKey) -> u64 {
        self.cells.read().get(key).map_or(0, |c| c.unwritten())
    }

    /// The limits that apply to a request at `now`.
    pub fn applicable(&self, s: &Subjects<'_>, now: DateTime<Utc>) -> Vec<Applicable> {
        let mut out = Vec::new();
        // The workspace's and organisation's limits, then those of the key's rate class.
        for limit in s.workspace.limits.iter().chain(&s.key.class_limits) {
            let spec = &limit.spec;
            let f = &spec.filter;
            if !f.models.is_empty() && !f.models.iter().any(|m| m == s.model) {
                continue;
            }
            if !f.templates.is_empty() && !f.templates.iter().any(|t| t == s.template) {
                continue;
            }
            if !f.tags.iter().all(|(k, v)| s.tags.get(k) == Some(v)) {
                continue;
            }
            if !f.keys.is_empty() && !f.keys.contains(&s.key.id) {
                continue;
            }
            if !f.intents.is_empty() && !s.intent.is_some_and(|i| f.intents.iter().any(|x| x == i)) {
                continue;
            }
            if s.internal && (spec.metric == Metric::Requests || spec.subject == LimitSubject::Key) {
                continue;
            }
            let subject = match spec.subject {
                LimitSubject::Organisation => s.workspace.organisation_id.clone(),
                LimitSubject::Workspace => s.workspace.id.clone(),
                LimitSubject::Key => s.key.id.clone(),
                // A call without an end user is not counted against end-user limits.
                LimitSubject::EndUser if s.end_user.is_empty() => continue,
                LimitSubject::EndUser => s.end_user.to_owned(),
            };
            let mut amount = spec.amount;
            if spec.subject == LimitSubject::EndUser
                && let Some(ov) = s.workspace.overrides.get(&(limit.id.clone(), s.end_user.to_owned()))
            {
                if ov.exempt {
                    continue;
                }
                if let Some(a) = ov.amount {
                    amount = a;
                }
            }
            // Organisation limits use UTC so that all workspaces share one window.
            let tz = if spec.subject == LimitSubject::Organisation { "UTC" } else { &s.workspace.doc.time_zone };
            let (start, end) = window::bounds(spec.window, tz, now);
            out.push(Applicable {
                limit: limit.clone(),
                subject,
                allowance: to_units(spec.metric, amount),
                window_start: start,
                window_end: end,
                elapsed: now.timestamp() - start,
                request_counted: false,
            });
        }
        out
    }

    /// What has been used in the window. Per-minute limits slide: the previous minute counts
    /// in proportion to how much of it still lies within the last 60 seconds.
    fn used(&self, a: &Applicable) -> u64 {
        used_by(a, |k| self.peek(k))
    }

    /// Checks every enforced limit and counts the request against requests limits. Either all
    /// limits admit the request or nothing is counted.
    pub fn admit(&self, applicable: &mut [Applicable]) -> Result<(), Refusal> {
        for a in applicable.iter() {
            let spec = &a.limit.spec;
            if spec.enforcement != Enforcement::Request {
                continue;
            }
            let used = self.used(a);
            let full = match spec.metric {
                // This request needs room for itself.
                Metric::Requests => used + 1 > a.allowance,
                // Tokens and spend are only known afterwards: refuse once nothing is left.
                _ => used >= a.allowance,
            };
            if full {
                let throttle = spec.window == Window::Minute;
                return Err(Refusal {
                    limit_name: spec.name.clone(),
                    metric: spec.metric,
                    window: spec.window,
                    subject_kind: spec.subject,
                    resets_at: (spec.window != Window::Lifetime)
                        .then(|| Utc.timestamp_opt(a.window_end, 0).single())
                        .flatten(),
                    retry_after_seconds: throttle.then(|| (60 - a.elapsed.clamp(0, 59)).max(1) as u32),
                    model_specific: !spec.filter.models.is_empty(),
                });
            }
        }
        for a in applicable.iter_mut() {
            if a.limit.spec.metric == Metric::Requests {
                self.add(a, 1);
                a.request_counted = true;
            }
        }
        Ok(())
    }

    /// Moves an admitted call to another model: the limits that apply to the new model and
    /// not to the old one are admitted, those of the old model only are released, and those
    /// that apply to both stay as they are, counted once. Nothing changes when the new limits
    /// refuse the call.
    pub fn readmit(&self, current: &mut Vec<Applicable>, next: Vec<Applicable>) -> Result<(), Refusal> {
        let same = |a: &Applicable, b: &Applicable| {
            a.limit.id == b.limit.id && a.subject == b.subject && a.window_start == b.window_start
        };
        let mut added: Vec<Applicable> = next.iter().filter(|n| !current.iter().any(|c| same(c, n))).cloned().collect();
        self.admit(&mut added)?;
        current.retain(|c| {
            let stays = next.iter().any(|n| same(c, n));
            if !stays {
                self.release(c);
            }
            stays
        });
        current.extend(added);
        Ok(())
    }

    /// Takes back the request an admission counted.
    pub fn release(&self, a: &Applicable) {
        if !a.request_counted {
            return;
        }
        let key = CellKey { limit_id: a.limit.id.clone(), subject: a.subject.clone(), window_start: a.window_start };
        // Once synchronised, the count is in the stored total: a call that moved on to
        // another model then stays counted here, a small overcount.
        let _ = self
            .cell(&key, a.window_end)
            .pending
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |p| p.checked_sub(1));
    }

    fn add(&self, a: &Applicable, units: u64) {
        if units == 0 {
            return;
        }
        let key = CellKey { limit_id: a.limit.id.clone(), subject: a.subject.clone(), window_start: a.window_start };
        self.cell(&key, a.window_end).pending.fetch_add(units, Ordering::Relaxed);
    }

    /// Adds a finished call's consumption to every limit that applied to it.
    pub fn settle(&self, applicable: &[Applicable], usage: &Usage, cost: Option<f64>) {
        for a in applicable {
            let units = match a.limit.spec.metric {
                Metric::Requests => u64::from(!a.request_counted),
                Metric::Tokens => usage.prompt_total() + usage.output_tokens,
                Metric::InputTokens => usage.prompt_total(),
                Metric::OutputTokens => usage.output_tokens,
                Metric::CacheReadTokens => usage.cache_read_tokens,
                Metric::CacheWriteTokens => usage.cache_write_tokens,
                Metric::ReasoningTokens => usage.reasoning_tokens,
                Metric::Spend => to_units(Metric::Spend, cost.unwrap_or(0.0)),
            };
            self.add(a, units);
        }
    }

    /// Adds the consumption of a call whose request was counted earlier, at admission, to every
    /// limit that applies now: a batch item, settled when the batch has ended.
    pub fn settle_counted(&self, mut applicable: Vec<Applicable>, usage: &Usage, cost: Option<f64>) {
        for a in &mut applicable {
            a.request_counted = true;
        }
        self.settle(&applicable, usage, cost);
    }

    /// The state of the given limits, for the quota endpoints.
    pub fn state(&self, applicable: &[Applicable]) -> Vec<LimitState> {
        state_by(applicable, |k| self.peek(k))
    }

    /// The state of the given limits from the stored totals, plus what this instance has
    /// counted and not yet written. For the management API: it may run in an instance that
    /// serves no traffic and has never touched these counters.
    pub async fn stored_state(
        &self,
        store: &crate::store::Store,
        applicable: &[Applicable],
    ) -> Result<Vec<LimitState>, sqlx::Error> {
        let keys: Vec<CellKey> = applicable.iter().flat_map(|a| a.keys_needed().into_iter().map(|(k, _)| k)).collect();
        let stored = sync::load_rows(store, &keys).await?;
        Ok(state_by(applicable, |k| stored.get(k).copied().unwrap_or(0) + self.unwritten(k)))
    }

    /// The state of the given limits from memory, once the stored totals of counters this
    /// instance has not used before are read. For the quota endpoint: a gateway may be asked
    /// about a key it has not served, whose usage other gateways counted. It waits for the
    /// synchronisation, at most `max_wait`, and does not query the database itself.
    pub async fn state_fresh(&self, applicable: &[Applicable], max_wait: Duration) -> Vec<LimitState> {
        let now = Utc::now().timestamp();
        let mut rounds = self.rounds.subscribe();
        let waiting: Vec<Arc<Cell>> = applicable
            .iter()
            .flat_map(Applicable::keys_needed)
            // A window that has only just begun holds next to nothing yet; not worth a wait.
            .filter(|(k, _)| now - k.window_start > 2)
            .map(|(k, end)| self.cell(&k, end))
            .filter(|c| !c.loaded.load(Ordering::Relaxed))
            .collect();
        let deadline = tokio::time::Instant::now() + max_wait;
        while waiting.iter().any(|c| !c.loaded.load(Ordering::Relaxed)) {
            match tokio::time::timeout_at(deadline, rounds.changed()).await {
                Ok(Ok(())) => {}
                // Waited long enough, or nothing synchronises (a test without a database).
                _ => break,
            }
        }
        self.state(applicable)
    }
}

/// What has been used in the window, with `lookup` giving a counter's value. Per-minute limits
/// slide: the previous minute counts in proportion to how much of it still lies within the
/// last 60 seconds.
fn used_by(a: &Applicable, lookup: impl Fn(&CellKey) -> u64) -> u64 {
    let key = a.key();
    let current = lookup(&key);
    if a.limit.spec.window != Window::Minute {
        return current;
    }
    let prev = lookup(&CellKey { window_start: a.window_start - 60, ..key });
    let weight = (60 - a.elapsed.clamp(0, 60)) as f64 / 60.0;
    current + (prev as f64 * weight).round() as u64
}

fn state_by(applicable: &[Applicable], lookup: impl Fn(&CellKey) -> u64) -> Vec<LimitState> {
    applicable
        .iter()
        .map(|a| {
            let spec = &a.limit.spec;
            let used = used_by(a, &lookup);
            LimitState {
                limit_id: a.limit.id.clone(),
                name: spec.name.clone(),
                subject: spec.subject,
                metric: spec.metric,
                window: spec.window,
                enforcement: spec.enforcement,
                amount: from_units(spec.metric, a.allowance),
                used: from_units(spec.metric, used),
                remaining: from_units(spec.metric, a.allowance.saturating_sub(used)),
                resets_at: (spec.window != Window::Lifetime)
                    .then(|| Utc.timestamp_opt(a.window_end, 0).single())
                    .flatten(),
            }
        })
        .collect()
}

impl LimitEngine {
    /// The state of every limit of the key's workspace and of its rate class, as it stands for
    /// this key and end user. Filters are ignored: a limit that counts only some calls is
    /// still shown.
    pub fn states_for(&self, ws: &WorkspaceRt, key: &KeyRt, end_user: &str, now: DateTime<Utc>) -> Vec<LimitState> {
        self.state(&self.applicable_for(ws, key, end_user, now))
    }

    /// Every limit of the key's workspace and of its rate class, as it applies to this key and
    /// end user; see [`states_for`](Self::states_for).
    pub fn applicable_for(&self, ws: &WorkspaceRt, key: &KeyRt, end_user: &str, now: DateTime<Utc>) -> Vec<Applicable> {
        let no_tags = std::collections::BTreeMap::new();
        let mut all = Vec::new();
        for limit in ws.limits.iter().chain(&key.class_limits) {
            let f = &limit.spec.filter;
            let subjects = Subjects {
                workspace: ws,
                key,
                end_user,
                model: f.models.first().map(String::as_str).unwrap_or_default(),
                template: f.templates.first().map(String::as_str).unwrap_or_default(),
                tags: if f.tags.is_empty() { &no_tags } else { &f.tags },
                intent: f.intents.first().map(String::as_str),
                internal: false,
            };
            all.extend(self.applicable(&subjects, now).into_iter().filter(|a| Arc::ptr_eq(&a.limit, limit)));
        }
        all
    }

    /// The state of the limits that count the whole workspace or its whole organisation.
    /// These need no key and no end user, so they can be watched from outside a request.
    pub fn scope_states(&self, ws: &WorkspaceRt, now: DateTime<Utc>) -> Vec<LimitState> {
        self.state(&Self::scope_applicable(ws, now))
    }

    /// The limits [`scope_states`](Self::scope_states) reports on.
    pub fn scope_applicable(ws: &WorkspaceRt, now: DateTime<Utc>) -> Vec<Applicable> {
        ws.limits
            .iter()
            .filter_map(|limit| {
                let spec = &limit.spec;
                let (subject, tz) = match spec.subject {
                    LimitSubject::Organisation => (ws.organisation_id.clone(), "UTC"),
                    LimitSubject::Workspace => (ws.id.clone(), ws.doc.time_zone.as_str()),
                    LimitSubject::Key | LimitSubject::EndUser => return None,
                };
                let (start, end) = window::bounds(spec.window, tz, now);
                Some(Applicable {
                    limit: limit.clone(),
                    subject,
                    allowance: to_units(spec.metric, spec.amount),
                    window_start: start,
                    window_end: end,
                    elapsed: now.timestamp() - start,
                    request_counted: false,
                })
            })
            .collect()
    }

    /// Forgets a subject's counters for one limit, in memory. The stored rows are reset by
    /// the caller. Used by the reset operation.
    pub fn reset(&self, limit_id: &str, subject: &str) {
        self.cells.write().retain(|k, _| !(k.limit_id == limit_id && k.subject == subject));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::envelope::Kek;
    use crate::snapshot::{Snapshot, build};
    use crate::store::Row;
    use serde_json::{Value, json};
    use std::collections::BTreeMap;

    fn row(kind: &str, id: &str, parent: Option<&str>, doc: Value) -> Row {
        Row {
            id: id.into(),
            kind: kind.into(),
            parent: parent.map(Into::into),
            name: None,
            doc,
            version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn snapshot(limits: Vec<Row>) -> Snapshot {
        let mut rows = vec![
            row("organisation", "org_1", None, json!({"name": "acme"})),
            row("workspace", "ws_1", Some("org_1"), json!({"name": "prod"})),
            row("api_key", "key_1", Some("ws_1"), json!({"name": "k", "secret_hmac": hex::encode([1u8; 32])})),
        ];
        rows.extend(limits);
        let s = build(1, rows, &[Kek::new("k", [0u8; 32])]);
        assert!(s.problems.is_empty(), "{:?}", s.problems);
        s
    }

    fn limit(id: &str, doc: Value) -> Row {
        row("limit", id, Some("ws_1"), doc)
    }

    struct Req<'a> {
        snap: &'a Snapshot,
        end_user: &'a str,
        model: &'a str,
        template: &'a str,
        tags: BTreeMap<String, String>,
    }

    impl<'a> Req<'a> {
        fn new(snap: &'a Snapshot) -> Self {
            Self { snap, end_user: "u1", model: "m", template: "", tags: BTreeMap::new() }
        }

        fn applicable(&self, e: &LimitEngine, now: DateTime<Utc>) -> Vec<Applicable> {
            let ws = &self.snap.workspaces["ws_1"];
            let key = self.snap.keys.values().next().unwrap();
            e.applicable(
                &Subjects {
                    workspace: ws,
                    key,
                    end_user: self.end_user,
                    model: self.model,
                    template: self.template,
                    tags: &self.tags,
                    intent: None,
                    internal: false,
                },
                now,
            )
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn tokens(input: u64, output: u64) -> Usage {
        Usage { input_tokens: input, output_tokens: output, ..Default::default() }
    }

    #[tokio::test]
    async fn without_a_database_counts_are_folded_read_and_pruned() {
        let snap = snapshot(vec![limit(
            "lim_1",
            json!({ "name": "rpm", "subject": "key", "metric": "requests", "window": "minute", "amount": 2.0 }),
        )]);
        let e = LimitEngine::local();
        let req = Req::new(&snap);
        let now = Utc::now();
        let mut a = req.applicable(&e, now);
        e.admit(&mut a).unwrap();
        let key = a[0].key();
        assert_eq!((e.peek(&key), e.unwritten(&key)), (1, 1));

        e.fold_local();
        // The count stays; it is no longer waiting to be written, so it can be pruned.
        assert_eq!((e.peek(&key), e.unwritten(&key)), (1, 0));
        assert!(e.to_load.lock().is_empty());
        let mut again = req.applicable(&e, now);
        e.admit(&mut again).unwrap();
        assert!(e.admit(&mut req.applicable(&e, now)).is_err(), "the third request in a minute is refused");

        // Nothing is stored, so the quota endpoint does not wait for a synchronisation.
        e.fold_local();
        let started = std::time::Instant::now();
        let state = e.state_fresh(&req.applicable(&e, now), Duration::from_secs(2)).await;
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(state[0].used, 2.0);

        // Once the window has ended, its counters go.
        assert!(e.prune(now + chrono::Duration::minutes(10)) > 0);
        assert_eq!(e.peek(&key), 0);
    }

    #[test]
    fn a_token_limit_refuses_once_used_up_and_resets_with_the_window() {
        let snap = snapshot(vec![limit(
            "lim_1",
            json!({
                "name": "daily-tokens", "subject": "end_user", "metric": "tokens", "window": "day", "amount": 1000.0
            }),
        )]);
        let e = LimitEngine::new();
        let req = Req::new(&snap);
        let noon = at("2026-10-04T12:00:00Z");

        let mut a = req.applicable(&e, noon);
        assert_eq!(a.len(), 1);
        e.admit(&mut a).unwrap();
        // The call may overshoot: tokens are only known afterwards.
        e.settle(&a, &tokens(900, 300), None);

        let mut a = req.applicable(&e, noon);
        let refusal = e.admit(&mut a).unwrap_err();
        assert_eq!(refusal.limit_name, "daily-tokens");
        assert_eq!(refusal.resets_at, Some(at("2026-10-05T00:00:00Z")));
        assert_eq!(refusal.retry_after_seconds, None, "a spent quota is not a throttle");
        let st = &e.state(&a)[0];
        assert_eq!((st.used, st.remaining, st.amount), (1200.0, 0.0, 1000.0));

        // Another end user has an allowance of their own; the next day starts afresh.
        let other = Req { end_user: "u2", ..Req::new(&snap) };
        assert!(e.admit(&mut other.applicable(&e, noon)).is_ok());
        assert!(e.admit(&mut req.applicable(&e, at("2026-10-05T00:00:01Z"))).is_ok());
    }

    #[test]
    fn a_requests_limit_counts_at_admission() {
        let snap = snapshot(vec![limit(
            "lim_1",
            json!({
                "name": "hourly", "subject": "key", "metric": "requests", "window": "hour", "amount": 3.0
            }),
        )]);
        let e = LimitEngine::new();
        let req = Req::new(&snap);
        let now = at("2026-10-04T12:10:00Z");
        for _ in 0..3 {
            let mut a = req.applicable(&e, now);
            e.admit(&mut a).unwrap();
            e.settle(&a, &tokens(5, 5), None); // not counted a second time
        }
        assert!(e.admit(&mut req.applicable(&e, now)).is_err());
        assert_eq!(e.state(&req.applicable(&e, now))[0].used, 3.0);
    }

    #[test]
    fn moving_a_call_to_another_model_counts_it_once() {
        let snap = snapshot(vec![
            limit(
                "lim_ws",
                json!({ "name": "all", "subject": "workspace", "metric": "requests", "window": "hour", "amount": 10.0 }),
            ),
            limit(
                "lim_a",
                json!({ "name": "only-a", "subject": "workspace", "metric": "requests", "window": "hour",
                "amount": 10.0, "filter": { "models": ["a"] } }),
            ),
            limit(
                "lim_b",
                json!({ "name": "only-b", "subject": "workspace", "metric": "requests", "window": "hour",
                "amount": 0.0, "filter": { "models": ["b"] } }),
            ),
            limit(
                "lim_c",
                json!({ "name": "only-c", "subject": "workspace", "metric": "tokens", "window": "hour",
                "amount": 100.0, "filter": { "models": ["c"] } }),
            ),
        ]);
        let e = LimitEngine::new();
        let now = at("2026-10-04T12:10:00Z");
        let on = |model| Req { model, ..Req::new(&snap) }.applicable(&e, now);
        let used = |id: &str| {
            e.state(&on("a")).into_iter().chain(e.state(&on("c"))).find(|s| s.limit_id == id).map(|s| s.used)
        };

        let mut current = on("a");
        e.admit(&mut current).unwrap();
        assert_eq!((used("lim_ws"), used("lim_a")), (Some(1.0), Some(1.0)));

        // b's own limit is full: the call stays where it was, nothing changes.
        let r = e.readmit(&mut current, on("b")).unwrap_err();
        assert!(r.model_specific);
        assert_eq!((used("lim_ws"), used("lim_a")), (Some(1.0), Some(1.0)));

        // c takes it: a's count is taken back, the workspace's stays at one.
        e.readmit(&mut current, on("c")).unwrap();
        assert_eq!((used("lim_ws"), used("lim_a")), (Some(1.0), Some(0.0)));
        let mut ids: Vec<&str> = current.iter().map(|a| a.limit.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["lim_c", "lim_ws"]);
        // Settling charges the tokens to c's limit.
        e.settle(&current, &tokens(20, 10), None);
        assert_eq!(used("lim_c"), Some(30.0));
        assert_eq!(used("lim_ws"), Some(1.0), "the request is not counted twice");
    }

    #[test]
    fn the_minute_limit_slides_and_is_a_throttle() {
        let snap = snapshot(vec![limit(
            "lim_1",
            json!({
                "name": "burst", "subject": "end_user", "metric": "requests", "window": "minute", "amount": 10.0
            }),
        )]);
        let e = LimitEngine::new();
        let req = Req::new(&snap);
        // Ten requests in the second half of one minute.
        for _ in 0..10 {
            e.admit(&mut req.applicable(&e, at("2026-10-04T12:00:45Z"))).unwrap();
        }
        let r = e.admit(&mut req.applicable(&e, at("2026-10-04T12:00:50Z"))).unwrap_err();
        assert_eq!(r.retry_after_seconds, Some(10));
        // Fifteen seconds into the next minute, three quarters of the previous minute's
        // requests still lie within the last 60 seconds: 7.5, rounded to 8. Two fit.
        let t = at("2026-10-04T12:01:15Z");
        assert!(e.admit(&mut req.applicable(&e, t)).is_ok());
        assert!(e.admit(&mut req.applicable(&e, t)).is_ok());
        assert!(e.admit(&mut req.applicable(&e, t)).is_err());
        // A full minute later the old requests no longer count.
        assert!(e.admit(&mut req.applicable(&e, at("2026-10-04T12:02:20Z"))).is_ok());
    }

    #[test]
    fn spend_limits_and_advisory_limits() {
        let snap = snapshot(vec![
            limit(
                "lim_spend",
                json!({
                    "name": "budget", "subject": "workspace", "metric": "spend", "window": "month", "amount": 0.05
                }),
            ),
            limit(
                "lim_adv",
                json!({
                    "name": "per-user", "subject": "end_user", "metric": "tokens", "window": "day", "amount": 10.0,
                    "enforcement": "advisory"
                }),
            ),
        ]);
        let e = LimitEngine::new();
        let req = Req::new(&snap);
        let now = at("2026-10-04T12:00:00Z");
        let mut a = req.applicable(&e, now);
        e.admit(&mut a).unwrap();
        e.settle(&a, &tokens(500, 500), Some(0.049));
        // The advisory limit is far over its allowance and still admits; it only reports.
        let mut a = req.applicable(&e, now);
        assert!(e.admit(&mut a).is_ok());
        let st = e.state(&a);
        let adv = st.iter().find(|s| s.name == "per-user").unwrap();
        assert_eq!((adv.used, adv.remaining), (1000.0, 0.0));
        let spend = st.iter().find(|s| s.name == "budget").unwrap();
        assert!((spend.used - 0.049).abs() < 1e-9 && (spend.remaining - 0.001).abs() < 1e-9);
        // One more cent and the budget refuses.
        e.settle(&a, &tokens(1, 1), Some(0.01));
        let r = e.admit(&mut req.applicable(&e, now)).unwrap_err();
        assert_eq!((r.limit_name.as_str(), r.metric), ("budget", Metric::Spend));
        assert_eq!(r.resets_at, Some(at("2026-11-01T00:00:00Z")));
    }

    #[test]
    fn filters_overrides_and_missing_end_user() {
        let snap = snapshot(vec![
            limit(
                "lim_1",
                json!({
                    "name": "user-facing", "subject": "end_user", "metric": "requests", "window": "day", "amount": 1.0,
                    "filter": {"tags": {"purpose": "synthesizer"}, "models": ["m"]}
                }),
            ),
            row(
                "end_user_override",
                "euo_1",
                Some("ws_1"),
                json!({"limit_id": "lim_1", "end_user": "vip", "amount": 5.0}),
            ),
            row(
                "end_user_override",
                "euo_2",
                Some("ws_1"),
                json!({"limit_id": "lim_1", "end_user": "staff", "exempt": true}),
            ),
        ]);
        let e = LimitEngine::new();
        let now = at("2026-10-04T12:00:00Z");
        let tagged = |user: &'static str| Req {
            end_user: user,
            tags: BTreeMap::from([("purpose".to_owned(), "synthesizer".to_owned())]),
            ..Req::new(&snap)
        };
        // Calls without the tag, on another model, or without an end user are not counted.
        assert!(Req::new(&snap).applicable(&e, now).is_empty());
        assert!(Req { model: "other", ..tagged("u1") }.applicable(&e, now).is_empty());
        assert!(tagged("").applicable(&e, now).is_empty());
        // The default allowance, an override and an exemption.
        assert_eq!(tagged("u1").applicable(&e, now)[0].allowance, 1);
        assert_eq!(tagged("vip").applicable(&e, now)[0].allowance, 5);
        assert!(tagged("staff").applicable(&e, now).is_empty());
    }

    #[test]
    fn a_zero_allowance_allows_nothing() {
        let snap = snapshot(vec![limit(
            "lim_1",
            json!({
                "name": "blocked", "subject": "key", "metric": "tokens", "window": "lifetime", "amount": 0.0
            }),
        )]);
        let e = LimitEngine::new();
        let r = e.admit(&mut Req::new(&snap).applicable(&e, Utc::now())).unwrap_err();
        assert_eq!(r.resets_at, None);
    }

    #[test]
    fn organisation_limits_count_every_workspace_together() {
        let mut rows = vec![row(
            "org_limit",
            "olm_1",
            Some("org_1"),
            json!({
                "name": "org-cap", "subject": "organisation", "metric": "requests", "window": "day", "amount": 2.0
            }),
        )];
        rows.push(row("workspace", "ws_2", Some("org_1"), json!({"name": "dev", "time_zone": "Asia/Tokyo"})));
        rows.push(row("api_key", "key_2", Some("ws_2"), json!({"name": "k2", "secret_hmac": hex::encode([2u8; 32])})));
        let snap = snapshot(rows);
        let e = LimitEngine::new();
        let now = at("2026-10-04T20:00:00Z");
        let tags = BTreeMap::new();
        let subj = |ws: &str| {
            let w = &snap.workspaces[ws];
            let k = snap.keys.values().find(|k| k.workspace_id == ws).unwrap();
            e.applicable(
                &Subjects {
                    workspace: w,
                    key: k,
                    end_user: "",
                    model: "m",
                    template: "",
                    tags: &tags,
                    intent: None,
                    internal: false,
                },
                now,
            )
        };
        e.admit(&mut subj("ws_1")).unwrap();
        e.admit(&mut subj("ws_2")).unwrap();
        // Both workspaces count against the same window, whatever their time zones.
        assert_eq!(subj("ws_1")[0].window_start, subj("ws_2")[0].window_start);
        assert!(e.admit(&mut subj("ws_1")).is_err());
        assert!(e.admit(&mut subj("ws_2")).is_err());
    }

    #[test]
    fn a_limit_for_one_key_counts_only_that_key() {
        let mut rows = vec![limit(
            "lim_1",
            json!({
                "name": "k1-only", "subject": "workspace", "metric": "requests", "window": "day", "amount": 1.0,
                "filter": { "keys": ["key_1"] }
            }),
        )];
        rows.push(row("api_key", "key_2", Some("ws_1"), json!({"name": "k2", "secret_hmac": hex::encode([2u8; 32])})));
        let snap = snapshot(rows);
        let e = LimitEngine::new();
        let now = at("2026-10-04T12:00:00Z");
        let tags = BTreeMap::new();
        let ws = &snap.workspaces["ws_1"];
        let for_key = |id: &str| {
            let k = snap.keys.values().find(|k| k.id == id).unwrap();
            e.applicable(
                &Subjects {
                    workspace: ws,
                    key: k,
                    end_user: "",
                    model: "m",
                    template: "",
                    tags: &tags,
                    intent: None,
                    internal: false,
                },
                now,
            )
        };
        assert!(for_key("key_2").is_empty(), "the other key is not counted");
        e.admit(&mut for_key("key_1")).unwrap();
        assert!(e.admit(&mut for_key("key_1")).is_err());
        // Only the key it is aimed at needs provider usage.
        let spend = limit(
            "lim_2",
            json!({
                "name": "k1-spend", "subject": "workspace", "metric": "spend", "window": "day", "amount": 1.0,
                "filter": { "keys": ["key_1"] }
            }),
        );
        let mut rows = vec![spend];
        rows.push(row("api_key", "key_2", Some("ws_1"), json!({"name": "k2", "secret_hmac": hex::encode([2u8; 32])})));
        let snap2 = snapshot(rows);
        let ws2 = &snap2.workspaces["ws_1"];
        let policy = |id: &str| snap2.keys.values().find(|k| k.id == id).unwrap().usage_policy(ws2);
        assert_eq!(policy("key_1"), crate::model::UsagePolicy::Require);
        assert_eq!(policy("key_2"), crate::model::UsagePolicy::Estimate);
    }

    #[test]
    fn token_limits_can_count_one_kind_of_token() {
        let kinds = [("cache_read_tokens", 40.0), ("cache_write_tokens", 30.0), ("reasoning_tokens", 20.0)];
        let rows = kinds
            .iter()
            .enumerate()
            .map(|(i, (metric, _))| {
                limit(
                    &format!("lim_{i}"),
                    json!({ "name": format!("l{i}"), "subject": "workspace", "metric": metric, "window": "day", "amount": 1000.0 }),
                )
            })
            .collect();
        let snap = snapshot(rows);
        let e = LimitEngine::new();
        let req = Req::new(&snap);
        let now = at("2026-10-04T12:00:00Z");
        let a = req.applicable(&e, now);
        let usage = Usage {
            input_tokens: 10,
            cache_read_tokens: 40,
            cache_write_tokens: 30,
            output_tokens: 50,
            reasoning_tokens: 20,
            ..Default::default()
        };
        e.settle(&a, &usage, None);
        let mut used: Vec<(String, f64)> = e.state(&a).into_iter().map(|s| (s.name, s.used)).collect();
        used.sort_by(|x, y| x.0.cmp(&y.0));
        assert_eq!(used, [("l0".to_owned(), 40.0), ("l1".to_owned(), 30.0), ("l2".to_owned(), 20.0)]);
    }
}
