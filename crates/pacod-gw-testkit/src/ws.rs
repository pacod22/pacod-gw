//! A WebSocket round trip for the smoke tests: open a session through the gateway, send one
//! text message, read the answer, close.

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Sends `text` on a WebSocket at `base` + `path` (an `http://` base: the smoke tests reach the
/// gateway without TLS) and returns the first text message that comes back, and the request id
/// of the handshake.
pub async fn round_trip(
    base: &str,
    path: &str,
    headers: &[(&str, &str)],
    text: &str,
) -> anyhow::Result<(String, Option<String>)> {
    let host = base.trim_end_matches('/').strip_prefix("http://").context("the gateway's address must be http://")?;
    let mut req = format!("ws://{host}{path}").into_client_request()?;
    for (k, v) in headers {
        req.headers_mut().insert(axum::http::HeaderName::from_bytes(k.as_bytes())?, v.parse()?);
    }
    let tcp = tokio::net::TcpStream::connect(host).await.with_context(|| format!("connecting to {host}"))?;
    let (mut ws, response) = tokio_tungstenite::client_async(req, tcp).await.map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => anyhow::anyhow!("the handshake answered {}", r.status()),
        other => anyhow::anyhow!(other),
    })?;
    let id = response.headers().get("x-gw-request-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    ws.send(Message::text(text)).await?;
    let answer = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(message) = ws.next().await {
            if let Message::Text(t) = message? {
                return Ok(t.to_string());
            }
        }
        anyhow::bail!("the session closed without an answer")
    })
    .await
    .context("no answer within 10 s")??;
    let _ = ws.close(None).await;
    Ok((answer, id))
}

/// A Realtime session at `base` + `path`: waits for `session.created`, asks for one response
/// and returns its `response.done`, and the request id of the handshake.
pub async fn realtime_response(
    base: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> anyhow::Result<(serde_json::Value, Option<String>)> {
    let host = base.trim_end_matches('/').strip_prefix("http://").context("the gateway's address must be http://")?;
    let mut req = format!("ws://{host}{path}").into_client_request()?;
    for (k, v) in headers {
        req.headers_mut().insert(axum::http::HeaderName::from_bytes(k.as_bytes())?, v.parse()?);
    }
    let tcp = tokio::net::TcpStream::connect(host).await.with_context(|| format!("connecting to {host}"))?;
    let (mut ws, response) = tokio_tungstenite::client_async(req, tcp).await.map_err(|e| match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => anyhow::anyhow!("the handshake answered {}", r.status()),
        other => anyhow::anyhow!(other),
    })?;
    let id = response.headers().get("x-gw-request-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let done = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut asked = false;
        while let Some(message) = ws.next().await {
            let Message::Text(t) = message? else { continue };
            let v: serde_json::Value = serde_json::from_str(&t)?;
            match v["type"].as_str().unwrap_or_default() {
                "session.created" if !asked => {
                    asked = true;
                    let ask = serde_json::json!({ "type": "response.create", "response": {
                        "output_modalities": ["text"], "instructions": "Say hello in three words.",
                    }});
                    ws.send(Message::text(ask.to_string())).await?;
                }
                "response.done" => return Ok(v),
                "error" => anyhow::bail!("the session answered an error: {}", v["error"]),
                _ => {}
            }
        }
        anyhow::bail!("the session closed without a response")
    })
    .await
    .context("no response within 20 s")??;
    let _ = ws.close(None).await;
    Ok((done, id))
}
