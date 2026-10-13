//! The real log of the minimal gateway, in JSON. Alone in its test binary: `tracing` caches
//! whether a log statement is wanted the first time any thread reaches it, and a test running
//! beside this one without a subscriber could make it seem unwanted here.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use pacod_gw_lite::config::{self, LogFormat};
use pacod_gw_lite::env::Vars;
use pacod_gw_lite::server::Server;
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

const OPENAI_KEY: &str = "sk-openai-test-secret";
const ANTHROPIC_KEY: &str = "sk-ant-test-secret";
const APP: &str = "pgw_app_AppSecret0123456789abcdef";

fn chat(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The real log, in JSON. A current-thread runtime keeps the server's tasks on this thread, so
/// the subscriber set for it sees every line.
#[tokio::test(flavor = "current_thread")]
async fn the_json_log_has_one_line_per_call_and_no_secret() {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let _guard = tracing::subscriber::set_default(pacod_gw_lite::log::subscriber(
        LogFormat::Json,
        Some("info"),
        false,
        move || writer.clone(),
    ));
    let mock = MockProvider::spawn().await;
    let openai = format!("{}/v1", mock.base_url());
    let vars = Vars::from_pairs([
        ("PGW_LISTEN", "127.0.0.1:0"),
        ("PGW_EGRESS_ALLOW_PRIVATE", "127.0.0.0/8"),
        ("PGW_DRAIN_SECONDS", "1"),
        ("PGW_LOG_BODIES", "on_request"),
        ("PGW_PROVIDER_OPENAI_KEY", OPENAI_KEY),
        ("PGW_PROVIDER_OPENAI_BASE_URL", &openai),
        ("PGW_PROVIDER_ANTHROPIC_KEY", ANTHROPIC_KEY),
        ("PGW_PROVIDER_ANTHROPIC_BASE_URL", &mock.base_url()),
        ("PGW_MODEL_GPT", "openai/gpt-test"),
        ("PGW_MODEL_GPT_PRICE", "input=1,output=2"),
        ("PGW_MODEL_CLAUDE", "anthropic/claude-test"),
        ("PGW_MODEL_CLAUDE_PRICE", "input=3,output=15"),
        ("PGW_POOL_FAST", "openai/gpt-test,anthropic/claude-test"),
        ("PGW_CLIENT_APP_KEY", APP),
    ]);
    let cfg = config::read(&vars).unwrap();
    let server = Server::start(&cfg, Arc::new(pacod_gw_lite::log::LogSink)).await.unwrap();
    let url = format!("http://{}", server.addr);
    let http = reqwest::Client::new();
    let openai_call = |model: &str, text: &str| {
        http.post(format!("{url}/v1/chat/completions")).bearer_auth(APP).json(&chat(model, text))
    };

    let r = openai_call("@fast", "logged").send().await.unwrap();
    assert_eq!(r.status(), 200);
    let _ = r.bytes().await;
    let r = openai_call("anthropic/claude-test", "with bodies").header("x-gw-audit", "true").send().await.unwrap();
    assert_eq!(r.status(), 200);
    let request_id = r.headers()["x-gw-request-id"].to_str().unwrap().to_owned();
    let _ = r.bytes().await;
    let r = http.get(format!("{url}/v1/models")).bearer_auth("pgw_app_NotTheSecret0123456789").send().await.unwrap();
    assert_eq!(r.status(), 401);
    tokio::time::sleep(Duration::from_millis(200)).await;
    server.shutdown().await;

    let text = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).expect("every line is JSON")).collect();
    let of = |target: &str| lines.iter().filter(|l| l["target"] == target).collect::<Vec<_>>();

    let calls = of("pacod_gw_lite::call");
    assert_eq!(calls.len(), 2, "{text}");
    let pool = calls[0];
    assert_eq!((pool["message"].as_str(), pool["level"].as_str()), (Some("call"), Some("INFO")));
    assert_eq!(
        (pool["key"].as_str(), pool["model"].as_str(), pool["pool"].as_str()),
        (Some("app"), Some("@fast"), Some("fast"))
    );
    assert_eq!((pool["served"].as_str(), pool["provider"].as_str()), (Some("openai/gpt-test"), Some("openai")));
    assert_eq!(
        (pool["status"].as_str(), pool["http"].as_u64(), pool["currency"].as_str()),
        (Some("ok"), Some(200), Some("USD"))
    );
    assert!(pool["cost"].as_f64().unwrap() > 0.0);
    assert!(pool["latency_ms"].as_f64().is_some() && pool["input_tokens"].as_u64().is_some());
    assert!(pool.get("error").is_none(), "empty fields are left out");

    let bodies = of("pacod_gw_lite::body");
    assert_eq!(bodies.len(), 1, "only the call that asked");
    assert_eq!(bodies[0]["request_id"].as_str(), Some(request_id.as_str()));
    assert!(bodies[0]["request"].as_str().unwrap().contains("with bodies"));
    assert!(bodies[0]["response"].as_str().unwrap().contains("echo: with bodies"));

    let refused = of("pacod_gw_lite::request");
    assert_eq!(refused.len(), 1, "{text}");
    assert_eq!((refused[0]["message"].as_str(), refused[0]["http"].as_u64()), (Some("rejected"), Some(401)));
    assert_eq!((refused[0]["key"].as_str(), refused[0]["path"].as_str()), (Some("app"), Some("/v1/models")));

    for secret in [OPENAI_KEY, ANTHROPIC_KEY, APP, "AppSecret", "NotTheSecret"] {
        assert!(!text.contains(secret), "the log shows {secret}");
    }
}
