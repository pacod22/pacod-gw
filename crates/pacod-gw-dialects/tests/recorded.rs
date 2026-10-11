//! The provider halves against what the providers answered (concept section 8.6): the
//! recorded fixtures in `crates/pacod-gw-testkit/fixtures`, read and written back.

use std::path::Path;

use pacod_gw_dialects::canonical::{Block, BlockStart, Delta, EchoReasoning, Event, Finish, ReasoningStyle};
use pacod_gw_dialects::{Notes, Profile, Request, gemini, openai};
use serde_json::{Value, json};

fn fixture(provider: &str, case: &str) -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../pacod-gw-testkit/fixtures/{provider}/{case}.json"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

/// A whole answer's bytes.
fn body(fx: &Value) -> Vec<u8> {
    serde_json::to_vec(&fx["response"]["body"]).unwrap()
}

/// The `data:` payloads of a recorded stream.
fn data(fx: &Value) -> Vec<String> {
    fx["response"]["text"]
        .as_str()
        .expect("a stream")
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(|d| d.trim().to_owned())
        .collect()
}

/// A number of the recorded answer, by JSON pointer.
fn n(fx: &Value, pointer: &str) -> u64 {
    fx["response"]["body"].pointer(pointer).and_then(Value::as_u64).unwrap_or_else(|| panic!("{pointer}"))
}

fn read_stream(reader: &mut Box<dyn pacod_gw_dialects::ProviderStream>, chunks: &[String]) -> Vec<Event> {
    let mut out = Vec::new();
    for c in chunks {
        reader.read("", c.as_bytes(), &mut out);
    }
    reader.finish(&mut out);
    out
}

/// Opens and stops alternate, except that tool calls may stay open together.
fn assert_blocks_well_formed(events: &[Event]) {
    let mut open: Option<u32> = None;
    for e in events {
        match e {
            Event::ContentStart { index, block } => {
                if !matches!(block, BlockStart::ToolCall { .. }) {
                    assert!(open.is_none(), "{index} opened inside {open:?}: {events:?}");
                    open = Some(*index);
                }
            }
            Event::ContentStop { index } if open == Some(*index) => open = None,
            _ => {}
        }
    }
}

fn deepseek() -> Profile {
    Profile { upstream_model: "deepseek-flash".into(), echo_reasoning: EchoReasoning::WithTools, ..Default::default() }
}

fn gemini_openai() -> Profile {
    Profile {
        upstream_model: "gemini-3.8-flash".into(),
        thought_signatures: true,
        reasoning_outside_completion: true,
        ..Default::default()
    }
}

fn gemini_native() -> Profile {
    Profile {
        family: "gemini".into(),
        upstream_model: "gemini-3.8-flash".into(),
        reasoning: ReasoningStyle::AlwaysOn,
        reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
        ..Default::default()
    }
}

#[test]
fn deepseek_answers_streams_and_its_tool_loop() {
    let fx = fixture("deepseek", "chat");
    let r = openai::read_response(&body(&fx), &deepseek()).unwrap();
    assert!(matches!(&r.content[0], Block::Reasoning { text, .. } if !text.is_empty()));
    assert!(matches!(&r.content[1], Block::Text { text, .. } if text == "pong"));
    let u = r.usage.unwrap();
    assert_eq!(
        (u.input_tokens, u.output_tokens, u.reasoning_tokens),
        (
            n(&fx, "/usage/prompt_tokens"),
            n(&fx, "/usage/completion_tokens"),
            n(&fx, "/usage/completion_tokens_details/reasoning_tokens")
        )
    );
    let fx = fixture("deepseek", "cache-2");
    let cached = openai::read_response(&body(&fx), &deepseek()).unwrap().usage.unwrap();
    assert!(cached.cache_read_tokens > 0, "the second call read the cache");
    assert_eq!(
        (cached.cache_read_tokens, cached.input_tokens),
        (n(&fx, "/usage/prompt_cache_hit_tokens"), n(&fx, "/usage/prompt_cache_miss_tokens"))
    );

    for case in ["chat-stream", "chat-stream-plain", "reasoning-stream", "tools-1-stream", "tools-2-stream"] {
        let events = read_stream(&mut openai::provider_stream(&deepseek()), &data(&fixture("deepseek", case)));
        assert_blocks_well_formed(&events);
        let Some(Event::MessageStop { usage: Some(u), .. }) = events.last() else { panic!("{case}: {events:?}") };
        assert!(u.output_tokens > 0, "{case}: usage even unasked");
    }

    // The tool loop: the call, and the reasoning sent back with the history.
    let call = openai::read_response(&body(&fixture("deepseek", "tools-1")), &deepseek()).unwrap();
    assert_eq!(call.finish, Finish::ToolCalls);
    let thought = match &call.content[0] {
        Block::Reasoning { text, .. } => text.clone(),
        other => panic!("{other:?}"),
    };
    let sent = &fixture("deepseek", "tools-2")["request"]["body"];
    let req: Request = openai::parse_request(sent.to_string().as_bytes()).unwrap();
    let mut notes = Notes::default();
    let rebuilt = openai::build_request(&req, &deepseek(), &mut notes).unwrap();
    assert_eq!(rebuilt["messages"][1]["reasoning_content"], json!(thought));
}

#[test]
fn glm_reports_usage_unasked_and_its_reasoning() {
    let p = Profile { upstream_model: "glm-4.7".into(), ..Default::default() };
    let fx = fixture("glm", "chat");
    let r = openai::read_response(&body(&fx), &p).unwrap();
    assert!(matches!(&r.content[0], Block::Reasoning { .. }));
    let u = r.usage.unwrap();
    assert_eq!(
        (u.input_tokens, u.output_tokens, u.reasoning_tokens),
        (
            n(&fx, "/usage/prompt_tokens"),
            n(&fx, "/usage/completion_tokens"),
            n(&fx, "/usage/completion_tokens_details/reasoning_tokens")
        )
    );
    for case in ["chat-stream", "chat-stream-plain", "reasoning-stream", "tools-1-stream"] {
        let events = read_stream(&mut openai::provider_stream(&p), &data(&fixture("glm", case)));
        assert_blocks_well_formed(&events);
        assert!(matches!(events.last(), Some(Event::MessageStop { usage: Some(_), .. })), "{case}");
    }
    let fx = fixture("glm", "cache-2");
    let cached = openai::read_response(&body(&fx), &p).unwrap().usage.unwrap();
    assert_eq!(cached.cache_read_tokens, n(&fx, "/usage/prompt_tokens_details/cached_tokens"));
}

#[test]
fn geminis_compatible_endpoint_hides_its_reasoning_in_the_total_and_signs_everything() {
    let fx = fixture("gemini-openai", "chat");
    let r = openai::read_response(&body(&fx), &gemini_openai()).unwrap();
    let u = r.usage.unwrap();
    // Thinking is the difference between the total and prompt plus completion, billed as output.
    let (prompt, completion, total) =
        (n(&fx, "/usage/prompt_tokens"), n(&fx, "/usage/completion_tokens"), n(&fx, "/usage/total_tokens"));
    let thinking = total - prompt - completion;
    assert!(thinking > 0, "the recording thought");
    assert_eq!((u.input_tokens, u.output_tokens, u.reasoning_tokens), (prompt, completion + thinking, thinking));
    assert!(
        matches!(r.content.last(), Some(Block::Reasoning { signature: Some(s), .. }) if s.starts_with("gw1.gemini."))
    );

    let call = openai::read_response(&body(&fixture("gemini-openai", "tools-1")), &gemini_openai()).unwrap();
    let raw = fixture("gemini-openai", "tools-1")["response"]["body"]["choices"][0]["message"]["tool_calls"][0]["extra_content"]["google"]
        ["thought_signature"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        matches!(&call.content[0], Block::Reasoning { signature: Some(s), .. } if s == &format!("gw1.gemini.{raw}"))
    );
    // Written back for Gemini: the signature where it came from.
    let req = Request {
        messages: vec![
            pacod_gw_dialects::canonical::Message {
                role: pacod_gw_dialects::canonical::Role::User,
                content: vec![Block::text("q")],
            },
            pacod_gw_dialects::canonical::Message {
                role: pacod_gw_dialects::canonical::Role::Assistant,
                content: call.content.clone(),
            },
        ],
        ..Default::default()
    };
    let mut notes = Notes::default();
    let v = openai::build_request(&req, &gemini_openai(), &mut notes).unwrap();
    assert_eq!(v["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"], json!(raw));
    assert!(notes.adjusted.is_empty());

    for case in ["chat-stream", "tools-1-stream", "tools-2-stream"] {
        let events =
            read_stream(&mut openai::provider_stream(&gemini_openai()), &data(&fixture("gemini-openai", case)));
        assert_blocks_well_formed(&events);
        assert!(
            events.iter().any(
                |e| matches!(e, Event::ContentDelta { delta: Delta::Signature(s), .. } if s.starts_with("gw1.gemini."))
            ),
            "{case}: {events:?}"
        );
    }
}

#[test]
fn geminis_own_api_answers_streams_and_tool_loop() {
    let fx = fixture("gemini", "generate");
    let r = gemini::read_response(&body(&fx), &gemini_native()).unwrap();
    assert!(matches!(&r.content[0], Block::Reasoning { signature: Some(_), .. }));
    assert!(matches!(&r.content[1], Block::Text { text, .. } if text == "pong"));
    let u = r.usage.unwrap();
    let thoughts = n(&fx, "/usageMetadata/thoughtsTokenCount");
    assert_eq!(
        (u.input_tokens, u.output_tokens, u.reasoning_tokens),
        (n(&fx, "/usageMetadata/promptTokenCount"), n(&fx, "/usageMetadata/candidatesTokenCount") + thoughts, thoughts)
    );

    let call = gemini::read_response(&body(&fixture("gemini", "tools-1")), &gemini_native()).unwrap();
    assert_eq!(call.finish, Finish::ToolCalls);
    let recorded_id =
        fixture("gemini", "tools-1")["response"]["body"]["candidates"][0]["content"]["parts"][0]["functionCall"]["id"]
            .clone();
    assert!(
        matches!(&call.content[1], Block::ToolCall { id, .. } if json!(id) == recorded_id),
        "Gemini 3 names its calls"
    );

    // The second turn as the gateway would write it: the recorded request, reproduced.
    let sent = fixture("gemini", "tools-2")["request"]["body"].clone();
    let req = Request {
        messages: vec![
            pacod_gw_dialects::canonical::Message {
                role: pacod_gw_dialects::canonical::Role::User,
                content: vec![Block::text(sent["contents"][0]["parts"][0]["text"].as_str().unwrap())],
            },
            pacod_gw_dialects::canonical::Message {
                role: pacod_gw_dialects::canonical::Role::Assistant,
                content: call.content.clone(),
            },
        ],
        ..Default::default()
    };
    let mut notes = Notes::default();
    let v = gemini::build_request(&req, &gemini_native(), &mut notes).unwrap();
    assert_eq!(v["contents"][1]["parts"][0]["thoughtSignature"], sent["contents"][1]["parts"][0]["thoughtSignature"]);
    assert_eq!(v["contents"][1]["parts"][0]["functionCall"]["id"], recorded_id);

    for case in ["stream-sse", "thoughts-stream", "tools-1-stream"] {
        let events = read_stream(&mut gemini::provider_stream(&gemini_native()), &data(&fixture("gemini", case)));
        assert_blocks_well_formed(&events);
        let Some(Event::MessageStop { usage: Some(u), .. }) = events.last() else { panic!("{case}: {events:?}") };
        assert!(u.input_tokens > 0, "{case}");
    }
}
