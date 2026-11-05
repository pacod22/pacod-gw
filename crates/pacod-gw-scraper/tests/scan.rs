//! The scan of one provider's site: a local site with a robots.txt and a sitemap, rendered by a
//! renderer of the test's own.

use std::collections::HashMap;
use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use pacod_gw_events::egress::EgressPolicy;
use pacod_gw_events::jobs::{BrowserStatus, ScanProvider, ScanRoot, ScanWork};
use pacod_gw_scraper::fetch::Fetcher;
use pacod_gw_scraper::render::{BoxFuture, Render, Rendered};
use pacod_gw_scraper::runner::{Core, Runner};
use pacod_gw_scraper::scan::scan_provider;

/// The site's pages by path: where they end up, their status, title, text and links.
struct Site {
    base: String,
    pages: HashMap<&'static str, (&'static str, u16, &'static str, &'static str, Vec<&'static str>)>,
}

impl Render for Site {
    fn render<'a>(&'a self, url: &'a str) -> BoxFuture<'a, anyhow::Result<Rendered>> {
        Box::pin(async move {
            let path = url.strip_prefix(&self.base).unwrap_or(url);
            let (ends, status, title, text, links) =
                self.pages.get(path).cloned().ok_or_else(|| anyhow::anyhow!("no page at {path}"))?;
            Ok(Rendered {
                status: Some(status),
                final_url: format!("{}{ends}", self.base),
                title: title.into(),
                text: text.into(),
                links: links
                    .into_iter()
                    .map(|l| if l.starts_with("http") { l.to_owned() } else { format!("{}{l}", self.base) })
                    .collect(),
                bytes: 0,
            })
        })
    }

    fn status(&self) -> BoxFuture<'_, BrowserStatus> {
        Box::pin(async { BrowserStatus { ok: true, ..Default::default() } })
    }
}

const PRICES: &str = "Model | Input | Output\ngpt-x | $2.00 / 1M tokens | $10.00 / 1M tokens\ngpt-x-mini | $0.40 / 1M tokens | $1.60 / 1M tokens\n";

#[tokio::test]
async fn the_scan_finds_the_pages_that_list_models_and_prices() {
    // robots.txt and the sitemap come from a server; the pages from the renderer.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sitemap = format!(
        "<urlset><url><loc>{base}/docs/models</loc></url><url><loc>{base}/blog/launch</loc></url>\
         <url><loc>{base}/private/pricing</loc></url><url><loc>{base}/docs/price-list.pdf</loc></url>\
         <url><loc>{base}/docs/rate-limits</loc></url></urlset>"
    );
    let robots = format!("User-agent: *\nDisallow: /private/\nSitemap: {base}/sitemap.xml\n");
    let app = Router::new()
        .route("/robots.txt", get(move || async move { robots }))
        .route("/sitemap.xml", get(move || async move { sitemap }));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let mut pages = HashMap::new();
    // The confirmed page moved to /docs/pricing, which lists prices.
    pages.insert(
        "/old-pricing",
        ("/docs/pricing", 200, "Pricing", PRICES, vec!["/docs/models", "https://elsewhere.example/pricing"]),
    );
    // A preset's page that is gone.
    pages.insert("/gone/pricing", ("/gone/pricing", 404, "Not found", "Not found", vec![]));
    pages.insert(
        "/docs/models",
        ("/docs/models", 200, "Models", "gpt-x | 128K context\ngpt-x-mini | 128K context", vec![]),
    );
    pages.insert("/docs/rate-limits", ("/docs/rate-limits", 200, "Rate limits", "Requests per minute.", vec![]));
    pages.insert("/docs/pricing", ("/docs/pricing", 200, "Pricing", PRICES, vec![]));
    let site = Arc::new(Site { base: base.clone(), pages });
    let policy = Arc::new(EgressPolicy::parse(&["127.0.0.1".to_owned()]).unwrap());
    let runner = Runner::new(
        Core::new("http://127.0.0.1:9", "unused-token-0123456789").unwrap(),
        Fetcher::new(policy, "test").unwrap(),
        site,
    );

    let provider = ScanProvider {
        provider_id: "prv_1".into(),
        name: "openai".into(),
        roots: vec![
            ScanRoot { url: format!("{base}/old-pricing"), page_id: Some("ppg_1".into()), seed: false },
            ScanRoot { url: format!("{base}/gone/pricing"), page_id: None, seed: true },
        ],
        known_urls: vec![format!("{base}/old-pricing")],
        known_models: vec!["gpt-x".into(), "gpt-x-mini".into()],
    };
    let keywords = ["pricing", "price", "models", "rates", "billing"].map(String::from).to_vec();
    let work = ScanWork { providers: vec![], max_pages_per_provider: 12, min_score: 10.0, keywords };
    let report = scan_provider(&runner, &provider, &work).await;

    let moved = &report.roots[0];
    assert_eq!(
        (moved.outcome.as_str(), moved.final_url.clone()),
        ("moved", format!("{base}/docs/pricing")),
        "{moved:?}"
    );
    assert!(moved.score.unwrap() >= 10.0);
    assert_eq!(report.roots[1].outcome, "gone");
    // From the links and the sitemap: on the site, with a keyword, allowed, a page not a file.
    let found: Vec<(&str, f64)> = report.found.iter().map(|f| (f.url.strip_prefix(&base).unwrap(), f.score)).collect();
    let urls: Vec<&str> = found.iter().map(|(u, _)| *u).collect();
    assert!(urls.contains(&"/docs/models"), "{found:?}");
    assert!(!urls.contains(&"/private/pricing"), "robots.txt leaves it alone: {found:?}");
    assert!(!urls.iter().any(|u| u.starts_with("/blog") || u.ends_with(".pdf")), "{found:?}");
    assert!(!urls.iter().any(|u| u.contains("elsewhere")), "another site: {found:?}");
    let models = report.found.iter().find(|f| f.url.ends_with("/docs/models")).unwrap();
    assert_eq!(models.model_hits, ["gpt-x", "gpt-x-mini"]);
    let limits = report.found.iter().find(|f| f.url.ends_with("/docs/rate-limits")).map(|f| f.score);
    assert!(limits.is_none_or(|s| s < 10.0), "a page without prices or models scores low: {limits:?}");
    assert!(report.considered >= 5 && report.rendered >= 3, "{report:?}");
    assert!(report.found.windows(2).all(|w| w[0].score >= w[1].score), "best first");
}
