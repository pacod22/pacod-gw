//! Templates in proxy mode (concept section 7.4), end to end: a gateway in process, a mock
//! provider that records the exact bytes it receives, a real PostgreSQL.

mod common;

use common::{Fixture, IngestSink, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

async fn setup(family: &str) -> (TestGw, MockProvider, IngestSink, Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let base = if family == "openai" { format!("{}/v1", mock.base_url()) } else { mock.base_url() };
    let fx = gw.fixture(family, &base).await;
    (gw, mock, sink, fx)
}

/// The request of concept appendix B, "Proxy mode with a template".
const PLANNER_REQUEST: &str = "{\"model\":\"@planner\",\"max_tokens\":8000,\n       \"messages\":[{\"role\":\"user\",\"content\":\"Plan the next step.\"}]}";
/// What the concept says Anthropic receives.
const PLANNER_UPSTREAM: &str = "{\"model\":\"claude-haiku-4-5\",\"max_tokens\":400,\n       \"messages\":[{\"role\":\"user\",\"content\":\"Plan the next step.\"}]}";

#[tokio::test]
async fn the_planner_example_reaches_the_provider_as_the_stated_bytes() {
    let (gw, mock, sink, fx) = setup("anthropic").await;
    gw.model(
        &fx,
        "anthropic/claude-haiku-4-5",
        "claude-haiku-4-5",
        json!({ "free": true, "capability_preset": "anthropic-budget" }),
    )
    .await;
    gw.template(
        &fx,
        "planner",
        json!({ "model": "anthropic/claude-haiku-4-5", "max_output_tokens": 400, "tags": { "purpose": "planner" } }),
    )
    .await;

    let resp = gw
        .http
        .post(format!("{}/proxy/anthropic/v1/messages", gw.data))
        .header("x-api-key", &fx.key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .header("x-gw-end-user", "u_1")
        .body(PLANNER_REQUEST)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "{}", resp.text().await.unwrap());
    assert_eq!(resp.headers()["x-gw-model-served"], "anthropic/claude-haiku-4-5");

    let captured = mock.captured();
    assert_eq!(std::str::from_utf8(&captured[0].body).unwrap(), PLANNER_UPSTREAM);
    assert_eq!(captured[0].header("content-length"), Some(PLANNER_UPSTREAM.len().to_string().as_str()));

    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.template.as_str(), e.template_version), ("planner", 1));
    assert_eq!(e.model_requested, "@planner");
    assert_eq!(e.model_served, "anthropic/claude-haiku-4-5");
    assert_eq!(e.tags["purpose"], "planner", "the template's tags are on the event");
    assert_eq!(e.end_user, "u_1");
    assert_eq!(e.usage_source, "provider");
    assert_eq!(e.cost, Some(0.0), "a model marked free costs nothing, and says so");

    // The template can be named in a header instead; the body's own model is then replaced.
    mock.clear();
    let resp = gw
        .http
        .post(format!("{}/proxy/anthropic/v1/messages", gw.data))
        .header("x-api-key", &fx.key)
        .header("anthropic-version", "2023-06-01")
        .header("x-gw-template", "planner")
        .json(
            &json!({ "model": "claude-opus-9", "max_tokens": 100, "messages": [{ "role": "user", "content": "hi" }] }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let sent: Value = mock.captured()[0].json().unwrap();
    assert_eq!(sent["model"], "claude-haiku-4-5");
    assert_eq!(sent["max_tokens"], 100, "a value under the ceiling is the caller's to choose");
}

#[tokio::test]
async fn a_template_must_fit_the_provider_and_the_key() {
    let (gw, mock, _sink, fx) = setup("anthropic").await;
    gw.model(
        &fx,
        "anthropic/claude-haiku-4-5",
        "claude-haiku-4-5",
        json!({ "free": true, "capability_preset": "anthropic-budget" }),
    )
    .await;
    gw.template(&fx, "planner", json!({ "model": "anthropic/claude-haiku-4-5", "max_output_tokens": 400 })).await;

    // A second provider with a template of its own.
    let other = gw
        .create(
            "/providers",
            json!({ "name": "openai", "family": "openai", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{other}/credentials"), json!({ "secret": "sk-other" })).await;
    let gpt = gw.draft_model(&other, "openai/gpt-x", "gpt-x", json!({ "free": true })).await;
    gw.activate(&gpt).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": gpt })).await;
    gw.template(&fx, "writer", json!({ "model": "openai/gpt-x" })).await;

    let call = |key: String, model: &'static str| {
        let (http, url) = (gw.http.clone(), format!("{}/proxy/anthropic/v1/messages", gw.data));
        async move {
            let r = http
                .post(url)
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .json(&json!({ "model": model, "max_tokens": 10, "messages": [{ "role": "user", "content": "hi" }] }))
                .send()
                .await
                .unwrap();
            let kind = r.headers().get("x-gw-error-type").map(|v| v.to_str().unwrap().to_owned());
            (r.status().as_u16(), kind, r.json::<Value>().await.unwrap())
        }
    };

    // The template's model is at another provider than the one in the URL.
    let (st, kind, body) = call(fx.key.clone(), "@writer").await;
    assert_eq!((st, kind.as_deref()), (400, Some("template_provider_mismatch")), "{body}");
    assert_eq!(body["type"], "error", "the error is in Anthropic's envelope");
    assert_eq!(body["error"]["type"], "invalid_request_error");

    let (st, kind, _) = call(fx.key.clone(), "@nothing").await;
    assert_eq!((st, kind.as_deref()), (404, Some("template_not_found")));

    // A key restricted to one template, and required to name one.
    let strict =
        gw.key(&fx, json!({ "name": "strict", "allowed_templates": ["planner"], "require_template": true })).await;
    let (st, kind, _) = call(strict.clone(), "claude-test").await;
    assert_eq!((st, kind.as_deref()), (403, Some("not_allowed")), "a model id is not a template");
    let (st, kind, _) = call(strict.clone(), "@writer").await;
    assert_eq!((st, kind.as_deref()), (403, Some("not_allowed")), "not a template this key may use");
    let (st, _, _) = call(strict, "@planner").await;
    assert_eq!(st, 200);

    // A model the workspace was not granted is not served, even by the provider's own id.
    let (st, kind, _) = call(fx.key.clone(), "claude-opus-9").await;
    assert_eq!((st, kind.as_deref()), (404, Some("model_not_found")));
    assert_eq!(mock.captured().len(), 1, "only the allowed call reached the provider");
}

#[tokio::test]
async fn a_check_template_writes_nothing_and_rejects_what_does_not_conform() {
    let (gw, mock, sink, fx) = setup("openai").await;
    gw.model(&fx, "openai/gpt-x", "gpt-x", json!({ "free": true, "capability_preset": "openai-chat" })).await;
    gw.template(
        &fx,
        "strict",
        json!({ "model": "openai/gpt-x", "proxy": "check", "max_output_tokens": 500, "temperature": { "value": 0.2, "locked": true } }),
    )
    .await;

    let url = format!("{}/proxy/openai/v1/chat/completions", gw.data);
    let send = |body: &'static str| {
        let (http, url, key) = (gw.http.clone(), url.clone(), fx.key.clone());
        async move {
            let r = http
                .post(url)
                .bearer_auth(key)
                .header("content-type", "application/json")
                .header("x-gw-template", "strict")
                .body(body)
                .send()
                .await
                .unwrap();
            let kind = r.headers().get("x-gw-error-type").map(|v| v.to_str().unwrap().to_owned());
            (r.status().as_u16(), kind, r.text().await.unwrap())
        }
    };

    // A conforming request passes as the bytes it came in.
    let ok = "{ \"model\":\"gpt-x\", \"temperature\":0.2,  \"max_tokens\":500,\"messages\":[{\"role\":\"user\",\"content\":\"hi\"}] }";
    let (st, _, body) = send(ok).await;
    assert_eq!(st, 200, "{body}");
    assert_eq!(std::str::from_utf8(&mock.captured()[0].body).unwrap(), ok);
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.template.as_str(), e.model_served.as_str()), ("strict", "openai/gpt-x"));

    for (body, why) in [
        (r#"{"model":"gpt-test","temperature":0.2,"max_tokens":500,"messages":[]}"#, "requires the model"),
        (r#"{"model":"gpt-x","temperature":0.9,"max_tokens":500,"messages":[]}"#, "temperature must be 0.2"),
        (r#"{"model":"gpt-x","temperature":0.2,"max_tokens":501,"messages":[]}"#, "at most 500"),
        (r#"{"model":"gpt-x","temperature":0.2,"messages":[]}"#, "must be set"),
    ] {
        let (st, kind, text) = send(body).await;
        assert_eq!((st, kind.as_deref()), (400, Some("template_violation")), "{body}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            v["error"]["code"], "template_violation",
            "OpenAI's envelope carries the gateway's type as the code"
        );
        assert!(v["error"]["message"].as_str().unwrap().contains(why), "{text}");
    }
    assert_eq!(mock.captured().len(), 1, "a rejected request never reaches the provider");
}

#[tokio::test]
async fn a_template_can_ask_for_usage_on_streams_and_set_sampling_parameters() {
    let (gw, mock, sink, fx) = setup("openai").await;
    gw.priced_model(
        &fx,
        "openai/gpt-x",
        "gpt-x",
        json!({ "capability_preset": "openai-chat" }),
        json!({ "currency": "USD", "input": 2.0, "output": 10.0 }),
    )
    .await;
    gw.template(
        &fx,
        "stream",
        json!({
            "model": "openai/gpt-x", "request_stream_usage": true,
            "temperature": { "value": 0.3 }, "top_p": { "omit": true },
            "frequency_penalty": { "value": 0.5, "locked": true }
        }),
    )
    .await;
    // A key that insists on usage: the template is what makes the stream acceptable.
    let key = gw.key(&fx, json!({ "name": "strict-usage", "usage_policy": "require" })).await;

    let body = "{\"model\":\"@stream\",\"stream\":true,\"top_p\":0.9,\"frequency_penalty\":0,\"messages\":[{\"role\":\"user\",\"content\":\"hello  world\"}]}";
    let resp = gw
        .http
        .post(format!("{}/proxy/openai/v1/chat/completions", gw.data))
        .bearer_auth(&key)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let text = resp.text().await.unwrap();
    assert!(text.contains("\"usage\":{"), "the client receives the extra usage chunk: {text}");
    assert!(!text.contains("\"cost\""), "proxy mode never changes the response");

    // Locked value written over the caller's, omitted one removed, default and model added;
    // the messages are the same bytes.
    assert_eq!(
        std::str::from_utf8(&mock.captured()[0].body).unwrap(),
        "{\"model\":\"gpt-x\",\"stream\":true,\"frequency_penalty\":0.5,\"messages\":[{\"role\":\"user\",\"content\":\"hello  world\"}],\"temperature\":0.3,\"stream_options\":{\"include_usage\":true}}"
    );

    let e = sink.wait_events(1).await.remove(0);
    assert_eq!(e.usage_source, "provider");
    assert!(e.usage.input_tokens > 0 && e.usage.output_tokens > 0);
    let expected = (e.usage.input_tokens as f64 * 2.0 + e.usage.output_tokens as f64 * 10.0) / 1_000_000.0;
    assert!((e.cost.unwrap() - expected).abs() < 1e-12, "{:?} vs {expected}", e.cost);
    assert_eq!((e.currency.as_str(), e.price_version), ("USD", 1));

    // Without the template the same key's stream is refused: nothing would report usage.
    let r = gw
        .http
        .post(format!("{}/proxy/openai/v1/chat/completions", gw.data))
        .bearer_auth(&key)
        .json(&json!({ "model": "gpt-x", "stream": true, "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-gw-error-type"], "usage_required");
}

#[tokio::test]
async fn a_retired_model_is_followed_only_through_a_template_and_only_at_the_same_provider() {
    let (gw, mock, sink, fx) = setup("openai").await;
    let old = gw.model(&fx, "openai/gpt-old", "gpt-old", json!({ "free": true })).await;
    let new = gw.model(&fx, "openai/gpt-new", "gpt-new", json!({ "free": true })).await;
    gw.template(&fx, "writer", json!({ "model": "openai/gpt-old" })).await;

    let url = format!("{}/proxy/openai/v1/chat/completions", gw.data);
    let call = |model: &'static str| {
        let (http, url, key) = (gw.http.clone(), url.clone(), fx.key.clone());
        async move {
            let r =
                http.post(url).bearer_auth(key).json(&json!({ "model": model, "messages": [] })).send().await.unwrap();
            let h = |n: &str| r.headers().get(n).map(|v| v.to_str().unwrap().to_owned());
            (r.status().as_u16(), h("x-gw-error-type"), h("x-gw-model-deprecated"), h("x-gw-model-served"))
        }
    };

    // Deprecated: still served, and the response says until when and what follows.
    let (st, v) = gw
        .api(
            "POST",
            &format!("/models/{old}/transition"),
            Some(json!({ "to": "deprecated", "successor_id": new, "sunset_at": "2027-03-01T00:00:00Z" })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let (st, _, deprecated, served) = call("gpt-old").await;
    assert_eq!(st, 200);
    assert_eq!(deprecated.as_deref(), Some("2027-03-01; successor=openai/gpt-new"));
    assert_eq!(served.as_deref(), Some("openai/gpt-old"));

    // Retired: the provider's own id is refused, the template follows the successor.
    let (st, v) = gw.api("POST", &format!("/models/{old}/transition"), Some(json!({ "to": "retired" }))).await;
    assert_eq!(st, 200, "{v}");
    let (st, kind, _, _) = call("gpt-old").await;
    assert_eq!((st, kind.as_deref()), (410, Some("model_retired")));
    mock.clear();
    let (st, _, _, served) = call("@writer").await;
    assert_eq!(st, 200);
    assert_eq!(served.as_deref(), Some("openai/gpt-new"));
    assert_eq!(mock.captured()[0].json().unwrap()["model"], "gpt-new");
    let ev = sink.wait_events(3).await;
    let e = ev.iter().find(|e| e.template == "writer").unwrap();
    assert_eq!((e.model_requested.as_str(), e.model_served.as_str()), ("@writer", "openai/gpt-new"));

    // The emergency switch.
    let (st, _) = gw.api("POST", &format!("/models/{new}/transition"), Some(json!({ "to": "disabled" }))).await;
    assert_eq!(st, 200);
    let (st, kind, _, _) = call("gpt-new").await;
    assert_eq!((st, kind.as_deref()), (503, Some("model_disabled")));
    let (st, _) = gw.api("POST", &format!("/models/{new}/transition"), Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 200);
    assert_eq!(call("gpt-new").await.0, 200);
}

#[tokio::test]
async fn a_field_a_newer_release_adds_loses_no_template_or_scheme() {
    let (gw, _mock, _sink, fx) = setup("openai").await;
    let scheme = gw.create(&format!("/orgs/{}/routing-schemes", fx.org), json!({ "name": "spread" })).await.id;
    let tpl = gw
        .template(&fx, "pool", json!({ "models": [{ "model": "openai/gpt-test" }], "routing_scheme": "spread" }))
        .await
        .id;

    // The API refuses a field it does not know, deep in the spec as at the top.
    let (st, v) = gw
        .api(
            "POST",
            &format!("/orgs/{}/workspaces/{}/templates", fx.org, fx.ws),
            Some(json!({ "name": "other", "spec": { "models": [{ "model": "openai/gpt-test", "future": 1 }] } })),
        )
        .await;
    assert_eq!(st, 422, "{v}");
    assert!(v.to_string().contains("future"), "{v}");

    // A newer release writes such a field into both stored documents; this release keeps them.
    let pool = sqlx::PgPool::connect(&gw.database_url).await.unwrap();
    for (id, path) in [(&tpl, "{versions,0,spec,models,0,future}"), (&scheme, "{failover,future}")] {
        sqlx::query("UPDATE config_objects SET doc = jsonb_set(doc, $2::text[], '1') WHERE id = $1")
            .bind(id)
            .bind(path)
            .execute(&pool)
            .await
            .unwrap();
    }
    // Rows edited by hand are not in the change log; the instance is told to read everything.
    gw.app.gw.config.reload_all().await.unwrap();
    let (_, status) = gw.api("GET", "/system/status", None).await;
    assert_eq!(status["problems"], json!([]), "{status}");
    let r = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "@pool", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
}

/// A proxy call through the fixture's key to the template `@name`: the status, and the
/// `max_tokens` the provider received.
async fn call_template(gw: &TestGw, mock: &MockProvider, key: &str, name: &str) -> (u16, Option<u64>) {
    mock.clear();
    let resp = gw
        .http
        .post(format!("{}/proxy/anthropic/v1/messages", gw.data))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({ "model": format!("@{name}"), "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let sent = mock.captured().first().and_then(|c| c.json().ok());
    (status, sent.and_then(|s| s["max_tokens"].as_u64()))
}

#[tokio::test]
async fn workspaces_inherit_the_organisations_templates() {
    let (gw, mock, sink, fx) = setup("anthropic").await;
    gw.model(
        &fx,
        "anthropic/claude-haiku-4-5",
        "claude-haiku-4-5",
        json!({ "free": true, "capability_preset": "anthropic-budget" }),
    )
    .await;
    let org_templates = format!("/orgs/{}/templates", fx.org);
    let shared = gw
        .create(
            &org_templates,
            json!({ "name": "planner", "spec": { "model": "anthropic/claude-haiku-4-5", "max_output_tokens": 400 } }),
        )
        .await;
    assert_eq!(shared.body["current_version"], 1);

    // The workspace's key reaches the organisation's template by its name.
    assert_eq!(call_template(&gw, &mock, &fx.key, "planner").await, (200, Some(400)));
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.template.as_str(), e.template_version), ("planner", 1));

    let inherited = format!("/orgs/{}/workspaces/{}/inherited-templates", fx.org, fx.ws);
    let (st, list) = gw.api("GET", &inherited, None).await;
    assert_eq!(st, 200, "{list}");
    assert_eq!(list[0]["id"], shared.id.as_str());
    assert_eq!((&list[0]["hidden"], &list[0]["problem"]), (&json!(false), &Value::Null));

    // A template of the workspace's own with the same name applies instead, until it is deleted.
    let own =
        gw.template(&fx, "planner", json!({ "model": "anthropic/claude-haiku-4-5", "max_output_tokens": 100 })).await;
    assert_eq!(call_template(&gw, &mock, &fx.key, "planner").await, (200, Some(100)));
    assert_eq!(gw.api("GET", &inherited, None).await.1[0]["hidden"], true);
    let (st, v) = gw.api("DELETE", &format!("/orgs/{}/workspaces/{}/templates/{}", fx.org, fx.ws, own.id), None).await;
    assert_eq!(st, 204, "{v}");
    assert_eq!(call_template(&gw, &mock, &fx.key, "planner").await, (200, Some(400)));

    // Versions and rollback work as for a workspace's template.
    let one = format!("{org_templates}/{}", shared.id);
    let (st, v) = gw
        .api(
            "PUT",
            &one,
            Some(json!({ "name": "planner", "spec": { "model": "anthropic/claude-haiku-4-5", "max_output_tokens": 300 } })),
        )
        .await;
    assert_eq!((st, &v["current_version"]), (200, &json!(2)), "{v}");
    assert_eq!(call_template(&gw, &mock, &fx.key, "planner").await, (200, Some(300)));
    let (st, versions) = gw.api("GET", &format!("{one}/versions"), None).await;
    assert_eq!((st, versions.as_array().map(Vec::len)), (200, Some(2)), "{versions}");
    let (st, v) = gw.api("POST", &format!("{one}/rollback"), Some(json!({ "version": 1 }))).await;
    assert_eq!((st, &v["current_version"]), (200, &json!(1)), "{v}");
    assert_eq!(call_template(&gw, &mock, &fx.key, "planner").await, (200, Some(400)));
    let (st, v) = gw.api("GET", &format!("{one}/check"), None).await;
    assert_eq!((st, &v["routing"]["source"]), (200, &json!("built_in")), "{v}");

    // A key restricted to the template keeps it from being deleted.
    gw.key(&fx, json!({ "name": "only-planner", "allowed_templates": ["planner"] })).await;
    let (st, v) = gw.api("DELETE", &one, None).await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"]["details"]["keys"], json!(["prod/only-planner"]));

    // What is bound to one workspace stays with workspaces' templates.
    for (field, value, code) in [
        ("pool_from", json!({ "query": { "kind": "model" } }), "org_template_pool_from"),
        ("broker_offer", json!({ "visibility": "organisation" }), "org_template_broker_offer"),
    ] {
        let mut spec = json!({ "model": "anthropic/claude-haiku-4-5" });
        spec[field] = value;
        let (st, v) = gw.api("POST", &org_templates, Some(json!({ "name": "broker", "spec": spec }))).await;
        assert_eq!((st, &v["error"]["details"]["code"]), (422, &json!(code)), "{v}");
    }
}

#[tokio::test]
async fn an_inherited_template_on_a_model_the_workspace_is_not_granted_reports_it() {
    let (gw, mock, _sink, fx) = setup("anthropic").await;
    let haiku = gw
        .model(
            &fx,
            "anthropic/claude-haiku-4-5",
            "claude-haiku-4-5",
            json!({ "free": true, "capability_preset": "anthropic-budget" }),
        )
        .await;
    // The workspace narrows the organisation's grants to one model.
    gw.create(&format!("/orgs/{}/workspaces/{}/grants", fx.org, fx.ws), json!({ "model_id": haiku })).await;
    gw.create(
        &format!("/orgs/{}/templates", fx.org),
        json!({ "name": "writer", "spec": { "model": "anthropic/claude-test" } }),
    )
    .await;

    let (st, list) = gw.api("GET", &format!("/orgs/{}/workspaces/{}/inherited-templates", fx.org, fx.ws), None).await;
    assert_eq!(st, 200, "{list}");
    let problem = list[0]["problem"].as_str().unwrap_or_default();
    assert!(problem.contains("not granted to this workspace"), "{list}");
    let (status, sent) = call_template(&gw, &mock, &fx.key, "writer").await;
    assert_eq!((status, sent), (404, None), "the workspace's grants apply to what it inherits");
}

#[tokio::test]
async fn a_workspace_admin_reads_what_the_workspace_inherits_but_cannot_change_it() {
    let (gw, _mock, _sink, fx) = setup("anthropic").await;
    let shared = gw
        .create(
            &format!("/orgs/{}/templates", fx.org),
            json!({ "name": "planner", "spec": { "model": "anthropic/m" } }),
        )
        .await;
    let user =
        gw.create("/users", json!({ "email": "ws-admin@example.test", "password": "a long enough password" })).await;
    gw.create(
        "/role-bindings",
        json!({ "principal": user.id, "role": "workspace_admin", "scope": { "type": "workspace", "id": fx.ws } }),
    )
    .await;
    let token = gw.login("ws-admin@example.test", "a long enough password").await;
    let one = format!("/orgs/{}/templates/{}", fx.org, shared.id);

    for path in [
        format!("/orgs/{}/templates", fx.org),
        one.clone(),
        format!("{one}/versions"),
        format!("{one}/check"),
        format!("/orgs/{}/workspaces/{}/inherited-templates", fx.org, fx.ws),
    ] {
        let (st, v) = gw.api_as(&token, "GET", &path, None).await;
        assert_eq!(st, 200, "GET {path}: {v}");
    }
    let body = json!({ "name": "planner", "spec": { "model": "anthropic/m", "max_output_tokens": 10 } });
    for (method, path, body) in [
        (
            "POST",
            format!("/orgs/{}/templates", fx.org),
            Some(json!({ "name": "other", "spec": { "model": "anthropic/m" } })),
        ),
        ("PUT", one.clone(), Some(body)),
        ("POST", format!("{one}/rollback"), Some(json!({ "version": 1 }))),
        ("DELETE", one.clone(), None),
    ] {
        let (st, v) = gw.api_as(&token, method, &path, body).await;
        assert_eq!(st, 403, "{method} {path}: {v}");
    }
}
