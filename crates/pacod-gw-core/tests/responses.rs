//! OpenAI's Responses API in managed mode (`POST /v1/responses`), stateless: relayed to a
//! provider that serves it, translated to Anthropic and to a chat-completions endpoint.

mod common;

use chrono::SubsecRound;
use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_core::store::responses::{Chain, StoredResponse, Whose};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// A Claude model, an OpenAI model at a provider that serves the Responses API, and a model at
/// an OpenAI-compatible provider that serves chat completions only. All priced.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    let rates = json!({ "currency": "USD", "input": 2.0, "output": 10.0, "cache_read": 0.2 });
    gw.priced_model(
        &fx,
        "anthropic/claude",
        "claude-sonnet-5-5",
        json!({ "capability_preset": "anthropic-adaptive", "max_output_tokens": 16000 }),
        rates.clone(),
    )
    .await;
    for (name, quirks, model, upstream, preset) in [
        ("openai", json!({ "responses_api": true }), "openai/gpt", "gpt-x", "openai-reasoning"),
        ("compat", json!({}), "compat/chat", "gpt-compat", "openai-chat"),
    ] {
        let provider = gw
            .create(
                "/providers",
                json!({ "name": name, "family": "openai", "base_url": format!("{}/v1", mock.base_url()), "quirks": quirks }),
            )
            .await
            .id;
        gw.create(&format!("/providers/{provider}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
        let id = gw.draft_model(&provider, model, upstream, json!({ "capability_preset": preset })).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(rates.clone())).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    World { gw, mock, sink, fx }
}

fn script(mock: &MockProvider, entries: &[&str]) {
    mock.set_script(Behavior::parse_all(entries.iter().copied()).expect("script"));
}

struct Reply {
    status: u16,
    text: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }

    /// The events of a server-sent stream: the name and the data.
    fn events(&self) -> Vec<(String, Value)> {
        self.text
            .split("\n\n")
            .filter(|e| !e.trim().is_empty())
            .filter_map(|e| {
                let name = e.lines().find_map(|l| l.strip_prefix("event: ")).unwrap_or_default().to_owned();
                let data = e.lines().find_map(|l| l.strip_prefix("data: "))?;
                Some((name, serde_json::from_str(data).unwrap_or(Value::String(data.to_owned()))))
            })
            .collect()
    }
}

async fn post(w: &World, body: Value) -> Reply {
    let r =
        w.gw.http.post(format!("{}/v1/responses", w.gw.data)).bearer_auth(&w.fx.key).json(&body).send().await.unwrap();
    Reply { status: r.status().as_u16(), text: r.text().await.unwrap() }
}

impl World {
    fn sent(&self, suffix: &str) -> Vec<Value> {
        self.mock.captured().into_iter().filter(|c| c.path.ends_with(suffix)).map(|c| c.json().unwrap()).collect()
    }
}

#[tokio::test]
async fn a_responses_provider_is_relayed_to_and_keeps_nothing() {
    let w = setup().await;
    script(&w.mock, &["thinking"]);
    let r = post(&w, json!({ "model": "openai/gpt", "input": "hello", "gateway": { "end_user": "u-1" } })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v = r.json();
    assert_eq!(v["object"], "response");
    let text =
        v["output"].as_array().unwrap().iter().find(|i| i["type"] == "message").unwrap()["content"][0]["text"].clone();
    assert_eq!(text, "echo: hello");
    assert!(v["usage"]["cost"]["amount"].as_f64().is_some_and(|c| c > 0.0), "{}", v["usage"]);

    // The provider was asked at its own path, for its own model, to keep nothing, and to give
    // the reasoning back encrypted: without a stored response, that is how it crosses turns.
    let sent = w.sent("/v1/responses");
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0]["model"].as_str(), sent[0]["store"].as_bool()), (Some("gpt-x"), Some(false)));
    assert_eq!(sent[0]["include"], json!(["reasoning.encrypted_content"]));
    assert!(sent[0].get("gateway").is_none());
    let reasoning = v["output"].as_array().unwrap().iter().find(|i| i["type"] == "reasoning").unwrap().clone();
    assert!(reasoning["encrypted_content"].is_string(), "{reasoning}");

    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.interface.as_str(), e.path.as_str()), ("responses", "/v1/responses"));
    assert_eq!((e.usage_source.as_str(), e.end_user.as_str()), ("provider", "u-1"));
    assert!(e.usage.input_tokens > 0 && e.usage.reasoning_tokens > 0, "{:?}", e.usage);
    assert!(e.cost.is_some_and(|c| c > 0.0));
}

#[tokio::test]
async fn a_relayed_stream_gets_the_cost_on_its_last_event() {
    let w = setup().await;
    let r = post(&w, json!({ "model": "openai/gpt", "input": "hello", "stream": true })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let events = r.events();
    assert_eq!(events.first().map(|e| e.0.as_str()), Some("response.created"));
    let (last, data) = events.last().unwrap();
    assert_eq!(last, "response.completed");
    assert!(data["response"]["usage"]["cost"]["amount"].as_f64().is_some_and(|c| c > 0.0), "{data}");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage_source.as_str(), e.stream), ("provider", true));
}

#[tokio::test]
async fn state_is_refused_and_nothing_stored_can_be_read() {
    let w = setup().await;
    for (body, word) in [
        (json!({ "model": "openai/gpt", "input": "hi", "store": true }), "store"),
        (
            json!({ "model": "anthropic/claude", "input": "hi", "previous_response_id": "resp_1" }),
            "previous_response_id",
        ),
        (json!({ "model": "openai/gpt", "input": "hi", "conversation": "conv_1" }), "conversation"),
        (json!({ "model": "openai/gpt", "input": "hi", "background": true }), "background"),
        (json!({ "model": "openai/gpt" }), "input is required"),
    ] {
        let r = post(&w, body.clone()).await;
        assert_eq!(r.status, 400, "{body}: {}", r.text);
        assert!(r.json()["error"]["message"].as_str().unwrap().starts_with(word), "{}", r.text);
    }
    // Nothing is kept to read: the gateway keeps responses, but none for this workspace.
    for (method, path, code) in [
        ("GET", "/v1/responses/resp_1", "not_found"),
        ("DELETE", "/v1/responses/resp_1", "not_found"),
        ("POST", "/v1/responses/resp_1/cancel", "not_found"),
        ("POST", "/v1/responses/compact", "not_stored"),
    ] {
        let r =
            w.gw.http
                .request(method.parse().unwrap(), format!("{}{path}", w.gw.data))
                .bearer_auth(&w.fx.key)
                .send()
                .await
                .unwrap();
        assert_eq!(r.status(), 404, "{method} {path}");
        let v: Value = r.json().await.unwrap();
        assert_eq!(v["error"]["code"], code, "{v}");
    }
    assert!(w.mock.captured().is_empty(), "no provider was asked");
}

#[tokio::test]
async fn claude_answers_a_responses_caller_whole() {
    let w = setup().await;
    let r = post(
        &w,
        json!({ "model": "anthropic/claude", "instructions": "Be brief.", "input": "hello", "max_output_tokens": 500, "metadata": { "k": "v" } }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v = r.json();
    assert!(v["id"].as_str().unwrap().starts_with("resp_"), "{}", v["id"]);
    assert_eq!(
        (v["object"].as_str(), v["status"].as_str(), v["store"].as_bool()),
        (Some("response"), Some("completed"), Some(false))
    );
    assert_eq!((v["instructions"].as_str(), v["max_output_tokens"].as_u64()), (Some("Be brief."), Some(500)));
    assert_eq!(v["output"][0]["content"][0]["text"], "echo: hello");
    assert!(v["usage"]["input_tokens"].as_u64().is_some_and(|n| n > 0));
    assert!(v["usage"]["cost"]["amount"].is_number());
    let sent = w.sent("/v1/messages");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["system"][0]["text"], "Be brief.");
    assert_eq!(sent[0]["max_tokens"], 500);
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.interface.as_str(), e.usage_source.as_str()), ("responses", "provider"));
}

#[tokio::test]
async fn claude_streams_the_events_the_sdks_read() {
    let w = setup().await;
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "hello", "stream": true })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let events = r.events();
    let names: Vec<&str> = events.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(names[..2], ["response.created", "response.in_progress"]);
    assert!(names.contains(&"response.output_text.delta"), "{names:?}");
    assert_eq!(names.last(), Some(&"response.completed"));
    let deltas: String = events
        .iter()
        .filter(|e| e.0 == "response.output_text.delta")
        .map(|e| e.1["delta"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(deltas, "echo: hello");
    let numbers: Vec<u64> = events.iter().map(|e| e.1["sequence_number"].as_u64().unwrap()).collect();
    assert_eq!(numbers, (0..events.len() as u64).collect::<Vec<_>>());
    let last = &events.last().unwrap().1["response"];
    assert!(last["usage"]["cost"]["amount"].is_number(), "{last}");
}

#[tokio::test]
async fn a_chat_completions_provider_is_reached_through_translation() {
    let w = setup().await;
    let r = post(&w, json!({ "model": "compat/chat", "input": [{ "role": "user", "content": "hello" }] })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json()["output"][0]["content"][0]["text"], "echo: hello");
    assert!(w.sent("/v1/responses").is_empty());
    let sent = w.sent("/v1/chat/completions");
    assert_eq!(sent[0]["model"], "gpt-compat");
    assert_eq!(sent[0]["messages"][0]["content"], "hello");
}

#[tokio::test]
async fn built_in_tools_need_a_responses_provider() {
    let w = setup().await;
    let body = |model: &str| json!({ "model": model, "input": "search for it", "tools": [{ "type": "web_search" }] });
    let r = post(&w, body("anthropic/claude")).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert_eq!(r.json()["error"]["code"], "unsupported_feature", "{}", r.text);
    let r = post(&w, body("openai/gpt")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    // The refused call has its event too.
    let e = w.sink.wait_events(2).await.into_iter().find(|e| e.status == "ok").unwrap();
    assert_eq!(e.usage.server_tool_calls, 1, "the search is counted");
}

#[tokio::test]
async fn a_tool_call_and_its_output_cross_the_translation() {
    let w = setup().await;
    script(&w.mock, &["tool=weather"]);
    let tools =
        json!([{ "type": "function", "name": "weather", "parameters": { "type": "object", "properties": {} } }]);
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "Bern?", "tools": tools })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let call = r.json()["output"].as_array().unwrap().iter().find(|i| i["type"] == "function_call").unwrap().clone();
    assert_eq!(call["name"], "weather");
    let call_id = call["call_id"].as_str().unwrap().to_owned();

    let r = post(
        &w,
        json!({ "model": "anthropic/claude", "tools": tools, "input": [
            { "role": "user", "content": "Bern?" },
            call,
            { "type": "function_call_output", "call_id": call_id, "output": "sunny" }
        ] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.sent("/v1/messages");
    let messages = sent[1]["messages"].as_array().unwrap();
    assert_eq!(messages[1]["content"][0]["type"], "tool_use");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], call_id);
}

#[tokio::test]
async fn reasoning_another_provider_wrote_is_not_sent_to_openai() {
    let w = setup().await;
    let r = post(
        &w,
        json!({ "model": "openai/gpt", "input": [
            { "role": "user", "content": "one" },
            { "type": "reasoning", "summary": [], "encrypted_content": "gw1.anthropic.SIG" },
            { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "two" }] },
            { "role": "user", "content": "three" }
        ] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let input = w.sent("/v1/responses")[0]["input"].clone();
    assert_eq!(input.as_array().unwrap().len(), 3, "{input}");
    assert!(!input.to_string().contains("gw1."));
}

#[tokio::test]
async fn proxy_mode_on_the_shared_credential_does_not_reuse_stored_state() {
    let w = setup().await;
    let url = format!("{}/proxy/openai/v1/responses", w.gw.data);
    let body = json!({ "model": "gpt-x", "input": "hi", "previous_response_id": "resp_other" });
    let r = w.gw.http.post(&url).bearer_auth(&w.fx.key).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 403);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "own_credential_required", "{v}");
    let r =
        w.gw.http
            .post(&url)
            .bearer_auth(&w.fx.key)
            .json(&json!({ "model": "gpt-x", "input": "hi" }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
}

/// A stored turn of the workspace `ws`, expiring in `days` (in the past when negative).
fn turn(id: &str, ws: &str, previous: Option<&str>, end_user: &str, days: i64) -> StoredResponse {
    let now = chrono::Utc::now();
    StoredResponse {
        id: id.into(),
        organisation_id: "org_1".into(),
        workspace_id: ws.into(),
        key_id: "key_1".into(),
        end_user: end_user.into(),
        user_id: format!("user-of-{end_user}"),
        previous_id: previous.map(str::to_owned),
        conversation_id: None,
        model: "openai/gpt".into(),
        status: "completed".into(),
        input: format!(r#"[{{"role":"user","content":"{id}"}}]"#).into_bytes(),
        response: format!(r#"{{"id":"{id}","output":[]}}"#).into_bytes(),
        // As PostgreSQL keeps times: to the microsecond.
        created_at: now.trunc_subsecs(6),
        expires_at: (now + chrono::Duration::days(days)).trunc_subsecs(6),
    }
}

#[tokio::test]
async fn stored_turns_are_walked_within_their_workspace_until_they_expire() {
    let gw = TestGw::start(None).await;
    let store = gw.app.gw.objects.clone().unwrap();
    assert!(store.keeps_responses());
    let turns = [
        turn("resp_1", "ws_a", None, "alice", 30),
        turn("resp_2", "ws_a", Some("resp_1"), "alice", 30),
        turn("resp_3", "ws_a", Some("resp_2"), "bob", 30),
        turn("resp_b", "ws_b", None, "alice", 30),
        turn("resp_cross", "ws_a", Some("resp_b"), "alice", 30),
        turn("resp_orphan", "ws_a", Some("resp_gone"), "alice", 30),
        turn("resp_old", "ws_a", None, "alice", -1),
        turn("resp_after_old", "ws_a", Some("resp_old"), "alice", 30),
    ];
    for t in &turns {
        assert!(store.response_keep(t).await.unwrap());
    }
    assert_eq!(store.response("ws_a", "resp_3").await.unwrap().as_ref(), Some(&turns[2]), "kept as written");
    // A second write of the same id is the same call's and changes nothing.
    let mut again = turn("resp_1", "ws_a", None, "carol", 30);
    again.response = b"{}".to_vec();
    assert!(store.response_keep(&again).await.unwrap());
    assert_eq!(store.response("ws_a", "resp_1").await.unwrap().as_ref(), Some(&turns[0]));

    // Oldest first, within the workspace only.
    match store.response_chain("ws_a", "resp_3", 1 << 20).await.unwrap() {
        Chain::Found(chain) => assert_eq!(chain, turns[..3]),
        other => panic!("not found: {other:?}"),
    }
    assert_eq!(store.response_chain("ws_b", "resp_3", 1 << 20).await.unwrap(), Chain::Missing("resp_3".into()));
    assert_eq!(store.response_chain("ws_a", "resp_cross", 1 << 20).await.unwrap(), Chain::Missing("resp_b".into()));
    assert_eq!(store.response_chain("ws_a", "resp_orphan", 1 << 20).await.unwrap(), Chain::Missing("resp_gone".into()));
    // Each turn holds about 64 bytes: the chain of three does not fit in 100.
    assert_eq!(store.response_chain("ws_a", "resp_3", 100).await.unwrap(), Chain::TooLarge);

    // An expired turn is not read, breaks the chains through it, and goes at the next sweep.
    assert_eq!(store.response("ws_a", "resp_old").await.unwrap(), None);
    assert_eq!(
        store.response_chain("ws_a", "resp_after_old", 1 << 20).await.unwrap(),
        Chain::Missing("resp_old".into())
    );
    assert_eq!(store.responses_expire().await.unwrap(), 1);
    assert_eq!(store.responses_expire().await.unwrap(), 0);

    // Deleting a turn breaks the chains through it; another workspace cannot delete it.
    assert!(!store.response_delete("ws_b", "resp_2").await.unwrap());
    assert!(store.response_delete("ws_a", "resp_2").await.unwrap());
    assert!(!store.response_delete("ws_a", "resp_2").await.unwrap());
    assert_eq!(store.response_chain("ws_a", "resp_3", 1 << 20).await.unwrap(), Chain::Missing("resp_2".into()));

    // Erasure: by end user or by user, in one workspace.
    assert_eq!(store.responses_erase("ws_a", Whose::EndUser("bob")).await.unwrap(), 1);
    assert_eq!(store.responses_erase("ws_a", Whose::User("user-of-alice")).await.unwrap(), 4);
    assert!(store.response("ws_b", "resp_b").await.unwrap().is_some(), "another workspace's alice is not erased");
}

/// Sets the workspace's stored responses (`settings.responses`).
async fn keep_responses(w: &World, responses: Value) {
    let path = format!("/orgs/{}/workspaces/{}", w.fx.org, w.fx.ws);
    let body = json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "responses": responses } });
    let (st, v) = w.gw.api("PUT", &path, Some(body)).await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
}

/// The texts of a list of input items, output items or messages, in order.
fn texts(items: &Value) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| match &i["content"] {
            Value::String(s) => Some(s.clone()),
            Value::Array(parts) => parts.iter().find_map(|p| p["text"].as_str()).map(str::to_owned),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_kept_response_is_continued_at_another_provider_and_back() {
    let w = setup().await;
    keep_responses(&w, json!({ "store": "on" })).await;

    // Turn 1 at OpenAI, without `store`: kept, as at OpenAI, under the gateway's own id.
    let r = post(&w, json!({ "model": "openai/gpt", "input": "one", "gateway": { "end_user": "alice" } })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let one = r.json();
    let id1 = one["id"].as_str().unwrap().to_owned();
    assert!(id1.starts_with("resp_") && !id1.contains("mock"), "{id1}");
    assert_eq!((one["store"].as_bool(), one["previous_response_id"].is_null()), (Some(true), true));
    assert_eq!(w.sent("/v1/responses")[0]["store"], false, "the provider keeps nothing");
    let kept = w.gw.app.gw.objects.clone().unwrap().response(&w.fx.ws, &id1).await.unwrap().expect("kept");
    assert_eq!((kept.end_user.as_str(), kept.model.as_str()), ("alice", "openai/gpt"));

    // Turn 2 at Claude: it is sent turn 1 again, and its answer's items carry the new id.
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "two", "previous_response_id": id1 })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let two = r.json();
    let id2 = two["id"].as_str().unwrap().to_owned();
    assert_ne!(id1, id2);
    assert_eq!((two["previous_response_id"].as_str(), two["store"].as_bool()), (Some(id1.as_str()), Some(true)));
    assert!(two["output"][0]["id"].as_str().unwrap().contains(id2.trim_start_matches("resp_")), "{two}");
    assert_eq!(texts(&w.sent("/v1/messages")[0]["messages"]), ["one", "echo: one", "two"]);

    // Turn 3 back at OpenAI, streamed: the whole conversation, and every event names the
    // response by the gateway's id.
    let r =
        post(&w, json!({ "model": "openai/gpt", "input": "three", "previous_response_id": id2, "stream": true })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let events = r.events();
    let (last, data) = events.last().unwrap();
    assert_eq!(last, "response.completed");
    let id3 = data["response"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        (data["response"]["previous_response_id"].as_str(), data["response"]["store"].as_bool()),
        (Some(id2.as_str()), Some(true))
    );
    assert!(data["response"]["usage"]["cost"]["amount"].is_number(), "the cost is still added: {data}");
    for (name, e) in events.iter().filter(|e| e.1.get("response").is_some()) {
        assert_eq!(e["response"]["id"], id3, "{name}");
    }
    let sent = &w.sent("/v1/responses")[1];
    assert_eq!(sent["store"], false);
    assert!(sent.get("previous_response_id").is_none(), "{sent}");
    assert_eq!(texts(&sent["input"]), ["one", "echo: one", "two", "echo: two", "three"]);

    // Right after the stream's last event, the next turn finds it.
    let r = post(&w, json!({ "model": "openai/gpt", "input": "four", "previous_response_id": id3 })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(texts(&w.sent("/v1/responses")[2]["input"]).len(), 7);

    // Every turn has its event, as any call has.
    let events = w.sink.wait_events(4).await;
    assert!(events.iter().all(|e| e.status == "ok"), "{events:?}");
}

#[tokio::test]
async fn kept_responses_are_asked_for_and_stay_in_their_workspace() {
    let w = setup().await;
    // Off, by default: refused as before, with the reason.
    let r = post(&w, json!({ "model": "openai/gpt", "input": "hi", "store": true })).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert!(r.json()["error"]["message"].as_str().unwrap().contains("responses.store is off"), "{}", r.text);
    let r = post(&w, json!({ "model": "openai/gpt", "input": "hi" })).await;
    let unkept = r.json();
    assert_eq!(unkept["store"], false);

    keep_responses(&w, json!({ "store": "on" })).await;
    let follow = |previous: &Value| json!({ "model": "openai/gpt", "input": "next", "previous_response_id": previous });
    // What was answered while it was off, or with `store: false`, is not kept.
    let r = post(&w, follow(&unkept["id"])).await;
    assert_eq!(
        (r.status, r.json()["error"]["code"].as_str()),
        (400, Some("previous_response_not_found")),
        "{}",
        r.text
    );
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "hi", "store": false })).await;
    assert_eq!(r.json()["store"], false);
    assert_eq!(post(&w, follow(&r.json()["id"])).await.status, 400);

    // Identical calls get ids of their own, though the provider gives the same one twice.
    let same = json!({ "model": "openai/gpt", "input": "same" });
    let (a, b) = (post(&w, same.clone()).await.json(), post(&w, same).await.json());
    assert_ne!(a["id"], b["id"]);
    assert_eq!(post(&w, follow(&a["id"])).await.status, 200);

    // Another workspace's response is unknown here, though that workspace keeps them too.
    let other = w.gw.create(&format!("/orgs/{}/workspaces", w.fx.org), json!({ "name": "other" })).await.id;
    let path = format!("/orgs/{}/workspaces/{other}", w.fx.org);
    let (st, v) =
        w.gw.api("PUT", &path, Some(json!({ "name": "other", "settings": { "responses": { "store": "on" } } }))).await;
    assert_eq!(st, 200, "{v}");
    let key = w.gw.create(&format!("{path}/keys"), json!({ "name": "svc" })).await.body["secret"]
        .as_str()
        .unwrap()
        .to_owned();
    w.gw.wait_applied().await;
    let r =
        w.gw.http
            .post(format!("{}/v1/responses", w.gw.data))
            .bearer_auth(&key)
            .json(&follow(&a["id"]))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 400);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["code"], "previous_response_not_found");

    // The audit rule forbid keeps nothing of the workspace's calls.
    let (st, v) =
        w.gw.api(
            "PUT",
            &format!("/orgs/{}", w.fx.org),
            Some(json!({ "name": "acme", "settings": { "audit": { "rule": "forbid" } } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
    let r = post(&w, json!({ "model": "openai/gpt", "input": "hi", "store": true })).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert!(r.json()["error"]["message"].as_str().unwrap().contains("forbid"), "{}", r.text);
    assert_eq!(post(&w, json!({ "model": "openai/gpt", "input": "hi" })).await.json()["store"], false);
}

#[tokio::test]
async fn claude_s_kept_reasoning_is_replayed_to_claude_only() {
    let w = setup().await;
    keep_responses(&w, json!({ "store": "on" })).await;
    script(&w.mock, &["thinking"]);
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "one" })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let one = r.json();
    assert!(one["output"].to_string().contains("gw1.anthropic."), "Claude's reasoning carries its family: {one}");

    // OpenAI is given the conversation without the reasoning it cannot read.
    let r = post(&w, json!({ "model": "openai/gpt", "input": "two", "previous_response_id": one["id"] })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = &w.sent("/v1/responses")[0];
    assert!(!sent["input"].to_string().contains("gw1."), "{sent}");
    assert_eq!(texts(&sent["input"]), ["one", "echo: one", "two"]);

    // Claude is given its own reasoning back, signed as it wrote it.
    let r = post(&w, json!({ "model": "anthropic/claude", "input": "three", "previous_response_id": r.json()["id"] }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let messages = w.sent("/v1/messages")[1]["messages"].to_string();
    assert!(messages.contains("\"thinking\"") && !messages.contains("gw1."), "{messages}");
}

async fn call(w: &World, method: &str, path: &str) -> Reply {
    let url = format!("{}{path}", w.gw.data);
    let r = w.gw.http.request(method.parse().unwrap(), url).bearer_auth(&w.fx.key).send().await.unwrap();
    Reply { status: r.status().as_u16(), text: r.text().await.unwrap() }
}

#[tokio::test]
async fn a_kept_response_is_read_its_input_listed_and_it_is_deleted() {
    let w = setup().await;
    keep_responses(&w, json!({ "store": "on" })).await;
    let one = post(&w, json!({ "model": "openai/gpt", "input": "one", "stream": true })).await;
    let one = one.events().last().unwrap().1["response"].clone();
    let id1 = one["id"].as_str().unwrap().to_owned();
    let input = json!([
        { "role": "user", "content": "two" },
        { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "three" }] },
        { "role": "user", "content": "four" }
    ]);
    let two =
        post(&w, json!({ "model": "anthropic/claude", "input": input, "previous_response_id": id1 })).await.json();
    let id2 = two["id"].as_str().unwrap().to_owned();

    // Read back as the caller was given it, the end of a stream included.
    let r = call(&w, "GET", &format!("/v1/responses/{id1}")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json(), one);
    assert_eq!(call(&w, "GET", &format!("/v1/responses/{id2}")).await.json(), two);

    // This turn's own input, newest first, each item with an id and its text as a part.
    let r = call(&w, "GET", &format!("/v1/responses/{id2}/input_items")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let page = r.json();
    assert_eq!((page["object"].as_str(), page["has_more"].as_bool()), (Some("list"), Some(false)));
    assert_eq!(texts(&page["data"]), ["four", "three", "two"]);
    assert!(page["data"].as_array().unwrap().iter().all(|i| i["id"].is_string() && i["type"] == "message"));
    assert_eq!(page["data"][0]["content"], json!([{ "type": "input_text", "text": "four" }]));
    let r = call(&w, "GET", &format!("/v1/responses/{id2}/input_items?order=asc&limit=2")).await.json();
    assert_eq!((texts(&r["data"]), r["has_more"].as_bool()), (vec!["two".to_owned(), "three".to_owned()], Some(true)));
    let after = r["last_id"].as_str().unwrap();
    let r = call(&w, "GET", &format!("/v1/responses/{id2}/input_items?order=asc&limit=2&after={after}")).await.json();
    assert_eq!((texts(&r["data"]), r["has_more"].as_bool()), (vec!["four".to_owned()], Some(false)));
    assert_eq!(call(&w, "GET", &format!("/v1/responses/{id2}/input_items?limit=500")).await.status, 400);

    // Nothing runs in the background to cancel, and nothing streams again.
    let r = call(&w, "POST", &format!("/v1/responses/{id2}/cancel")).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert_eq!(call(&w, "GET", &format!("/v1/responses/{id2}?stream=true")).await.status, 400);

    // Another workspace does not see it.
    let other = w.gw.create(&format!("/orgs/{}/workspaces", w.fx.org), json!({ "name": "other" })).await.id;
    let key = w.gw.create(&format!("/orgs/{}/workspaces/{other}/keys", w.fx.org), json!({ "name": "svc" })).await;
    w.gw.wait_applied().await;
    let r =
        w.gw.http
            .delete(format!("{}/v1/responses/{id1}", w.gw.data))
            .bearer_auth(key.body["secret"].as_str().unwrap())
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 404);

    // Deleted, it is gone, and so is every chain through it.
    let r = call(&w, "DELETE", &format!("/v1/responses/{id1}")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json(), json!({ "id": id1, "object": "response", "deleted": true }));
    assert_eq!(call(&w, "GET", &format!("/v1/responses/{id1}")).await.status, 404);
    assert_eq!(call(&w, "DELETE", &format!("/v1/responses/{id1}")).await.status, 404);
    let r = post(&w, json!({ "model": "openai/gpt", "input": "five", "previous_response_id": id2 })).await;
    assert_eq!(
        (r.status, r.json()["error"]["code"].as_str()),
        (400, Some("previous_response_not_found")),
        "{}",
        r.text
    );
    assert!(r.json()["error"]["message"].as_str().unwrap().contains(&id1), "{}", r.text);

    // Each of these calls is in the request log.
    let events = w.sink.wait_events(12).await;
    let reads: Vec<&str> =
        events.iter().filter(|e| e.path.starts_with("/v1/responses/")).map(|e| e.path.as_str()).collect();
    assert!(reads.contains(&"/v1/responses/{id}/input_items") && reads.contains(&"/v1/responses/{id}"), "{reads:?}");
}
