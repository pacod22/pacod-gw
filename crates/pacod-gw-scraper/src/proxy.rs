//! The egress proxy the browser goes out through: the browser has no other route out, so every
//! request a page makes, its own and those of its scripts, connects only where the egress policy
//! allows. HTTPS is tunnelled (`CONNECT`): the browser speaks TLS with the site itself, which
//! sees the browser it expects. Plain `http://` (a mock on the development network) is
//! forwarded one request per connection. Each direction of a connection is capped in bytes.

use std::sync::Arc;

use pacod_gw_events::egress::EgressPolicy;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::fetch::resolve_allowed;

/// The longest request head the proxy reads.
const MAX_HEAD: usize = 16 * 1024;
/// What one connection may bring in from the outside and send out to it.
const MAX_IN: u64 = 64 << 20;
const MAX_OUT: u64 = 16 << 20;

/// Serves the proxy on `listener` until the task is aborted.
pub async fn serve(listener: TcpListener, policy: Arc<EgressPolicy>) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let policy = policy.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(stream, &policy).await {
                tracing::debug!(%peer, error = %e, "a proxy connection ended with an error");
            }
        });
    }
}

/// Reads the request head: up to the blank line, at most [`MAX_HEAD`] bytes.
async fn read_head(stream: &mut TcpStream) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            anyhow::bail!("the connection closed before a request");
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Ok((buf, rest));
        }
        if buf.len() > MAX_HEAD {
            anyhow::bail!("the request head is too long");
        }
    }
}

async fn refuse(stream: &mut TcpStream, status: &str, why: &str) -> anyhow::Result<()> {
    let body = format!("{why}\n");
    let answer = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(answer.as_bytes()).await?;
    Ok(())
}

/// Connects to `host:port` where the egress policy allows: the name is checked, and then every
/// address it resolves to, so that a public name pointing inside is refused too.
async fn connect(policy: &EgressPolicy, host: &str, port: u16, scheme: &str) -> Result<TcpStream, String> {
    let shown = if host.contains(':') { format!("[{host}]") } else { host.to_owned() };
    policy.check_url(&format!("{scheme}://{shown}:{port}/")).map_err(|e| e.to_string())?;
    let addrs = resolve_allowed(policy, host, port).await.map_err(|e| e.to_string())?;
    TcpStream::connect(&addrs[..]).await.map_err(|e| format!("{host}:{port}: {e}"))
}

/// Copies both ways until either side closes, each direction within its cap.
async fn pipe(client: TcpStream, upstream: TcpStream, early: Vec<u8>) -> anyhow::Result<()> {
    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();
    if !early.is_empty() {
        uw.write_all(&early).await?;
    }
    let out = async move {
        let _ = tokio::io::copy(&mut (&mut cr).take(MAX_OUT), &mut uw).await;
        let _ = uw.shutdown().await;
    };
    let back = async move {
        let _ = tokio::io::copy(&mut (&mut ur).take(MAX_IN), &mut cw).await;
        let _ = cw.shutdown().await;
    };
    tokio::join!(out, back);
    Ok(())
}

async fn handle(mut client: TcpStream, policy: &EgressPolicy) -> anyhow::Result<()> {
    let (head, early) = read_head(&mut client).await?;
    let text = String::from_utf8_lossy(&head).into_owned();
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let (method, target, version) =
        (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if method.eq_ignore_ascii_case("CONNECT") {
        let Some((host, port)) =
            target.rsplit_once(':').and_then(|(h, p)| Some((h.trim_matches(['[', ']']), p.parse::<u16>().ok()?)))
        else {
            return refuse(&mut client, "400 Bad Request", "CONNECT takes host:port").await;
        };
        let upstream = match connect(policy, host, port, "https").await {
            Ok(s) => s,
            Err(why) => {
                tracing::info!(host, port, %why, "the egress proxy refused a connection");
                return refuse(&mut client, "403 Forbidden", &why).await;
            }
        };
        client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;
        return pipe(client, upstream, early).await;
    }
    // Plain HTTP: the request names the whole address; it goes out in origin form.
    let Ok(url) = url::Url::parse(target) else {
        return refuse(&mut client, "400 Bad Request", "a proxy request names an absolute http:// address").await;
    };
    if url.scheme() != "http" {
        return refuse(&mut client, "400 Bad Request", "only http:// is forwarded; https:// goes through CONNECT")
            .await;
    }
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']).to_owned();
    let port = url.port_or_known_default().unwrap_or(80);
    let upstream = match connect(policy, &host, port, "http").await {
        Ok(s) => s,
        Err(why) => {
            tracing::info!(host, port, %why, "the egress proxy refused a request");
            return refuse(&mut client, "403 Forbidden", &why).await;
        }
    };
    let path = match url.query() {
        Some(q) => format!("{}?{q}", url.path()),
        None => url.path().to_owned(),
    };
    let mut forwarded = format!("{method} {path} {version}\r\n");
    for line in lines.filter(|l| !l.is_empty()) {
        let name = line.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
        // The proxy's own headers stay here; one request per connection keeps forwarding simple.
        if !matches!(name.as_str(), "proxy-connection" | "proxy-authorization" | "connection" | "keep-alive") {
            forwarded.push_str(line);
            forwarded.push_str("\r\n");
        }
    }
    forwarded.push_str("Connection: close\r\n\r\n");
    let mut first = forwarded.into_bytes();
    first.extend_from_slice(&early);
    pipe(client, upstream, first).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn proxy(allow: &[&str]) -> std::net::SocketAddr {
        let policy = EgressPolicy::parse(&allow.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, Arc::new(policy)));
        addr
    }

    async fn origin() -> std::net::SocketAddr {
        let app = axum::Router::new()
            .route("/hello", axum::routing::get(|| async { "hello through the proxy" }))
            .route("/big", axum::routing::get(|| async { "x".repeat(70 << 20) }));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    async fn exchange(proxy: std::net::SocketAddr, request: &str) -> String {
        let mut s = TcpStream::connect(proxy).await.unwrap();
        s.write_all(request.as_bytes()).await.unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out).await;
        String::from_utf8_lossy(&out).into_owned()
    }

    #[tokio::test]
    async fn internal_addresses_are_refused_whichever_way_they_are_asked_for() {
        let p = proxy(&[]).await;
        let o = origin().await;
        for request in [
            format!("CONNECT {o} HTTP/1.1\r\nHost: {o}\r\n\r\n"),
            format!("GET http://{o}/hello HTTP/1.1\r\nHost: {o}\r\n\r\n"),
            "CONNECT 169.254.169.254:443 HTTP/1.1\r\n\r\n".to_owned(),
            "GET http://localhost:9/ HTTP/1.1\r\n\r\n".to_owned(),
            "CONNECT [::1]:443 HTTP/1.1\r\n\r\n".to_owned(),
        ] {
            let answer = exchange(p, &request).await;
            assert!(answer.starts_with("HTTP/1.1 403"), "{request}: {answer}");
        }
        assert!(exchange(p, "GET /relative HTTP/1.1\r\n\r\n").await.starts_with("HTTP/1.1 400"));
    }

    #[tokio::test]
    async fn an_allowed_host_is_forwarded_and_tunnelled_within_the_caps() {
        let p = proxy(&["127.0.0.1"]).await;
        let o = origin().await;
        let answer =
            exchange(p, &format!("GET http://{o}/hello HTTP/1.1\r\nHost: {o}\r\nProxy-Connection: keep-alive\r\n\r\n"))
                .await;
        assert!(answer.starts_with("HTTP/1.1 200") && answer.ends_with("hello through the proxy"), "{answer}");
        let tunnel = exchange(
            p,
            &format!("CONNECT {o} HTTP/1.1\r\n\r\nGET /hello HTTP/1.1\r\nHost: {o}\r\nConnection: close\r\n\r\n"),
        )
        .await;
        assert!(tunnel.starts_with("HTTP/1.1 200 Connection established"), "{tunnel}");
        assert!(tunnel.ends_with("hello through the proxy"), "the request after the CONNECT went through: {tunnel}");
        // More than a connection may bring in is cut off.
        let big = exchange(p, &format!("GET http://{o}/big HTTP/1.1\r\nHost: {o}\r\n\r\n")).await;
        assert!(big.len() as u64 <= MAX_IN + 1024, "{}", big.len());
    }
}
