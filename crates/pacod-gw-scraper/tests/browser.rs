//! Pages rendered in a real headless browser, behind the scraper's egress proxy. They run where
//! `GW_TEST_BROWSER_URL` names a browser (`make test-services` starts one on 29222) and are
//! skipped otherwise. The proxy runs in the test process, and the browser reaches it as
//! `host.docker.internal`; the pages are served on 127.0.0.1, which only the proxy connects to.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::RawQuery;
use axum::http::{HeaderMap, StatusCode};
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

const IMAGES: &str = r#"<html><body><p>gpt-x costs $2.00 per million tokens.</p><img src="/pixel.png">
<video src="/clip.mp4" autoplay muted></video></body></html>"#;

const INSIDE: &str = r#"<html><body><p id="out">waiting</p><script>
fetch('http://169.254.169.254/latest/meta-data/').then(() => { document.getElementById('out').textContent = 'reached'; })
  .catch(() => { document.getElementById('out').textContent = 'refused'; });</script></body></html>"#;

const SHADOW: &str = r#"<html><body><price-card></price-card><script>
customElements.define('price-card', class extends HTMLElement { constructor() { super();
this.attachShadow({ mode: 'open' }).innerHTML = '<div role="row"><span role="cell">gpt-x</span><span role="cell">$3.00</span></div>'; } });
</script></body></html>"#;

/// What a page finds out about the browser it runs in, as rows of a table, which the normaliser
/// reads back as `name | value` lines. Its workers, dedicated, shared and service ones, fetch
/// too: they are targets of their own, which an override of the page's user agent misses, the
/// usual giveaway.
const PROBE: &str = r#"<html><head><title>Probe</title></head><body><table id="facts"></table><script>
const put = (k, v) => { const r = document.getElementById('facts').insertRow(); r.insertCell().textContent = k;
  r.insertCell().textContent = JSON.stringify(v); };
put('webdriver', navigator.webdriver);
put('chrome', typeof window.chrome);
put('plugins', navigator.plugins.length);
put('languages', navigator.languages);
put('userAgent', navigator.userAgent);
put('platform', navigator.platform);
put('brands', navigator.userAgentData ? navigator.userAgentData.brands.map((b) => b.brand) : null);
put('screen', [screen.width, screen.height, screen.availHeight]);
const work = "fetch('" + location.origin + "/headers?worker').then(() => postMessage(navigator.userAgent))";
Promise.all([
  navigator.permissions.query({ name: 'notifications' }).then((p) => p.state),
  new Promise((done) => { const w = new Worker(URL.createObjectURL(new Blob([work], { type: 'text/javascript' })));
    w.onmessage = (e) => done(e.data); w.onerror = () => done('the worker failed'); }),
  fetch('/headers?page'),
  new SharedWorker('/shared.js'),
  navigator.serviceWorker.register('/sw.js'),
]).then(([permission, worker]) => { put('notifications', [Notification.permission, permission]); put('worker', worker);
  // Not while the page is parsed: headless mode gives a new document its window's size a few
  // milliseconds after it starts, and 0 before.
  put('window', [outerWidth, outerHeight, innerWidth, innerHeight]); });
</script></body></html>"#;

/// The requests the site was sent, by path and query, with their headers.
type Seen = Arc<Mutex<Vec<(String, HeaderMap)>>>;

/// A page whose scripts change what the normaliser would call, were it read in the page's world.
const TAMPERED: &str = r#"<html><body><table><tr><td>gpt-x</td><td>$2.00</td></tr></table><script>
Array.prototype.join = function () { return 'tampered'; };
String.prototype.replace = function () { return 'tampered'; };
</script></body></html>"#;

struct Site {
    base: String,
    images: Arc<AtomicUsize>,
    media: Arc<AtomicUsize>,
    seen: Seen,
}

async fn site() -> Site {
    let images = Arc::new(AtomicUsize::new(0));
    let counted = images.clone();
    let media = Arc::new(AtomicUsize::new(0));
    let played = media.clone();
    let seen: Seen = Arc::default();
    let (probed, asked) = (seen.clone(), seen.clone());
    let app = Router::new()
        .route(
            "/probe",
            get(move |headers: HeaderMap| {
                probed.lock().unwrap().push(("/probe".to_owned(), headers));
                async { Html(PROBE) }
            }),
        )
        .route(
            "/headers",
            get(move |RawQuery(query): RawQuery, headers: HeaderMap| {
                asked.lock().unwrap().push((format!("/headers?{}", query.unwrap_or_default()), headers));
                async { StatusCode::NO_CONTENT }
            }),
        )
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
        .route(
            "/clip.mp4",
            get(move || {
                played.fetch_add(1, Ordering::Relaxed);
                async { StatusCode::NO_CONTENT }
            }),
        )
        .route("/inside", get(|| async { Html(INSIDE) }))
        .route("/shadow", get(|| async { Html(SHADOW) }))
        .route("/tampered", get(|| async { Html(TAMPERED) }))
        .route("/shared.js", get(|| async { script("fetch('/headers?shared')") }))
        .route("/sw.js", get(|| async { script("self.addEventListener('install', () => fetch('/headers?service'))") }))
        .route("/moved", get(|| async { Redirect::temporary("/table") }))
        .route("/missing", get(|| async { (StatusCode::NOT_FOUND, Html("<html><body>Not found</body></html>")) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Site { base, images, media, seen }
}

fn script(body: &'static str) -> impl axum::response::IntoResponse {
    ([(axum::http::header::CONTENT_TYPE, "text/javascript")], body)
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

    // Images are loaded, as a person's browser loads them; video is not.
    let page = b.render(&format!("{}/images", site.base)).await.unwrap();
    assert!(page.text.contains("$2.00 per million"), "{}", page.text);
    assert_eq!(site.images.load(Ordering::Relaxed), 1, "the image was fetched");
    assert_eq!(site.media.load(Ordering::Relaxed), 0, "the video was not");

    // An open shadow root, and an ARIA row.
    let page = b.render(&format!("{}/shadow", site.base)).await.unwrap();
    assert!(page.text.lines().any(|l| l == "gpt-x | $3.00"), "{}", page.text);

    // The page's scripts cannot reach the reading.
    let page = b.render(&format!("{}/tampered", site.base)).await.unwrap();
    assert!(page.text.lines().any(|l| l == "gpt-x | $2.00"), "{}", page.text);

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
async fn pages_find_a_browser_not_a_headless_one() {
    let Some(url) = browser_url() else { return };
    let site = site().await;
    let b = browser(&url).await;
    let page = b.render(&format!("{}/probe", site.base)).await.unwrap();
    // A service worker installs in its own time.
    for _ in 0..50 {
        if site.seen.lock().unwrap().len() >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let facts: HashMap<&str, serde_json::Value> = page
        .text
        .lines()
        .filter_map(|l| l.split_once(" | "))
        .map(|(k, v)| (k, serde_json::from_str(v).unwrap_or_default()))
        .collect();
    let seen = site.seen.lock().unwrap().clone();
    let headers: Vec<(String, HashMap<String, String>)> = seen
        .iter()
        .map(|(path, h)| {
            let h = h.iter().map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_owned())).collect();
            (path.clone(), h)
        })
        .collect();
    let fact = |name: &str| facts.get(name).cloned().unwrap_or_default();
    let ua = fact("userAgent").as_str().unwrap_or_default().to_owned();
    assert!(ua.contains(" Chrome/") && !ua.contains("Headless") && !ua.contains("scraper"), "{facts:#?}");
    assert_eq!(fact("worker"), fact("userAgent"), "a worker sees what the page sees");
    let brands = fact("brands");
    assert!(brands.as_array().is_some_and(|b| !b.is_empty()) && !brands.to_string().contains("Headless"), "{facts:#?}");
    assert_eq!(fact("webdriver"), false, "{facts:#?}");
    assert_eq!(fact("chrome"), "object", "{facts:#?}");
    assert!(fact("plugins").as_u64().is_some_and(|n| n > 0), "{facts:#?}");
    assert_eq!(fact("languages"), serde_json::json!(["en-US", "en"]));
    // A desktop's screen with a bar along its top, and a window maximised on the rest of it,
    // the browser's frame around the page.
    assert_eq!(fact("screen"), serde_json::json!([1920, 1080, 1048]));
    let window: Vec<u64> = serde_json::from_value(fact("window")).unwrap();
    assert!(window[0] == 1920 && window[1] == 1048 && window[2] == 1920 && window[3] < window[1], "{window:?}");
    assert_eq!(fact("notifications"), serde_json::json!(["default", "prompt"]), "the two answers agree");

    // The site is sent what the page sees, from the page and from every worker, and the client
    // hints a browser sends: from the page, not from its workers, as Chromium itself does.
    let mut paths: Vec<&str> = headers.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["/headers?page", "/headers?service", "/headers?shared", "/headers?worker", "/probe"]);
    for (path, h) in &headers {
        assert_eq!(h.get("user-agent"), Some(&ua), "{path}");
        assert!(h.get("accept-language").is_some_and(|l| l.starts_with("en-US,en")), "{path}: {h:#?}");
        if ["/probe", "/headers?page"].contains(&path.as_str()) {
            let hints = h.get("sec-ch-ua").cloned().unwrap_or_default();
            assert!(hints.contains("Chromium") && !hints.contains("Headless"), "{path}: {h:#?}");
            assert_eq!(h.get("sec-ch-ua-platform").map(String::as_str), Some("\"Linux\""), "{path}");
        }
    }
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
