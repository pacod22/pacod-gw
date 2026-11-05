//! Pages rendered in a real headless browser, behind the scraper's egress proxy. They run where
//! `GW_TEST_BROWSER_URL` names a browser (`make test-services` starts one on 29222) and are
//! skipped otherwise. The proxy runs in the test process, and the browser reaches it as
//! `host.docker.internal`; the pages are served on 127.0.0.1, which only the proxy connects to.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{Html, Redirect};
use axum::routing::get;
use pacod_gw_events::egress::EgressPolicy;
use pacod_gw_scraper::browser::Browser;
use pacod_gw_scraper::render::Render;
use pacod_gw_scraper::text::fingerprint;

fn browser_url() -> Option<String> {
    std::env::var("GW_TEST_BROWSER_URL").ok().filter(|v| !v.is_empty())
}

const TABLE: &str = r#"<html><head><title>Pricing</title><style>td{color:red}</style><script>var x = '$9.99';</script></head>
<body><h1>Models and prices</h1><p>Prices are per one million tokens.</p>
<table><thead><tr><th>Model</th><th>Context</th><th>Input</th><th>Output</th></tr></thead>
<tbody><tr><td>gpt-x</td><td>128K</td><td>$2.00</td><td>$10.00</td></tr>
<tr><td>gpt-x-mini</td><td>128K</td><td><b>$0.40</b></td><td>$1.60</td></tr></tbody></table>
<div class="tab" hidden><table><tr><td>gpt-x</td><td>Batch</td><td>$1.00</td><td>$5.00</td></tr></table></div>
<a href="/models">Models</a> <a href="https://elsewhere.example/docs">Docs</a></body></html>"#;

const LATER: &str = r#"<html><body><h1>Pricing</h1><div id="prices">Loading…</div>
<script>setTimeout(async () => { const r = await fetch('/api/prices'); const p = await r.json();
document.getElementById('prices').textContent = p.model + ': $' + p.input + ' / 1M input tokens'; }, 300);</script></body></html>"#;

const IMAGES: &str = r#"<html><body><p>gpt-x costs $2.00 per million tokens.</p><img src="/pixel.png"></body></html>"#;

const INSIDE: &str = r#"<html><body><p id="out">waiting</p><script>
fetch('http://169.254.169.254/latest/meta-data/').then(() => { document.getElementById('out').textContent = 'reached'; })
  .catch(() => { document.getElementById('out').textContent = 'refused'; });</script></body></html>"#;

const SHADOW: &str = r#"<html><body><price-card></price-card><script>
customElements.define('price-card', class extends HTMLElement { constructor() { super();
this.attachShadow({ mode: 'open' }).innerHTML = '<div role="row"><span role="cell">gpt-x</span><span role="cell">$3.00</span></div>'; } });
</script></body></html>"#;

struct Site {
    base: String,
    images: Arc<AtomicUsize>,
}

async fn site() -> Site {
    let images = Arc::new(AtomicUsize::new(0));
    let counted = images.clone();
    let app = Router::new()
        .route("/table", get(|| async { Html(TABLE) }))
        .route("/later", get(|| async { Html(LATER) }))
        .route("/api/prices", get(|| async { axum::Json(serde_json::json!({ "model": "gpt-x", "input": 2.5 })) }))
        .route("/images", get(|| async { Html(IMAGES) }))
        .route(
            "/pixel.png",
            get(move || {
                counted.fetch_add(1, Ordering::Relaxed);
                async { StatusCode::NO_CONTENT }
            }),
        )
        .route("/inside", get(|| async { Html(INSIDE) }))
        .route("/shadow", get(|| async { Html(SHADOW) }))
        .route("/moved", get(|| async { Redirect::temporary("/table") }))
        .route("/missing", get(|| async { (StatusCode::NOT_FOUND, Html("<html><body>Not found</body></html>")) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Site { base, images }
}

/// A browser whose pages go out through a proxy in this process, which allows 127.0.0.1 only.
async fn browser(url: &str) -> Browser {
    let policy = EgressPolicy::parse(&["127.0.0.1".to_owned()]).unwrap();
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(pacod_gw_scraper::proxy::serve(listener, Arc::new(policy)));
    Browser::new(url, &format!("http://host.docker.internal:{port}"), None, Duration::from_secs(20))
}

#[tokio::test]
async fn pages_render_into_rows_of_text_behind_the_proxy() {
    let Some(url) = browser_url() else { return };
    let site = site().await;
    let b = browser(&url).await;
    let status = b.status().await;
    assert!(status.ok && status.product.starts_with("Chrome/"), "{status:?}");

    // A table: one row per line, its cells joined; scripts and styles left out; a hidden tab
    // panel kept.
    let page = b.render(&format!("{}/table", site.base)).await.unwrap();
    assert_eq!((page.status, page.title.as_str()), (Some(200), "Pricing"));
    let lines: Vec<&str> = page.text.lines().collect();
    assert!(lines.contains(&"Model | Context | Input | Output"), "{}", page.text);
    assert!(lines.contains(&"gpt-x-mini | 128K | $0.40 | $1.60"), "{}", page.text);
    assert!(lines.contains(&"gpt-x | Batch | $1.00 | $5.00"), "a hidden panel: {}", page.text);
    assert!(!page.text.contains("$9.99") && !page.text.contains("color:red"), "{}", page.text);
    assert!(
        page.links.iter().any(|l| l.ends_with("/models"))
            && page.links.iter().any(|l| l.starts_with("https://elsewhere"))
    );

    // Prices a script fetches after the page loaded are there.
    let page = b.render(&format!("{}/later", site.base)).await.unwrap();
    assert!(page.text.contains("gpt-x: $2.5 / 1M input tokens"), "{}", page.text);

    // Images are not loaded.
    let page = b.render(&format!("{}/images", site.base)).await.unwrap();
    assert!(page.text.contains("$2.00 per million"), "{}", page.text);
    assert_eq!(site.images.load(Ordering::Relaxed), 0, "the image was not fetched");

    // An open shadow root, and an ARIA row.
    let page = b.render(&format!("{}/shadow", site.base)).await.unwrap();
    assert!(page.text.lines().any(|l| l == "gpt-x | $3.00"), "{}", page.text);

    // A page whose script reaches for an internal address is refused by the proxy.
    let page = b.render(&format!("{}/inside", site.base)).await.unwrap();
    assert!(page.text.contains("refused"), "{}", page.text);

    // A redirect ends where it ends; a missing page says so.
    let page = b.render(&format!("{}/moved", site.base)).await.unwrap();
    assert!(page.final_url.ends_with("/table"), "{}", page.final_url);
    assert_eq!(b.render(&format!("{}/missing", site.base)).await.unwrap().status, Some(404));

    // An address the egress policy refuses: over http the proxy answers 403 with the reason,
    // over https the tunnel is refused and the page cannot be opened at all.
    let page = b.render("http://10.1.2.3/pricing").await.unwrap();
    assert_eq!(page.status, Some(403), "{page:?}");
    assert!(page.text.contains("PGW_EGRESS_ALLOW_PRIVATE"), "{}", page.text);
    let e = b.render("https://10.1.2.3/pricing").await.unwrap_err();
    assert!(format!("{e:#}").contains("could not be opened"), "{e:#}");
}

#[tokio::test]
async fn the_fingerprint_follows_the_prices_not_the_page_around_them() {
    let Some(url) = browser_url() else { return };
    let site = site().await;
    let b = browser(&url).await;
    let known = vec!["gpt-x".to_owned(), "gpt-x-mini".to_owned()];
    let a = b.render(&format!("{}/table", site.base)).await.unwrap();
    let again = b.render(&format!("{}/table", site.base)).await.unwrap();
    assert_eq!(fingerprint(&a.text, &known), fingerprint(&again.text, &known), "the same page twice");
    let changed = a.text.replace("128K", "256K");
    assert_ne!(fingerprint(&a.text, &known), fingerprint(&changed, &known), "only the window changed");
}

#[tokio::test]
async fn a_browser_that_is_not_there_is_named() {
    let b = Browser::new("http://127.0.0.1:9", "http://127.0.0.1:9", None, Duration::from_secs(5));
    let status = b.status().await;
    assert!(!status.ok && status.message.contains("127.0.0.1:9"), "{status:?}");
    assert!(b.render("https://example.com/").await.is_err());
}
