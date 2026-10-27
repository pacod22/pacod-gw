//! OpenAI's Conversations API (`/v1/conversations`) on the conversations a workspace keeps, and
//! `/v1/responses` calls that name one: given its items first, and adding their own.

mod common;

use common::{Fixture, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    fx: Fixture,
}

/// A Claude model and an OpenAI model at a provider that serves the Responses API, in a
/// workspace that keeps responses.
async fn setup() -> World {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    let rates = json!({ "currency": "USD", "input": 2.0, "output": 10.0 });
    gw.priced_model(
        &fx,
        "anthropic/claude",
        "claude-sonnet-5-5",
        json!({ "capability_preset": "anthropic-adaptive" }),
        rates.clone(),
    )
    .await;
    let provider = gw
        .create(
            "/providers",
            json!({ "name": "openai", "family": "openai", "base_url": format!("{}/v1", mock.base_url()),
                    "quirks": { "responses_api": true } }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{provider}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let id = gw.draft_model(&provider, "openai/gpt", "gpt-x", json!({ "capability_preset": "openai-reasoning" })).await;
    let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(rates)).await;
    assert_eq!(st, 201, "{v}");
    gw.activate(&id).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    let w = World { gw, mock, fx };
    keep(&w, true).await;
    w
}

/// Switches the workspace's stored responses, and with them its conversations, on or off.
async fn keep(w: &World, on: bool) {
    let path = format!("/orgs/{}/workspaces/{}", w.fx.org, w.fx.ws);
    let body = json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "responses": { "store": if on { "on" } else { "off" } } } });
    let (st, v) = w.gw.api("PUT", &path, Some(body)).await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
}

struct Reply {
    status: u16,
    body: Value,
}

async fn call(w: &World, method: &str, path: &str, body: Option<Value>) -> Reply {
    call_as(w, &w.fx.key, method, path, body).await
}

async fn call_as(w: &World, key: &str, method: &str, path: &str, body: Option<Value>) -> Reply {
    let mut req = w.gw.http.request(method.parse().unwrap(), format!("{}{path}", w.gw.data)).bearer_auth(key);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    Reply { status, body: serde_json::from_str(&text).unwrap_or(Value::String(text)) }
}

/// The texts of a list of items or messages, in order: every text part of each (Anthropic's
/// messages join a user's turns that follow each other).
fn texts(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|i| match &i["content"] {
            Value::String(s) => vec![s.clone()],
            Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).map(str::to_owned).collect(),
            _ => Vec::new(),
        })
        .collect()
}

fn sent(w: &World, suffix: &str) -> Vec<Value> {
    w.mock.captured().into_iter().filter(|c| c.path.ends_with(suffix)).map(|c| c.json().unwrap()).collect()
}

#[tokio::test]
async fn a_conversation_holds_its_turns_at_any_provider() {
    let w = setup().await;
    let r = call(
        &w,
        "POST",
        "/v1/conversations",
        Some(json!({ "metadata": { "topic": "demo" }, "items": [{ "role": "user", "content": "hi there" }] })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let conv = r.body["id"].as_str().unwrap().to_owned();
    assert!(conv.starts_with("conv_"));
    assert_eq!((r.body["object"].as_str(), r.body["metadata"]["topic"].as_str()), (Some("conversation"), Some("demo")));

    // A turn at OpenAI is given the conversation first, and its answer names it.
    let r = call(
        &w,
        "POST",
        "/v1/responses",
        Some(json!({ "model": "openai/gpt", "input": "one", "conversation": { "id": conv } })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["conversation"], json!({ "id": conv }));
    let first = r.body["id"].as_str().unwrap().to_owned();
    let to_openai = &sent(&w, "/v1/responses")[0];
    assert_eq!(texts(&to_openai["input"]), ["hi there", "one"]);
    assert!(to_openai.get("conversation").is_none(), "{to_openai}");

    // A turn at Claude, not kept itself, still adds to the conversation.
    let r = call(
        &w,
        "POST",
        "/v1/responses",
        Some(json!({ "model": "anthropic/claude", "input": "two", "conversation": conv, "store": false })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.body["store"], false);
    assert_eq!(texts(&sent(&w, "/v1/messages")[0]["messages"]), ["hi there", "one", "echo: one", "two"]);
    assert_eq!(call(&w, "GET", &format!("/v1/responses/{}", r.body["id"].as_str().unwrap()), None).await.status, 404);

    // The items, oldest first on request, each with an id; one of them read on its own.
    let r = call(&w, "GET", &format!("/v1/conversations/{conv}/items?order=asc"), None).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(texts(&r.body["data"]), ["hi there", "one", "echo: one", "two", "echo: two"]);
    let items = r.body["data"].as_array().unwrap().clone();
    assert!(items.iter().all(|i| i["id"].is_string()), "{items:?}");
    let r = call(&w, "GET", &format!("/v1/conversations/{conv}/items?limit=2"), None).await;
    assert_eq!(
        (texts(&r.body["data"]), r.body["has_more"].as_bool()),
        (vec!["echo: two".into(), "two".into()], Some(true))
    );
    let r =
        call(&w, "GET", &format!("/v1/conversations/{conv}/items/{}", items[1]["id"].as_str().unwrap()), None).await;
    assert_eq!(texts(&json!([r.body])), ["one"]);

    // Items added directly are given to the next turn; one deleted is not.
    let r = call(
        &w,
        "POST",
        &format!("/v1/conversations/{conv}/items"),
        Some(json!({ "items": [{ "role": "user", "content": "three" }, { "role": "user", "content": "four" }] })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let three = r.body["data"][0]["id"].as_str().unwrap().to_owned();
    let r = call(&w, "DELETE", &format!("/v1/conversations/{conv}/items/{three}"), None).await;
    assert_eq!((r.status, r.body["id"].as_str()), (200, Some(conv.as_str())), "{}", r.body);
    let r = call(
        &w,
        "POST",
        "/v1/responses",
        Some(json!({ "model": "openai/gpt", "input": "five", "conversation": conv })),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.body);
    let input = &sent(&w, "/v1/responses")[1]["input"];
    assert_eq!(texts(input), ["hi there", "one", "echo: one", "two", "echo: two", "four", "five"]);
    // What the provider is given carries no id the gateway made.
    assert!(input.as_array().unwrap().iter().filter(|i| i["role"] == "user").all(|i| i.get("id").is_none()), "{input}");

    // A turn of a conversation is continued with the conversation, and not with both.
    let r = call(
        &w,
        "POST",
        "/v1/responses",
        Some(json!({ "model": "openai/gpt", "input": "x", "previous_response_id": first })),
    )
    .await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(r.body["error"]["message"].as_str().unwrap().contains(&conv), "{}", r.body);
    let both = json!({ "model": "openai/gpt", "input": "x", "previous_response_id": first, "conversation": conv });
    assert_eq!(call(&w, "POST", "/v1/responses", Some(both)).await.status, 400);

    // Its metadata replaced; then it is deleted, with its items.
    let r =
        call(&w, "POST", &format!("/v1/conversations/{conv}"), Some(json!({ "metadata": { "topic": "done" } }))).await;
    assert_eq!(r.body["metadata"], json!({ "topic": "done" }));
    let r = call(&w, "DELETE", &format!("/v1/conversations/{conv}"), None).await;
    assert_eq!(r.body, json!({ "id": conv, "object": "conversation.deleted", "deleted": true }));
    assert_eq!(call(&w, "GET", &format!("/v1/conversations/{conv}"), None).await.status, 404);
    assert_eq!(call(&w, "GET", &format!("/v1/conversations/{conv}/items"), None).await.status, 404);
    let r =
        call(&w, "POST", "/v1/responses", Some(json!({ "model": "openai/gpt", "input": "x", "conversation": conv })))
            .await;
    assert_eq!(r.status, 404, "{}", r.body);
}

#[tokio::test]
async fn a_conversation_stays_in_its_workspace_and_is_asked_for() {
    let w = setup().await;
    let conv = call(&w, "POST", "/v1/conversations", Some(json!({}))).await.body["id"].as_str().unwrap().to_owned();
    for (body, why) in [
        (json!({ "items": "hi" }), "items"),
        (json!({ "metadata": { "k": 1 } }), "metadata"),
        (json!({ "items": (0..21).map(|_| json!({ "role": "user", "content": "x" })).collect::<Vec<_>>() }), "items"),
    ] {
        let r = call(&w, "POST", "/v1/conversations", Some(body.clone())).await;
        assert_eq!(r.status, 400, "{body}");
        assert!(r.body["error"]["message"].as_str().unwrap().contains(why), "{}", r.body);
    }

    // Another workspace's key finds nothing.
    let other = w.gw.create(&format!("/orgs/{}/workspaces", w.fx.org), json!({ "name": "other" })).await.id;
    let key = w.gw.create(&format!("/orgs/{}/workspaces/{other}/keys", w.fx.org), json!({ "name": "svc" })).await;
    w.gw.wait_applied().await;
    let key = key.body["secret"].as_str().unwrap();
    assert_eq!(call_as(&w, key, "GET", &format!("/v1/conversations/{conv}"), None).await.status, 404);
    assert_eq!(call_as(&w, key, "DELETE", &format!("/v1/conversations/{conv}"), None).await.status, 404);

    // Switched off, nothing new is kept, but what was kept is still read and deleted.
    keep(&w, false).await;
    let r = call(&w, "POST", "/v1/conversations", Some(json!({}))).await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(r.body["error"]["message"].as_str().unwrap().contains("responses.store is off"), "{}", r.body);
    let r =
        call(&w, "POST", "/v1/responses", Some(json!({ "model": "openai/gpt", "input": "x", "conversation": conv })))
            .await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert_eq!(call(&w, "GET", &format!("/v1/conversations/{conv}"), None).await.status, 200);
    assert_eq!(call(&w, "DELETE", &format!("/v1/conversations/{conv}"), None).await.status, 200);

    // The audit rule forbid keeps nothing, however the workspace is set.
    keep(&w, true).await;
    let (st, v) =
        w.gw.api(
            "PUT",
            &format!("/orgs/{}", w.fx.org),
            Some(json!({ "name": "acme", "settings": { "audit": { "rule": "forbid" } } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
    let r = call(&w, "POST", "/v1/conversations", Some(json!({}))).await;
    assert_eq!(r.status, 400, "{}", r.body);
    assert!(r.body["error"]["message"].as_str().unwrap().contains("forbid"), "{}", r.body);
    assert_eq!(call(&w, "GET", "/v1/conversations", None).await.status, 404, "no such path");
}

#[tokio::test]
async fn a_conversation_expires_its_retention_after_its_last_turn() {
    use pacod_gw_core::store::conversations::{Conversation, ConversationItem};
    let gw = TestGw::start(None).await;
    let store = gw.app.gw.objects.clone().unwrap();
    let now = chrono::Utc::now();
    let conversation = |id: &str, days: i64| Conversation {
        id: id.into(),
        organisation_id: "org_1".into(),
        workspace_id: "ws_1".into(),
        key_id: "key_1".into(),
        end_user: String::new(),
        user_id: String::new(),
        metadata: json!({}),
        created_at: now,
        used_at: now,
        expires_at: now + chrono::Duration::days(days),
    };
    let item = ConversationItem {
        item_id: "msg_1".into(),
        end_user: String::new(),
        user_id: String::new(),
        response_id: None,
        item: b"{}".to_vec(),
    };
    store.conversation_create(&conversation("conv_old", -1), std::slice::from_ref(&item)).await.unwrap();
    store.conversation_create(&conversation("conv_new", 1), std::slice::from_ref(&item)).await.unwrap();
    // A turn pushes the expiry out; an expired conversation takes none and is not read.
    let later = now + chrono::Duration::days(30);
    assert!(store.conversation_add("ws_1", "conv_new", std::slice::from_ref(&item), later).await.unwrap());
    assert_eq!(
        store.conversation("ws_1", "conv_new").await.unwrap().unwrap().expires_at.timestamp(),
        later.timestamp()
    );
    assert!(!store.conversation_add("ws_1", "conv_old", std::slice::from_ref(&item), later).await.unwrap());
    assert!(store.conversation("ws_1", "conv_old").await.unwrap().is_none());
    assert_eq!(store.conversations_expire().await.unwrap(), 1);
    assert_eq!(store.conversations_expire().await.unwrap(), 0);
    assert!(store.conversation("ws_1", "conv_new").await.unwrap().is_some());
}
