//! Template pools and routing schemes (concept section 7.5), end to end: a gateway in process,
//! two mock providers, a real PostgreSQL.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

/// A gateway with an OpenAI-compatible provider (`openai`, models `openai/m` and
/// `openai/gpt-test`) and an Anthropic provider (`backup`, model `backup/claude`), each at
/// its own mock.
struct Setup {
    gw: TestGw,
    sink: IngestSink,
    fx: Fixture,
    a: MockProvider,
    b: MockProvider,
}

async fn setup() -> Setup {
    let sink = IngestSink::start().await;
    let a = MockProvider::spawn().await;
    let b = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", a.base_url())).await;
    let backup =
        gw.create("/providers", json!({ "name": "backup", "family": "anthropic", "base_url": b.base_url() })).await.id;
    gw.create(&format!("/providers/{backup}/credentials"), json!({ "label": "shared", "secret": PROVIDER_SECRET }))
        .await;
    let id = gw.draft_model(&backup, "backup/claude", "claude-test", json!({ "free": true })).await;
    gw.activate(&id).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    Setup { gw, sink, fx, a, b }
}

fn script(mock: &MockProvider, specs: &[&str]) {
    mock.set_script(Behavior::parse_all(specs).expect("script"));
}

impl Setup {
    fn ws_path(&self, rest: &str) -> String {
        format!("/orgs/{}/workspaces/{}{rest}", self.fx.org, self.fx.ws)
    }

    async fn scheme(&self, body: Value) -> String {
        self.gw.create(&format!("/orgs/{}/routing-schemes", self.fx.org), body).await.id
    }

    async fn chat(&self, model: &str, headers: &[(&str, &str)], extra: Value) -> reqwest::Response {
        let mut body = json!({ "model": model, "messages": [{ "role": "user", "content": "hello" }] });
        body.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let mut req = self.gw.http.post(format!("{}/v1/chat/completions", self.gw.data)).bearer_auth(&self.fx.key);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        req.json(&body).send().await.unwrap()
    }

    /// Replaces an object through the management API with a change to its document.
    async fn edit(&self, path: &str, change: impl FnOnce(&mut Value)) -> (u16, Value) {
        let (st, mut doc) = self.gw.api("GET", path, None).await;
        assert_eq!(st, 200, "{doc}");
        change(&mut doc);
        self.gw.api("PUT", path, Some(doc)).await
    }
}

fn header<'a>(r: &'a reqwest::Response, name: &str) -> Option<&'a str> {
    r.headers().get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn a_failing_model_hands_the_call_to_the_next_in_its_dialect() {
    let s = setup().await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "models": [
        { "model": "openai/m" },
        { "model": "backup/claude", "priority": 1 }
    ] }),
    )
    .await;
    // Counts every call of the workspace, and calls on the first model only.
    for (name, filter) in [("all", json!({})), ("only-m", json!({ "models": ["openai/m"] }))] {
        s.gw.create(
            &s.ws_path("/limits"),
            json!({ "name": name, "subject": "workspace", "metric": "requests", "window": "hour",
                    "amount": 100.0, "filter": filter }),
        )
        .await;
    }
    script(&s.a, &["status=503"]);

    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "x-gw-model-served"), Some("backup/claude"));
    assert_eq!(header(&r, "x-gw-failovers"), Some("1"));
    assert_eq!(header(&r, "x-gw-attempts"), Some("2"), "one attempt each: the next model is the retry");
    assert_eq!(header(&r, "x-gw-route"), None, "no scheme: the built-in routing by priority");
    let body: Value = r.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion", "the Anthropic answer is translated for the OpenAI caller");
    assert_eq!(s.a.captured().len(), 1);
    assert_eq!(s.b.captured()[0].path, "/v1/messages");

    let e = s.sink.wait_events(1).await.remove(0);
    assert_eq!((e.model_served.as_str(), e.provider.as_str()), ("backup/claude", "backup"));
    assert_eq!((e.failovers, e.models_tried.clone()), (1, vec!["openai/m".to_owned()]));
    assert_eq!(e.routing_source, "built_in");
    assert_eq!(e.status, "ok");

    // The call counts once, against the limits of the model that served it.
    let (_, state) = s.gw.api("GET", &s.ws_path("/limit-state"), None).await;
    let used = |name: &str| state.as_array().unwrap().iter().find(|l| l["name"] == name).map(|l| l["used"].clone());
    assert_eq!(used("all"), Some(json!(1.0)));
    assert_eq!(used("only-m"), Some(json!(0.0)));

    // The outage the backup absorbed still shows.
    let (_, health) = s.gw.api("GET", "/monitoring/health", None).await;
    let m = health.as_array().unwrap().iter().find(|h| h["model"] == "openai/m").expect("openai/m in health");
    assert_eq!((m["calls"].as_u64(), m["failed"].as_u64()), (Some(1), Some(1)), "{health}");
}

#[tokio::test]
async fn what_moves_a_call_on_and_what_does_not() {
    let s = setup().await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "timeout_seconds": 1, "models": [
        { "model": "openai/m" }, { "model": "backup/claude", "priority": 1 }
    ] }),
    )
    .await;

    // The request itself is wrong: the provider's answer stands, nothing else is tried.
    script(&s.a, &["status=400"]);
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!(r.status(), 400);
    assert_eq!(header(&r, "x-gw-failovers"), None);
    assert!(s.b.captured().is_empty());

    // A timeout does not move on by default: the provider may be generating already.
    script(&s.a, &["delay=1500"]);
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!(r.status(), 504);
    assert!(s.b.captured().is_empty());

    // The caller allows one attempt in all: no failover.
    script(&s.a, &["status=503"]);
    let r = s.chat("@bot", &[("x-gw-retries", "1")], json!({})).await;
    assert_eq!(r.status(), 502);
    assert_eq!(header(&r, "x-gw-attempts"), Some("1"));
    assert!(s.b.captured().is_empty());

    // A scheme that moves on after timeouts.
    s.scheme(json!({ "name": "patient", "failover": { "on": { "timeout": true } } })).await;
    let path = s.ws_path("/templates");
    let (_, list) = s.gw.api("GET", &path, None).await;
    let id = list["items"][0]["id"].as_str().unwrap().to_owned();
    let (st, v) = s.edit(&format!("{path}/{id}"), |d| d["spec"]["routing_scheme"] = json!("patient")).await;
    assert_eq!(st, 200, "{v}");
    script(&s.a, &["delay=1500"]);
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "x-gw-model-served"), Some("backup/claude"));
    assert_eq!(header(&r, "x-gw-route"), Some("patient/default"));
}

#[tokio::test]
async fn a_rate_limited_model_is_set_aside_for_as_long_as_it_asks() {
    let s = setup().await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "models": [
        { "model": "openai/m" }, { "model": "backup/claude", "priority": 1 }
    ] }),
    )
    .await;
    script(&s.a, &["status=429,retry_after=30"]);
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!((r.status().as_u16(), header(&r, "x-gw-model-served")), (200, Some("backup/claude")));
    // The breaker is open for 30 s: the next call goes to the backup first.
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!(header(&r, "x-gw-model-served"), Some("backup/claude"));
    assert_eq!(header(&r, "x-gw-failovers"), None);
    assert_eq!(s.a.captured().len(), 1);
    assert_eq!(s.b.captured().len(), 2);

    let (st, state) = s.gw.api("GET", "/monitoring/routing", None).await;
    assert_eq!(st, 200, "{state}");
    assert!(state.as_array().unwrap().iter().any(|t| t["breaker"] == "open"), "{state}");
}

#[tokio::test]
async fn a_streamed_call_moves_on_before_its_first_byte_only() {
    let s = setup().await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "models": [
        { "model": "openai/m" }, { "model": "backup/claude", "priority": 1 }
    ] }),
    )
    .await;
    script(&s.a, &["status=500"]);
    let r = s.chat("@bot", &[], json!({ "stream": true })).await;
    assert_eq!(r.status(), 200);
    assert_eq!(header(&r, "x-gw-model-served"), Some("backup/claude"));
    let text = r.text().await.unwrap();
    assert!(text.contains("chat.completion.chunk") && text.contains("[DONE]"), "{text}");

    // Once the answer has started, a broken stream is the caller's to see.
    script(&s.a, &["reset_after=1,chunks=3"]);
    let r = s.chat("@bot", &[], json!({ "stream": true })).await;
    assert_eq!(header(&r, "x-gw-model-served"), Some("openai/m"));
    let _ = r.text().await;
    assert_eq!(s.b.captured().len(), 1);
}

#[tokio::test]
async fn a_limit_on_one_model_sends_the_call_to_another() {
    let s = setup().await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "models": [
        { "model": "openai/m" }, { "model": "backup/claude", "priority": 1 }
    ] }),
    )
    .await;
    s.gw.create(
        &s.ws_path("/limits"),
        json!({ "name": "no-m", "subject": "workspace", "metric": "requests", "window": "day", "amount": 0.0,
                "filter": { "models": ["openai/m"] } }),
    )
    .await;
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!((r.status().as_u16(), header(&r, "x-gw-model-served")), (200, Some("backup/claude")));
    assert!(s.a.captured().is_empty());
    let e = s.sink.wait_events(1).await.remove(0);
    assert_eq!(e.models_tried, ["openai/m"]);

    // A limit on every model is the answer.
    s.gw.create(
        &s.ws_path("/limits"),
        json!({ "name": "none", "subject": "workspace", "metric": "requests", "window": "day", "amount": 0.0 }),
    )
    .await;
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!(r.status(), 402, "a day's quota is used up");
}

#[tokio::test]
async fn rules_route_on_what_the_caller_says() {
    let s = setup().await;
    s.scheme(json!({ "name": "tiers", "rules": [
        { "name": "cheap", "when": { "complexity": ["low"] }, "then": { "tiers": ["small"] } },
        { "name": "review", "when": { "task": { "in": ["code-review"] }, "agent_id": { "prefix": ["ci-"] } },
          "then": { "tiers": ["large"] } },
        { "name": "search", "when": { "tags": { "purpose": { "in": ["search"] } } }, "then": { "tiers": ["backup"] } }
    ], "default": { "tiers": ["large"] } }))
        .await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "routing_scheme": "tiers", "models": [
        { "model": "openai/m", "tier": "small" },
        { "model": "openai/gpt-test", "tier": "large" },
        { "model": "backup/claude", "tier": "backup" }
    ] }),
    )
    .await;

    let served = |r: &reqwest::Response| {
        (header(r, "x-gw-model-served").map(str::to_owned), header(r, "x-gw-route").map(str::to_owned))
    };
    let r = s.chat("@bot", &[("x-gw-complexity", "low")], json!({})).await;
    assert_eq!(served(&r), (Some("openai/m".into()), Some("tiers/cheap".into())));
    let r = s.chat("@bot", &[], json!({ "gateway": { "task": "code-review", "agent_id": "ci-7" } })).await;
    assert_eq!(served(&r), (Some("openai/gpt-test".into()), Some("tiers/review".into())));
    let r = s.chat("@bot", &[("x-gw-task", "code-review"), ("x-gw-agent-id", "human")], json!({})).await;
    assert_eq!(served(&r), (Some("openai/gpt-test".into()), Some("tiers/default".into())), "the agent does not match");
    let r = s.chat("@bot", &[("x-gw-tags", "purpose=search")], json!({})).await;
    assert_eq!(served(&r), (Some("backup/claude".into()), Some("tiers/search".into())));

    let events = s.sink.wait_events(4).await;
    let rules: Vec<&str> = events.iter().map(|e| e.routing_rule.as_str()).collect();
    assert!(rules.contains(&"cheap") && rules.contains(&"search"), "{rules:?}");
    assert!(events.iter().all(|e| e.routing_scheme == "tiers" && e.routing_source == "template"));
}

#[tokio::test]
async fn the_most_specific_assignment_wins_unless_the_organisation_locks_it() {
    let s = setup().await;
    for (name, tier) in [("to-small", "small"), ("to-large", "large")] {
        s.scheme(json!({ "name": name, "default": { "tiers": [tier] } })).await;
    }
    let tpl =
        s.gw.template(
            &s.fx,
            "bot",
            json!({ "models": [
            { "model": "openai/m", "tier": "small" }, { "model": "openai/gpt-test", "tier": "large" }
        ] }),
        )
        .await
        .id;
    let route = async || {
        let r = s.chat("@bot", &[], json!({})).await;
        header(&r, "x-gw-route").map(str::to_owned)
    };
    assert_eq!(route().await, None);

    let org = format!("/orgs/{}", s.fx.org);
    let (st, v) = s.edit(&org, |d| d["settings"]["routing_scheme"] = json!("to-small")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(route().await.as_deref(), Some("to-small/default"));

    let ws = s.ws_path("");
    let (st, v) = s.edit(&ws, |d| d["settings"]["routing_scheme"] = json!("to-large")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(route().await.as_deref(), Some("to-large/default"));

    let tpl_path = s.ws_path(&format!("/templates/{tpl}"));
    s.edit(&tpl_path, |d| d["spec"]["routing_scheme"] = json!("to-small")).await;
    assert_eq!(route().await.as_deref(), Some("to-small/default"));

    // The organisation locks the setting: its scheme applies everywhere.
    let (st, v) = s
        .edit(&org, |d| {
            d["settings"]["routing_scheme"] = json!("to-large");
            d["settings"]["locked"] = json!(["routing_scheme"]);
        })
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(route().await.as_deref(), Some("to-large/default"));
    // The template keeps the scheme it named and stays editable; naming another is refused.
    let (st, v) = s.edit(&tpl_path, |d| d["description"] = json!("still editable")).await;
    assert_eq!(st, 200, "{v}");
    let (st, check) = s.gw.api("GET", &format!("{tpl_path}/check"), None).await;
    assert_eq!((st, check["routing"]["scheme"].as_str()), (200, Some("to-large")), "{check}");
    assert!(check["routing"]["warnings"][0].as_str().unwrap().contains("organisation decides"), "{check}");
    let (st, v) = s.edit(&tpl_path, |d| d["spec"]["routing_scheme"] = json!("to-large")).await;
    assert_eq!((st, v["error"]["details"]["code"].as_str()), (422, Some("routing_scheme_locked")), "{v}");
}

#[tokio::test]
async fn an_input_too_long_for_a_model_goes_to_one_that_takes_it() {
    let s = setup().await;
    s.gw.model(&s.fx, "openai/short", "short", json!({ "free": true, "context_window": 1000 })).await;
    s.gw.model(&s.fx, "openai/long", "long", json!({ "free": true, "context_window": 1000000 })).await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "models": [
        { "model": "openai/short" }, { "model": "openai/long", "priority": 1 } ] }),
    )
    .await;
    let r = s.chat("@bot", &[], json!({})).await;
    assert_eq!(header(&r, "x-gw-model-served"), Some("openai/short"));
    let r = s.chat("@bot", &[], json!({ "messages": [{ "role": "user", "content": "x".repeat(8_000) }] })).await;
    assert_eq!(header(&r, "x-gw-model-served"), Some("openai/long"), "about 2000 tokens do not fit in 1000");
}

#[tokio::test]
async fn schemes_are_checked_and_cannot_vanish_while_used() {
    let s = setup().await;
    let schemes = format!("/orgs/{}/routing-schemes", s.fx.org);
    let (st, v) =
        s.gw.api(
            "POST",
            &schemes,
            Some(json!({ "name": "bad", "rules": [
            { "name": "a", "then": {} }, { "name": "a", "then": {} } ] })),
        )
        .await;
    assert_eq!(
        (st, v["error"]["details"]["code"].as_str(), v["error"]["details"]["rule"].as_str()),
        (422, Some("duplicate_rule"), Some("a")),
        "{v}"
    );
    let (st, v) = s.gw.api("POST", &schemes, Some(json!({ "name": "x", "default": { "colour": "red" } }))).await;
    assert_eq!(st, 422, "unknown fields are refused: {v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("default.colour"), "{v}");

    let id = s
        .scheme(json!({ "name": "tiers", "rules": [
        { "name": "gpu", "when": {}, "then": { "tiers": ["gpu"] } } ] }))
        .await;
    let (st, v) =
        s.gw.api(
            "POST",
            &s.ws_path("/templates"),
            Some(json!({ "name": "bot", "spec": { "routing_scheme": "nope", "model": "openai/m" } })),
        )
        .await;
    assert_eq!((st, v["error"]["details"]["code"].as_str()), (422, Some("routing_scheme_unknown")), "{v}");
    let tpl =
        s.gw.template(&s.fx, "bot", json!({ "routing_scheme": "tiers", "models": [{ "model": "openai/m" }] })).await.id;

    // The check names tiers the scheme wants and the pool lacks.
    let (st, check) = s.gw.api("GET", &s.ws_path(&format!("/templates/{tpl}/check")), None).await;
    assert_eq!(st, 200, "{check}");
    assert_eq!(check["routing"]["scheme"], "tiers");
    assert_eq!(check["routing"]["source"], "template");
    assert!(check["routing"]["warnings"][0].as_str().unwrap().contains("gpu"), "{check}");
    assert_eq!(check["models"][0]["model"], "openai/m");

    let one = format!("{schemes}/{id}");
    let (st, v) = s.gw.api("DELETE", &one, None).await;
    assert_eq!((st, v["error"]["details"]["code"].as_str()), (409, Some("routing_scheme_in_use")), "{v}");
    assert_eq!(v["error"]["details"]["by"][0], "template prod/bot");
    let (st, v) = s.edit(&one, |d| d["name"] = json!("renamed")).await;
    assert_eq!(st, 409, "{v}");

    // Pools are checked too.
    let (st, v) =
        s.gw.api(
            "POST",
            &s.ws_path("/templates"),
            Some(json!({ "name": "dup", "spec": { "models": [
            { "model": "openai/m" }, { "model": "openai/m", "tier": "x" } ] } })),
        )
        .await;
    assert_eq!((st, v["error"]["details"]["code"].as_str()), (422, Some("duplicate_model")), "{v}");
    let (st, _) =
        s.gw.api(
            "POST",
            &s.ws_path("/templates"),
            Some(json!({ "name": "both", "spec": { "model": "openai/m",
            "models": [{ "model": "openai/m" }] } })),
        )
        .await;
    assert_eq!(st, 422);
}

#[tokio::test]
async fn explain_shows_the_rule_and_the_candidates() {
    let s = setup().await;
    s.scheme(json!({ "name": "tiers", "failover": { "max_targets": 5 }, "rules": [
        { "name": "cheap", "when": { "complexity": ["low"] }, "then": { "tiers": ["small"], "overflow": "rest_of_pool" } }
    ] }))
    .await;
    let tpl =
        s.gw.template(
            &s.fx,
            "bot",
            json!({ "routing_scheme": "tiers", "models": [
            { "model": "openai/gpt-test", "tier": "large" }, { "model": "openai/m", "tier": "small" }
        ] }),
        )
        .await
        .id;
    let (st, v) =
        s.gw.api("POST", &s.ws_path(&format!("/templates/{tpl}/route")), Some(json!({ "complexity": "low" }))).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (v["scheme"].as_str(), v["rule"].as_str(), v["source"].as_str()),
        (Some("tiers"), Some("cheap"), Some("template"))
    );
    let models: Vec<&str> = v["candidates"].as_array().unwrap().iter().map(|c| c["model"].as_str().unwrap()).collect();
    assert_eq!(models, ["openai/m", "openai/gpt-test"]);
    assert_eq!(v["candidates"][0]["role"], "primary");
    assert_eq!(v["candidates"][1]["role"], "backup");

    // The playground's preview shows the routing too.
    let (st, v) =
        s.gw.api(
            "POST",
            &s.ws_path("/playground/transform"),
            Some(json!({ "interface": "openai", "headers": { "x-gw-complexity": "low" },
                         "request": { "model": "@bot", "messages": [{ "role": "user", "content": "hi" }] } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["routing"]["rule"], "cheap");
    assert_eq!(v["model_served"], "openai/m");
}

#[tokio::test]
async fn proxy_mode_routes_among_the_models_at_the_providers_url() {
    let s = setup().await;
    s.scheme(json!({ "name": "rr", "default": { "algorithm": "round_robin" } })).await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "routing_scheme": "rr", "models": [
        { "model": "backup/claude" }, { "model": "openai/m" }, { "model": "openai/gpt-test" }
    ] }),
    )
    .await;
    let mut served = Vec::new();
    for _ in 0..4 {
        let r =
            s.gw.http
                .post(format!("{}/proxy/openai/v1/chat/completions", s.gw.data))
                .bearer_auth(&s.fx.key)
                .json(&json!({ "model": "@bot", "messages": [{ "role": "user", "content": "hi" }] }))
                .send()
                .await
                .unwrap();
        assert_eq!(r.status(), 200);
        served.push(header(&r, "x-gw-model-served").unwrap().to_owned());
    }
    assert!(!served.iter().any(|m| m == "backup/claude"), "only models at the URL's provider: {served:?}");
    assert!(served.contains(&"openai/m".to_owned()) && served.contains(&"openai/gpt-test".to_owned()), "{served:?}");
    assert!(s.b.captured().is_empty());

    // A template whose models are all elsewhere does not fit this provider.
    s.gw.template(&s.fx, "elsewhere", json!({ "models": [{ "model": "backup/claude" }] })).await;
    let r =
        s.gw.http
            .post(format!("{}/proxy/openai/v1/chat/completions", s.gw.data))
            .bearer_auth(&s.fx.key)
            .json(&json!({ "model": "@elsewhere", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 400);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "template_provider_mismatch", "{v}");

    // A failing model hands the call on to the next one at this provider, before the first byte.
    s.scheme(json!({ "name": "steady", "failover": { "on": { "server_error": true } } })).await;
    s.gw.template(
        &s.fx,
        "pair",
        json!({ "routing_scheme": "steady", "models": [
        { "model": "openai/m", "priority": 1 }, { "model": "openai/gpt-test", "priority": 2 } ] }),
    )
    .await;
    s.a.clear();
    script(&s.a, &["fail=500x1"]);
    let r =
        s.gw.http
            .post(format!("{}/proxy/openai/v1/chat/completions", s.gw.data))
            .bearer_auth(&s.fx.key)
            .json(&json!({ "model": "@pair", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!((header(&r, "x-gw-model-served"), header(&r, "x-gw-failovers")), (Some("openai/gpt-test"), Some("1")));
    let sent: Vec<_> = s.a.captured().iter().map(|c| c.json().unwrap()["model"].clone()).collect();
    assert_eq!(sent, [json!("m"), json!("gpt-test")], "the same body, with the next model written in");
}

#[tokio::test]
async fn export_and_import_carry_schemes_and_pools() {
    let s = setup().await;
    s.scheme(json!({ "name": "tiers", "default": { "algorithm": "weighted" } })).await;
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "routing_scheme": "tiers", "models": [
        { "model": "openai/m", "weight": 3 }, { "model": "backup/claude" } ] }),
    )
    .await;
    let (st, export) = s.gw.api("GET", "/system/config-export", None).await;
    assert_eq!(st, 200);
    let kinds: Vec<&str> = export["objects"].as_array().unwrap().iter().map(|o| o["kind"].as_str().unwrap()).collect();
    let at = |k: &str| kinds.iter().position(|x| *x == k).unwrap();
    assert!(at("organisation") < at("routing_scheme") && at("routing_scheme") < at("workspace"));
    // Importing the same configuration changes nothing.
    let (st, v) = s.gw.api("POST", "/system/config-import?dry_run=true", Some(export)).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!((v["created"].clone(), v["updated"].clone()), (json!([]), json!([])), "{v}");
}

#[tokio::test]
async fn the_organisations_admins_assign_its_scheme_and_its_lock() {
    let s = setup().await;
    for (name, tier) in [("to-small", "small"), ("to-large", "large")] {
        s.scheme(json!({ "name": name, "default": { "tiers": [tier] } })).await;
    }
    s.gw.template(
        &s.fx,
        "bot",
        json!({ "routing_scheme": "to-small", "models": [
        { "model": "openai/m", "tier": "small" }, { "model": "openai/gpt-test", "tier": "large" } ] }),
    )
    .await;
    let path = format!("/orgs/{}/routing-assignment", s.fx.org);

    // A viewer of the organisation reads it but cannot change it; its administrators can.
    let mut tokens = Vec::new();
    for (email, role) in [("viewer@example.test", "viewer"), ("oa@example.test", "org_admin")] {
        let user = s.gw.create("/users", json!({ "email": email, "password": "a long password 1" })).await.id;
        s.gw.create(
            "/role-bindings",
            json!({ "principal": user, "role": role, "scope": { "type": "organisation", "id": s.fx.org } }),
        )
        .await;
        tokens.push(s.gw.login(email, "a long password 1").await);
    }
    let (viewer, org_admin) = (&tokens[0], &tokens[1]);
    let (st, v) = s.gw.api_as(viewer, "GET", &path, None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (v["scheme"].clone(), v["locked"].clone(), v["writable"].clone()),
        (Value::Null, json!(false), json!(false))
    );
    assert_eq!(v["own_assignments"], json!(["template prod/bot: to-small"]));
    let (st, _) = s.gw.api_as(viewer, "PUT", &path, Some(json!({ "scheme": "to-large", "locked": true }))).await;
    assert_eq!(st, 403);
    let (_, a) = s.gw.api_as(org_admin, "GET", &path, None).await;
    assert_eq!(a["writable"], json!(true));

    // Assigned and locked, the organisation's scheme wins over the template's.
    let route = async || header(&s.chat("@bot", &[], json!({})).await, "x-gw-route").map(str::to_owned);
    assert_eq!(route().await.as_deref(), Some("to-small/default"));
    let (st, v) =
        s.gw.api("PUT", &path, Some(json!({ "scheme": "to-large", "locked": true, "version": v["version"] }))).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(
        (v["scheme"].as_str(), v["locked"].as_bool(), v["writable"].as_bool()),
        (Some("to-large"), Some(true), Some(true))
    );
    assert_eq!(route().await.as_deref(), Some("to-large/default"));
    let (_, org) = s.gw.api("GET", &format!("/orgs/{}", s.fx.org), None).await;
    assert_eq!(
        (org["settings"]["routing_scheme"].as_str(), org["settings"]["locked"].clone()),
        (Some("to-large"), json!(["routing_scheme"]))
    );

    // Unlocked, the template's own applies again; a stale version and an unknown scheme are refused.
    let (st, v2) =
        s.gw.api("PUT", &path, Some(json!({ "scheme": "to-large", "locked": false, "version": v["version"] }))).await;
    assert_eq!(st, 200, "{v2}");
    assert_eq!(route().await.as_deref(), Some("to-small/default"));
    let (st, e) =
        s.gw.api("PUT", &path, Some(json!({ "scheme": null, "locked": false, "version": v["version"] }))).await;
    assert_eq!((st, e["error"]["details"]["code"].as_str()), (409, Some("version_conflict")), "{e}");
    let (st, e) = s.gw.api("PUT", &path, Some(json!({ "scheme": "nope", "locked": false }))).await;
    assert_eq!((st, e["error"]["details"]["code"].as_str()), (422, Some("routing_scheme_unknown")), "{e}");
    // The scheme the organisation names cannot be deleted.
    let (_, list) = s.gw.api("GET", &format!("/orgs/{}/routing-schemes", s.fx.org), None).await;
    let large = list["items"].as_array().unwrap().iter().find(|i| i["name"] == "to-large").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (st, e) = s.gw.api("DELETE", &format!("/orgs/{}/routing-schemes/{large}", s.fx.org), None).await;
    assert_eq!((st, e["error"]["details"]["by"][0].as_str()), (409, Some("organisation acme")), "{e}");
}
