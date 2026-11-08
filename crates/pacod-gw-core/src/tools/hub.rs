//! What the gateway needs to call tool servers: their egress policy, the client that keeps to
//! it, and how each server is reached. One per process; the minimal gateway has none, so its
//! `/mcp` serves no tool server.
//!
//! How a server is reached is found once per instance and kept: that it speaks the 2026-07-28
//! revision, or the session it gave in the 2025-11-25 one. A session is kept per server,
//! credential and workspace, so that calls of different workspaces never share one, and opened
//! again once when the server has forgotten it. A server whose sessions hold state of their own
//! is set to `per_call` and gets a new session for every call.

use std::collections::HashMap;
use std::sync::Arc;

use pacod_gw_events::egress::EgressPolicy;
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::client::{Answer, ClientError, Link, ToolClient};
use crate::model::SessionMode;
use crate::snapshot::ToolServerRt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LinkKey {
    server: String,
    credential: String,
    workspace: String,
}

pub struct ToolHub {
    /// Checked when a tool server's address is saved, and on every request.
    pub egress: Arc<EgressPolicy>,
    pub client: ToolClient,
    links: Mutex<HashMap<LinkKey, Link>>,
}

impl ToolHub {
    pub fn new(egress: Arc<EgressPolicy>) -> anyhow::Result<Self> {
        Ok(Self { client: ToolClient::new(egress.clone())?, egress, links: Mutex::default() })
    }

    /// Calls `tool` of a server for a workspace, with its own `Mcp-Param-*` headers, and returns
    /// the server's answer; the revision it speaks is the answer's link.
    pub async fn call_tool(
        &self,
        server: &ToolServerRt,
        workspace: &str,
        tool: &str,
        arguments: Value,
        headers: &[(String, String)],
    ) -> Result<(Answer, Link), ClientError> {
        let target = server.target(Some(workspace), server.doc.result.max_bytes as usize);
        let key = LinkKey {
            server: server.id.clone(),
            credential: server.credential_for(workspace).map(|s| s.id.clone()).unwrap_or_default(),
            workspace: workspace.to_owned(),
        };
        let pooled = server.doc.session == SessionMode::Pooled;
        let params = json!({ "name": tool, "arguments": arguments });
        for attempt in 0..2 {
            let kept = if pooled { self.links.lock().get(&key).cloned() } else { None };
            let link = match kept {
                Some(link) => link,
                None => {
                    let link = self.client.connect(&target).await?;
                    if pooled {
                        self.links.lock().insert(key.clone(), link.clone());
                    }
                    link
                }
            };
            match self.client.call_with(&target, &link, Some(1), "tools/call", params.clone(), headers).await {
                Ok(answer) => return Ok((answer, link)),
                // The server forgot the session (a restart): nothing was done, so once more.
                Err(ClientError::SessionGone) if attempt == 0 => {
                    self.links.lock().remove(&key);
                }
                Err(e) => {
                    // What was kept may be why: the next call finds out again.
                    if matches!(e, ClientError::Status(..) | ClientError::Malformed(_) | ClientError::SessionGone) {
                        self.links.lock().remove(&key);
                    }
                    return Err(e);
                }
            }
        }
        Err(ClientError::SessionGone)
    }
}

impl crate::snapshot::ToolServerRt {
    /// How to reach the server for calls of `workspace` (with the organisation's credential
    /// when no workspace is given), reading answers up to `max_bytes`.
    pub fn target(&self, workspace: Option<&str>, max_bytes: usize) -> super::client::Target<'_> {
        let secret = match workspace {
            Some(ws) => self.credential_for(ws),
            None => self.credential.as_ref(),
        };
        super::client::Target {
            url: &self.doc.url,
            auth: secret
                .map(|s| (self.doc.auth.header.as_str(), format!("{}{}", self.doc.auth.prefix, s.value.as_str()))),
            timeout: std::time::Duration::from_millis(u64::from(self.doc.timeout_ms)),
            max_bytes,
        }
    }
}
