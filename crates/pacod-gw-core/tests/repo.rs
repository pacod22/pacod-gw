//! The management API of the model repository, templates and limits (concept sections 6, 7,
//! 12 and 13.1), end to end against a real PostgreSQL.

mod common;

use common::{Fixture, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

async fn setup() -> (TestGw, MockProvider, Fixture) {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    (gw, mock, fx)
}

fn code(v: &Value) -> &str {
    v["error"]["type"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn a_model_passes_its_gates_before_it_becomes_callable() {
    let (gw, _mock, fx) = setup().await;
    let (st, presets) = gw.api("GET", "/capability-presets", None).await;
    assert_eq!(st, 200);
    assert!(presets.as_array().unwrap().iter().any(|p| p["name"] == "anthropic-adaptive"));

    let m = gw
        .create(
            "/models",
            json!({ "name": "openai/gpt-x", "provider_id": fx.provider, "upstream_id": "gpt-x", "capability_preset": "openai-reasoning",
                    "context_window": 400000, "max_output_tokens": 100000 }),
        )
        .await;
    assert_eq!(m.body["state"], "draft");
    assert_eq!(m.body["capabilities"]["max_tokens_field"], "max_completion_tokens");
    assert_eq!(m.body["capabilities"]["temperature"], "rejected");
    let path = format!("/models/{}", m.id);
    let transition = format!("{path}/transition");

    // A draft cannot be granted, and names follow the rules.
    let (st, v) = gw.api("POST", &format!("/orgs/{}/grants", fx.org), Some(json!({ "model_id": m.id }))).await;
    assert_eq!(st, 422, "{v}");
    for bad in ["@template-like", "has space", ""] {
        let (st, _) = gw
            .api("POST", "/models", Some(json!({ "name": bad, "provider_id": fx.provider, "upstream_id": "x" })))
            .await;
        assert_eq!(st, 422, "{bad}");
    }
    let (st, _) = gw
        .api(
            "POST",
            "/models",
            Some(json!({ "name": "openai/gpt-x", "provider_id": fx.provider, "upstream_id": "other" })),
        )
        .await;
    assert_eq!(st, 409, "model ids are unique");
    let (st, _) =
        gw.api("POST", "/models", Some(json!({ "name": "x/y", "provider_id": "prv_nope", "upstream_id": "y" }))).await;
    assert_eq!(st, 422);

    // Gate 1: a price, or an explicit "free".
    let (st, v) = gw.api("POST", &transition, Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 422, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("no price"));
    let (st, price) =
        gw.api("PUT", &format!("{path}/price"), Some(json!({ "currency": "USD", "input": 2.0, "output": 10.0 }))).await;
    assert_eq!(st, 201, "{price}");
    assert_eq!(
        (price["version"].as_u64(), price["source"].as_str(), price["in_effect"].as_bool()),
        (Some(1), Some("manual"), Some(true))
    );

    // A dry run changes nothing and shows what the change would touch.
    let (st, dry) = gw.api("POST", &format!("{transition}?dry_run=true"), Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 200, "{dry}");
    assert_eq!(
        (dry["applied"].as_bool(), dry["from"].as_str(), dry["to"].as_str()),
        (Some(false), Some("draft"), Some("active"))
    );
    assert_eq!(gw.api("GET", &path, None).await.1["state"], "draft");

    // Gate 2: the provider knows the model. The probe asks it.
    let (st, probe) = gw.api("POST", &format!("{path}/probe"), None).await;
    assert_eq!((st, probe["ok"].as_bool()), (200, Some(true)), "{probe}");
    let unknown = gw
        .create(
            "/models",
            json!({ "name": "openai/ghost", "provider_id": fx.provider, "upstream_id": "unknown-model", "free": true }),
        )
        .await
        .id;
    let (st, v) = gw.api("POST", &format!("/models/{unknown}/transition"), Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 422, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("probe failed"), "{v}");
    assert_eq!(v["error"]["details"]["ok"], false);

    let (st, done) = gw.api("POST", &transition, Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 200, "{done}");
    assert_eq!(
        (done["applied"].as_bool(), done["model"]["state"].as_str(), done["model"]["priced"].as_bool()),
        (Some(true), Some("active"), Some(true))
    );

    // The life cycle allows only its own steps, and only drafts can be deleted.
    let (st, v) = gw.api("POST", &transition, Some(json!({ "to": "draft" }))).await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(gw.api("DELETE", &path, None).await.0, 409);
    assert_eq!(gw.api("DELETE", &format!("/models/{unknown}"), None).await.0, 204);

    // Disabling remembers where to return to.
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": "openai/gpt-x", "max_output_tokens": 5000 }))
        .await;
    let (st, _) =
        gw.api("POST", &transition, Some(json!({ "to": "deprecated", "sunset_at": "2027-06-30T00:00:00Z" }))).await;
    assert_eq!(st, 200);
    assert_eq!(gw.api("POST", &transition, Some(json!({ "to": "disabled" }))).await.0, 200);
    let (st, v) = gw.api("POST", &transition, Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 409, "a disabled model returns to the state it had: {v}");
    let (st, back) = gw.api("POST", &transition, Some(json!({ "to": "deprecated" }))).await;
    assert_eq!(st, 200, "{back}");
    assert_eq!(back["model"]["sunset_at"], "2027-06-30T00:00:00Z");

    // What uses the model is listed before it is retired.
    gw.template(&fx, "writer", json!({ "model": "openai/m" })).await;
    let (st, impact) = gw.api("GET", &format!("{path}/impact"), None).await;
    assert_eq!(st, 200);
    assert_eq!(impact["organisations"], json!(["acme"]));
    assert_eq!(impact["workspaces"], json!(["acme/prod"]));

    // Every transition is in the audit log.
    let (_, log) = gw.api("GET", "/audit-log?limit=50", None).await;
    let actions: Vec<&str> = log["items"].as_array().unwrap().iter().filter_map(|e| e["action"].as_str()).collect();
    for expected in [
        "transition:draft->active",
        "transition:active->deprecated",
        "transition:deprecated->disabled",
        "transition:disabled->deprecated",
    ] {
        assert!(actions.contains(&expected), "{expected} missing from {actions:?}");
    }
}

#[tokio::test]
async fn prices_have_versions_and_never_apply_backwards() {
    let (gw, _mock, fx) = setup().await;
    let m = gw.draft_model(&fx.provider, "openai/gpt-x", "gpt-x", json!({})).await;
    let price = format!("/models/{m}/price");

    let (st, v) = gw.api("PUT", &price, Some(json!({ "currency": "usd", "input": 1.0 }))).await;
    assert_eq!(st, 422, "{v}");
    let (st, v) = gw.api("PUT", &price, Some(json!({ "currency": "USD", "input": -1.0 }))).await;
    assert_eq!(st, 422, "{v}");
    let (st, v) = gw
        .api("PUT", &price, Some(json!({ "currency": "USD", "input": 1.0, "effective_from": "2020-01-01T00:00:00Z" })))
        .await;
    assert_eq!(st, 422, "{v}");

    let (st, _) = gw.api("PUT", &price, Some(json!({ "currency": "USD", "input": 3.0, "output": 15.0 }))).await;
    assert_eq!(st, 201);
    let (st, second) = gw
        .api(
            "PUT",
            &price,
            Some(json!({ "currency": "USD", "input": 2.0, "output": 10.0, "cache_read": 0.2,
                         "variants": { "batch": { "input": 1.0, "output": 5.0 } }, "evidence_url": "https://example.test/pricing" })),
        )
        .await;
    assert_eq!(st, 201, "{second}");
    assert_eq!(second["version"], 2);

    let (_, versions) = gw.api("GET", &format!("/models/{m}/prices"), None).await;
    let v = versions.as_array().unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(
        (v[0]["version"].as_u64(), v[0]["in_effect"].as_bool(), v[0]["status"].as_str()),
        (Some(2), Some(true), Some("active"))
    );
    assert_eq!(
        (v[1]["version"].as_u64(), v[1]["in_effect"].as_bool(), v[1]["status"].as_str()),
        (Some(1), Some(false), Some("superseded"))
    );
    assert_eq!(v[0]["variants"]["batch"]["output"], 5.0);

    // A price announced for later does not replace the one in effect.
    let later = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
    let (st, third) =
        gw.api("PUT", &price, Some(json!({ "currency": "USD", "input": 9.0, "effective_from": later }))).await;
    assert_eq!(st, 201, "{third}");
    let (_, current) = gw.api("GET", "/prices", None).await;
    let c = current.as_array().unwrap().iter().find(|p| p["model"] == "openai/gpt-x").unwrap();
    assert_eq!((c["version"].as_u64(), c["input"].as_f64()), (Some(2), Some(2.0)));
}

#[tokio::test]
async fn grants_are_decided_one_level_up() {
    let (gw, _mock, fx) = setup().await;
    let m = gw.draft_model(&fx.provider, "openai/gpt-x", "gpt-x", json!({ "free": true })).await;
    gw.activate(&m).await;

    let mut tokens = std::collections::HashMap::new();
    for (name, role, scope) in [
        ("orgadmin", "org_admin", json!({ "type": "organisation", "id": fx.org })),
        ("wsadmin", "workspace_admin", json!({ "type": "workspace", "id": fx.ws })),
    ] {
        let email = format!("{name}@example.test");
        let user = gw.create("/users", json!({ "email": email, "password": "a long password 1" })).await.id;
        gw.create("/role-bindings", json!({ "principal": user, "role": role, "scope": scope })).await;
        tokens.insert(name, gw.login(&email, "a long password 1").await);
    }
    let org_grants = format!("/orgs/{}/grants", fx.org);
    let ws_grants = format!("/orgs/{}/workspaces/{}/grants", fx.org, fx.ws);

    // The platform grants to an organisation; the organisation cannot grant to itself.
    let (st, _) = gw.api_as(&tokens["orgadmin"], "POST", &org_grants, Some(json!({ "model_id": m }))).await;
    assert_eq!(st, 403);
    // A workspace cannot be granted what its organisation does not have.
    let (st, v) = gw.api_as(&tokens["orgadmin"], "POST", &ws_grants, Some(json!({ "model_id": m }))).await;
    assert_eq!(st, 422, "{v}");
    let grant = gw.create(&org_grants, json!({ "model_id": "openai/gpt-x", "max_output_tokens": 8000 })).await;
    assert_eq!(grant.body["model_id"], m.as_str(), "a model id is stored as the model's object id");
    assert_eq!(grant.body["follow_successor"], true);
    assert_eq!(gw.api("POST", &org_grants, Some(json!({ "model_id": m }))).await.0, 409, "one grant per model");

    // The organisation grants to its workspaces; a workspace admin cannot widen its own access.
    let (st, _) = gw.api_as(&tokens["wsadmin"], "POST", &ws_grants, Some(json!({ "model_id": m }))).await;
    assert_eq!(st, 403);
    let (st, v) = gw
        .api_as(&tokens["orgadmin"], "POST", &ws_grants, Some(json!({ "model_id": m, "follow_successor": false })))
        .await;
    assert_eq!(st, 201, "{v}");
    assert_eq!(gw.api_as(&tokens["wsadmin"], "GET", &ws_grants, None).await.0, 200);

    // Limits follow the same rule: a cap on an organisation is the platform's to set.
    let org_limit = json!({ "name": "org-spend", "subject": "organisation", "metric": "spend", "window": "month", "amount": 1000.0 });
    let (st, _) =
        gw.api_as(&tokens["orgadmin"], "POST", &format!("/orgs/{}/limits", fx.org), Some(org_limit.clone())).await;
    assert_eq!(st, 403);
    assert_eq!(gw.api("POST", &format!("/orgs/{}/limits", fx.org), Some(org_limit)).await.0, 201);
}

#[tokio::test]
async fn templates_are_checked_versioned_and_copied() {
    let (gw, _mock, fx) = setup().await;
    gw.model(
        &fx,
        "openai/fixed",
        "fixed",
        json!({ "free": true, "capability_preset": "openai-compatible-fixed-temperature" }),
    )
    .await;
    let ungranted = gw.draft_model(&fx.provider, "openai/private", "private", json!({ "free": true })).await;
    gw.activate(&ungranted).await;
    let base = format!("/orgs/{}/workspaces/{}/templates", fx.org, fx.ws);

    // What a template may name.
    for (spec, why) in [
        (json!({ "model": "openai/nothing" }), "unknown_model"),
        (json!({ "model": "openai/private" }), "model_not_granted"),
    ] {
        let (st, v) = gw.api("POST", &base, Some(json!({ "name": "t", "spec": spec }))).await;
        assert_eq!(st, 422, "{v}");
        assert_eq!(v["error"]["details"]["code"], why);
    }
    for spec in [
        json!({ "model": "openai/m", "temperature": { "value": 3.0 } }),
        json!({ "model": "openai/m", "timeout_seconds": 601 }),
        json!({ "model": "openai/m", "retry_max_attempts": 0 }),
        json!({ "model": "openai/m", "tags": { "undeclared": "x" } }),
    ] {
        let (st, v) = gw.api("POST", &base, Some(json!({ "name": "t", "spec": spec }))).await;
        assert_eq!(st, 422, "{v}");
        assert_eq!(code(&v), "validation_failed", "{v}");
    }

    // Every change of the configuration is a version; other edits are not.
    let t = gw
        .create(&base, json!({ "name": "synthesizer", "spec": { "model": "openai/fixed", "temperature": { "value": 0.3 }, "max_output_tokens": 4000 }, "comment": "first" }))
        .await;
    let one = format!("{base}/{}", t.id);
    assert_eq!((t.body["current_version"].as_u64(), t.body["spec"]["model"].as_str()), (Some(1), Some("openai/fixed")));
    let (st, v) = gw
        .api(
            "PUT",
            &one,
            Some(json!({ "name": "synthesizer", "description": "answers", "spec": { "model": "openai/fixed", "temperature": { "value": 0.3 }, "max_output_tokens": 4000 } })),
        )
        .await;
    assert_eq!((st, v["current_version"].as_u64()), (200, Some(1)), "a description is not a new version: {v}");
    let (st, v) = gw
        .api("PUT", &one, Some(json!({ "name": "synthesizer", "spec": { "model": "openai/fixed", "max_output_tokens": 2000 }, "comment": "smaller" })))
        .await;
    assert_eq!((st, v["current_version"].as_u64()), (200, Some(2)), "{v}");
    assert_eq!(v["versions"].as_array().unwrap().len(), 2);
    assert_eq!(v["versions"][0]["comment"], "smaller");
    assert!(v["versions"][0]["created_by"].as_str().unwrap().starts_with("usr_"));

    // The template's own values the model will not take are shown, not hidden.
    let (st, first) = gw.api("GET", &format!("{one}/versions/1"), None).await;
    assert_eq!((st, first["spec"]["max_output_tokens"].as_u64()), (200, Some(4000)));
    let (st, v) = gw.api("POST", &format!("{one}/rollback"), Some(json!({ "version": 1 }))).await;
    assert_eq!(
        (st, v["current_version"].as_u64(), v["spec"]["max_output_tokens"].as_u64()),
        (200, Some(1), Some(4000)),
        "{v}"
    );
    let (st, check) = gw.api("GET", &format!("{one}/check"), None).await;
    assert_eq!(st, 200);
    assert_eq!(check["ignored_params"], json!(["temperature"]));
    assert_eq!(check["model_state"], "active");
    assert_eq!(gw.api("POST", &format!("{one}/rollback"), Some(json!({ "version": 9 }))).await.0, 404);

    // A copy must be valid where it lands.
    let other_ws = gw.create(&format!("/orgs/{}/workspaces", fx.org), json!({ "name": "staging" })).await.id;
    let (st, copy) =
        gw.api("POST", &format!("{one}/clone"), Some(json!({ "workspace_id": other_ws, "name": "synth-copy" }))).await;
    assert_eq!(st, 201, "{copy}");
    assert_eq!((copy["name"].as_str(), copy["current_version"].as_u64()), (Some("synth-copy"), Some(1)));
    let (_, listed) = gw.api("GET", &format!("/orgs/{}/workspaces/{other_ws}/templates", fx.org), None).await;
    assert_eq!(listed["items"].as_array().unwrap().len(), 1);

    // A template a key is restricted to cannot be deleted from under it.
    let keys = format!("/orgs/{}/workspaces/{}/keys", fx.org, fx.ws);
    gw.create(&keys, json!({ "name": "pinned", "allowed_templates": ["synthesizer"], "require_template": true })).await;
    let (st, v) = gw.api("DELETE", &one, None).await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"]["details"]["keys"], json!(["pinned"]));

    // A deprecated model cannot be chosen for a new template; an existing one keeps it.
    let (_, models) = gw.api("GET", "/models", None).await;
    let fixed = models["items"].as_array().unwrap().iter().find(|m| m["name"] == "openai/fixed").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        gw.api("POST", &format!("/models/{fixed}/transition"), Some(json!({ "to": "deprecated" }))).await.0,
        200
    );
    let (st, v) = gw.api("POST", &base, Some(json!({ "name": "late", "spec": { "model": "openai/fixed" } }))).await;
    assert_eq!(st, 422, "{v}");
    assert_eq!(v["error"]["details"]["code"], "model_not_assignable");
    let (st, _) = gw
        .api(
            "PUT",
            &one,
            Some(json!({ "name": "synthesizer", "spec": { "model": "openai/fixed", "max_output_tokens": 1000 } })),
        )
        .await;
    assert_eq!(st, 200, "the template may keep the model it already uses");
    let (_, check) = gw.api("GET", &format!("{one}/check"), None).await;
    assert_eq!(check["model_state"], "deprecated");
}

#[tokio::test]
async fn limits_and_overrides_are_validated() {
    let (gw, _mock, fx) = setup().await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    for bad in [
        json!({ "name": "Bad Name", "subject": "workspace", "metric": "requests", "window": "day", "amount": 1.0 }),
        json!({ "name": "neg", "subject": "workspace", "metric": "requests", "window": "day", "amount": -1.0 }),
        json!({ "name": "org", "subject": "organisation", "metric": "requests", "window": "day", "amount": 1.0 }),
        json!({ "name": "spm", "subject": "workspace", "metric": "spend", "window": "minute", "amount": 1.0 }),
        json!({ "name": "tag", "subject": "workspace", "metric": "requests", "window": "day", "amount": 1.0, "filter": { "tags": { "undeclared": "x" } } }),
        json!({ "name": "alert", "subject": "workspace", "metric": "requests", "window": "day", "amount": 1.0, "alert_thresholds": [0] }),
    ] {
        let (st, v) = gw.api("POST", &limits, Some(bad.clone())).await;
        assert_eq!(st, 422, "{bad}: {v}");
    }
    let ws_limit = gw
        .create(&limits, json!({ "name": "ws-requests", "subject": "workspace", "metric": "requests", "window": "day", "amount": 0.0, "alert_thresholds": [50, 80, 100] }))
        .await;
    assert_eq!(ws_limit.body["enforcement"], "request", "a limit is enforced unless it says otherwise");
    let user_limit = gw
        .create(&limits, json!({ "name": "user-tokens", "subject": "end_user", "metric": "tokens", "window": "week", "amount": 5000.0, "filter": { "tags": { "purpose": "chat" } } }))
        .await;
    assert_eq!(
        gw.api(
            "POST",
            &limits,
            Some(
                json!({ "name": "ws-requests", "subject": "key", "metric": "requests", "window": "day", "amount": 1.0 })
            )
        )
        .await
        .0,
        409
    );

    // Overrides belong to end-user limits.
    let overrides = format!("/orgs/{}/workspaces/{}/end-user-overrides", fx.org, fx.ws);
    for bad in [
        json!({ "limit_id": ws_limit.id, "end_user": "u_1", "amount": 10.0 }),
        json!({ "limit_id": user_limit.id, "end_user": " ", "amount": 10.0 }),
        json!({ "limit_id": user_limit.id, "end_user": "u_1" }),
        json!({ "limit_id": "lim_nope", "end_user": "u_1", "exempt": true }),
    ] {
        let (st, v) = gw.api("POST", &overrides, Some(bad.clone())).await;
        assert_eq!(st, 422, "{bad}: {v}");
    }
    gw.create(&overrides, json!({ "limit_id": user_limit.id, "end_user": "u_vip", "exempt": true })).await;
    assert_eq!(
        gw.api("POST", &overrides, Some(json!({ "limit_id": user_limit.id, "end_user": "u_vip", "amount": 1.0 })))
            .await
            .0,
        409
    );

    // The live state lists every limit of the workspace, whatever its filter.
    let (st, state) =
        gw.api("GET", &format!("/orgs/{}/workspaces/{}/limit-state?end_user=u_1", fx.org, fx.ws), None).await;
    assert_eq!(st, 200, "{state}");
    let names: Vec<&str> = state.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["ws-requests", "user-tokens"]);
    assert_eq!(
        (state[1]["amount"].as_f64(), state[1]["used"].as_f64(), state[1]["remaining"].as_f64()),
        (Some(5000.0), Some(0.0), Some(5000.0))
    );
    // An exempt end user has no end-user limit to show.
    let (_, vip) =
        gw.api("GET", &format!("/orgs/{}/workspaces/{}/limit-state?end_user=u_vip", fx.org, fx.ws), None).await;
    assert_eq!(vip.as_array().unwrap().len(), 1);

    // A limit of zero allows nothing, at once.
    let r = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "openai/m", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 402);
    assert_eq!(gw.api("DELETE", &format!("{limits}/{}", ws_limit.id), None).await.0, 204);
    let r = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "openai/m", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "removing the limit takes effect without a restart");
}

#[tokio::test]
async fn the_openapi_document_covers_the_repository() {
    let (gw, _mock, _fx) = setup().await;
    let (st, doc) = gw.api("GET", "/openapi.json", None).await;
    assert_eq!(st, 200);
    let paths = doc["paths"].as_object().unwrap();
    for p in [
        "/models",
        "/models/{id}/transition",
        "/models/{id}/price",
        "/prices",
        "/capability-presets",
        "/provider-presets",
        "/orgs/{org}/grants",
        "/orgs/{org}/limits",
        "/orgs/{org}/workspaces/{ws}/grants",
        "/orgs/{org}/workspaces/{ws}/templates",
        "/orgs/{org}/workspaces/{ws}/templates/{id}/rollback",
        "/orgs/{org}/workspaces/{ws}/limits",
        "/orgs/{org}/workspaces/{ws}/limit-state",
        "/orgs/{org}/workspaces/{ws}/end-user-overrides",
    ] {
        assert!(paths.contains_key(p), "{p} is missing from the OpenAPI document");
    }
    let schemas = doc["components"]["schemas"].as_object().unwrap();
    for s in ["TemplateSpec", "Capabilities", "LimitSpec", "Quirks", "TransitionInput"] {
        assert!(schemas.contains_key(s), "{s} is missing from the schemas");
    }
}

#[tokio::test]
async fn discovery_compares_the_provider_with_the_repository() {
    let (gw, _mock, fx) = setup().await;
    gw.model(&fx, "openai/gpt-4o", "gpt-4o", json!({ "free": true })).await;
    let (st, found) = gw.api("GET", &format!("/providers/{}/discovery", fx.provider), None).await;
    assert_eq!(st, 200, "{found}");
    let listed = found["listed"].as_array().unwrap();
    let by_id = |id: &str| listed.iter().find(|m| m["upstream_id"] == id).unwrap_or_else(|| panic!("{id} not listed"));
    assert_eq!(by_id("gpt-4o")["model"], "openai/gpt-4o", "known to the repository");
    assert!(by_id("gpt-4o-mini")["model"].is_null(), "a candidate for a draft");
    // The fixture's models are not on the provider's list.
    assert_eq!(found["missing"], json!(["openai/claude-test", "openai/gpt-test", "openai/m"]));

    // Nothing was created by looking.
    let (_, models) = gw.api("GET", "/models", None).await;
    assert!(models["items"].as_array().unwrap().iter().all(|m| m["name"] != "gpt-4o-mini"));

    // An unreachable provider is reported as such, not as an empty list.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let p = gw
        .create("/providers", json!({ "name": "dead", "family": "openai", "base_url": format!("http://{dead}/v1") }))
        .await
        .id;
    gw.create(&format!("/providers/{p}/credentials"), json!({ "secret": "x" })).await;
    let (st, v) = gw.api("GET", &format!("/providers/{p}/discovery"), None).await;
    assert_eq!((st, code(&v)), (502, "provider_unreachable"), "{v}");
}

#[tokio::test]
async fn the_playground_shows_and_makes_calls_as_the_workspace() {
    let (gw, mock, fx) = setup().await;
    gw.template(&fx, "writer", json!({ "model": "openai/m", "max_output_tokens": 300 })).await;
    let base = format!("/orgs/{}/workspaces/{}/playground", fx.org, fx.ws);
    let request = json!({ "model": "@writer", "max_tokens": 9000, "temperature": 0.2, "messages": [{ "role": "user", "content": "hello there" }] });

    // What would be sent, without sending it.
    let (st, preview) =
        gw.api("POST", &format!("{base}/transform"), Some(json!({ "interface": "openai", "request": request }))).await;
    assert_eq!(st, 200, "{preview}");
    assert_eq!((preview["route"].as_str(), preview["provider"].as_str()), (Some("same_dialect"), Some("openai")));
    assert_eq!(
        (preview["model_served"].as_str(), preview["template"].as_str(), preview["template_version"].as_u64()),
        (Some("openai/m"), Some("writer"), Some(1))
    );
    assert!(preview["url"].as_str().unwrap().ends_with("/v1/chat/completions"));
    assert_eq!(
        preview["body"],
        json!({ "model": "m", "max_tokens": 300, "temperature": 0.2, "messages": [{ "role": "user", "content": "hello there" }] })
    );
    assert!(preview["warnings"][0].as_str().unwrap().contains("lowered from 9000 to 300"));
    assert!(mock.captured().is_empty(), "a preview calls nobody");

    // The same model through the native interface is a translation.
    let native = json!({ "model": "openai/m", "messages": [{ "role": "user", "content": "hello there" }], "max_output_tokens": 50 });
    let (st, preview) = gw
        .api(
            "POST",
            &format!("{base}/transform"),
            Some(json!({ "interface": "native", "request": native, "headers": { "x-gw-end-user": "u_1" } })),
        )
        .await;
    assert_eq!((st, preview["route"].as_str()), (200, Some("translated")), "{preview}");
    assert_eq!(preview["body"]["messages"], json!([{ "role": "user", "content": "hello there" }]));

    // A request the data plane would refuse is refused here in the same words.
    let (st, v) = gw
        .api(
            "POST",
            &format!("{base}/transform"),
            Some(json!({ "interface": "openai", "request": { "model": "openai/nothing", "messages": [] } })),
        )
        .await;
    // What the gateway would refuse is a finding about the request, with the status an
    // application would get beside it.
    assert_eq!((st, code(&v)), (422, "model_not_found"), "{v}");
    assert_eq!(v["error"]["details"]["gateway_status"], 404);
    let (st, _) = gw
        .api(
            "POST",
            &format!("{base}/transform"),
            Some(json!({ "interface": "openai", "request": request, "headers": { "authorization": "Bearer x" } })),
        )
        .await;
    assert_eq!(st, 422, "only gateway headers are taken");

    // A test call is a real call, answered as the data plane answers.
    let r = gw
        .http
        .post(format!("{}/api/v1{base}/chat", gw.admin))
        .bearer_auth(&gw.token)
        .json(&json!({ "interface": "openai", "request": request }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["x-gw-request-id"].to_str().unwrap().starts_with("gw_"));
    assert_eq!(r.headers()["x-gw-model-served"], "openai/m");
    let v = r.json::<Value>().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "echo: hello there");
    assert_eq!(mock.captured().len(), 1);

    // Spending a workspace's allowance is its administrator's business, not a viewer's.
    let user = gw.create("/users", json!({ "email": "viewer@example.test", "password": "a long password 1" })).await.id;
    gw.create(
        "/role-bindings",
        json!({ "principal": user, "role": "viewer", "scope": { "type": "workspace", "id": fx.ws } }),
    )
    .await;
    let token = gw.login("viewer@example.test", "a long password 1").await;
    let (st, _) = gw
        .api_as(
            &token,
            "POST",
            &format!("{base}/transform"),
            Some(json!({ "interface": "openai", "request": request })),
        )
        .await;
    assert_eq!(st, 403);
}

#[tokio::test]
async fn the_configuration_travels_as_one_document_without_secrets() {
    let (gw, mock, fx) = setup().await;
    gw.priced_model(
        &fx,
        "openai/gpt-x",
        "gpt-x",
        json!({}),
        json!({ "currency": "USD", "input": 2.0, "output": 10.0 }),
    )
    .await;
    gw.template(&fx, "writer", json!({ "model": "openai/gpt-x", "max_output_tokens": 300 })).await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    gw.create(
        &limits,
        json!({ "name": "daily", "subject": "workspace", "metric": "requests", "window": "day", "amount": 100.0 }),
    )
    .await;

    gw.create("/rate-classes", json!({ "name": "batch", "requests_per_minute": 60 })).await;
    let (st, export) = gw.api("GET", "/system/config-export", None).await;
    assert_eq!(st, 200);
    assert_eq!(export["format"], "pacod-gw-config/1");
    let objects = export["objects"].as_array().unwrap();
    let kinds: std::collections::BTreeSet<&str> = objects.iter().map(|o| o["kind"].as_str().unwrap()).collect();
    for kind in
        ["provider", "model", "price", "organisation", "org_grant", "workspace", "template", "limit", "rate_class"]
    {
        assert!(kinds.contains(kind), "{kind} is missing from {kinds:?}");
    }
    // No secrets and no people.
    for kind in ["credential", "api_key", "user", "admin_token", "role_binding"] {
        assert!(!kinds.contains(kind), "{kind} must not be exported");
    }
    let text = export.to_string();
    assert!(
        !text.contains(common::PROVIDER_SECRET)
            && !text.contains(&fx.key)
            && !text.contains("secret_hmac")
            && !text.contains("password_hash")
    );

    // Into an empty installation: a dry run changes nothing, the import creates everything.
    let other = TestGw::start(None).await;
    let (st, dry) = other.api("POST", "/system/config-import?dry_run=true", Some(export.clone())).await;
    // The response cache's install is in both from their first start: it is left as it is.
    assert_eq!(
        (st, dry["applied"].as_bool(), dry["created"].as_array().unwrap().len(), dry["unchanged"].as_u64()),
        (200, Some(false), objects.len() - 1, Some(1)),
        "{dry}"
    );
    assert!(other.api("GET", "/models", None).await.1["items"].as_array().unwrap().is_empty());
    let (st, done) = other.api("POST", "/system/config-import", Some(export.clone())).await;
    assert_eq!(
        (st, done["applied"].as_bool(), done["refused"].as_array().unwrap().len()),
        (200, Some(true), 0),
        "{done}"
    );
    let (_, template) = other.api("GET", &format!("/orgs/{}/workspaces/{}/templates", fx.org, fx.ws), None).await;
    assert_eq!(template["items"][0]["spec"]["max_output_tokens"], 300);
    let (_, classes) = other.api("GET", "/rate-classes", None).await;
    assert_eq!(classes["items"][0]["name"], "batch", "{classes}");
    // What was left out is entered where the document lands: a credential and a key, and it works.
    other
        .create(&format!("/providers/{}/credentials", fx.provider), json!({ "secret": "sk-other-installation" }))
        .await;
    let key = other.key(&fx, json!({ "name": "svc" })).await;
    let r = other
        .http
        .post(format!("{}/v1/chat/completions", other.data))
        .bearer_auth(&key)
        .json(&json!({ "model": "@writer", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(mock.captured().last().unwrap().header("authorization"), Some("Bearer sk-other-installation"));

    // A second import of the same document changes nothing; an edited one replaces.
    let (_, again) = other.api("POST", "/system/config-import", Some(export.clone())).await;
    assert_eq!(
        (again["unchanged"].as_u64(), again["created"].as_array().unwrap().len()),
        (Some(objects.len() as u64), 0)
    );
    let mut edited = export.clone();
    for o in edited["objects"].as_array_mut().unwrap() {
        if o["kind"] == "limit" {
            o["doc"]["amount"] = json!(5.0);
        }
    }
    let (_, r) = other.api("POST", "/system/config-import", Some(edited)).await;
    assert_eq!(r["updated"], json!(["limit daily of workspace prod"]));

    // A document that went through another tool comes back with its numbers respelled
    // (10.0 as 10) and its defaults dropped. It still says the same, and nothing is updated.
    fn respelled(v: &mut Value) {
        match v {
            Value::Number(n) => {
                if let Some(f) = n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() < 1e15) {
                    *v = json!(f as i64);
                }
            }
            Value::Array(a) => a.iter_mut().for_each(respelled),
            Value::Object(o) => {
                o.retain(|_, v| !v.is_null());
                o.values_mut().for_each(respelled);
            }
            _ => {}
        }
    }
    let (_, mut current) = other.api("GET", "/system/config-export", None).await;
    let before = current.to_string();
    respelled(&mut current);
    assert_ne!(current.to_string(), before, "the respelling changes the text");
    let (_, r) = other.api("POST", "/system/config-import?dry_run=true", Some(current)).await;
    assert_eq!(
        (r["updated"].as_array().unwrap().len(), r["unchanged"].as_u64()),
        (0, Some(objects.len() as u64)),
        "{r}"
    );

    // A document that cannot be taken in full is not taken at all.
    let mut broken = export.clone();
    let list = broken["objects"].as_array_mut().unwrap();
    list.push(json!({ "id": "key_stolen", "kind": "api_key", "parent": fx.ws, "name": "x", "doc": {} }));
    list.push(json!({ "id": "mdl_bad", "kind": "model", "name": "bad", "doc": { "name": 5 } }));
    list.push(json!({ "id": "org_other", "kind": "organisation", "name": "acme", "doc": { "name": "acme" } }));
    list.push(json!({ "id": "mdl_new", "kind": "model", "name": "openai/new", "doc": { "name": "openai/new", "provider_id": fx.provider, "upstream_id": "new" } }));
    let (st, r) = other.api("POST", "/system/config-import", Some(broken)).await;
    assert_eq!((st, r["applied"].as_bool(), r["refused"].as_array().unwrap().len()), (200, Some(false), 3), "{r}");
    // Refusals come in the order the objects are applied in (parents first), each with its reason.
    let refused = r["refused"].to_string();
    for reason in ["already has this name", "not a valid model", "are not imported"] {
        assert!(refused.contains(reason), "{reason} is missing from {refused}");
    }
    assert!(
        !other.api("GET", "/models", None).await.1.to_string().contains("openai/new"),
        "nothing of a refused document is applied"
    );
    let (st, _) = other.api("POST", "/system/config-import", Some(json!({ "format": "something-else", "exported_at": "2026-01-01T00:00:00Z", "config_version": 1, "objects": [] }))).await;
    assert_eq!(st, 422);
}

#[tokio::test]
async fn a_model_retires_at_its_sunset_and_lists_show_its_state() {
    let (gw, _mock, fx) = setup().await;
    let a = gw.model(&fx, "openai/old", "old", json!({ "free": true, "capability_preset": "openai-chat" })).await;
    let b = gw.model(&fx, "openai/new", "new", json!({ "free": true })).await;
    // Modalities follow the kind and the capabilities unless they are set.
    let (_, m) = gw.api("GET", &format!("/models/{a}"), None).await;
    assert_eq!(m["effective_modalities"], json!({ "input": ["text", "image", "document"], "output": ["text"] }));

    // Deprecated, with a sunset that has passed and the wish to retire by itself.
    let (st, v) = gw
        .api(
            "POST",
            &format!("/models/{a}/transition"),
            Some(json!({ "to": "deprecated", "successor_id": b, "sunset_at": "2026-01-01T00:00:00Z", "retire_at_sunset": true })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, m) = gw.api("GET", &format!("/models/{a}"), None).await;
        if m["state"] == "retired" {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "not retired: {m}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let (_, log) = gw.api("GET", "/audit-log?limit=5", None).await;
    assert!(log.to_string().contains("deprecated->retired"), "{log}");

    // A disabled model stays in the list, marked; a retired one leaves it.
    let (st, _) = gw.api("POST", &format!("/models/{b}/transition"), Some(json!({ "to": "disabled" }))).await;
    assert_eq!(st, 200);
    gw.wait_applied().await;
    let r = gw.http.get(format!("{}/v1/models", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
    let list: Value = r.json().await.unwrap();
    let find = |id: &str| list["data"].as_array().unwrap().iter().find(|m| m["id"] == id).cloned();
    assert_eq!(find("openai/new").unwrap()["gateway"]["state"], "disabled");
    assert!(find("openai/old").is_none());
    let r = gw.http.get(format!("{}/gw/v1/models", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
    let native: Value = r.json().await.unwrap();
    let new = native["models"].as_array().unwrap().iter().find(|m| m["id"] == "openai/new").unwrap();
    assert_eq!((new["state"].as_str(), new["modalities"]["output"][0].as_str()), (Some("disabled"), Some("text")));
}

#[tokio::test]
async fn every_template_version_is_kept_and_a_template_travels_as_a_file() {
    let (gw, _mock, fx) = setup().await;
    let base = format!("/orgs/{}/workspaces/{}/templates", fx.org, fx.ws);
    let t = gw.template(&fx, "writer", json!({ "model": "openai/gpt-test", "max_output_tokens": 100 })).await;
    let path = format!("{base}/{}", t.id);
    for n in 2..=55u32 {
        let (_, cur) = gw.api("GET", &path, None).await;
        let body = json!({ "name": "writer", "version": cur["version"], "spec": { "model": "openai/gpt-test", "max_output_tokens": 100 + n } });
        let (st, v) = gw.api("PUT", &path, Some(body)).await;
        assert_eq!(st, 200, "{v}");
    }
    let (_, all) = gw.api("GET", &format!("{path}/versions"), None).await;
    assert_eq!(all.as_array().unwrap().len(), 55, "every version");
    assert_eq!(all[0]["version"], 55);
    // The first version has left the document but is still there, and can come back.
    let (st, v1) = gw.api("GET", &format!("{path}/versions/1"), None).await;
    assert_eq!((st, v1["spec"]["max_output_tokens"].as_u64()), (200, Some(100)), "{v1}");
    let (st, v) = gw.api("POST", &format!("{path}/rollback"), Some(json!({ "version": 1 }))).await;
    assert_eq!((st, v["current_version"].as_u64()), (200, Some(1)), "{v}");

    // Exported, and created again in another workspace from the file as it is.
    let (_, file) = gw.api("GET", &format!("{path}/export"), None).await;
    assert_eq!(
        (file["format"].as_str(), file["spec"]["model"].as_str()),
        (Some("pacod-gw-template/1"), Some("openai/gpt-test"))
    );
    let other = gw.create(&format!("/orgs/{}/workspaces", fx.org), json!({ "name": "staging" })).await.id;
    let (st, v) = gw.api("POST", &format!("/orgs/{}/workspaces/{other}/templates", fx.org), Some(file.clone())).await;
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["spec"]["max_output_tokens"], 100);
    // The same name twice is a conflict, which the console reports.
    let (st, _) = gw.api("POST", &format!("/orgs/{}/workspaces/{other}/templates", fx.org), Some(file)).await;
    assert_eq!(st, 409);
}

#[tokio::test]
async fn discovery_proposes_drafts_and_flags_what_went_away() {
    let (gw, _mock, fx) = setup().await;
    // The provider answers with its model list and the shared credential.
    let (st, probe) = gw.api("POST", &format!("/providers/{}/probe", fx.provider), None).await;
    assert_eq!((st, probe["ok"].as_bool(), probe["models"].as_u64()), (200, Some(true), Some(4)), "{probe}");

    assert_eq!(gw.api("POST", "/discovery/runs", None).await.0, 202);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let runs = loop {
        let (_, runs) = gw.api("GET", "/discovery/runs", None).await;
        if runs[0]["finished_at"].is_string() {
            break runs;
        }
        assert!(std::time::Instant::now() < deadline, "no run: {runs}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    assert_eq!(runs[0]["result"]["openai"]["new"], 4, "{runs}");

    let (_, open) = gw.api("GET", "/discovery/proposals", None).await;
    let open = open.as_array().unwrap();
    let new: Vec<&str> =
        open.iter().filter(|p| p["kind"] == "new").map(|p| p["upstream_id"].as_str().unwrap()).collect();
    assert_eq!(new.len(), 4, "{open:?}");
    assert!(new.contains(&"gpt-4o"));
    // The fixture's models are not in the provider's list: flagged, not changed.
    assert_eq!(open.iter().filter(|p| p["kind"] == "gone").count(), 3);

    // Accepted as a draft, nothing more; another one dismissed.
    let id =
        |upstream: &str| open.iter().find(|p| p["upstream_id"] == upstream).unwrap()["id"].as_str().unwrap().to_owned();
    let (st, model) = gw.api("POST", &format!("/discovery/proposals/{}/accept", id("gpt-4o")), None).await;
    assert_eq!(st, 201, "{model}");
    assert_eq!((model["name"].as_str(), model["state"].as_str()), (Some("openai/gpt-4o"), Some("draft")));
    assert_eq!(gw.api("POST", &format!("/discovery/proposals/{}/dismiss", id("tts-1")), None).await.0, 204);
    let (_, open) = gw.api("GET", "/discovery/proposals", None).await;
    assert_eq!(open.as_array().unwrap().iter().filter(|p| p["kind"] == "new").count(), 2);
}

#[tokio::test]
async fn a_probe_counts_the_providers_time_as_the_providers() {
    let sink = common::IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let m = gw
        .create(
            "/models",
            json!({ "name": "openai/slow", "provider_id": fx.provider, "upstream_id": "gpt-slow", "free": true }),
        )
        .await
        .id;
    // The provider takes 300 ms to answer; the gateway's own share is a tiny fraction of it.
    mock.set_script(["delay=300".parse().unwrap()]);
    let (st, probe) = gw.api("POST", &format!("/models/{m}/probe"), None).await;
    assert_eq!((st, probe["ok"].as_bool()), (200, Some(true)), "{probe}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let event = loop {
        if let Some(e) = sink.events().into_iter().find(|e| e.interface == "probe") {
            break e;
        }
        assert!(std::time::Instant::now() < deadline, "no probe event");
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    };
    let t = &event.timing;
    assert!(t.upstream_first_byte_ms >= 250.0, "{t:?}");
    assert!(t.gateway_delay_ms < 100.0, "the provider's 300 ms are not the gateway's: {t:?}");
}

#[tokio::test]
async fn saving_a_grant_as_read_keeps_its_ceilings() {
    let (gw, _mock, fx) = setup().await;
    let grants = format!("/orgs/{}/grants", fx.org);
    let (_, list) = gw.api("GET", &grants, None).await;
    let id = list["items"][0]["id"].as_str().unwrap().to_owned();
    let path = format!("{grants}/{id}");
    let (_, mut grant) = gw.api("GET", &path, None).await;
    grant["max_reasoning_tokens"] = json!(2000);
    grant["max_timeout_seconds"] = json!(30);
    let (st, v) = gw.api("PUT", &path, Some(grant)).await;
    assert_eq!(st, 200, "{v}");
    // What a reader sees, sent back unchanged, changes nothing.
    let (_, read) = gw.api("GET", &path, None).await;
    assert_eq!((read["max_reasoning_tokens"].as_u64(), read["max_timeout_seconds"].as_u64()), (Some(2000), Some(30)));
    let (st, v) = gw.api("PUT", &path, Some(read)).await;
    assert_eq!(st, 200, "{v}");
    let (_, again) = gw.api("GET", &path, None).await;
    assert_eq!((again["max_reasoning_tokens"].as_u64(), again["max_timeout_seconds"].as_u64()), (Some(2000), Some(30)));
}
