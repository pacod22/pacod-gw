//! The engine against a stub server that can be slow, fail, break off or hang, and that
//! counts how many calls it holds at once.

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use bytes::Bytes;
use pacod_gw_load::client::Ready;
use pacod_gw_load::contract::{AuthHeader, Ended, HttpVersion, Mode, Profile, RequestSpec, Stage};
use pacod_gw_load::engine::{Job, Live, Stop, run};
use serde::Deserialize;
use tokio::sync::watch;

#[derive(Default)]
struct Counter {
    now: AtomicU64,
    peak: AtomicU64,
    seen: AtomicU64,
}

struct Held(Arc<Counter>);

impl Held {
    fn new(c: &Arc<Counter>) -> Held {
        let now = c.now.fetch_add(1, Ordering::SeqCst) + 1;
        c.peak.fetch_max(now, Ordering::SeqCst);
        c.seen.fetch_add(1, Ordering::SeqCst);
        Held(c.clone())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0.now.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Deserialize)]
struct Slow {
    ms: u64,
}

async fn slow(State(c): State<Arc<Counter>>, Query(q): Query<Slow>) -> &'static str {
    let _held = Held::new(&c);
    tokio::time::sleep(Duration::from_millis(q.ms)).await;
    "done"
}

async fn ok(State(c): State<Arc<Counter>>) -> Response {
    let _held = Held::new(&c);
    ([("x-gw-delay-ms", "1.250")], "fine").into_response()
}

async fn fail(State(c): State<Arc<Counter>>) -> StatusCode {
    let _held = Held::new(&c);
    StatusCode::SERVICE_UNAVAILABLE
}

/// A first chunk, then the body breaks off.
async fn reset() -> Response {
    use futures_util::StreamExt;
    let first = futures_util::stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"data: 1\n\n")) });
    // The break comes a moment later, as from a provider that stops mid-stream: an error ready
    // at once would cut the connection before the response head is out.
    let cut = futures_util::stream::once(async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        Err(std::io::Error::other("cut"))
    });
    Response::new(Body::from_stream(first.chain(cut)))
}

async fn hang() -> Result<&'static str, Infallible> {
    std::future::pending::<()>().await;
    Ok("never")
}

async fn stub() -> (url::Url, Arc<Counter>) {
    let counter = Arc::new(Counter::default());
    let app = Router::new()
        .route("/slow", post(slow))
        .route("/ok", post(ok))
        .route("/fail", post(fail))
        .route("/reset", post(reset))
        .route("/hang", get(hang).post(hang))
        .with_state(counter.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url::Url::parse(&format!("http://{addr}")).unwrap(), counter)
}

fn stages(s: &[(u64, f64)]) -> Vec<Stage> {
    s.iter().map(|(d, t)| Stage { duration_s: *d, target: *t }).collect()
}

fn profile(mode: Mode, stages: Vec<Stage>) -> Profile {
    Profile {
        mode,
        stages,
        max_in_flight: None,
        timeout_s: 10,
        max_calls: None,
        drain_s: 10,
        http: HttpVersion::Http1,
        warmup_connections: 0,
    }
}

fn job(base: &url::Url, path: &str, profile: Profile) -> Arc<Job> {
    let spec = RequestSpec {
        method: "POST".into(),
        path: path.into(),
        headers: [("content-type".to_owned(), "application/json".to_owned())].into(),
        body: "{}".into(),
        stream: false,
    };
    let ready = Ready::new(base, &spec, AuthHeader::Authorization, "pgw_test_key").unwrap();
    let http = pacod_gw_load::client::build(base, profile.http, 64, None).unwrap();
    let timeout = Duration::from_secs(profile.timeout_s);
    Arc::new(Job { http, ready, profile, timeout })
}

async fn go(job: Arc<Job>) -> (Ended, Arc<Live>) {
    let live = Live::new();
    let (_stop, rx) = watch::channel(None);
    let ended = run(job, live.clone(), rx).await;
    (ended, live)
}

#[tokio::test]
async fn the_closed_loop_holds_its_target_concurrency() {
    let (base, counter) = stub().await;
    let (ended, live) =
        go(job(&base, "/slow?ms=100", profile(Mode::Concurrency, stages(&[(0, 20.0), (2, 20.0)])))).await;
    assert_eq!(ended, Ended::Duration);
    let peak = counter.peak.load(Ordering::SeqCst);
    assert!((18..=20).contains(&peak), "peak {peak}");
    let totals = live.totals();
    assert_eq!(totals.ok, totals.completed);
    assert!(totals.ok > 250, "{} calls", totals.ok);
    assert_eq!(totals.errors.cancelled, 0);
    assert!(totals.total.percentile(0.5) >= 100_000.0);
    let intervals = live.intervals_after(None);
    assert!(intervals.len() >= 2 && intervals[0].t == 0 && intervals[1].t == 1, "{intervals:?}");
    assert_eq!(intervals[0].target, 20.0);
}

#[tokio::test]
async fn the_open_loop_starts_calls_at_the_rate() {
    let (base, _) = stub().await;
    let (ended, live) = go(job(&base, "/ok", profile(Mode::Rate, stages(&[(0, 200.0), (2, 200.0)])))).await;
    assert_eq!(ended, Ended::Duration);
    let totals = live.totals();
    assert!((360..=420).contains(&totals.started), "{} started", totals.started);
    assert_eq!(totals.skipped, 0);
    // The stub's own figure is read from the header.
    assert_eq!(totals.gw_delay.n, totals.ok);
    assert_eq!(totals.gw_delay.min, 1250);
}

#[tokio::test]
async fn the_open_loop_skips_arrivals_above_max_in_flight() {
    let (base, counter) = stub().await;
    let mut p = profile(Mode::Rate, stages(&[(0, 200.0), (1, 200.0)]));
    p.max_in_flight = Some(5);
    let (_, live) = go(job(&base, "/slow?ms=200", p)).await;
    let totals = live.totals();
    assert!(counter.peak.load(Ordering::SeqCst) <= 5);
    assert!(totals.skipped > 100, "{} skipped", totals.skipped);
    assert!((195..=200).contains(&(totals.started + totals.skipped)));
}

#[tokio::test]
async fn max_calls_is_exact_across_workers() {
    let (base, counter) = stub().await;
    let mut p = profile(Mode::Concurrency, stages(&[(0, 10.0), (60, 10.0)]));
    p.max_calls = Some(57);
    let (ended, live) = go(job(&base, "/ok", p)).await;
    assert_eq!(ended, Ended::MaxCalls);
    assert_eq!(live.totals().completed, 57);
    assert_eq!(counter.seen.load(Ordering::SeqCst), 57);
}

#[tokio::test]
async fn failures_are_told_apart() {
    let (base, _) = stub().await;
    let one = |path: &str, timeout_s: u64| {
        let mut p = profile(Mode::Concurrency, stages(&[(0, 1.0), (60, 1.0)]));
        p.max_calls = Some(1);
        p.timeout_s = timeout_s;
        job(&base, path, p)
    };
    let (_, live) = go(one("/fail", 5)).await;
    let t = live.totals();
    assert_eq!((t.ok, t.completed, t.status.get(&503).copied(), t.errors.failed()), (0, 1, Some(1), 0));
    assert!(t.total.is_empty(), "a failure's latency is not recorded");

    let (_, live) = go(one("/hang", 1)).await;
    assert_eq!(live.totals().errors.timeout, 1);

    let (_, live) = go(one("/reset", 5)).await;
    let t = live.totals();
    assert_eq!((t.errors.body, t.ok, t.status.get(&200).copied()), (1, 0, Some(1)));

    let closed = url::Url::parse("http://127.0.0.1:9").unwrap();
    let mut p = profile(Mode::Concurrency, stages(&[(0, 1.0), (60, 1.0)]));
    p.max_calls = Some(1);
    let (_, live) = go(job(&closed, "/ok", p)).await;
    assert_eq!(live.totals().errors.connect, 1);
}

#[tokio::test]
async fn a_stop_ends_the_calls_at_once_and_counts_them_cancelled() {
    let (base, _) = stub().await;
    let job = job(&base, "/slow?ms=5000", profile(Mode::Concurrency, stages(&[(0, 8.0), (60, 8.0)])));
    let live = Live::new();
    let (stop, rx) = watch::channel(None);
    let running = tokio::spawn(run(job, live.clone(), rx));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(live.in_flight(), 8);
    stop.send(Some(Stop::LeaseExpired)).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(2), running).await.expect("stops at once").unwrap();
    assert_eq!(ended, Ended::LeaseExpired);
    let t = live.totals();
    assert_eq!((t.errors.cancelled, t.completed, live.in_flight()), (8, 0, 0));
}

#[tokio::test]
async fn the_drain_lets_running_calls_finish_or_cuts_them_off() {
    let (base, _) = stub().await;
    let p = profile(Mode::Concurrency, stages(&[(0, 5.0), (1, 5.0)]));
    let (ended, live) = go(job(&base, "/slow?ms=1500", p.clone())).await;
    assert_eq!(ended, Ended::Duration);
    let t = live.totals();
    assert_eq!((t.completed, t.errors.cancelled), (5, 0));

    let mut cut = p;
    cut.drain_s = 0;
    let (_, live) = go(job(&base, "/slow?ms=1500", cut)).await;
    let t = live.totals();
    assert_eq!((t.completed, t.errors.cancelled), (0, 5));
}

#[tokio::test]
async fn reported_seconds_are_dropped_once_acknowledged() {
    let (base, _) = stub().await;
    let (_, live) = go(job(&base, "/ok", profile(Mode::Rate, stages(&[(0, 20.0), (3, 20.0)])))).await;
    let all = live.intervals_after(None);
    assert!(all.len() >= 3);
    let rest = live.intervals_after(Some(1));
    assert_eq!(rest.first().map(|iv| iv.t), Some(2));
    assert_eq!(live.intervals_after(None).len(), rest.len());
}
