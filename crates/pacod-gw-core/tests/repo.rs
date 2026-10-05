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
