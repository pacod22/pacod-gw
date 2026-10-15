//! The load engine: calls at a target concurrency or rate that follows the stages, recorded
//! second by second.
//!
//! Two ways to make load, because they answer different questions:
//!
//! - **Concurrency** (closed loop): `target` workers, each starting a call when its previous
//!   one has ended. It finds how much one set of clients gets through, but a slow gateway
//!   slows the clients down with it and hides part of its own slowness.
//! - **Rate** (open loop): `target` calls a second, started on time whether or not earlier
//!   calls have finished, as independent clients would. A slow gateway then shows as calls
//!   piling up; above `max_in_flight` arrivals are skipped and counted.
//!
//! The end: when the stages are over, or `max_calls` are used up, no new call starts and the
//! running ones get `drain_s` to finish. A stop (cancel, or the lease running out) ends the
//! calls at once. Calls that did not finish are counted as cancelled, not as errors.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::client::{self, CallOutcome, ErrorKind, Ready};
use crate::contract::{Ended, Interval, Lag, Mode, Profile, RunState, Stage};
use crate::cpu::ProcessCpu;

/// How often the closed loop adjusts its number of workers to the target.
const CONTROL_EVERY: Duration = Duration::from_millis(50);
/// The open loop's clock: arrivals due are started once per tick.
const TICK: Duration = Duration::from_millis(1);
/// A worker whose call could not connect waits this long before the next, so that a gateway
/// that is down is not hammered by thousands of workers in a tight loop.
const CONNECT_BACKOFF: Duration = Duration::from_millis(10);

/// One run's work: the request, the client and the profile.
pub struct Job {
    pub http: reqwest::Client,
    pub ready: Ready,
    pub profile: Profile,
    pub timeout: Duration,
}

/// Why a run is told to stop before its end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Cancel,
    LeaseExpired,
}

/// The target at `t` seconds: a straight line through each stage from where the previous one
/// ended (0 at the start); a stage of 0 seconds jumps to its target.
pub fn target_at(stages: &[Stage], t: f64) -> f64 {
    let (mut start, mut from) = (0.0, 0.0);
    for s in stages {
        let d = s.duration_s as f64;
        if d == 0.0 {
            from = s.target;
            continue;
        }
        if t < start + d {
            return from + (s.target - from) * (t - start).max(0.0) / d;
        }
        start += d;
        from = s.target;
    }
    from
}

/// Calls due by `t` seconds in rate mode: the integral of [`target_at`].
pub fn arrivals(stages: &[Stage], t: f64) -> f64 {
    let (mut start, mut from, mut total) = (0.0, 0.0, 0.0);
    for s in stages {
        let d = s.duration_s as f64;
        if d == 0.0 {
            from = s.target;
            continue;
        }
        let x = (t - start).min(d);
        if x <= 0.0 {
            break;
        }
        total += from * x + (s.target - from) * x * x / (2.0 * d);
        start += d;
        from = s.target;
    }
    total
}

/// Adds one second (or one agent) to a total.
pub fn absorb(into: &mut Interval, iv: &Interval) {
    into.started += iv.started;
    into.completed += iv.completed;
    into.ok += iv.ok;
    into.errors.connect += iv.errors.connect;
    into.errors.timeout += iv.errors.timeout;
    into.errors.body += iv.errors.body;
    into.errors.other += iv.errors.other;
    into.errors.cancelled += iv.errors.cancelled;
    for (s, n) in &iv.status {
        *into.status.entry(*s).or_default() += n;
    }
    into.bytes += iv.bytes;
    into.skipped += iv.skipped;
    into.lag_ms.max = into.lag_ms.max.max(iv.lag_ms.max);
    into.ttfb.merge(&iv.ttfb);
    into.total.merge(&iv.total);
    into.gw_delay.merge(&iv.gw_delay);
}

/// The second being filled, the seconds not yet reported, and the run's total.
#[derive(Default)]
struct Recorder {
    next_t: u64,
    current: Interval,
    lag_max: f64,
    lag_sum: f64,
    lag_n: u64,
    closed: VecDeque<Interval>,
    totals: Interval,
}

impl Recorder {
    fn record(&mut self, o: &CallOutcome) {
        let iv = &mut self.current;
        iv.completed += 1;
        if let Some(s) = o.status {
            *iv.status.entry(s).or_default() += 1;
        }
        iv.bytes += o.bytes;
        match o.error {
            Some(ErrorKind::Connect) => iv.errors.connect += 1,
            Some(ErrorKind::Timeout) => iv.errors.timeout += 1,
            Some(ErrorKind::Body) => iv.errors.body += 1,
            Some(ErrorKind::Other) => iv.errors.other += 1,
            None => {}
        }
        if o.ok() {
            iv.ok += 1;
            iv.ttfb.record(o.ttfb_us);
            iv.total.record(o.total_us);
            if let Some(d) = o.gw_delay_us {
                iv.gw_delay.record(d);
            }
        }
    }

    fn lag(&mut self, ms: f64) {
        self.lag_max = self.lag_max.max(ms);
        self.lag_sum += ms;
        self.lag_n += 1;
    }

    fn close(&mut self, target: f64, in_flight: u64, cpu: Option<f64>) {
        let mut iv = std::mem::take(&mut self.current);
        iv.t = self.next_t;
        self.next_t += 1;
        iv.target = target;
        iv.in_flight = in_flight;
        iv.cpu_pct = cpu;
        iv.lag_ms =
            Lag { max: self.lag_max, mean: if self.lag_n > 0 { self.lag_sum / self.lag_n as f64 } else { 0.0 } };
        (self.lag_max, self.lag_sum, self.lag_n) = (0.0, 0.0, 0);
        absorb(&mut self.totals, &iv);
        self.closed.push_back(iv);
    }
}

/// What a run shares with whoever reports on it, while it runs and after.
pub struct Live {
    started: OnceLock<Instant>,
    finished: OnceLock<Duration>,
    in_flight: AtomicU64,
    calls: AtomicU64,
    phase: Mutex<(RunState, Option<Ended>)>,
    rec: Mutex<Recorder>,
}

impl Live {
    pub fn new() -> Arc<Live> {
        Arc::new(Live {
            started: OnceLock::new(),
            finished: OnceLock::new(),
            in_flight: AtomicU64::new(0),
            calls: AtomicU64::new(0),
            phase: Mutex::new((RunState::Prepared, None)),
            rec: Mutex::new(Recorder::default()),
        })
    }

    pub fn in_flight(&self) -> u64 {
        self.in_flight.load(Ordering::Relaxed)
    }

    /// Calls finished, whatever their outcome, cancelled ones not included.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::Relaxed)
    }

    /// Since the start, and fixed once the run has ended.
    pub fn elapsed(&self) -> Duration {
        if let Some(d) = self.finished.get() {
            return *d;
        }
        self.started.get().map_or(Duration::ZERO, |s| s.elapsed())
    }

    pub fn state(&self) -> (RunState, Option<Ended>) {
        *self.phase.lock()
    }

    pub fn set_state(&self, state: RunState, ended: Option<Ended>) {
        *self.phase.lock() = (state, ended);
    }

    /// The closed seconds after `after`. Seconds up to `after` were received and are dropped,
    /// so a report that went missing is sent again with the next one.
    pub fn intervals_after(&self, after: Option<u64>) -> Vec<Interval> {
        let mut rec = self.rec.lock();
        if let Some(after) = after {
            while rec.closed.front().is_some_and(|iv| iv.t <= after) {
                rec.closed.pop_front();
            }
        }
        rec.closed.iter().cloned().collect()
    }

    /// The whole run so far, in closed seconds.
    pub fn totals(&self) -> Interval {
        self.rec.lock().totals.clone()
    }
}

/// One call in flight. Dropping it before [`Flight::finish`] (the task was aborted) counts
/// the call as cancelled.
struct Flight {
    live: Arc<Live>,
    done: bool,
}

impl Flight {
    fn start(live: &Arc<Live>) -> Flight {
        live.in_flight.fetch_add(1, Ordering::Relaxed);
        live.rec.lock().current.started += 1;
        Flight { live: live.clone(), done: false }
    }

    fn finish(mut self, o: &CallOutcome) {
        self.done = true;
        self.live.calls.fetch_add(1, Ordering::Relaxed);
        self.live.rec.lock().record(o);
    }
}

impl Drop for Flight {
    fn drop(&mut self) {
        if !self.done {
            self.live.rec.lock().current.errors.cancelled += 1;
        }
        self.live.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The calls left, when the run has a `max_calls`.
struct Budget(Option<AtomicU64>);

impl Budget {
    fn take(&self) -> bool {
        let Some(left) = &self.0 else { return true };
        let mut n = left.load(Ordering::Acquire);
        loop {
            if n == 0 {
                return false;
            }
            match left.compare_exchange_weak(n, n - 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(now) => n = now,
            }
        }
    }

    fn used_up(&self) -> bool {
        self.0.as_ref().is_some_and(|left| left.load(Ordering::Acquire) == 0)
    }
}

struct Control {
    desired: AtomicUsize,
    ending: AtomicBool,
    budget: Budget,
}

/// Runs the job to its end and says why it ended. `live` is updated as it goes.
pub async fn run(job: Arc<Job>, live: Arc<Live>, mut stop: watch::Receiver<Option<Stop>>) -> Ended {
    let start = Instant::now();
    let _ = live.started.set(start);
    live.set_state(RunState::Running, None);
    let duration = Duration::from_secs(job.profile.duration_s());
    let ctl = Arc::new(Control {
        desired: AtomicUsize::new(0),
        ending: AtomicBool::new(false),
        budget: Budget(job.profile.max_calls.map(AtomicU64::new)),
    });
    let ticker = tokio::spawn(tick_seconds(job.profile.stages.clone(), duration, live.clone(), start));

    let mut calls = Calls::default();
    let reason = match job.profile.mode {
        Mode::Concurrency => closed_loop(&job, &live, &ctl, &mut calls, &mut stop, start, duration).await,
        Mode::Rate => open_loop(&job, &live, &ctl, &mut calls, &mut stop, start, duration).await,
    };
    let reason = match reason {
        Some(stopped) => stopped,
        None => {
            // Natural end: no new calls, and the running ones get the drain time.
            ctl.ending.store(true, Ordering::Relaxed);
            live.set_state(RunState::Draining, None);
            let ended = if ctl.budget.used_up() { Ended::MaxCalls } else { Ended::Duration };
            let deadline = Instant::now() + Duration::from_secs(job.profile.drain_s);
            loop {
                if live.in_flight() == 0 || Instant::now() >= deadline {
                    break ended;
                }
                if let Some(s) = *stop.borrow() {
                    break stopped(s);
                }
                tokio::time::sleep(CONTROL_EVERY).await;
            }
        }
    };
    ctl.ending.store(true, Ordering::Relaxed);
    calls.abort_all().await;
    ticker.abort();
    let _ = ticker.await;
    let _ = live.finished.set(start.elapsed());
    let target = target_at(&job.profile.stages, start.elapsed().as_secs_f64().min(duration.as_secs_f64()));
    live.rec.lock().close(target, live.in_flight(), None);
    reason
}

fn stopped(s: Stop) -> Ended {
    match s {
        Stop::Cancel => Ended::Cancelled,
        Stop::LeaseExpired => Ended::LeaseExpired,
    }
}

/// The tasks that make calls, for aborting them at the end.
#[derive(Default)]
struct Calls {
    workers: Vec<Option<JoinHandle<()>>>,
    arrivals: JoinSet<()>,
}

impl Calls {
    async fn abort_all(&mut self) {
        for w in self.workers.iter_mut().filter_map(Option::take) {
            w.abort();
            let _ = w.await;
        }
        self.arrivals.abort_all();
        while self.arrivals.join_next().await.is_some() {}
    }
}

/// Closes one interval a second until aborted.
async fn tick_seconds(stages: Vec<Stage>, duration: Duration, live: Arc<Live>, start: Instant) {
    let mut cpu = ProcessCpu::new();
    let mut every = tokio::time::interval_at(start + Duration::from_secs(1), Duration::from_secs(1));
    loop {
        let at = every.tick().await;
        let t = at.duration_since(start).min(duration).as_secs_f64();
        let sample = cpu.sample();
        live.rec.lock().close(target_at(&stages, t), live.in_flight(), sample);
    }
}

/// `Some` when stopped from outside, `None` at the natural end.
async fn closed_loop(
    job: &Arc<Job>,
    live: &Arc<Live>,
    ctl: &Arc<Control>,
    calls: &mut Calls,
    stop: &mut watch::Receiver<Option<Stop>>,
    start: Instant,
    duration: Duration,
) -> Option<Ended> {
    loop {
        if let Some(s) = *stop.borrow() {
            return Some(stopped(s));
        }
        let elapsed = start.elapsed();
        if elapsed >= duration || ctl.budget.used_up() {
            return None;
        }
        let desired = target_at(&job.profile.stages, elapsed.as_secs_f64()).round().max(0.0) as usize;
        ctl.desired.store(desired, Ordering::Relaxed);
        if calls.workers.len() < desired {
            calls.workers.resize_with(desired, || None);
        }
        // A worker above the target ends after its call; one below it that ended (the target
        // dipped and rose again) is started anew.
        for (i, slot) in calls.workers.iter_mut().enumerate().take(desired) {
            if slot.as_ref().is_none_or(JoinHandle::is_finished) {
                *slot = Some(tokio::spawn(worker(i, job.clone(), live.clone(), ctl.clone())));
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(CONTROL_EVERY) => {}
            _ = stop.changed() => {}
        }
    }
}

async fn worker(i: usize, job: Arc<Job>, live: Arc<Live>, ctl: Arc<Control>) {
    loop {
        if ctl.ending.load(Ordering::Relaxed) || i >= ctl.desired.load(Ordering::Relaxed) || !ctl.budget.take() {
            return;
        }
        let flight = Flight::start(&live);
        let o = client::call(&job.http, &job.ready, job.timeout, 0).await;
        let refused = o.error == Some(ErrorKind::Connect);
        flight.finish(&o);
        if refused {
            tokio::time::sleep(CONNECT_BACKOFF).await;
        }
    }
}

async fn open_loop(
    job: &Arc<Job>,
    live: &Arc<Live>,
    ctl: &Arc<Control>,
    calls: &mut Calls,
    stop: &mut watch::Receiver<Option<Stop>>,
    start: Instant,
    duration: Duration,
) -> Option<Ended> {
    let max_in_flight = u64::from(job.profile.max_in_flight.unwrap_or(u32::MAX));
    let mut dispatched: u64 = 0;
    let mut next = start + TICK;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(next) => {}
            _ = stop.changed() => {}
        }
        if let Some(s) = *stop.borrow() {
            return Some(stopped(s));
        }
        let now = Instant::now();
        live.rec.lock().lag(now.saturating_duration_since(next).as_secs_f64() * 1000.0);
        next += TICK;
        let elapsed = now.duration_since(start);
        if elapsed >= duration || ctl.budget.used_up() {
            return None;
        }
        let due = arrivals(&job.profile.stages, elapsed.as_secs_f64()).floor() as u64;
        while dispatched < due {
            dispatched += 1;
            if live.in_flight() >= max_in_flight {
                live.rec.lock().current.skipped += 1;
                continue;
            }
            if !ctl.budget.take() {
                break;
            }
            let flight = Flight::start(live);
            let job = job.clone();
            calls.arrivals.spawn(async move {
                let o = client::call(&job.http, &job.ready, job.timeout, 0).await;
                flight.finish(&o);
            });
        }
        while calls.arrivals.try_join_next().is_some() {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stages(s: &[(u64, f64)]) -> Vec<Stage> {
        s.iter().map(|(d, t)| Stage { duration_s: *d, target: *t }).collect()
    }

    #[test]
    fn the_target_follows_the_stages() {
        let s = stages(&[(10, 100.0), (20, 100.0), (10, 0.0)]);
        assert_eq!(target_at(&s, 0.0), 0.0);
        assert_eq!(target_at(&s, 5.0), 50.0);
        assert_eq!(target_at(&s, 15.0), 100.0);
        assert_eq!(target_at(&s, 35.0), 50.0);
        assert_eq!(target_at(&s, 99.0), 0.0);
        let jump = stages(&[(0, 40.0), (10, 40.0)]);
        assert_eq!(target_at(&jump, 0.0), 40.0);
        assert_eq!(target_at(&jump, 9.9), 40.0);
    }

    #[test]
    fn arrivals_are_the_area_under_the_rate() {
        let s = stages(&[(10, 100.0), (20, 100.0)]);
        assert_eq!(arrivals(&s, 10.0), 500.0);
        assert_eq!(arrivals(&s, 20.0), 1500.0);
        assert_eq!(arrivals(&s, 30.0), 2500.0);
        assert_eq!(arrivals(&s, 99.0), 2500.0);
        assert_eq!(arrivals(&stages(&[(0, 50.0), (4, 50.0)]), 2.0), 100.0);
    }
}
