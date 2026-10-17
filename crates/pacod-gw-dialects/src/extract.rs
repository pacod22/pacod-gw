//! The text of a conversation, read from a request body of any dialect, for those who look at
//! what a call is about without changing it: the plugins (`docs/plugin-concept.md`, section
//! 3.4).
//!
//! The reader is lenient on purpose. It takes roles and text and ignores every other field, so
//! that a body a dialect's `parse_request` would refuse (a parameter it does not know, a block
//! type that is new) still yields its text. Images, documents and audio are counted, not read.
//! Tool definitions are kept by name only.

use serde_json::Value;

/// The dialects a body can be read in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// OpenAI's chat completions.
    OpenAiChat,
    /// OpenAI's Responses API.
    OpenAiResponses,
    Anthropic,
    /// Gemini's `generateContent`, on AI Studio and Vertex AI.
    Gemini,
    /// The gateway's native interface.
    Native,
    /// Ollama's native chat.
    Ollama,
}

/// Who said a turn. System prompts are kept apart, in [`Conversation::system`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
    /// The result of a tool call, as the application sent it back.
    Tool,
}

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub text: String,
}

/// What a request says, as text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conversation {
    /// System prompts and instructions, in order.
    pub system: Vec<String>,
    /// The turns, oldest first. A turn without text (an image alone) is left out.
    pub turns: Vec<Turn>,
    /// Images, documents, audio and files, counted.
    pub attachments: u32,
    /// The names of the tools the request defines.
    pub tools: Vec<String>,
}

impl Conversation {
    pub fn is_empty(&self) -> bool {
        self.system.is_empty() && self.turns.is_empty()
    }

    /// The newest turn of the user, if any.
    pub fn last_user(&self) -> Option<&Turn> {
        self.turns.iter().rev().find(|t| t.role == Role::User)
    }
}

/// Reads the conversation of a request body. A body that is not a JSON object gives an empty
/// conversation.
pub fn conversation(dialect: Dialect, body: &[u8]) -> Conversation {
    let Ok(Value::Object(top)) = serde_json::from_slice::<Value>(body) else {
        return Conversation::default();
    };
    let top = Value::Object(top);
    let mut c = Conversation::default();
    match dialect {
        Dialect::OpenAiChat | Dialect::Ollama => {
            for m in list(&top, "messages") {
                let role = m.get("role").and_then(Value::as_str).unwrap_or_default();
                let mut text = Vec::new();
                openai_content(m.get("content"), &mut text, &mut c.attachments);
                c.attachments += m.get("images").and_then(Value::as_array).map_or(0, |a| a.len() as u32);
                push(&mut c, role, text);
            }
            // Ollama's `/api/generate` and OpenAI's legacy completions take a prompt instead.
            if let Some(system) = top.get("system").and_then(Value::as_str) {
                push(&mut c, "system", vec![system.to_owned()]);
            }
            match top.get("prompt") {
                Some(Value::String(p)) => push(&mut c, "user", vec![p.clone()]),
                Some(Value::Array(ps)) => {
                    push(&mut c, "user", ps.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                }
                _ => {}
            }
            for t in list(&top, "tools") {
                if let Some(n) = t.pointer("/function/name").and_then(Value::as_str) {
                    c.tools.push(n.to_owned());
                }
            }
        }
        Dialect::OpenAiResponses => {
            if let Some(s) = top.get("instructions").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                c.system.push(s.to_owned());
            }
            match top.get("input") {
                Some(Value::String(s)) => push(&mut c, "user", vec![s.clone()]),
                Some(Value::Array(items)) => {
                    for item in items {
                        match item.get("type").and_then(Value::as_str) {
                            Some("function_call_output") => {
                                let out = item.get("output").map(text_of).unwrap_or_default();
                                push(&mut c, "tool", vec![out]);
                            }
                            Some("message") | None => {
                                let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
                                let mut text = Vec::new();
                                openai_content(item.get("content"), &mut text, &mut c.attachments);
                                push(&mut c, role, text);
                            }
                            // Reasoning, tool calls the model made, and items that are new.
                            Some(_) => {}
                        }
                    }
                }
                _ => {}
            }
            for t in list(&top, "tools") {
                if let Some(n) = t.get("name").and_then(Value::as_str) {
                    c.tools.push(n.to_owned());
                }
            }
        }
        Dialect::Anthropic | Dialect::Native => {
            let mut system = Vec::new();
            blocks(top.get("system"), &mut system, &mut c.attachments, &mut Vec::new());
            c.system.extend(system.into_iter().filter(|s| !s.trim().is_empty()));
            for m in list(&top, "messages") {
                let role = m.get("role").and_then(Value::as_str).unwrap_or_default();
                let (mut text, mut results) = (Vec::new(), Vec::new());
                blocks(m.get("content"), &mut text, &mut c.attachments, &mut results);
                // Tool results travel in the user's turn on Anthropic's API; they are the
                // application's data, not what the user said.
                if !results.is_empty() {
                    push(&mut c, "tool", results);
                }
                push(&mut c, role, text);
            }
            for t in list(&top, "tools") {
                if let Some(n) = t.get("name").and_then(Value::as_str) {
                    c.tools.push(n.to_owned());
                }
            }
        }
        Dialect::Gemini => {
            let instruction = top.get("systemInstruction").or_else(|| top.get("system_instruction"));
            if let Some(parts) = instruction.and_then(|s| s.get("parts")).and_then(Value::as_array) {
                for p in parts {
                    if let Some(t) = p.get("text").and_then(Value::as_str).filter(|t| !t.trim().is_empty()) {
                        c.system.push(t.to_owned());
                    }
                }
            }
            for m in list(&top, "contents") {
                let role = match m.get("role").and_then(Value::as_str) {
                    Some("model") => "assistant",
                    _ => "user",
                };
                let (mut text, mut results) = (Vec::new(), Vec::new());
                for p in m.get("parts").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                        if p.get("thought").and_then(Value::as_bool) != Some(true) {
                            text.push(t.to_owned());
                        }
                    } else if let Some(r) = p.get("functionResponse").or_else(|| p.get("function_response")) {
                        results.push(r.get("response").map(Value::to_string).unwrap_or_default());
                    } else if ["inlineData", "inline_data", "fileData", "file_data"].iter().any(|k| p.get(*k).is_some())
                    {
                        c.attachments += 1;
                    }
                }
                if !results.is_empty() {
                    push(&mut c, "tool", results);
                }
                push(&mut c, role, text);
            }
            for t in list(&top, "tools") {
                let decls = t.get("functionDeclarations").or_else(|| t.get("function_declarations"));
                for d in decls.and_then(Value::as_array).into_iter().flatten() {
                    if let Some(n) = d.get("name").and_then(Value::as_str) {
                        c.tools.push(n.to_owned());
                    }
                }
            }
        }
    }
    c
}

fn list<'a>(v: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    v.get(key).and_then(Value::as_array).into_iter().flatten()
}

/// A value as text: a string as it is, anything else as its JSON.
fn text_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Adds a turn by its role in the dialect's words. System and developer messages are system
/// prompts; text-less turns are left out.
fn push(c: &mut Conversation, role: &str, text: Vec<String>) {
    let text: Vec<String> = text.into_iter().filter(|t| !t.trim().is_empty()).collect();
    if text.is_empty() {
        return;
    }
    let text = text.join("\n");
    let role = match role {
        "system" | "developer" => {
            c.system.push(text);
            return;
        }
        "assistant" | "model" => Role::Assistant,
        "tool" | "function" => Role::Tool,
        _ => Role::User,
    };
    c.turns.push(Turn { role, text });
}

/// OpenAI's message content: a string, or a list of parts.
fn openai_content(content: Option<&Value>, text: &mut Vec<String>, attachments: &mut u32) {
    match content {
        Some(Value::String(s)) => text.push(s.clone()),
        Some(Value::Array(parts)) => {
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text" | "input_text" | "output_text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            text.push(t.to_owned());
                        }
                    }
                    Some("image_url" | "input_image" | "input_audio" | "file" | "input_file") => *attachments += 1,
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Anthropic's and the native interface's content: a string, or a list of blocks. Tool results
/// go to `results`.
fn blocks(content: Option<&Value>, text: &mut Vec<String>, attachments: &mut u32, results: &mut Vec<String>) {
    match content {
        Some(Value::String(s)) => text.push(s.clone()),
        Some(Value::Array(list)) => {
            for b in list {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            text.push(t.to_owned());
                        }
                    }
                    Some("image" | "document" | "audio" | "file") => *attachments += 1,
                    Some("tool_result") => {
                        let mut inner = Vec::new();
                        blocks(b.get("content"), &mut inner, attachments, &mut Vec::new());
                        results.push(inner.join("\n"));
                    }
                    // Tool calls, thinking, and blocks that are new.
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn read(d: Dialect, body: Value) -> Conversation {
        conversation(d, body.to_string().as_bytes())
    }

    fn texts(c: &Conversation) -> Vec<(&'static str, &str)> {
        c.turns.iter().map(|t| (t.role.name(), t.text.as_str())).collect()
    }

    #[test]
    fn openai_chat_keeps_roles_text_and_counts_images() {
        let c = read(
            Dialect::OpenAiChat,
            json!({
                "model": "m", "some_new_parameter": { "x": 1 },
                "messages": [
                    { "role": "developer", "content": "Be brief." },
                    { "role": "user", "content": [
                        { "type": "text", "text": "What is in this picture?" },
                        { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } }
                    ] },
                    { "role": "assistant", "content": null, "tool_calls": [{ "id": "c", "type": "function" }] },
                    { "role": "tool", "tool_call_id": "c", "content": "42" },
                    { "role": "user", "content": "And now?" }
                ],
                "tools": [{ "type": "function", "function": { "name": "lookup" } }]
            }),
        );
        assert_eq!(c.system, vec!["Be brief."]);
        assert_eq!(texts(&c), vec![("user", "What is in this picture?"), ("tool", "42"), ("user", "And now?")]);
        assert_eq!(c.attachments, 1);
        assert_eq!(c.tools, vec!["lookup"]);
        assert_eq!(c.last_user().map(|t| t.text.as_str()), Some("And now?"));
    }

    #[test]
    fn anthropic_separates_tool_results_from_what_the_user_said() {
        let c = read(
            Dialect::Anthropic,
            json!({
                "system": [{ "type": "text", "text": "You are a bot." }],
                "messages": [
                    { "role": "user", "content": "Hello" },
                    { "role": "assistant", "content": [{ "type": "thinking", "thinking": "hm" }, { "type": "text", "text": "Hi" }] },
                    { "role": "user", "content": [
                        { "type": "tool_result", "tool_use_id": "t", "content": [{ "type": "text", "text": "result" }] },
                        { "type": "document", "source": {} },
                        { "type": "text", "text": "Summarise it" }
                    ] }
                ],
                "tools": [{ "name": "search" }]
            }),
        );
        assert_eq!(c.system, vec!["You are a bot."]);
        assert_eq!(
            texts(&c),
            vec![("user", "Hello"), ("assistant", "Hi"), ("tool", "result"), ("user", "Summarise it")]
        );
        assert_eq!((c.attachments, c.tools.clone()), (1, vec!["search".to_owned()]));
    }

    #[test]
    fn gemini_reads_parts_and_skips_thoughts() {
        let c = read(
            Dialect::Gemini,
            json!({
                "systemInstruction": { "parts": [{ "text": "Answer in German." }] },
                "contents": [
                    { "role": "user", "parts": [{ "text": "Wie spät ist es?" }, { "inlineData": { "mimeType": "image/png" } }] },
                    { "role": "model", "parts": [{ "text": "secret", "thought": true }, { "text": "Zwölf." }] }
                ]
            }),
        );
        assert_eq!(c.system, vec!["Answer in German."]);
        assert_eq!(texts(&c), vec![("user", "Wie spät ist es?"), ("assistant", "Zwölf.")]);
        assert_eq!(c.attachments, 1);
    }

    #[test]
    fn responses_api_input_as_text_and_as_items() {
        let c = read(Dialect::OpenAiResponses, json!({ "instructions": "Be kind.", "input": "Hi there" }));
        assert_eq!((c.system.clone(), texts(&c)), (vec!["Be kind.".to_owned()], vec![("user", "Hi there")]));
        let c = read(
            Dialect::OpenAiResponses,
            json!({ "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "first" }] },
                { "type": "function_call_output", "call_id": "c", "output": "{\"ok\":true}" },
                { "type": "reasoning", "summary": [] }
            ] }),
        );
        assert_eq!(texts(&c), vec![("user", "first"), ("tool", "{\"ok\":true}")]);
    }

    #[test]
    fn native_and_ollama() {
        let c = read(
            Dialect::Native,
            json!({ "system": "S", "messages": [{ "role": "user", "content": [{ "type": "text", "text": "x" }] }] }),
        );
        assert_eq!((c.system.clone(), texts(&c)), (vec!["S".to_owned()], vec![("user", "x")]));
        let c =
            read(Dialect::Ollama, json!({ "messages": [{ "role": "user", "content": "y", "images": ["a", "b"] }] }));
        assert_eq!((texts(&c), c.attachments), (vec![("user", "y")], 2));
        let c = read(Dialect::Ollama, json!({ "model": "llama", "system": "S", "prompt": "Why is the sky blue?" }));
        assert_eq!((c.system.clone(), texts(&c)), (vec!["S".to_owned()], vec![("user", "Why is the sky blue?")]));
    }

    #[test]
    fn unreadable_bodies_give_an_empty_conversation() {
        assert!(conversation(Dialect::OpenAiChat, b"not json").is_empty());
        assert!(conversation(Dialect::Anthropic, b"[1,2]").is_empty());
        assert!(read(Dialect::Gemini, json!({ "contents": "nonsense" })).is_empty());
    }
}
