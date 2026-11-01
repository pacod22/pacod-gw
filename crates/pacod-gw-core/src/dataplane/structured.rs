//! Structured output the gateway emulates (concept section 6.2). A model whose provider has a
//! JSON mode but does not hold an answer to a schema (DeepSeek, GLM) is asked in that mode, with
//! the schema as an instruction, and its answer is checked against the schema before the caller
//! receives it.
//!
//! The check is a safety net, not a guarantee the provider gives: a whole answer that does not
//! conform is refused when the caller asked for a strict schema, and passed on marked otherwise.
//! A stream is passed on as it arrives, unchecked.

use bytes::Bytes;
use pacod_gw_dialects::canonical::{Block, Finish, Request, Response, ResponseFormat};
use serde_json::{Value, json};

use crate::json::validate;

/// The response header that says what became of a schema the gateway emulated.
pub const HEADER: &str = "x-gw-structured-output";

/// A request for an answer to a JSON schema, asked for in the provider's JSON mode instead.
#[derive(Debug, Clone, PartialEq)]
pub struct Emulated {
    pub name: Option<String>,
    pub schema: Value,
    /// The caller asked for a strict schema: an answer that does not conform is refused.
    pub strict: bool,
}

/// What the check of an emulated answer found.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Valid,
    Invalid(Vec<String>),
    /// A stream, an answer cut off at the token limit, or one that calls tools instead.
    Unchecked,
}

impl Verdict {
    /// The value of [`HEADER`].
    pub fn header(&self) -> &'static str {
        match self {
            Verdict::Valid => "emulated",
            Verdict::Invalid(_) => "invalid",
            Verdict::Unchecked => "unchecked",
        }
    }
}

impl Emulated {
    /// The instruction that stands in for the schema. It names "json" in lower case, which
    /// DeepSeek's JSON mode requires of the prompt, and ends with the schema on a line of its
    /// own.
    pub fn instruction(&self) -> String {
        let named = self.name.as_deref().map(|n| format!(" named \"{n}\"")).unwrap_or_default();
        format!(
            "Answer with one json object and nothing else: no text before or after it, no code fence. \
             The object must conform to this JSON Schema{named}:\n{}",
            self.schema
        )
    }

    /// Reads an OpenAI-dialect `response_format` of type `json_schema`.
    pub fn from_openai(format: &Value) -> Option<Self> {
        let inner = format.get("json_schema")?;
        Some(Self {
            name: inner.get("name").and_then(Value::as_str).map(str::to_owned),
            schema: inner.get("schema").cloned().unwrap_or_else(|| json!({ "type": "object" })),
            strict: inner.get("strict").and_then(Value::as_bool) == Some(true),
        })
    }

    /// Checks the text of an answer. Returns the text without a code fence around it, when it
    /// had one: models in JSON mode add one now and then, and the caller asked for JSON.
    fn check_text(&self, text: &str) -> (Verdict, Option<String>) {
        let trimmed = text.trim();
        let unfenced = strip_fence(trimmed);
        let rewritten = (unfenced.len() != trimmed.len()).then(|| unfenced.to_owned());
        let value: Value = match serde_json::from_str(unfenced) {
            Ok(v) => v,
            Err(e) => return (Verdict::Invalid(vec![format!("the answer is not JSON: {e}")]), rewritten),
        };
        match validate(&self.schema, &value) {
            problems if problems.is_empty() => (Verdict::Valid, rewritten),
            problems => (Verdict::Invalid(problems), rewritten),
        }
    }

    /// Checks a translated answer, and takes a code fence off its text.
    pub fn check_neutral(&self, response: &mut Response) -> Verdict {
        let calls_tools = response.content.iter().any(|b| matches!(b, Block::ToolCall { .. }));
        if response.finish != Finish::Stop || calls_tools {
            return Verdict::Unchecked;
        }
        let texts: Vec<usize> = response
            .content
            .iter()
            .enumerate()
            .filter_map(|(i, b)| matches!(b, Block::Text { .. }).then_some(i))
            .collect();
        let Some(&first) = texts.first() else { return Verdict::Unchecked };
        let joined: String = texts
            .iter()
            .filter_map(|&i| match &response.content[i] {
                Block::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let (verdict, rewritten) = self.check_text(&joined);
        if let Some(text) = rewritten {
            // The answer becomes the one text it should have been.
            for &i in texts.iter().skip(1).rev() {
                response.content.remove(i);
            }
            response.content[first] = Block::text(text);
        }
        verdict
    }

    /// Checks an OpenAI-dialect chat completion as the provider wrote it. Returns the body
    /// again only when a code fence was taken off an answer.
    pub fn check_openai(&self, body: &Bytes) -> (Verdict, Option<Bytes>) {
        let Ok(mut doc) = serde_json::from_slice::<Value>(body) else { return (Verdict::Unchecked, None) };
        let Some(choices) = doc.get_mut("choices").and_then(Value::as_array_mut) else {
            return (Verdict::Unchecked, None);
        };
        let (mut problems, mut unchecked, mut changed) = (Vec::new(), false, false);
        for choice in choices.iter_mut() {
            let stopped = choice.get("finish_reason").and_then(Value::as_str) == Some("stop");
            let message = choice.get_mut("message");
            let calls_tools = message.as_ref().and_then(|m| m.get("tool_calls")).is_some_and(|c| !c.is_null());
            let Some(content) = message.and_then(|m| m.get_mut("content")).filter(|c| c.is_string()) else {
                unchecked = true;
                continue;
            };
            if !stopped || calls_tools {
                unchecked = true;
                continue;
            }
            let (verdict, rewritten) = self.check_text(content.as_str().unwrap_or_default());
            if let Some(text) = rewritten {
                *content = Value::String(text);
                changed = true;
            }
            if let Verdict::Invalid(p) = verdict {
                problems.extend(p);
            }
        }
        let verdict = match (problems.is_empty(), unchecked) {
            (false, _) => Verdict::Invalid(problems),
            (true, true) => Verdict::Unchecked,
            (true, false) => Verdict::Valid,
        };
        let body = changed.then(|| Bytes::from(serde_json::to_vec(&doc).expect("a JSON value always serialises")));
        (verdict, body)
    }
}

/// Takes a request for a JSON schema out of a neutral request and asks for JSON mode with the
/// schema as an instruction instead. The instruction follows the caller's system prompt, so
/// that a cached prefix stays the same.
pub fn rewrite(req: &mut Request) -> Option<Emulated> {
    match req.response_format.take() {
        Some(ResponseFormat::JsonSchema { name, schema, strict }) => {
            let emulated = Emulated { name, schema, strict: strict == Some(true) };
            req.response_format = Some(ResponseFormat::JsonObject);
            req.system.push(Block::text(emulated.instruction()));
            Some(emulated)
        }
        other => {
            req.response_format = other;
            None
        }
    }
}

/// Adds the instruction to an OpenAI-dialect conversation: to the leading system or developer
/// message, or as one of its own before the rest. Some servers take one system message only,
/// and only first.
pub fn instruct_openai(messages: &mut Vec<Value>, instruction: &str) {
    let leading =
        messages.first_mut().filter(|m| matches!(m.get("role").and_then(Value::as_str), Some("system" | "developer")));
    match leading.and_then(|m| m.get_mut("content")) {
        Some(Value::String(text)) => {
            text.push_str("\n\n");
            text.push_str(instruction);
        }
        Some(Value::Array(parts)) => parts.push(json!({ "type": "text", "text": instruction })),
        _ => messages.insert(0, json!({ "role": "system", "content": instruction })),
    }
}

/// The text inside a code fence (```` ```json ... ``` ````), or the text itself.
fn strip_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else { return text };
    let Some(inner) = rest.strip_suffix("```") else { return text };
    // The fence's first line names the language, if anything.
    match inner.split_once('\n') {
        Some((_, body)) => body.trim(),
        None => inner.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word() -> Emulated {
        Emulated {
            name: Some("answer".into()),
            schema: json!({
                "type": "object", "properties": { "word": { "type": "string" } },
                "required": ["word"], "additionalProperties": false
            }),
            strict: true,
        }
    }

    #[test]
    fn the_instruction_names_json_and_ends_with_the_schema() {
        let text = word().instruction();
        assert!(text.contains("json object"), "DeepSeek wants the word in lower case: {text}");
        assert!(text.contains("named \"answer\""));
        let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(last, word().schema);
    }

    #[test]
    fn a_neutral_request_is_asked_in_json_mode() {
        let mut req = Request {
            system: vec![Block::text("Be brief.")],
            response_format: Some(ResponseFormat::JsonSchema {
                name: Some("answer".into()),
                schema: word().schema,
                strict: Some(true),
            }),
            ..Default::default()
        };
        assert_eq!(rewrite(&mut req), Some(word()));
        assert_eq!(req.response_format, Some(ResponseFormat::JsonObject));
        assert_eq!(req.system.len(), 2, "after the caller's own system prompt");
        assert_eq!(req.system[0], Block::text("Be brief."));
        // Nothing else is touched.
        let mut plain = Request { response_format: Some(ResponseFormat::JsonObject), ..Default::default() };
        assert_eq!(rewrite(&mut plain), None);
        assert_eq!(plain.response_format, Some(ResponseFormat::JsonObject));
        assert!(plain.system.is_empty());
    }

    #[test]
    fn the_instruction_joins_the_leading_system_message() {
        let mut m =
            vec![json!({ "role": "system", "content": "Be brief." }), json!({ "role": "user", "content": "x" })];
        instruct_openai(&mut m, "ANSWER");
        assert_eq!(m.len(), 2);
        assert_eq!(m[0]["content"], "Be brief.\n\nANSWER");
        let mut m = vec![json!({ "role": "developer", "content": [{ "type": "text", "text": "a" }] })];
        instruct_openai(&mut m, "ANSWER");
        assert_eq!(m[0]["content"][1], json!({ "type": "text", "text": "ANSWER" }));
        let mut m = vec![json!({ "role": "user", "content": "x" })];
        instruct_openai(&mut m, "ANSWER");
        assert_eq!(m[0], json!({ "role": "system", "content": "ANSWER" }));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn answers_are_checked() {
        let e = word();
        assert_eq!(e.check_text(" {\"word\": \"pong\"} "), (Verdict::Valid, None));
        assert_eq!(
            e.check_text("```json\n{\"word\": \"pong\"}\n```"),
            (Verdict::Valid, Some("{\"word\": \"pong\"}".into())),
            "a fence is taken off"
        );
        let (v, _) = e.check_text("{\"echo\": 1}");
        assert!(matches!(&v, Verdict::Invalid(p) if p.iter().any(|m| m.contains("\"word\" is required"))), "{v:?}");
        let (v, _) = e.check_text("pong");
        assert!(matches!(&v, Verdict::Invalid(p) if p[0].starts_with("the answer is not JSON")), "{v:?}");
    }

    #[test]
    fn an_openai_answer_is_checked_and_unfenced() {
        let e = word();
        let body = |content: &str, finish: &str| {
            Bytes::from(
                json!({ "id": "x", "choices": [{ "index": 0, "message": { "role": "assistant", "content": content }, "finish_reason": finish }], "usage": {} })
                    .to_string(),
            )
        };
        assert_eq!(e.check_openai(&body("{\"word\":\"a\"}", "stop")), (Verdict::Valid, None));
        let (v, rewritten) = e.check_openai(&body("```\n{\"word\":\"a\"}\n```", "stop"));
        assert_eq!(v, Verdict::Valid);
        let doc: Value = serde_json::from_slice(&rewritten.unwrap()).unwrap();
        assert_eq!(doc["choices"][0]["message"]["content"], "{\"word\":\"a\"}");
        let keys: Vec<_> = doc.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["id", "choices", "usage"], "the provider's order stays");
        assert!(matches!(e.check_openai(&body("{}", "stop")).0, Verdict::Invalid(_)));
        assert_eq!(e.check_openai(&body("{\"word\"", "length")).0, Verdict::Unchecked, "cut off at the limit");
    }

    #[test]
    fn a_neutral_answer_is_checked_and_unfenced() {
        let e = word();
        let mut r = Response {
            id: "x".into(),
            model: "m".into(),
            content: vec![Block::text("```json\n{\"word\":"), Block::text("\"a\"}\n```")],
            finish: Finish::Stop,
            native_finish: "stop".into(),
            usage: None,
            extra: Default::default(),
        };
        assert_eq!(e.check_neutral(&mut r), Verdict::Valid);
        assert_eq!(r.content, vec![Block::text("{\"word\":\"a\"}")]);
        r.finish = Finish::Length;
        assert_eq!(e.check_neutral(&mut r), Verdict::Unchecked);
    }
}
