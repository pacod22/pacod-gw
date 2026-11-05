//! A run of the weekly scan. For each provider it opens the places it starts from (the
//! provider's confirmed pages and the pages its preset names), gathers the addresses of the
//! provider's sites from their sitemaps and from the links on those pages, keeps the ones whose
//! path holds a keyword and that the site's `robots.txt` lets crawlers have, opens the best of
//! them in the browser, and scores them. The core proposes what scores high enough.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use pacod_gw_events::jobs::{FoundPage, RootStatus, ScanProvider, ScanReport, ScanWork};
use pacod_gw_events::pages::normalise_url;
use serde_json::{Value, json};

use crate::runner::Runner;
use crate::score::{keyword_hits, price_lines, score, site_of};
use crate::sitemap::{Robots, parse_sitemap};

/// How many sitemaps a provider's scan reads, and how large one may be.
const MAX_SITEMAPS: usize = 12;
const MAX_SITEMAP_BYTES: usize = 8 << 20;
/// How many of a provider's sites' hosts the scan asks for their `robots.txt`.
const MAX_ORIGINS: usize = 8;
/// How many addresses a provider's scan looks at, at most.
const MAX_CONSIDERED: usize = 5000;

/// Scans every provider of the run while the run is this scraper's (`keep`).
pub async fn run(runner: &Runner, run_id: &str, work: &ScanWork, keep: &AtomicBool) -> anyhow::Result<Value> {
    let mut providers = Vec::new();
    for provider in &work.providers {
        if !keep.load(Ordering::Relaxed) {
            anyhow::bail!("the core gave the run to another scraper");
        }
        let report = scan_provider(runner, provider, work).await;
        match runner.core.scan_report(run_id, &report).await {
            Ok(reply) => {
                let proposed = reply["proposed"].as_array().map_or(0, Vec::len);
                tracing::info!(provider = %provider.name, found = report.found.len(), proposed, "provider scanned");
                providers.push(json!({ "provider": provider.name, "found": report.found.len(), "proposed": proposed }));
            }
            Err(e) => {
                tracing::warn!(provider = %provider.name, error = %format!("{e:#}"), "reporting a scan to the core failed");
                providers.push(json!({ "provider": provider.name, "message": format!("{e:#}") }));
            }
        }
    }
    Ok(json!({ "scraper": { "providers": providers } }))
}

fn origin_of(url: &url::Url) -> String {
    url.origin().ascii_serialization()
}

/// The `robots.txt` of each host, asked for once, of at most [`MAX_ORIGINS`] hosts.
struct RobotsCache<'a> {
    runner: &'a Runner,
    known: HashMap<String, Option<Robots>>,
}

impl RobotsCache<'_> {
    /// The rules of `origin`. `None` for a host beyond the limit: its addresses are left alone.
    async fn of(&mut self, origin: &str) -> Option<&Robots> {
        if !self.known.contains_key(origin) {
            let robots = if self.known.len() >= MAX_ORIGINS {
                None
            } else {
                // A site without a robots.txt lets crawlers have everything.
                Some(match self.runner.fetch.get(&format!("{origin}/robots.txt"), 512 << 10).await {
                    Ok(f) if f.status == 200 => Robots::parse(&String::from_utf8_lossy(&f.body)),
                    _ => Robots::default(),
                })
            };
            self.known.insert(origin.to_owned(), robots);
        }
        self.known.get(origin).and_then(Option::as_ref)
    }
}

/// What the scan finds for one provider. Nothing here fails the provider: what could not be
/// opened is reported as such.
pub async fn scan_provider(runner: &Runner, p: &ScanProvider, work: &ScanWork) -> ScanReport {
    let mut report = ScanReport { provider_id: p.provider_id.clone(), ..Default::default() };
    let known: HashSet<&str> = p.known_urls.iter().map(String::as_str).collect();
    let mut sites = BTreeSet::new();
    let mut origins = BTreeSet::new();
    let mut candidates: BTreeSet<String> = BTreeSet::new();
    let mut roots = HashSet::new();

    // The places the scan starts from: whether they are still there, and where they link to.
    for root in &p.roots {
        let Ok(at) = url::Url::parse(&root.url) else { continue };
        sites.insert(site_of(at.host_str().unwrap_or_default()));
        origins.insert(origin_of(&at));
        roots.insert(root.url.clone());
        let mut status = RootStatus { url: root.url.clone(), page_id: root.page_id.clone(), ..Default::default() };
        report.rendered += 1;
        match runner.render.render(&root.url).await {
            Err(e) => (status.outcome, status.message) = ("failed".into(), format!("{e:#}")),
            Ok(page) => {
                candidates.extend(page.links.iter().cloned());
                status.http_status = page.status;
                let ended = normalise_url(&page.final_url).unwrap_or_else(|_| root.url.clone());
                match page.status {
                    Some(404 | 410) => status.outcome = "gone".into(),
                    Some(s) if s >= 400 => {
                        (status.outcome, status.message) = ("failed".into(), format!("the page answered {s}"))
                    }
                    _ => {
                        let place = url::Url::parse(&ended).unwrap_or(at);
                        let (points, ..) = score(&place, &page.title, &page.text, &p.known_models, &work.keywords);
                        status.score = Some(points);
                        // A page that now ends elsewhere has moved, when that place lists models
                        // and prices; otherwise (a home page, a login) it is gone.
                        status.outcome = match (ended != root.url, points >= work.min_score) {
                            (false, _) => "ok",
                            (true, true) => "moved",
                            (true, false) => "gone",
                        }
                        .into();
                        status.final_url = ended;
                    }
                }
            }
        }
        report.roots.push(status);
    }

    // The sitemaps the sites name, or the one at the usual place.
    let mut robots = RobotsCache { runner, known: HashMap::new() };
    let mut maps: Vec<(String, u8)> = Vec::new();
    for origin in &origins {
        let named = robots.of(origin).await.map(|r| r.sitemaps.clone()).unwrap_or_default();
        if named.is_empty() {
            maps.push((format!("{origin}/sitemap.xml"), 0));
        }
        maps.extend(named.into_iter().map(|m| (m, 0)));
    }
    let mut read = 0;
    let mut seen_maps = HashSet::new();
    while let Some((map, depth)) = maps.pop() {
        if read >= MAX_SITEMAPS || map.ends_with(".gz") || !seen_maps.insert(map.clone()) {
            continue;
        }
        let on_site =
            url::Url::parse(&map).ok().is_some_and(|u| sites.contains(&site_of(u.host_str().unwrap_or_default())));
        if !on_site {
            continue;
        }
        read += 1;
        let Ok(fetched) = runner.fetch.get(&map, MAX_SITEMAP_BYTES).await else { continue };
        if fetched.status != 200 {
            continue;
        }
        let found = parse_sitemap(&String::from_utf8_lossy(&fetched.body));
        candidates.extend(found.pages);
        if depth < 2 {
            maps.extend(found.sitemaps.into_iter().map(|m| (m, depth + 1)));
        }
    }

    // On the provider's sites, a keyword in the path, allowed by the site, not known yet: the
    // most keywords first, then the shortest path.
    let mut ranked: BTreeMap<(std::cmp::Reverse<usize>, usize, String), ()> = BTreeMap::new();
    report.considered = candidates.len().min(MAX_CONSIDERED) as u32;
    for raw in candidates.into_iter().take(MAX_CONSIDERED) {
        let Ok(url) = normalise_url(&raw) else { continue };
        if known.contains(url.as_str()) || roots.contains(&url) {
            continue;
        }
        let Ok(at) = url::Url::parse(&url) else { continue };
        if !sites.contains(&site_of(at.host_str().unwrap_or_default())) {
            continue;
        }
        let Some(hits) = keyword_hits(&at, &work.keywords).filter(|h| *h > 0) else { continue };
        let allowed = robots.of(&origin_of(&at)).await.is_some_and(|r| !r.disallows(at.path()));
        if allowed {
            let depth = at.path().split('/').filter(|s| !s.is_empty()).count();
            ranked.insert((std::cmp::Reverse(hits), depth, url), ());
        }
    }

    // The best of them, opened and scored.
    for ((_, _, url), ()) in ranked.into_iter().take(work.max_pages_per_provider as usize) {
        report.rendered += 1;
        let Ok(page) = runner.render.render(&url).await else { continue };
        if page.status.is_some_and(|s| s >= 400) {
            continue;
        }
        let ended = normalise_url(&page.final_url).unwrap_or_else(|_| url.clone());
        let place = url::Url::parse(&ended).ok().unwrap_or_else(|| url::Url::parse(&url).expect("normalised"));
        let (points, price_hits, model_hits) = score(&place, &page.title, &page.text, &p.known_models, &work.keywords);
        if points > 0.0 {
            report.found.push(FoundPage {
                url,
                final_url: ended,
                title: page.title.chars().take(300).collect(),
                score: points,
                price_hits,
                model_hits,
                sample: price_lines(&page.text).into_iter().take(3).map(|l| l.chars().take(200).collect()).collect(),
            });
        }
    }
    report.found.sort_by(|a, b| b.score.total_cmp(&a.score));
    report
}
