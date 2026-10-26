//! Provider tools (concept sections 8.3 and 16): tools the provider defines and runs itself,
//! such as web search, web fetch, code execution and the MCP connector. A workspace may
//! restrict which of them its calls use, because some of them make the provider reach
//! servers of the caller's choosing.
//!
//! The check reads the body that goes to the provider, not the caller's request, so it holds
//! whichever interface or mode the call came through: a tool asked for in Anthropic's own
//! shape, as an OpenAI-interface tool, in the native interface's `provider_options`, or in
//! a relayed body.

use serde::Deserialize;
use serde::de::IgnoredAny;

use super::errors::GwError;
use crate::json::{Kind, TopLevel};

/// What the gateway needs to know about one entry of `tools`.
#[derive(Deserialize)]
struct ToolHead {
    /// Anthropic and OpenAI-compatible providers name the tool's type.
    #[serde(default, rename = "type")]
    kind: Option<String>,
    // Gemini's native API marks a provider tool by its key instead.
    #[serde(default, rename = "googleSearch", alias = "google_search")]
    google_search: Option<IgnoredAny>,
    #[serde(default, rename = "googleSearchRetrieval", alias = "google_search_retrieval")]
    google_search_retrieval: Option<IgnoredAny>,
    #[serde(default, rename = "codeExecution", alias = "code_execution")]
    code_execution: Option<IgnoredAny>,
    #[serde(default, rename = "urlContext", alias = "url_context")]
    url_context: Option<IgnoredAny>,
}

/// The keys of a Gemini tool entry the gateway knows: the caller's own functions, and the
/// provider tools [`ToolHead`] maps to the gateway's names.
const GEMINI_KNOWN: &[&str] = &[
    "functionDeclarations",
    "function_declarations",
    "googleSearch",
    "google_search",
    "googleSearchRetrieval",
    "google_search_retrieval",
    "codeExecution",
    "code_execution",
    "urlContext",
    "url_context",
];

/// The name of a provider tool without its version: `web_search_20260209` → `web_search`.
pub fn tool_family(kind: &str) -> &str {
    match kind.rfind('_') {
        Some(i) if kind[i + 1..].len() == 8 && kind[i + 1..].bytes().all(|b| b.is_ascii_digit()) => &kind[..i],
        _ => kind,
    }
}

/// A provider tool a body asks for: its type as sent, and its name without the version.
#[derive(Debug, PartialEq, Eq)]
pub struct Requested {
    pub kind: String,
    pub name: String,
}

impl Requested {
    fn of(kind: &str) -> Self {
        let name = match tool_family(kind) {
            // The MCP connector's tool set and its server list are one thing.
            "mcp_toolset" => "mcp",
            // OpenAI's Responses API names its versions otherwise (`web_search_preview_2025_03_11`)
            // and some of its tools differently.
            k if k.starts_with("web_search") => "web_search",
            k if k.starts_with("computer_use") => "computer_use",
            "code_interpreter" => "code_execution",
            other => other,
        };
        Self { kind: kind.to_owned(), name: name.to_owned() }
    }
}

/// `tools` or `mcp_servers` that cannot be read: what they ask for is not known.
#[derive(Debug, PartialEq, Eq)]
pub struct Unreadable;

/// The provider tools a request body asks for. Tools the caller defines itself (`function`,
/// `custom`, or an Anthropic tool without a type) are not provider tools. A `tools` that is
/// not a list of objects with a text `type`, or an entry that names a key twice, is
/// `Unreadable`: the provider might read a tool where the gateway sees none.
pub fn requested(body: &[u8], top: &TopLevel) -> Result<Vec<Requested>, Unreadable> {
    let mut out = Vec::new();
    if let Some(e) = top.get("tools").filter(|e| e.kind != Kind::Null) {
        if e.kind != Kind::Array {
            return Err(Unreadable);
        }
        let raw = &body[e.value.clone()];
        let tools = serde_json::from_slice::<Vec<ToolHead>>(raw).map_err(|_| Unreadable)?;
        // Gemini marks a provider tool by its key, and adds new ones over time (Google Maps,
        // file search, computer use): an entry that is neither typed nor a function the caller
        // defines counts every key the gateway does not know as a provider tool of that name.
        let entries =
            serde_json::from_slice::<Vec<serde_json::Map<String, serde_json::Value>>>(raw).map_err(|_| Unreadable)?;
        for entry in &entries {
            let defined_by_caller =
                entry.contains_key("type") || entry.contains_key("name") || entry.contains_key("function");
            if defined_by_caller {
                continue;
            }
            for key in entry.keys().filter(|k| !GEMINI_KNOWN.contains(&k.as_str())) {
                out.push(Requested::of(key));
            }
        }
        for t in tools {
            if let Some(kind) = t.kind.as_deref().filter(|k| !matches!(*k, "function" | "custom" | "")) {
                out.push(Requested::of(kind));
            }
            if t.google_search.is_some() || t.google_search_retrieval.is_some() {
                out.push(Requested::of("web_search"));
            }
            if t.code_execution.is_some() {
                out.push(Requested::of("code_execution"));
            }
            if t.url_context.is_some() {
                out.push(Requested::of("web_fetch"));
            }
        }
    }
    // Anthropic's MCP connector names its servers outside `tools`; anything there but an empty
    // list counts as the connector.
    if let Some(e) = top.get("mcp_servers").filter(|e| e.kind != Kind::Null)
        && (e.kind != Kind::Array
            || serde_json::from_slice::<Vec<IgnoredAny>>(&body[e.value.clone()]).map_or(true, |v| !v.is_empty()))
    {
        out.push(Requested::of("mcp"));
    }
    // OpenAI's search models switch web search on with a top-level object.
    if top.get("web_search_options").is_some_and(|e| e.kind == Kind::Object) {
        out.push(Requested::of("web_search"));
    }
    Ok(out)
}

/// Refuses a body that asks for a provider tool the workspace does not allow. `allowed` names
/// tools without their version (`web_search`); a full versioned type is accepted too.
pub fn check(body: &[u8], top: &TopLevel, allowed: &[String]) -> Result<(), GwError> {
    let requested = requested(body, top).map_err(|Unreadable| {
        GwError::invalid_request(
            "\"tools\" must be a list of objects, each with a text \"type\" if it has one and no key twice: \
             this workspace allows only some provider tools, so every entry is checked",
        )
    })?;
    for r in requested {
        if !allowed.iter().any(|a| *a == r.name || *a == r.kind) {
            return Err(GwError::not_allowed(format!("this workspace may not use the provider tool \"{}\"", r.name)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::scan;

    fn names(body: &str) -> Vec<String> {
        let top = scan(body.as_bytes()).unwrap();
        requested(body.as_bytes(), &top).unwrap().into_iter().map(|r| r.name).collect()
    }

    #[test]
    fn provider_tools_are_named_without_their_version() {
        assert_eq!(tool_family("web_search_20260209"), "web_search");
        assert_eq!(tool_family("code_execution_20250825"), "code_execution");
        assert_eq!(tool_family("text_editor_20250728"), "text_editor");
        assert_eq!(tool_family("custom"), "custom");
        assert_eq!(tool_family("bash_2025"), "bash_2025", "only a full date is a version");
        assert_eq!(tool_family(""), "");
    }

    #[test]
    fn finds_provider_tools_in_every_shape() {
        // Anthropic: a typed tool next to a client tool without a type.
        assert_eq!(
            names(r#"{"tools":[{"name":"f","input_schema":{}},{"type":"web_search_20260209","name":"web_search"}]}"#),
            ["web_search"]
        );
        // OpenAI-compatible: functions are the caller's own; anything else is the provider's.
        assert_eq!(
            names(r#"{"tools":[{"type":"function","function":{"name":"f"}},{"type":"web_search"}]}"#),
            ["web_search"]
        );
        // The MCP connector, by its servers and by its tool set.
        assert_eq!(names(r#"{"mcp_servers":[{"type":"url","url":"https://x"}]}"#), ["mcp"]);
        assert_eq!(names(r#"{"tools":[{"type":"mcp_toolset","mcp_server_name":"x"}]}"#), ["mcp"]);
        assert!(names(r#"{"mcp_servers":[]}"#).is_empty());
        assert!(names(r#"{"mcp_servers":[ ]}"#).is_empty());
        // Gemini's native API.
        assert_eq!(
            names(
                r#"{"tools":[{"functionDeclarations":[]},{"googleSearch":{}},{"code_execution":{}},{"urlContext":{}}]}"#
            ),
            ["web_search", "code_execution", "web_fetch"]
        );
        // A Gemini tool the gateway does not know is a provider tool by its key: refused
        // unless the workspace allows it by that name.
        assert_eq!(
            names(r#"{"tools":[{"googleMaps":{}},{"fileSearch":{"fileSearchStoreNames":["x"]}}]}"#),
            ["googleMaps", "fileSearch"]
        );
        assert!(
            names(r#"{"tools":[{"name":"f","input_schema":{}},{"type":"function","function":{"name":"g"}}]}"#)
                .is_empty()
        );
        let body = br#"{"tools":[{"googleMaps":{}}]}"#;
        let top = scan(body).unwrap();
        assert!(check(body, &top, &["web_search".into()]).is_err());
        assert!(check(body, &top, &["googleMaps".into()]).is_ok());
        // OpenAI's search models.
        assert_eq!(names(r#"{"web_search_options":{}}"#), ["web_search"]);
        // OpenAI's Responses API: its own names for the same tools.
        assert_eq!(
            names(
                r#"{"tools":[{"type":"function","name":"f"},{"type":"web_search_preview_2025_03_11"},{"type":"computer_use_preview"},{"type":"code_interpreter"},{"type":"file_search"}]}"#
            ),
            ["web_search", "computer_use", "code_execution", "file_search"]
        );
        assert!(names(r#"{"messages":[]}"#).is_empty());
    }

    #[test]
    fn the_check_accepts_a_name_or_a_full_type() {
        let body = br#"{"tools":[{"type":"web_search_20260209"}]}"#;
        let top = scan(body).unwrap();
        assert!(check(body, &top, &["web_search".into()]).is_ok());
        assert!(check(body, &top, &["web_search_20260209".into()]).is_ok());
        let e = check(body, &top, &["bash".into()]).unwrap_err();
        assert_eq!((e.status.as_u16(), e.kind), (403, "not_allowed"));
        assert!(check(body, &top, &[]).is_err(), "an empty list allows none");
    }

    #[test]
    fn tools_that_cannot_be_read_are_refused_not_skipped() {
        let allowed = ["web_search".to_owned()];
        for body in [
            // A provider reading the last "type" would run code; the gateway cannot tell.
            r#"{"tools":[{"type":"function","type":"code_execution_20250825"}]}"#,
            r#"{"tools":[{"type":7}]}"#,
            r#"{"tools":["code_execution"]}"#,
            r#"{"tools":{"type":"code_execution"}}"#,
        ] {
            let top = scan(body.as_bytes()).unwrap();
            assert_eq!(requested(body.as_bytes(), &top), Err(Unreadable), "{body}");
            let e = check(body.as_bytes(), &top, &allowed).unwrap_err();
            assert_eq!(e.status.as_u16(), 400, "{body}");
        }
        // MCP servers in any other shape than an empty list count as the connector.
        assert_eq!(names(r#"{"mcp_servers":{"url":"https://x"}}"#), ["mcp"]);
        assert!(names(r#"{"tools":null,"mcp_servers":null}"#).is_empty());
    }
}
