//! The mock provider as a process, for the development stack:
//! `PID_MOCK_ISSUER` (its public URL), `PID_MOCK_LISTEN` (default 0.0.0.0:7080) and
//! `PID_MOCK_CLIENTS` (`id:secret,…`).

use anyhow::Context;
use pacod_id_mock_idp::MockIdp;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let issuer = std::env::var("PID_MOCK_ISSUER").unwrap_or_else(|_| "http://127.0.0.1:17080".into());
    let listen = std::env::var("PID_MOCK_LISTEN").unwrap_or_else(|_| "0.0.0.0:7080".into());
    let clients = std::env::var("PID_MOCK_CLIENTS").context("PID_MOCK_CLIENTS is not set (id:secret,…)")?;
    let pairs: Vec<(String, String)> = clients
        .split(',')
        .filter(|p| !p.trim().is_empty())
        .map(|p| {
            p.trim()
                .split_once(':')
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .context("PID_MOCK_CLIENTS entries look like id:secret")
        })
        .collect::<anyhow::Result<_>>()?;
    let refs: Vec<(&str, &str)> = pairs.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let idp = MockIdp::new(&issuer, &refs)?;
    let listener = tokio::net::TcpListener::bind(&listen).await.with_context(|| format!("binding {listen}"))?;
    eprintln!("mock identity provider {issuer} on {listen}");
    idp.serve(listener).await?;
    Ok(())
}
