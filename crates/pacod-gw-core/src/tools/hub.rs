//! What the gateway needs to call tool servers: their egress policy and the client that keeps
//! to it. One per process; the minimal gateway has none, so its `/mcp` serves no tool server.

use std::sync::Arc;

use pacod_gw_events::egress::EgressPolicy;

use super::client::ToolClient;

pub struct ToolHub {
    /// Checked when a tool server's address is saved, and on every connection.
    pub egress: Arc<EgressPolicy>,
    pub client: ToolClient,
}

impl ToolHub {
    pub fn new(egress: Arc<EgressPolicy>) -> anyhow::Result<Self> {
        Ok(Self { client: ToolClient::new(egress.clone())?, egress })
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
