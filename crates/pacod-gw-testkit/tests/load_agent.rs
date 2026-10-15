//! The load agent over HTTP, as the tester drives it, against the mock provider: prepare,
//! start, reports, cancel, the lease, and runs against another base URL.

use std::time::Duration;

use pacod_gw_load::agent::config::{AgentConfig, Settings};
use pacod_gw_load::agent::{Agent, router, spawn_lease_keeper};
use pacod_gw_load::contract::{
    AuthHeader, CONTRACT, Caps, Ended, HttpVersion, Info, Interval, Mode, Prepare, Prepared, Profile, Report,
    RequestSpec, RunState, Stage,
};
use pacod_gw_load::engine::absorb;
use pacod_gw_testkit::mock::MockProvider;
use reqwest::StatusCode;
use serde_json::{Value, json};

const TOKEN: &str = "test-only-load-agent-token-0123456789";

struct Running {
    url: String,
    http: reqwest::Client,
}

impl Running {
    async fn start(target: &str) -> Running {
        let settings = Settings {
            listen: "127.0.0.1:0".parse().unwrap(),
            name: "test".into(),
            target: reqwest::Url::parse(target).unwrap(),
            caps: Caps { max_concurrency: 50, max_rate: 500.0, max_duration_s: 120, max_body_bytes: 65536 },
            ca_file: None,
        };
        let agent = Agent::new(AgentConfig::new(settings, TOKEN).unwrap());
        spawn_lease_keeper(agent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router(agent)).await.unwrap() });
        Running { url: format!("http://{addr}"), http: reqwest::Client::new() }
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut req = self.http.request(method, format!("{}{path}", self.url)).bearer_auth(TOKEN);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.unwrap();
        let status = resp.status();
        (status, resp.json().await.unwrap_or(Value::Null))
    }

    async fn prepare(&self, p: &Prepare) -> (StatusCode, Value) {
        self.call(reqwest::Method::POST, "/v1/runs", Some(serde_json::to_value(p).unwrap())).await
    }

    async fn report(&self, id: &str) -> Report {
        let (status, body) = self.call(reqwest::Method::GET, &format!("/v1/runs/{id}/report"), None).await;
        assert_eq!(status, 200, "{body}");
        serde_json::from_value(body).unwrap()
    }

    /// Polls until the run has ended, adding up its seconds as the tester does.
    async fn finish(&self, id: &str) -> (Report, Interval) {
        let mut total = Interval::default();
        let mut after: Option<u64> = None;
        for _ in 0..200 {
            let path = match after {
                Some(t) => format!("/v1/runs/{id}/report?after={t}"),
                None => format!("/v1/runs/{id}/report"),
            };
            let (_, body) = self.call(reqwest::Method::GET, &path, None).await;
            let r: Report = serde_json::from_value(body).unwrap();
            for iv in &r.intervals {
                absorb(&mut total, iv);
                after = Some(iv.t);
            }
            if r.state.finished() {
                return (r, total);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("run {id} did not end");
    }
}

fn plan(id: &str, stream: bool, max_calls: Option<u64>, duration_s: u64) -> Prepare {
    Prepare {
        contract: CONTRACT,
        run_id: id.into(),
        part: 0,
        parts: 1,
        key: "pgw_key_secret-that-must-not-leak".into(),
        auth_header: AuthHeader::Authorization,
        base_url: None,
        request: RequestSpec {
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            headers: [("content-type".to_owned(), "application/json".to_owned())].into(),
            body: json!({ "model": "gpt-mock", "stream": stream, "messages": [{ "role": "user", "content": "one two three four" }] })
                .to_string(),
            stream,
        },
        profile: Profile {
            mode: Mode::Concurrency,
            stages: vec![Stage { duration_s: 0, target: 5.0 }, Stage { duration_s, target: 5.0 }],
            max_in_flight: None,
            timeout_s: 10,
            max_calls,
            drain_s: 5,
            http: HttpVersion::Http1,
            warmup_connections: 5,
        },
        lease_s: 30,
    }
}

#[tokio::test]
async fn a_run_is_prepared_started_reported_and_ends_at_its_call_budget() {
    let mock = MockProvider::spawn().await;
    let agent = Running::start(&format!("{}/x-mock/delay=50,chunks=4", mock.base_url())).await;

    let anonymous = agent.http.get(format!("{}/v1/info", agent.url)).send().await.unwrap();
    assert_eq!(anonymous.status(), 401);
    let (_, info) = agent.call(reqwest::Method::GET, "/v1/info", None).await;
    let info: Info = serde_json::from_value(info).unwrap();
    assert_eq!((info.contract, info.busy), (CONTRACT, None));

    let (status, body) = agent.prepare(&plan("run-1", true, Some(60), 60)).await;
    assert_eq!(status, 201, "{body}");
    let prepared: Prepared = serde_json::from_value(body).unwrap();
    assert_eq!((prepared.state, prepared.probe.status), (RunState::Prepared, Some(200)));
    assert!(prepared.probe.ttfb_ms >= 50.0, "{:?}", prepared.probe);

    let (status, _) = agent.prepare(&plan("run-2", true, None, 10)).await;
    assert_eq!(status, 409, "one run at a time");

    let (status, _) = agent.call(reqwest::Method::POST, "/v1/runs/run-1/start", None).await;
    assert_eq!(status, 202);
    let (report, total) = agent.finish("run-1").await;
    assert_eq!((report.state, report.ended), (RunState::Done, Some(Ended::MaxCalls)));
    assert_eq!((total.ok, total.completed, report.calls), (60, 60, 60));
    assert!(total.ttfb.percentile(0.5) >= 50_000.0);
    assert!(total.total.percentile(0.5) >= total.ttfb.percentile(0.5));
    let posts = mock.captured().iter().filter(|c| c.method == "POST").count();
    assert_eq!(posts, 61, "60 calls and the probe");
    // Every call carried the key; the warm-up's GETs carry none.
    let calls = mock.captured().into_iter().filter(|c| c.method == "POST").collect::<Vec<_>>();
    assert!(calls.iter().all(|c| c.headers.iter().any(|(n, v)| n == "authorization" && v.contains("secret"))));
}

#[tokio::test]
async fn a_cancel_stops_the_run_and_a_broken_stream_counts_as_a_body_error() {
    let mock = MockProvider::spawn().await;
    let agent = Running::start(&format!("{}/x-mock/reset_after=2,chunks=4", mock.base_url())).await;
    let (status, body) = agent.prepare(&plan("broken", true, Some(10), 30)).await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["probe"]["error"], "the answer broke off");
    agent.call(reqwest::Method::POST, "/v1/runs/broken/start", None).await;
    let (_, total) = agent.finish("broken").await;
    assert_eq!((total.errors.body, total.ok), (10, 0));

    let slow = MockProvider::spawn().await;
    let agent = Running::start(&format!("{}/x-mock/delay=3000", slow.base_url())).await;
    agent.prepare(&plan("long", false, None, 60)).await;
    agent.call(reqwest::Method::POST, "/v1/runs/long/start", None).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let (status, body) = agent.call(reqwest::Method::POST, "/v1/runs/long/cancel", None).await;
    assert_eq!((status.as_u16(), body["state"].as_str()), (202, Some("cancelling")));
    let (report, total) = agent.finish("long").await;
    assert_eq!((report.state, report.ended), (RunState::Cancelled, Some(Ended::Cancelled)));
    assert_eq!(total.errors.cancelled, 5);
}

#[tokio::test]
async fn a_run_nobody_asks_about_stops_when_its_lease_runs_out() {
    let mock = MockProvider::spawn().await;
    let agent = Running::start(&format!("{}/x-mock/delay=100", mock.base_url())).await;
    let mut p = plan("orphan", false, None, 60);
    p.lease_s = 5;
    agent.prepare(&p).await;
    agent.call(reqwest::Method::POST, "/v1/runs/orphan/start", None).await;
    tokio::time::sleep(Duration::from_secs(7)).await;
    let (_, info) = agent.call(reqwest::Method::GET, "/v1/info", None).await;
    assert_eq!(info["busy"], Value::Null, "{info}");
    let report = agent.report("orphan").await;
    assert_eq!((report.state, report.ended), (RunState::Cancelled, Some(Ended::LeaseExpired)));
}

#[tokio::test]
async fn a_plan_can_aim_at_another_gateway_and_a_refused_key_makes_no_load() {
    let mock = MockProvider::spawn().await;
    // The agent's own target is a closed port: only the plan's base URL can answer.
    let agent = Running::start("http://127.0.0.1:9").await;
    let mut p = plan("external", false, Some(20), 30);
    p.base_url = Some(format!("{}/x-mock/delay=10", mock.base_url()));
    p.auth_header = AuthHeader::XApiKey;
    let (status, body) = agent.prepare(&p).await;
    assert_eq!((status.as_u16(), body["probe"]["status"].as_u64()), (201, Some(200)), "{body}");
    agent.call(reqwest::Method::POST, "/v1/runs/external/start", None).await;
    let (_, total) = agent.finish("external").await;
    assert_eq!(total.ok, 20);
    let calls = mock.captured().into_iter().filter(|c| c.method == "POST").collect::<Vec<_>>();
    assert!(calls.iter().all(|c| c.headers.iter().any(|(n, _)| n == "x-api-key")));

    let mut bad = plan("bad", false, Some(20), 30);
    bad.base_url = Some("http://user:secret@example.com".into());
    let (status, body) = agent.prepare(&bad).await;
    assert_eq!(status, 422, "{body}");

    let mut refused = plan("refused", false, Some(20), 30);
    refused.base_url = Some(format!("{}/x-mock/status=401", mock.base_url()));
    let (status, body) = agent.prepare(&refused).await;
    assert_eq!((status.as_u16(), body["state"].as_str()), (201, Some("failed")), "{body}");
    assert!(!body.to_string().contains("secret-that-must-not-leak"), "{body}");
    let (status, _) = agent.call(reqwest::Method::POST, "/v1/runs/refused/start", None).await;
    assert_eq!(status, 409);
    let report = agent.report("refused").await;
    assert!(report.error.contains("refused the key"), "{}", report.error);
    let before = mock.captured().len();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(mock.captured().len(), before, "no load after a refused probe");
}
