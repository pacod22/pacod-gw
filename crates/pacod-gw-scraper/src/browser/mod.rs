//! Rendering pages in the headless browser (`gw-browser`). Every page gets a browser context of
//! its own, which goes out through the scraper's egress proxy alone and is thrown away
//! afterwards: no cookie, cache or storage passes from one page to the next. Images, fonts and
//! media are not loaded. The page is read as text inside the browser, once the network is idle.

pub mod cdp;

use std::sync::Arc;
use std::time::{Duration, Instant};

use pacod_gw_events::jobs::BrowserStatus;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::render::{BoxFuture, Render, Rendered};
use cdp::Cdp;

/// The script that turns the rendered page into text, in the page. Scripts, styles and inline
/// images are skipped, open shadow roots and hidden tab panels kept; a table row (or an ARIA
/// row) becomes one line of its cells joined by ` | `, so that a price stays next to its model.
pub const NORMALISE: &str = r#"(() => {
  const SKIP = new Set(['SCRIPT', 'STYLE', 'NOSCRIPT', 'TEMPLATE', 'SVG', 'CANVAS', 'IFRAME', 'OBJECT']);
  const BLOCK = new Set(['ADDRESS', 'ARTICLE', 'ASIDE', 'BLOCKQUOTE', 'BR', 'DD', 'DETAILS', 'DIV', 'DL', 'DT',
    'FIELDSET', 'FIGCAPTION', 'FIGURE', 'FOOTER', 'FORM', 'H1', 'H2', 'H3', 'H4', 'H5', 'H6', 'HEADER', 'HR', 'LI',
    'MAIN', 'NAV', 'OL', 'P', 'PRE', 'SECTION', 'SUMMARY', 'TABLE', 'TBODY', 'THEAD', 'TFOOT', 'UL', 'CAPTION']);
  const CELL = '[role=cell],[role=gridcell],[role=columnheader],[role=rowheader]';
  const clean = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const lines = [];
  let line = '';
  const flush = () => { const t = clean(line); if (t) lines.push(t); line = ''; };
  const row = (cells) => { flush(); const texts = cells.map((c) => clean(c.textContent)); if (texts.some((t) => t)) lines.push(texts.join(' | ')); };
  const walk = (node) => {
    if (node.nodeType === Node.TEXT_NODE) { line += node.nodeValue; return; }
    if (node.nodeType !== Node.ELEMENT_NODE && node.nodeType !== Node.DOCUMENT_FRAGMENT_NODE) return;
    const tag = node.tagName ? node.tagName.toUpperCase() : '';
    if (SKIP.has(tag)) return;
    if (tag === 'TR') { row([...node.children].filter((c) => c.tagName === 'TD' || c.tagName === 'TH')); return; }
    if (node.getAttribute && node.getAttribute('role') === 'row') { row([...node.querySelectorAll(CELL)]); return; }
    const block = BLOCK.has(tag);
    if (block) flush();
    if (node.shadowRoot) node.shadowRoot.childNodes.forEach(walk);
    node.childNodes.forEach(walk);
    if (block) flush();
  };
  walk(document.body || document.documentElement);
  flush();
  const links = [...new Set([...document.querySelectorAll('a[href]')].map((a) => a.href).filter((h) => /^https?:/.test(h)))];
  const nav = performance.getEntriesByType('navigation')[0];
  return { title: document.title, text: lines.join('\n'), links: links.slice(0, 2000), href: location.href,
           status: nav && nav.responseStatus ? nav.responseStatus : null,
           bytes: performance.getEntriesByType('resource').reduce((n, r) => n + (r.transferSize || 0), nav ? nav.transferSize || 0 : 0) };
})()"#;

/// The resources a page renders without: they cost bandwidth and say nothing about prices.
const BLOCKED: &[&str] = &["Image", "Font", "Media"];

/// How long the network must have been idle, at most, after the page loaded.
const SETTLE_AFTER_LOAD: Duration = Duration::from_secs(3);

/// The headless browser, reached over its DevTools.
pub struct Browser {
    /// `http://gw-browser:9222`.
    base: String,
    /// The egress proxy as the browser reaches it; every page context goes out through it.
    proxy: String,
    user_agent: Option<String>,
    timeout: Duration,
    /// One page at a time, over one connection, opened when needed and again when it closed.
    conn: Mutex<Option<(Arc<Cdp>, String)>>,
}

impl Browser {
    pub fn new(base: &str, proxy: &str, user_agent: Option<String>, timeout: Duration) -> Self {
        Self { base: base.to_owned(), proxy: proxy.to_owned(), user_agent, timeout, conn: Mutex::new(None) }
    }

    /// The connection, and the user agent pages are fetched with: the browser's own, with
    /// `HeadlessChrome` written as `Chrome` and this service's name after it, unless one is set.
    async fn connection(&self) -> anyhow::Result<(Arc<Cdp>, String)> {
        let mut conn = self.conn.lock().await;
        if let Some((c, ua)) = conn.as_ref().filter(|(c, _)| c.is_open()) {
            return Ok((c.clone(), ua.clone()));
        }
        let endpoint = cdp::endpoint(&self.base).await?;
        let cdp = Arc::new(Cdp::connect(&endpoint).await?);
        let ua = self.user_agent.clone().unwrap_or_else(|| {
            format!(
                "{} pacod-gw-scraper/{}",
                endpoint.user_agent.replace("HeadlessChrome", "Chrome"),
                env!("CARGO_PKG_VERSION")
            )
        });
        *conn = Some((cdp.clone(), ua.clone()));
        Ok((cdp, ua))
    }

    async fn render_page(&self, url: &str) -> anyhow::Result<Rendered> {
        let (cdp, ua) = self.connection().await?;
        let context = cdp
            .call(
                "Target.createBrowserContext",
                json!({ "disposeOnDetach": true, "proxyServer": self.proxy, "proxyBypassList": "<-loopback>" }),
                None,
            )
            .await?;
        let context = context["browserContextId"].as_str().unwrap_or_default().to_owned();
        let rendered = self.in_context(&cdp, &context, &ua, url).await;
        // Thrown away whatever came of the page, so that nothing of it stays for the next.
        let _ = cdp.call("Target.disposeBrowserContext", json!({ "browserContextId": context }), None).await;
        rendered
    }

    async fn in_context(&self, cdp: &Cdp, context: &str, ua: &str, url: &str) -> anyhow::Result<Rendered> {
        let target =
            cdp.call("Target.createTarget", json!({ "url": "about:blank", "browserContextId": context }), None).await?;
        let target = target["targetId"].as_str().unwrap_or_default().to_owned();
        let attached = cdp.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None).await?;
        let session = attached["sessionId"].as_str().unwrap_or_default().to_owned();
        let mut events = cdp.events(&session);
        let s = Some(session.as_str());
        let setup = async {
            cdp.call("Page.enable", json!({}), s).await?;
            cdp.call("Page.setLifecycleEventsEnabled", json!({ "enabled": true }), s).await?;
            cdp.call("Network.setUserAgentOverride", json!({ "userAgent": ua }), s).await?;
            let patterns: Vec<Value> = BLOCKED.iter().map(|t| json!({ "resourceType": t })).collect();
            cdp.call("Fetch.enable", json!({ "patterns": patterns }), s).await?;
            anyhow::Ok(())
        };
        let result = async {
            setup.await?;
            let nav = cdp.call("Page.navigate", json!({ "url": url }), s).await?;
            if let Some(error) = nav["errorText"].as_str().filter(|e| !e.is_empty()) {
                anyhow::bail!("{url} could not be opened: {error}");
            }
            // The lifecycle of this navigation, not of the blank page the target started with.
            let loader = nav["loaderId"].as_str().unwrap_or_default().to_owned();
            self.settle(cdp, s, &loader, &mut events).await;
            let read = cdp
                .call(
                    "Runtime.evaluate",
                    json!({ "expression": NORMALISE, "returnByValue": true, "awaitPromise": true }),
                    s,
                )
                .await?;
            if let Some(e) = read.get("exceptionDetails") {
                anyhow::bail!("reading {url} failed: {}", e["text"].as_str().unwrap_or("an exception"));
            }
            let v = &read["result"]["value"];
            Ok(Rendered {
                status: v["status"].as_u64().map(|s| s as u16),
                final_url: v["href"].as_str().unwrap_or(url).to_owned(),
                title: v["title"].as_str().unwrap_or_default().to_owned(),
                text: v["text"].as_str().unwrap_or_default().to_owned(),
                links: v["links"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|l| l.as_str().map(str::to_owned))
                    .collect(),
                bytes: v["bytes"].as_u64().unwrap_or(0),
            })
        }
        .await;
        cdp.forget(&session);
        result
    }

    /// Waits until the page's network is idle, or a while after it loaded, within the page's
    /// time. Meanwhile it refuses the images, fonts and media the page asks for.
    async fn settle(
        &self,
        cdp: &Cdp,
        session: Option<&str>,
        loader: &str,
        events: &mut tokio::sync::mpsc::UnboundedReceiver<Value>,
    ) {
        let deadline = Instant::now() + self.timeout;
        let mut loaded: Option<Instant> = None;
        loop {
            let until = match loaded {
                Some(at) => (at + SETTLE_AFTER_LOAD).min(deadline),
                None => deadline,
            };
            let Ok(Some(event)) = tokio::time::timeout_at(until.into(), events.recv()).await else { return };
            match event["method"].as_str() {
                Some("Fetch.requestPaused") => {
                    let id = event["params"]["requestId"].clone();
                    let _ = cdp
                        .call(
                            "Fetch.failRequest",
                            json!({ "requestId": id, "errorReason": "BlockedByClient" }),
                            session,
                        )
                        .await;
                }
                Some("Page.lifecycleEvent") if event["params"]["loaderId"] == loader => {
                    match event["params"]["name"].as_str() {
                        Some("networkIdle") => return,
                        Some("load") if loaded.is_none() => loaded = Some(Instant::now()),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

impl Render for Browser {
    fn render<'a>(&'a self, url: &'a str) -> BoxFuture<'a, anyhow::Result<Rendered>> {
        Box::pin(async move {
            // The page's own time, and a little more for the browser to give it back.
            match tokio::time::timeout(self.timeout + Duration::from_secs(15), self.render_page(url)).await {
                Ok(r) => r,
                Err(_) => {
                    // A browser that does not answer in time is asked again from a new connection.
                    *self.conn.lock().await = None;
                    anyhow::bail!("{url} did not render within {} s", self.timeout.as_secs())
                }
            }
        })
    }

    fn status(&self) -> BoxFuture<'_, BrowserStatus> {
        Box::pin(async move {
            match cdp::endpoint(&self.base).await {
                Ok(e) => BrowserStatus { ok: true, product: e.product, message: String::new() },
                Err(e) => BrowserStatus { ok: false, product: String::new(), message: format!("{}: {e:#}", self.base) },
            }
        })
    }
}
