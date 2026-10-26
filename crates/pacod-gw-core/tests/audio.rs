//! Speech to text in managed mode (`/v1/audio/transcriptions`, `/v1/audio/translations`):
//! relayed to OpenAI-shaped providers, translated to Gemini, priced by the seconds or tokens
//! the provider reports.

mod common;

use bytes::Bytes;
use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_dialects::multipart::{OwnedPart, content_type, write};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// Whisper priced by the second and gpt-4o-transcribe priced by tokens at the fixture's
/// OpenAI provider; a Gemini model at Gemini's own API.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let kind = json!({ "kind": "transcription", "capability_preset": "transcription" });
    gw.priced_model(
        &fx,
        "openai/whisper",
        "whisper-1",
        kind.clone(),
        json!({ "currency": "USD", "per_audio_second": 0.0001 }),
    )
    .await;
    gw.priced_model(
        &fx,
        "openai/transcribe",
        "gpt-4o-transcribe",
        kind.clone(),
        json!({ "currency": "USD", "input": 2.5, "input_audio": 6.0, "output": 10.0 }),
    )
    .await;
    let gemini = gw
        .create(
            "/providers",
            json!({ "name": "gemini", "family": "gemini", "managed_api": "native",
                    "base_url": format!("{}/emulate/gemini/v1beta", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{gemini}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let id = gw.draft_model(&gemini, "gemini/flash-audio", "gemini-3.8-flash", kind).await;
    let (st, v) = gw
        .api("PUT", &format!("/models/{id}/price"), Some(json!({ "currency": "USD", "input": 0.3, "output": 2.5 })))
        .await;
    assert_eq!(st, 201, "{v}");
    gw.activate(&id).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    World { gw, mock, sink, fx }
}

struct Reply {
    status: u16,
    content_type: String,
    text: String,
}

impl World {
    /// 48,000 bytes of audio: three seconds at the mock's rate.
    async fn transcribe(&self, path: &str, model: &str, extra: &[(&str, &str)]) -> Reply {
        let mut parts = vec![
            OwnedPart {
                name: "model".into(),
                filename: None,
                content_type: None,
                body: Bytes::copy_from_slice(model.as_bytes()),
            },
            OwnedPart {
                name: "file".into(),
                filename: Some("hello.wav".into()),
                content_type: Some("audio/wav".into()),
                body: Bytes::from(vec![7u8; 48_000]),
            },
        ];
        for (k, v) in extra {
            parts.push(OwnedPart {
                name: (*k).into(),
                filename: None,
                content_type: None,
                body: Bytes::copy_from_slice(v.as_bytes()),
            });
        }
        let r = self
            .gw
            .http
            .post(format!("{}{path}", self.gw.data))
            .bearer_auth(&self.fx.key)
            .header("content-type", content_type("audio-boundary"))
            .body(write("audio-boundary", &parts))
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        let content_type = r.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
        Reply { status, content_type, text: r.text().await.unwrap() }
    }

    /// The form a provider received, as text (the audio is not text, but the parts around it are).
    fn sent(&self, suffix: &str) -> Vec<String> {
        self.mock
            .captured()
            .into_iter()
            .filter(|c| c.path.ends_with(suffix))
            .map(|c| String::from_utf8_lossy(&c.body).into_owned())
            .collect()
    }
}

fn part<'a>(form: &'a str, name: &str) -> Option<&'a str> {
    let head = format!("name=\"{name}\"\r\n\r\n");
    let start = form.find(&head)? + head.len();
    Some(&form[start..start + form[start..].find("\r\n")?])
}

const TRANSCRIPTIONS: &str = "/v1/audio/transcriptions";

#[tokio::test]
async fn whisper_is_asked_for_its_duration_and_priced_by_the_second() {
    let w = setup().await;
    // The caller asks for json: the provider is asked for verbose_json, which says how long
    // the audio was, and the caller gets json back with that duration in its usage.
    let r = w.transcribe(TRANSCRIPTIONS, "openai/whisper", &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v: Value = serde_json::from_str(&r.text).unwrap();
    assert_eq!(v, json!({ "text": "Transcript of hello.wav", "usage": { "type": "duration", "seconds": 3 } }));
    let sent = w.sent(TRANSCRIPTIONS).remove(0);
    assert_eq!((part(&sent, "model"), part(&sent, "response_format")), (Some("whisper-1"), Some("verbose_json")));
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage.audio_ms, e.usage_source.as_str()), (3000, "provider"));
    assert!((e.cost.unwrap() - 0.0003).abs() < 1e-12, "{:?}", e.cost);

    // text is written from verbose_json too.
    let r = w.transcribe(TRANSCRIPTIONS, "openai/whisper", &[("response_format", "text")]).await;
    assert_eq!((r.status, r.text.as_str()), (200, "Transcript of hello.wav\n"));
    assert!(r.content_type.starts_with("text/plain"), "{}", r.content_type);

    // srt is relayed as it is, and its last cue says how long the audio was.
    let r = w.transcribe(TRANSCRIPTIONS, "openai/whisper", &[("response_format", "srt")]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.text.contains("00:00:00,000 --> 00:00:03,000"), "{}", r.text);
    assert_eq!(part(&w.sent(TRANSCRIPTIONS)[2], "response_format"), Some("srt"));
    let events = w.sink.wait_events(3).await;
    assert!(
        events.iter().all(|e| e.usage.audio_ms == 3000),
        "{:?}",
        events.iter().map(|e| e.usage.audio_ms).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn gpt_4o_transcribe_is_priced_by_its_audio_tokens() {
    let w = setup().await;
    let r = w.transcribe(TRANSCRIPTIONS, "openai/transcribe", &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v: Value = serde_json::from_str(&r.text).unwrap();
    assert_eq!(v["usage"]["type"], "tokens");
    assert_eq!(part(&w.sent(TRANSCRIPTIONS)[0], "response_format"), None, "json carries its tokens");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage.audio_input_tokens, e.usage.audio_ms), (30, 0));
    let expected = (30.0 * 6.0 + e.usage.output_tokens as f64 * 10.0) / 1e6;
    assert!((e.cost.unwrap() - expected).abs() < 1e-12, "{:?} vs {expected}", e.cost);

    // A caller that asks for text gets text; the provider is asked for json, which has the usage.
    let r = w.transcribe(TRANSCRIPTIONS, "openai/transcribe", &[("response_format", "text")]).await;
    assert_eq!((r.status, r.text.as_str()), (200, "Transcript of hello.wav\n"));
    assert_eq!(part(&w.sent(TRANSCRIPTIONS)[1], "response_format"), Some("json"));

    // A stream is relayed; its last event carries the usage.
    let r = w.transcribe(TRANSCRIPTIONS, "openai/transcribe", &[("stream", "true")]).await;
    assert_eq!(r.status, 200);
    assert!(r.text.contains("transcript.text.done"), "{}", r.text);
    let events = w.sink.wait_events(3).await;
    assert!(events.iter().all(|e| e.usage.audio_input_tokens == 30));
}

#[tokio::test]
async fn a_translation_reaches_the_translations_path() {
    let w = setup().await;
    let r = w.transcribe("/v1/audio/translations", "openai/whisper", &[]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.text.contains("English translation of hello.wav"), "{}", r.text);
    assert_eq!(w.sent("/v1/audio/translations").len(), 1);
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.path.as_str(), e.usage.audio_ms), ("/v1/audio/translations", 3000));
}

#[tokio::test]
async fn gemini_hears_the_audio_inline_and_answers_json() {
    let w = setup().await;
    let r = w.transcribe(TRANSCRIPTIONS, "gemini/flash-audio", &[("language", "de")]).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v: Value = serde_json::from_str(&r.text).unwrap();
    assert!(v["text"].is_string() && v["usage"]["type"] == "tokens", "{v}");
    let sent = w.mock.captured().into_iter().find(|c| c.path.ends_with(":generateContent")).unwrap().json().unwrap();
    let parts = &sent["contents"][0]["parts"];
    assert_eq!(parts[0]["inlineData"]["mimeType"], "audio/wav");
    assert!(parts[1]["text"].as_str().unwrap().contains("de"));

    let r = w.transcribe(TRANSCRIPTIONS, "gemini/flash-audio", &[("response_format", "srt")]).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert!(r.text.contains("unsupported_feature"), "{}", r.text);
}

#[tokio::test]
async fn only_transcription_models_hear() {
    let w = setup().await;
    w.gw.priced_model(
        &w.fx,
        "openai/chat",
        "gpt-mock",
        json!({}),
        json!({ "currency": "USD", "input": 1.0, "output": 1.0 }),
    )
    .await;
    let r = w.transcribe(TRANSCRIPTIONS, "openai/chat", &[]).await;
    assert_eq!(r.status, 400, "{}", r.text);
    assert!(r.text.contains("is not a transcription model"), "{}", r.text);
    assert!(w.sent(TRANSCRIPTIONS).is_empty());
}
