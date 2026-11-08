//! The methods of the gateway's MCP server: the handshake of the 2025-11-25 revision, the
//! discovery of the 2026-07-28 one, the tool list, and tool calls.

use std::collections::BTreeMap;
use std::time::Instant;

use axum::http::StatusCode;
use axum::response::Response;
use pacod_gw_events::tool::verdict;
use pacod_gw_events::usage::status;
use serde_json::{Value, json};

use super::{
    Call, Era, INTERNAL_ERROR, INVALID_PARAMS, LEGACY_REVISIONS, METHOD_NOT_FOUND, Pending, broker, rpc_error,
    rpc_result, server_info,
};
use crate::bootstrap::BrokerLink;
use crate::limits::Subjects;
use crate::model::{ResultLimits, ToolBundle};
use crate::telemetry::metrics;
use crate::tools::client::{ClientError, LEGACY, MODERN};
use crate::tools::grant::{self, Granted};
use crate::tools::pin;

/// How long a caller may keep a tool list, in milliseconds: approvals take effect within it.
const LIST_TTL_MS: u64 = 60_000;
/// The error of a tool call a limit refused: an implementation-defined server error, since MCP
/// has none of its own for it.
const LIMITED: i64 = -32000;
const METHODS: [&str; 5] = ["initialize", "ping", "server/discover", "tools/list", "tools/call"];

/// The method as a metric label: one of [`METHODS`], or `other`, so that a caller cannot add
/// series by inventing methods.
pub(super) fn label(req: &Value) -> &'static str {
    let method = req["method"].as_str().unwrap_or_default();
    METHODS.iter().find(|m| **m == method).copied().unwrap_or("other")
}

pub(super) async fn answer(call: &Call<'_>, req: Value, bundle: Option<&str>) -> Response {
    let id = req["id"].clone();
    let method = label(&req);
    let (answer, outcome) = match (method, call.era) {
        ("initialize", Era::Legacy) => (initialize(&req, &id), "answered"),
        ("ping", _) => (rpc_result(call.era, &id, json!({})).0, "answered"),
        ("server/discover", Era::Modern) => (discover(&id), "answered"),
        ("tools/list", _) => match Scope::of(call, bundle) {
            Some(scope) => (list(call, &id, &scope).await, "answered"),
            None => (unknown_bundle(call, &id), "refused"),
        },
        ("tools/call", _) => return tools_call(call, &req, bundle).await,
        _ => (
            rpc_error(
                call.era,
                &id,
                METHOD_NOT_FOUND,
                "the gateway's MCP server offers tools only",
                None,
                StatusCode::NOT_FOUND,
            )
            .0,
            "refused",
        ),
    };
    metrics::record_mcp(method, outcome);
    answer
}

/// The 2025-11-25 handshake, which the gateway answers itself: the revision the client asks
/// for when the gateway knows it, else the newest of that line, and no session id, since no
/// state is kept between requests.
fn initialize(req: &Value, id: &Value) -> Response {
    let asked = req["params"]["protocolVersion"].as_str().unwrap_or_default();
    let version = if LEGACY_REVISIONS.contains(&asked) { asked } else { LEGACY };
    let result = json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": server_info(),
        "instructions": "Tools of this organisation's tool servers, approved by its administrators, as <server>__<tool>."
    });
    rpc_result(Era::Legacy, id, result).0
}

/// The 2026-07-28 revision's discovery.
fn discover(id: &Value) -> Response {
    let result = json!({
        "supportedVersions": [MODERN],
        "capabilities": { "tools": {} },
        "instructions": "Tools of this organisation's tool servers, approved by its administrators, as <server>__<tool>.",
        "ttlMs": LIST_TTL_MS,
        "cacheScope": "private"
    });
    rpc_result(Era::Modern, id, result).0
}

fn unknown_bundle(call: &Call<'_>, id: &Value) -> Response {
    rpc_error(call.era, id, INVALID_PARAMS, "this key has no such tool bundle", None, StatusCode::NOT_FOUND).0
}

/// What a caller may reach: the bundles of its key (one of them on `/mcp/{bundle}`) and the
/// broker's tools (on `/mcp` only).
struct Scope<'a> {
    bundles: Vec<&'a ToolBundle>,
    broker: Option<&'a BrokerLink>,
}

impl<'a> Scope<'a> {
    /// `None` when `/mcp/{bundle}` names a bundle the key does not have: told apart from one
    /// that does not exist by nothing.
    fn of(call: &'a Call<'a>, bundle: Option<&str>) -> Option<Self> {
        let ws = &call.caller.workspace;
        let named = call.caller.key.doc.tool_bundles.iter().filter_map(|n| ws.tool_bundles.get(n).map(|b| &**b));
        match bundle {
            None => Some(Self { bundles: named.collect(), broker: call.gw.boot.broker.as_ref() }),
            Some(name) => {
                let one = named.into_iter().find(|b| b.name == name)?;
                Some(Self { bundles: vec![one], broker: None })
            }
        }
    }

    fn granted(&self, call: &'a Call<'a>) -> BTreeMap<&'a str, Granted<'a>> {
        match call.caller.snapshot.organisations.get(&call.caller.workspace.organisation_id) {
            Some(org) => grant::granted(org, &call.caller.workspace, &self.bundles),
            None => BTreeMap::new(),
        }
    }
}

async fn list(call: &Call<'_>, id: &Value, scope: &Scope<'_>) -> Response {
    let mut tools: Vec<Value> = scope
        .granted(call)
        .into_iter()
        .map(|(name, g)| {
            let mut definition = g.pin.definition.clone();
            definition["name"] = json!(name);
            definition
        })
        .collect();
    if let Some(link) = scope.broker
        && let Some(found) = broker::list(link).await
    {
        tools.extend(found);
    }
    let mut result = json!({ "tools": tools });
    if call.era == Era::Modern {
        result["ttlMs"] = json!(LIST_TTL_MS);
        // The list depends on the caller's key: a shared cache must not hand it to another.
        result["cacheScope"] = json!("private");
    }
    rpc_result(call.era, id, result).0
}

async fn tools_call(call: &Call<'_>, req: &Value, bundle: Option<&str>) -> Response {
    let id = req["id"].clone();
    let name = req["params"]["name"].as_str().unwrap_or_default();
    let arguments = req["params"]["arguments"].clone();
    let Some(scope) = Scope::of(call, bundle) else {
        let mut pending = Pending::new(call, call.record(req, "", name));
        refused(pending.record(), verdict::DENIED, INVALID_PARAMS, "this key has no such tool bundle");
        metrics::record_mcp("tools/call", "refused");
        return pending.finish((unknown_bundle(call, &id), 0));
    };
    if let Some(link) = scope.broker.filter(|_| broker::TOOLS.contains(&name)) {
        return broker_call(call, req, link, name, &arguments).await;
    }
    let granted = scope.granted(call);
    let Some(g) = granted.get(name) else {
        // Unknown and not granted read alike, so that a caller learns nothing of other tools.
        let mut pending = Pending::new(call, call.record(req, "", name));
        let why = format!("unknown tool: {name}");
        refused(pending.record(), verdict::DENIED, INVALID_PARAMS, &why);
        metrics::record_mcp("tools/call", "refused");
        return pending.finish(rpc_error(call.era, &id, INVALID_PARAMS, &why, None, StatusCode::OK));
    };
    let mut pending = Pending::new(call, call.record(req, &g.server.doc.name, &g.pin.tool));
    {
        let r = pending.record();
        r.target_id = g.server.id.clone();
        r.exposed_name = name.to_owned();
        r.pin_hash = g.pin.hash.clone();
        r.verdict = verdict::ALLOWED.into();
    }
    if let Some(answer) = admit(call, pending.record(), &g.server.doc.name, &g.pin.tool, &id) {
        return pending.finish(answer);
    }
    let Some(hub) = call.gw.tools.as_ref() else {
        return pending.finish(rpc_error(
            call.era,
            &id,
            INTERNAL_ERROR,
            "this gateway calls no tool servers",
            None,
            StatusCode::OK,
        ));
    };
    let headers = pin::header_params(&g.pin.definition).map(|p| pin::param_headers(&p, &arguments)).unwrap_or_default();
    let sent = Instant::now();
    let outcome = hub.call_tool(g.server, &call.caller.workspace.id, &g.pin.tool, arguments, &headers).await;
    let r = pending.record();
    r.upstream_ms = sent.elapsed().as_secs_f32() * 1000.0;
    r.http_status = 200;
    let answer = match outcome {
        Ok((answer, _)) => match (answer.message.get("error"), answer.message.get("result")) {
            (Some(error), _) => {
                let code = error["code"].as_i64().unwrap_or(INTERNAL_ERROR);
                let text = error["message"].as_str().unwrap_or("the tool server refused the call");
                (r.status, r.error_type, r.error_message) =
                    (status::ERROR.into(), "upstream_error".into(), text.into());
                r.rpc_error_code = i32::try_from(code).unwrap_or_default();
                rpc_error(call.era, &id, code, &format!("{}: {text}", g.server.doc.name), None, StatusCode::OK)
            }
            (None, Some(result)) => match shape(result, &g.server.doc.result) {
                Ok(result) => {
                    r.status = status::OK.into();
                    r.result_error = result["isError"].as_bool().unwrap_or_default();
                    rpc_result(call.era, &id, result)
                }
                Err(why) => {
                    (r.status, r.error_type, r.error_message) =
                        (status::ERROR.into(), "content_refused".into(), why.clone());
                    r.rpc_error_code = INTERNAL_ERROR as i32;
                    rpc_error(call.era, &id, INTERNAL_ERROR, &why, None, StatusCode::OK)
                }
            },
            (None, None) => unreachable!("the client returns results and errors only"),
        },
        Err(e) => {
            r.status = if matches!(e, ClientError::Timeout(_)) { status::TIMEOUT } else { status::ERROR }.into();
            r.error_type = e.kind().into();
            r.error_message = e.to_string();
            r.truncated = matches!(e, ClientError::TooLarge(_));
            r.rpc_error_code = INTERNAL_ERROR as i32;
            rpc_error(call.era, &id, INTERNAL_ERROR, &format!("{}: {e}", g.server.doc.name), None, StatusCode::OK)
        }
    };
    let outcome = if pending.record().status == status::OK { "answered" } else { "failed" };
    metrics::record_mcp("tools/call", outcome);
    pending.finish(answer)
}

async fn broker_call(call: &Call<'_>, req: &Value, link: &BrokerLink, name: &str, arguments: &Value) -> Response {
    let id = req["id"].clone();
    let mut pending = Pending::new(call, call.record(req, broker::TARGET, name));
    pending.record().verdict = verdict::ALLOWED.into();
    if let Some(answer) = admit(call, pending.record(), broker::TARGET, name, &id) {
        return pending.finish(answer);
    }
    let sent = Instant::now();
    let outcome = broker::call(link, name, arguments).await;
    let r = pending.record();
    r.upstream_ms = sent.elapsed().as_secs_f32() * 1000.0;
    r.http_status = 200;
    let answer = match outcome {
        Ok(message) => match message.get("result") {
            Some(result) => {
                r.status = status::OK.into();
                r.result_error = result["isError"].as_bool().unwrap_or_default();
                rpc_result(call.era, &id, result.clone())
            }
            None => {
                let code = message["error"]["code"].as_i64().unwrap_or(INTERNAL_ERROR);
                let text = message["error"]["message"].as_str().unwrap_or("the broker refused the call");
                (r.status, r.error_type, r.error_message) =
                    (status::ERROR.into(), "upstream_error".into(), text.into());
                r.rpc_error_code = i32::try_from(code).unwrap_or_default();
                rpc_error(call.era, &id, code, text, None, StatusCode::OK)
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "the broker's MCP server did not answer");
            (r.status, r.error_type, r.error_message) = (status::ERROR.into(), "unavailable".into(), e.clone());
            r.rpc_error_code = INTERNAL_ERROR as i32;
            rpc_error(call.era, &id, INTERNAL_ERROR, "the broker does not answer", None, StatusCode::OK)
        }
    };
    let outcome = if pending.record().status == status::OK { "answered" } else { "unavailable" };
    metrics::record_mcp("tools/call", outcome);
    pending.finish(answer)
}

/// Counts a tool call against the limits of tool calls that apply to it (runbook section 21.2),
/// or refuses it: then the record says `limited`, and the answer, which names the limit, is
/// returned.
fn admit(
    call: &Call<'_>,
    r: &mut pacod_gw_events::ToolCallRecord,
    server: &str,
    tool: &str,
    id: &Value,
) -> Option<(Response, usize)> {
    let subjects = Subjects {
        workspace: &call.caller.workspace,
        key: &call.caller.key,
        end_user: &r.end_user,
        model: "",
        template: "",
        tags: &r.tags,
        intent: None,
        internal: false,
        tool: Some((server, tool)),
    };
    let mut applicable = call.gw.limits.applicable(&subjects, chrono::Utc::now());
    let Err(refusal) = call.gw.limits.admit(&mut applicable) else { return None };
    let why = format!("the limit {} has no room for another tool call", refusal.limit_name);
    refused(r, verdict::LIMITED, LIMITED, &why);
    r.error_type = "limited".into();
    metrics::record_mcp("tools/call", "limited");
    let data = json!({ "limit": refusal.limit_name, "resets_at": refusal.resets_at,
                       "retry_after_seconds": refusal.retry_after_seconds });
    Some(rpc_error(call.era, id, LIMITED, &why, Some(data), StatusCode::OK))
}

/// A call the gateway did not pass on.
pub(super) fn refused(r: &mut pacod_gw_events::ToolCallRecord, verdict: &str, code: i64, why: &str) {
    (r.verdict, r.status, r.http_status) = (verdict.into(), status::REJECTED.into(), 200);
    (r.rpc_error_code, r.error_type, r.error_message) = (code as i32, "not_allowed".into(), why.into());
}

/// What of a tool's result reaches the caller (catalogue MCP-13): its content, when every part
/// is of a kind the server passes on, its structured content and whether it is an error. The
/// server's own `_meta` stays behind; the answer's size was held to the server's limit when it
/// was read.
fn shape(result: &Value, limits: &ResultLimits) -> Result<Value, String> {
    let content = result.get("content").cloned().unwrap_or_else(|| json!([]));
    let parts = content.as_array().ok_or("the tool's content is not a list")?;
    for part in parts {
        let kind = part["type"].as_str().unwrap_or_default();
        if !limits.content_types.iter().any(|t| t == kind) {
            return Err(format!(
                "the tool answered content of the kind {kind:?}, which this tool server may not pass on"
            ));
        }
    }
    let mut out = json!({ "content": content });
    if let Some(structured) = result.get("structuredContent") {
        out["structuredContent"] = structured.clone();
    }
    if let Some(is_error) = result.get("isError") {
        out["isError"] = is_error.clone();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_keeps_its_content_and_loses_the_servers_meta() {
        let limits = ResultLimits::default();
        let result = json!({ "content": [{ "type": "text", "text": "hi" }], "isError": false,
                             "structuredContent": { "n": 1 }, "_meta": { "trace": "x" }, "resultType": "complete" });
        let shaped = shape(&result, &limits).unwrap();
        assert_eq!(
            shaped,
            json!({ "content": [{ "type": "text", "text": "hi" }], "structuredContent": { "n": 1 }, "isError": false })
        );
        let audio = json!({ "content": [{ "type": "audio", "data": "", "mimeType": "audio/wav" }] });
        assert!(shape(&audio, &limits).unwrap_err().contains("audio"), "audio is not passed by default");
    }
}
