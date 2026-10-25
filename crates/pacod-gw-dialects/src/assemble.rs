//! A stream and a complete answer are two forms of one response. The response cache keeps an
//! answer in its complete form, whichever way it arrived, and gives it back in either form, in
//! the caller's dialect (`docs/plugin-concept.md`, section 15).
//!
//! [`Assembler`] folds the neutral events of a stream into a [`Response`]; [`events_of`] turns a
//! response back into the events of a stream. For the blocks a stream carries, one undoes the
//! other.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::canonical::{Block, BlockStart, Delta, Event, Response};
use crate::util::str_of;

/// Anthropic's encrypted reasoning travels on a stream as a block of its own, which a complete
/// answer reads as reasoning without text.
const REDACTED: &str = "redacted_thinking";
const ANTHROPIC: &str = "anthropic";

/// A content block of the stream, as far as it has come.
enum Open {
    Text { text: String, citations: Vec<Value> },
    Reasoning { text: String, signature: Option<String> },
    ToolCall { id: String, name: String, arguments: String },
    Provider { family: String, raw: Value, deltas: Vec<Value> },
}

impl Open {
    fn of(start: &BlockStart) -> Self {
        match start {
            BlockStart::Text => Open::Text { text: String::new(), citations: Vec::new() },
            BlockStart::Reasoning => Open::Reasoning { text: String::new(), signature: None },
            BlockStart::ToolCall { id, name } => {
                Open::ToolCall { id: id.clone(), name: name.clone(), arguments: String::new() }
            }
            BlockStart::Provider { family, raw } => {
                Open::Provider { family: family.clone(), raw: raw.clone(), deltas: Vec::new() }
            }
        }
    }

    fn add(&mut self, delta: &Delta) {
        match (self, delta) {
            (Open::Text { text, .. }, Delta::Text(t)) => text.push_str(t),
            (Open::Text { citations, .. }, Delta::Citation(c)) => citations.push(c.clone()),
            (Open::Reasoning { text, .. }, Delta::Reasoning(t)) => text.push_str(t),
            (Open::Reasoning { signature, .. }, Delta::Signature(s)) => {
                signature.get_or_insert_with(String::new).push_str(s);
            }
            (Open::ToolCall { arguments, .. }, Delta::ToolArguments(a)) => arguments.push_str(a),
            (Open::Provider { deltas, .. }, Delta::Provider(d)) => deltas.push(d.clone()),
            // A delta of another kind than its block has no place in the complete answer.
            _ => {}
        }
    }

    fn close(self) -> Block {
        match self {
            Open::Text { text, citations } => {
                Block::Text { text, citations: (!citations.is_empty()).then_some(Value::Array(citations)), cache: None }
            }
            Open::Reasoning { text, signature } => Block::Reasoning { text, signature, redacted: None },
            Open::ToolCall { id, name, arguments } => Block::ToolCall { id, name, arguments, cache: None },
            Open::Provider { family, raw, deltas } => {
                if family == ANTHROPIC && deltas.is_empty() && str_of(&raw, "type") == Some(REDACTED) {
                    let data = str_of(&raw, "data").map(str::to_owned);
                    return Block::Reasoning { text: String::new(), signature: None, redacted: data };
                }
                Block::Provider { family, raw: fold_input(raw, &deltas) }
            }
        }
    }
}

/// Puts a provider block's argument fragments back into its `input`, as the complete answer
/// carries them. Deltas of another kind are left out.
fn fold_input(mut raw: Value, deltas: &[Value]) -> Value {
    if deltas.is_empty() || !deltas.iter().all(|d| str_of(d, "type") == Some("input_json_delta")) {
        return raw;
    }
    let text: String = deltas.iter().filter_map(|d| str_of(d, "partial_json")).collect();
    if let (Ok(input), Some(obj)) = (serde_json::from_str::<Value>(&text), raw.as_object_mut()) {
        obj.insert("input".into(), input);
    }
    raw
}

/// Folds the neutral events of a stream into the complete answer.
#[derive(Default)]
pub struct Assembler {
    id: String,
    model: String,
    /// Blocks in the order they opened, by the stream's index.
    open: BTreeMap<u32, (usize, Open)>,
    closed: Vec<(usize, Block)>,
    opened: usize,
    end: Option<Response>,
    failed: bool,
}

impl Assembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: &Event) {
        if self.failed || self.end.is_some() {
            return;
        }
        match event {
            Event::MessageStart { id, model } => {
                self.id.clone_from(id);
                self.model.clone_from(model);
            }
            Event::ContentStart { index, block } => {
                self.open.insert(*index, (self.opened, Open::of(block)));
                self.opened += 1;
            }
            Event::ContentDelta { index, delta } => {
                if let Some((_, open)) = self.open.get_mut(index) {
                    open.add(delta);
                }
            }
            Event::ContentStop { index } => {
                if let Some((at, open)) = self.open.remove(index) {
                    self.closed.push((at, open.close()));
                }
            }
            Event::MessageStop { finish, native_finish, usage } => {
                // Blocks the provider left open end with the message.
                for (_, (at, open)) in std::mem::take(&mut self.open) {
                    self.closed.push((at, open.close()));
                }
                let mut blocks = std::mem::take(&mut self.closed);
                blocks.sort_by_key(|(at, _)| *at);
                self.end = Some(Response {
                    id: std::mem::take(&mut self.id),
                    model: std::mem::take(&mut self.model),
                    content: blocks.into_iter().map(|(_, b)| b).collect(),
                    finish: *finish,
                    native_finish: native_finish.clone(),
                    usage: *usage,
                    extra: Map::new(),
                });
            }
            Event::Ping => {}
            Event::Error { .. } => self.failed = true,
        }
    }

    /// The complete answer, when the stream ended with its `MessageStop` and without an error.
    pub fn finish(self) -> Option<Response> {
        if self.failed { None } else { self.end }
    }
}

/// The events of a stream that carries `response`. Images, documents and tool results, which
/// no answer holds, are left out; `extra` has no place on a stream.
pub fn events_of(response: &Response) -> Vec<Event> {
    let mut out = vec![Event::MessageStart { id: response.id.clone(), model: response.model.clone() }];
    let mut index = 0u32;
    let mut block = |out: &mut Vec<Event>, start: BlockStart, deltas: Vec<Delta>| {
        out.push(Event::ContentStart { index, block: start });
        out.extend(deltas.into_iter().map(|delta| Event::ContentDelta { index, delta }));
        out.push(Event::ContentStop { index });
        index += 1;
    };
    for b in &response.content {
        match b {
            Block::Text { text, citations, .. } => {
                let mut deltas = vec![Delta::Text(text.clone())];
                if let Some(Value::Array(list)) = citations {
                    deltas.extend(list.iter().cloned().map(Delta::Citation));
                }
                block(&mut out, BlockStart::Text, deltas);
            }
            Block::Reasoning { redacted: Some(data), .. } => {
                let raw = json!({ "type": REDACTED, "data": data });
                block(&mut out, BlockStart::Provider { family: ANTHROPIC.into(), raw }, Vec::new());
            }
            Block::Reasoning { text, signature, redacted: None } => {
                let mut deltas = Vec::new();
                if !text.is_empty() {
                    deltas.push(Delta::Reasoning(text.clone()));
                }
                if let Some(s) = signature {
                    deltas.push(Delta::Signature(s.clone()));
                }
                block(&mut out, BlockStart::Reasoning, deltas);
            }
            Block::ToolCall { id, name, arguments, .. } => {
                let deltas =
                    if arguments.is_empty() { Vec::new() } else { vec![Delta::ToolArguments(arguments.clone())] };
                block(&mut out, BlockStart::ToolCall { id: id.clone(), name: name.clone() }, deltas);
            }
            Block::Provider { family, raw } => {
                block(&mut out, BlockStart::Provider { family: family.clone(), raw: raw.clone() }, Vec::new());
            }
            Block::Image { .. } | Block::Document { .. } | Block::ToolResult { .. } => {}
        }
    }
    out.push(Event::MessageStop {
        finish: response.finish,
        native_finish: response.native_finish.clone(),
        usage: response.usage,
    });
    out
}

#[cfg(test)]
mod tests {
    use pacod_gw_events::Usage;

    use super::*;
    use crate::canonical::Finish;

    fn answer(content: Vec<Block>) -> Response {
        Response {
            id: "msg_1".into(),
            model: "m".into(),
            content,
            finish: Finish::ToolCalls,
            native_finish: "tool_use".into(),
            usage: Some(Usage { input_tokens: 10, output_tokens: 5, ..Default::default() }),
            extra: Map::new(),
        }
    }

    fn fold(events: &[Event]) -> Option<Response> {
        let mut a = Assembler::new();
        for e in events {
            a.push(e);
        }
        a.finish()
    }

    #[test]
    fn a_response_survives_the_way_through_a_stream() {
        let r = answer(vec![
            Block::Reasoning { text: "let me think".into(), signature: Some("sig==".into()), redacted: None },
            Block::Reasoning { text: String::new(), signature: None, redacted: Some("enc".into()) },
            Block::Text {
                text: "It is sunny.".into(),
                citations: Some(json!([{ "url": "https://x.test" }])),
                cache: None,
            },
            Block::ToolCall {
                id: "c1".into(),
                name: "weather".into(),
                arguments: "{\"city\":\"Oslo\"}".into(),
                cache: None,
            },
            Block::ToolCall { id: "c2".into(), name: "now".into(), arguments: String::new(), cache: None },
            Block::Provider { family: "anthropic".into(), raw: json!({ "type": "server_tool_use", "input": {} }) },
        ]);
        assert_eq!(fold(&events_of(&r)), Some(r));
    }

    #[test]
    fn deltas_are_put_together_in_the_order_blocks_opened() {
        let events = vec![
            Event::MessageStart { id: "a".into(), model: "m".into() },
            Event::ContentStart { index: 0, block: BlockStart::Text },
            Event::ContentStart { index: 1, block: BlockStart::ToolCall { id: "c".into(), name: "f".into() } },
            Event::ContentDelta { index: 1, delta: Delta::ToolArguments("{\"a\":".into()) },
            Event::ContentDelta { index: 0, delta: Delta::Text("Hel".into()) },
            Event::ContentDelta { index: 1, delta: Delta::ToolArguments("1}".into()) },
            Event::Ping,
            Event::ContentStop { index: 1 },
            Event::ContentDelta { index: 0, delta: Delta::Text("lo".into()) },
            Event::MessageStop { finish: Finish::Stop, native_finish: "stop".into(), usage: None },
        ];
        let r = fold(&events).expect("a stream that ended");
        assert_eq!(
            r.content,
            vec![
                Block::text("Hello"),
                Block::ToolCall { id: "c".into(), name: "f".into(), arguments: "{\"a\":1}".into(), cache: None },
            ]
        );
        assert_eq!((r.id.as_str(), r.finish, r.usage), ("a", Finish::Stop, None));
    }

    #[test]
    fn a_provider_block_gets_its_input_back() {
        let raw = json!({ "type": "server_tool_use", "id": "s1", "name": "web_search", "input": {} });
        let events = vec![
            Event::ContentStart { index: 0, block: BlockStart::Provider { family: "anthropic".into(), raw } },
            Event::ContentDelta {
                index: 0,
                delta: Delta::Provider(json!({ "type": "input_json_delta", "partial_json": "{\"query\":\"x\"}" })),
            },
            Event::ContentStop { index: 0 },
            Event::MessageStop { finish: Finish::Stop, native_finish: "end_turn".into(), usage: None },
        ];
        let r = fold(&events).unwrap();
        let Block::Provider { raw, .. } = &r.content[0] else { panic!("a provider block") };
        assert_eq!(raw["input"], json!({ "query": "x" }));
    }

    #[test]
    fn a_stream_that_failed_or_did_not_end_gives_no_answer() {
        let begun = vec![
            Event::MessageStart { id: "a".into(), model: "m".into() },
            Event::ContentStart { index: 0, block: BlockStart::Text },
            Event::ContentDelta { index: 0, delta: Delta::Text("Hel".into()) },
        ];
        assert_eq!(fold(&begun), None);
        let mut failed = begun.clone();
        failed.push(Event::Error { kind: "overloaded_error".into(), message: "busy".into() });
        failed.push(Event::MessageStop { finish: Finish::Stop, native_finish: "stop".into(), usage: None });
        assert_eq!(fold(&failed), None);
    }

    #[test]
    fn a_response_serialises_and_reads_back() {
        let r = answer(vec![
            Block::text("hi"),
            Block::Reasoning { text: "t".into(), signature: Some("s".into()), redacted: None },
            Block::ToolCall { id: "c1".into(), name: "f".into(), arguments: "{}".into(), cache: None },
        ]);
        let encoded = serde_json::to_vec(&r).unwrap();
        assert_eq!(serde_json::from_slice::<Response>(&encoded).unwrap(), r);
    }
}
