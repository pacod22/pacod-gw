//! The discovery query and its execution (docs/concept.md §4, BRK-06).
//!
//! The pipeline:
//! 1. **Candidates** from posting lists: kind, tags, protocols, capabilities, regions, provider,
//!    GPU model, software, text words; the intersection of every list the query names.
//! 2. **Hard filters** per candidate: lease alive; visible to the caller; the caller's
//!    reputation good enough for the resource; available at the instant or over the span asked;
//!    prices within the maxima at that time; model limits; hardware and software; policy;
//!    reputation thresholds.
//! 3. **Sort** within one kind (price, latency or reputation are only compared within a kind):
//!    by relevance, name, a price rate, latency or reputation; ties by id.
//! 4. **Page**: a cursor carries the offset.
//!
//! Every hit says why it matched and the values it was ranked by, so callers can see how the
//! broker chose, not only what.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::Arc;

use jiff::Timestamp;
use pacod_broker_model::availability::{At, AvailabilitySummary};
use pacod_broker_model::pricing::{EffectivePrice, PriceSummary, Rates};
use pacod_broker_model::schedule::{Clock, Span};
use pacod_broker_model::{HardwareType, LiveStatus, Reputation, Visibility};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::build::{Entry, Index};
use crate::live::LiveView;

/// What a consumer asks for.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Query {
    /// The kind. Needed when sorting by price, latency or reputation: those only compare within
    /// one kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Words to search for in names, titles, summaries, descriptions and tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    /// All of these tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// At least one of these protocols.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub protocols: Vec<String>,
    /// All of these capabilities: `tools`, `vision`, `reasoning`, `structured_output`,
    /// `streaming`, `prompt_cache`, `embeddings`, `mcp_tool:<name>`, `extra:<key>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Served from at least one of these regions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regions: Vec<String>,
    /// Data processed only in these places: every place the resource lists must be one of them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residency: Vec<String>,
    /// Inputs never used for training.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub no_training_use: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelQuery>,
    /// The highest acceptable value of each rate, by rate name (`input`, `per_gpu_hour`, …), at
    /// `at`, or as the time-weighted mean over `during`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub max_price: BTreeMap<String, f64>,
    /// Keep resources that have no price for a rate `max_price` names. Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_unpriced: bool,
    /// Only resources priced in this currency (ISO 4217), and free ones. The broker converts
    /// no currency: a price sort or a maximum price compares numbers, so give the currency
    /// whenever prices in several currencies may be listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// The instant prices and availability are evaluated at; now by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<Timestamp>,
    /// A span to evaluate prices and availability over, instead of an instant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub during: Option<Span>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<AvailabilityQuery>,
    /// Also return resources that are not available at `at` (or during `during`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_unavailable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hardware: Option<HardwareQuery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub software: Vec<SoftwareQuery>,
    /// The lowest acceptable reputation score of the resource, 0 to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_reputation: Option<f64>,
    /// With `min_reputation`: the lowest confidence. Resources with less evidence are left out
    /// unless this is 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<f64>,
    #[serde(default)]
    pub sort: Sort,
    /// 20 by default, at most 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_output_tokens: Option<u64>,
    /// All of these input modalities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantisation: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityQuery {
    /// The capacity needed (in the resource's unit). Without it any capacity will do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_capacity: Option<u32>,
    /// Over a span: available all of it (`full`, the default) or some of it (`any`).
    #[serde(default)]
    pub coverage: Coverage,
    /// Leave out resources whose live status is `degraded`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub exclude_degraded: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    #[default]
    Full,
    Any,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HardwareQuery {
    /// A GPU model, as the provider names it, any case: `H100-SXM`, `MI300X`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gpu_memory_gb: Option<f64>,
    /// The number of GPUs of one item (one machine type), at least.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_gpus: Option<u32>,
    /// Some item of this type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<HardwareType>,
    /// Some memory item of at least this size, in GB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_memory_gb: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SoftwareQuery {
    pub name: String,
    /// A semantic version requirement: `>=0.7`, `^12`, `=2.6.0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// How results are ordered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Sort {
    /// Text relevance when there is a text query, else name.
    #[default]
    Default,
    Relevance,
    Name,
    /// A price rate, lowest first; unpriced last.
    Price(String),
    /// Time to first token, lowest first: the live value from heartbeats, else the declared one.
    Ttft,
    /// Reputation score, highest first.
    Reputation,
}

impl Serialize for Sort {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Default => s.serialize_str("default"),
            Self::Relevance => s.serialize_str("relevance"),
            Self::Name => s.serialize_str("name"),
            Self::Price(r) => s.collect_str(&format_args!("price:{r}")),
            Self::Ttft => s.serialize_str("ttft"),
            Self::Reputation => s.serialize_str("reputation"),
        }
    }
}

impl<'de> Deserialize<'de> for Sort {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl std::str::FromStr for Sort {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        Ok(match s {
            "" | "default" => Self::Default,
            "relevance" => Self::Relevance,
            "name" => Self::Name,
            "ttft" => Self::Ttft,
            "reputation" => Self::Reputation,
            _ => match s.strip_prefix("price:") {
                Some(r) if Rates::is_known(r) => Self::Price(r.to_string()),
                _ => return Err(format!("sort {s:?}: default, relevance, name, ttft, reputation or price:<rate>")),
            },
        })
    }
}

impl JsonSchema for Sort {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Sort".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({"type": "string", "pattern": "^(default|relevance|name|ttft|reputation|price:[a-z_]+)$", "examples": ["price:input", "reputation"]})
    }
}

/// Who is asking.
#[derive(Debug, Clone, Default)]
pub struct Viewer {
    /// Sees every visibility: open mode, or an admin.
    pub all: bool,
    /// Passes providers' client thresholds too: an admin reviewing the catalogue. Open mode sees
    /// every visibility but is still held to them, since clients there have reputations as well.
    pub reviewer: bool,
    /// Signed in (sees `platform`).
    pub authenticated: bool,
    pub organisation: Option<String>,
    /// The caller's own reputation (score, confidence), for providers' thresholds.
    pub reputation: Option<(f64, f64)>,
    /// Anonymous callers see `public` resources only when the broker allows it.
    pub public_discovery: bool,
}

impl Viewer {
    pub fn everything() -> Self {
        Self { all: true, reviewer: true, authenticated: true, ..Self::default() }
    }

    pub fn can_see(&self, v: &Visibility) -> bool {
        if self.all {
            return true;
        }
        match v {
            Visibility::Public => self.authenticated || self.public_discovery,
            Visibility::Platform => self.authenticated,
            Visibility::Organisation(o) => self.organisation.as_deref() == Some(o.as_str()),
        }
    }
}

/// Reputation scores, as the reputation engine computed them last.
pub trait Scores: Send + Sync {
    fn resource(&self, id: &str) -> Option<&Reputation>;
    fn provider(&self, id: &str) -> Option<&Reputation>;
}

/// No scores: every subject is new.
pub struct NoScores;

impl Scores for NoScores {
    fn resource(&self, _: &str) -> Option<&Reputation> {
        None
    }

    fn provider(&self, _: &str) -> Option<&Reputation> {
        None
    }
}

/// What the query runs against besides the index.
pub struct Ctx<'a> {
    pub now: Timestamp,
    pub viewer: &'a Viewer,
    pub scores: &'a dyn Scores,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    Invalid(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for QueryError {}

/// One result.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Hit {
    #[serde(skip)]
    #[schemars(skip)]
    pub entry: Arc<Entry>,
    pub id: String,
    /// The price at `at`, when the query asked for an instant.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<EffectivePrice>,
    /// The prices over `during`, when the query asked for a span.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price_summary: Option<PriceSummary>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability_summary: Option<AvailabilitySummary>,
    pub live: LiveView,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reputation: Option<Reputation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_reputation: Option<Reputation>,
    /// Why it matched and what it was ranked by.
    pub why: Why,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Why {
    /// The filters the resource passed, in words.
    pub matched: Vec<String>,
    /// The sort key and its value.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sorted_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relevance: Option<f64>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Results {
    pub generation: u64,
    pub at: Timestamp,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub during: Option<Span>,
    /// Results that passed every filter, before paging.
    pub total: usize,
    pub hits: Vec<Hit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 200;

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j, mut out) = (0, 0, Vec::with_capacity(a.len().min(b.len())));
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

fn union(lists: &[&Vec<u32>]) -> Vec<u32> {
    let mut out: Vec<u32> = lists.iter().flat_map(|l| l.iter().copied()).collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The candidate set so far: `None` means every entry.
struct Candidates(Option<Vec<u32>>);

impl Candidates {
    fn and(&mut self, list: Vec<u32>) {
        self.0 = Some(match self.0.take() {
            None => list,
            Some(prev) => intersect(&prev, &list),
        });
    }
}

impl Query {
    fn check(&self) -> Result<(), QueryError> {
        let bad = |m: String| Err(QueryError::Invalid(m));
        for r in self.max_price.keys() {
            if !Rates::is_known(r) {
                return bad(format!("max_price.{r}: not a rate; rates are {}", Rates::NAMES.join(", ")));
            }
        }
        if self.at.is_some() && self.during.is_some() {
            return bad("give `at` or `during`, not both".into());
        }
        if let Some(d) = &self.during {
            Span::new(d.from, d.to).map_err(|e| QueryError::Invalid(format!("during: {e}")))?;
        }
        if matches!(self.sort, Sort::Price(_) | Sort::Ttft | Sort::Reputation) && self.kind.is_none() {
            return bad("sorting by price, ttft or reputation compares within one kind: give `kind`".into());
        }
        for (name, v) in [("min_reputation", self.min_reputation), ("min_confidence", self.min_confidence)] {
            if v.is_some_and(|v| !(0.0..=1.0).contains(&v)) {
                return bad(format!("{name}: between 0 and 1"));
            }
        }
        for s in &self.software {
            if let Some(v) = &s.version {
                semver::VersionReq::parse(v)
                    .map_err(|e| QueryError::Invalid(format!("software {}: version {v:?}: {e}", s.name)))?;
            }
        }
        if self.limit == Some(0) {
            return bad("limit: at least 1".into());
        }
        Ok(())
    }

    fn offset(&self) -> Result<usize, QueryError> {
        match &self.cursor {
            None => Ok(0),
            Some(c) => c.parse().map_err(|_| QueryError::Invalid("cursor: not one this broker gave out".into())),
        }
    }
}

/// What the cheap pass keeps of a candidate: enough to sort it and to build its hit later.
struct Candidate {
    i: u32,
    key: Option<f64>,
    available: bool,
    capacity: Option<u32>,
    availability_summary: Option<AvailabilitySummary>,
    price_summary: Option<PriceSummary>,
    relevance: Option<f64>,
}

impl Index {
    /// Runs a discovery query.
    ///
    /// Two passes: the first checks every candidate without allocating, keeping only what the
    /// sort needs; the second builds full hits (effective prices, live views, reasons) for the
    /// page returned, not for every match.
    pub fn discover(&self, q: &Query, ctx: &Ctx<'_>) -> Result<Results, QueryError> {
        q.check()?;
        let offset = q.offset()?;
        let limit = q.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
        let at = q.at.unwrap_or(ctx.now);
        let clock = Clock::new(at);
        let reqs: Vec<Option<semver::VersionReq>> =
            q.software.iter().map(|s| s.version.as_deref().and_then(|v| semver::VersionReq::parse(v).ok())).collect();
        let gpu = q
            .hardware
            .as_ref()
            .and_then(|h| h.gpu_model.as_ref())
            .map(|g| g.trim().to_ascii_lowercase().replace(' ', "-"));

        // 1. Candidates from posting lists.
        let empty = Vec::new();
        let mut c = Candidates(None);
        if let Some(k) = &q.kind {
            c.and(self.by_kind.get(k).cloned().unwrap_or_default());
        }
        for t in &q.tags {
            c.and(self.by_tag.get(t).cloned().unwrap_or_default());
        }
        for cap in &q.capabilities {
            c.and(self.by_capability.get(cap).cloned().unwrap_or_default());
        }
        if !q.protocols.is_empty() {
            c.and(union(&q.protocols.iter().map(|p| self.by_protocol.get(p).unwrap_or(&empty)).collect::<Vec<_>>()));
        }
        if !q.regions.is_empty() {
            c.and(union(&q.regions.iter().map(|p| self.by_region.get(p).unwrap_or(&empty)).collect::<Vec<_>>()));
        }
        if let Some(p) = &q.provider {
            c.and(self.by_provider.get(p).cloned().unwrap_or_default());
        }
        if let Some(g) = &gpu {
            c.and(self.by_gpu.get(g).cloned().unwrap_or_default());
        }
        for s in &q.software {
            c.and(self.by_software.get(&s.name).cloned().unwrap_or_default());
        }
        let text = q.q.as_deref().and_then(|t| self.text.search(t));
        if let Some(t) = &text {
            let mut ids: Vec<u32> = t.keys().copied().collect();
            ids.sort_unstable();
            c.and(ids);
        }

        // 2. Hard filters, without allocating.
        let pass = Pass { q, ctx, clock: &clock, reqs: &reqs, gpu: gpu.as_deref() };
        let mut out: Vec<Candidate> = Vec::with_capacity(c.0.as_ref().map_or(self.entries.len(), Vec::len));
        let mut check = |i: u32| {
            if let Some(mut cand) = pass.check(i, &self.entries[i as usize]) {
                cand.relevance = text.as_ref().and_then(|t| t.get(&i).copied());
                cand.key = sort_key(&self.entries[i as usize], &cand, q, ctx, &clock);
                out.push(cand);
            }
        };
        match &c.0 {
            Some(list) => list.iter().copied().for_each(&mut check),
            None => (0..self.entries.len() as u32).for_each(&mut check),
        }

        // 3. Sort.
        let sort_name = match &q.sort {
            Sort::Default if text.is_some() => "relevance".to_string(),
            Sort::Default | Sort::Name => "name".to_string(),
            Sort::Relevance => "relevance".to_string(),
            Sort::Price(r) => format!("price:{r}"),
            Sort::Ttft => "ttft".to_string(),
            Sort::Reputation => "reputation".to_string(),
        };
        let descending =
            matches!(q.sort, Sort::Reputation | Sort::Relevance) || (q.sort == Sort::Default && text.is_some());
        let by_name = sort_name == "name";
        let cmp = |a: &Candidate, b: &Candidate| {
            let ord = if by_name {
                self.name_rank[a.i as usize].cmp(&self.name_rank[b.i as usize])
            } else {
                match (a.key, b.key) {
                    (Some(x), Some(y)) => {
                        let o = x.partial_cmp(&y).unwrap_or(Ordering::Equal);
                        if descending { o.reverse() } else { o }
                    }
                    // A resource without the value goes last either way.
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => Ordering::Equal,
                }
            };
            ord.then(a.i.cmp(&b.i))
        };
        let total = out.len();
        let end = (offset + limit).min(total);
        // Only the first `end` need to be in order: select them, then sort that part.
        if end < total && end > 0 {
            out.select_nth_unstable_by(end - 1, cmp);
            out.truncate(end);
        }
        out.sort_unstable_by(cmp);

        // 4. Page, and build the hits returned.
        let next_cursor = (offset + limit < total).then(|| (offset + limit).to_string());
        let hits = out
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|c| {
                let mut h = self.hit(c, q, ctx, &clock);
                h.why.sorted_by = Some(sort_name.clone());
                if by_name {
                    h.why.sort_value = None;
                }
                h
            })
            .collect();
        Ok(Results { generation: self.generation, at, during: q.during, total, hits, next_cursor })
    }

    /// Builds the full hit for a candidate on the page.
    fn hit(&self, c: Candidate, q: &Query, ctx: &Ctx<'_>, clock: &Clock) -> Hit {
        let e = &self.entries[c.i as usize];
        let r = &e.record;
        let s = &r.spec;
        let mut why = Vec::new();
        if q.model.is_some() {
            why.push("model limits".into());
        }
        if !q.residency.is_empty() {
            why.push(format!("data stays in {}", s.policy.residency.join(", ")));
        }
        if q.no_training_use {
            why.push("no training use".into());
        }
        if q.hardware.is_some() {
            why.push("hardware".into());
        }
        for sq in &q.software {
            why.push(format!(
                "software {}{}",
                sq.name,
                sq.version.as_deref().map(|v| format!(" {v}")).unwrap_or_default()
            ));
        }
        if c.available {
            why.push(if q.during.is_some() { "available over the span".into() } else { "available".into() });
        }
        let price = match (&q.during, &s.pricing) {
            (None, Some(p)) => p.at_clock(clock),
            _ => None,
        };
        for (rate, max) in &q.max_price {
            let v = match (&c.price_summary, &price) {
                (Some(sum), _) => sum.rates.get(rate).map(|r| r.avg).or(sum.free.then_some(0.0)),
                (None, Some(p)) => p.rates.get(rate).or(p.free.then_some(0.0)),
                _ => None,
            };
            why.push(match v {
                Some(v) => format!("{rate} {v} ≤ {max}"),
                None => format!("{rate} unpriced"),
            });
        }
        let reputation = ctx.scores.resource(&r.id).cloned();
        let provider_reputation = ctx.scores.provider(&r.provider_id).cloned();
        if let Some(min) = q.min_reputation {
            let score = reputation.as_ref().map_or(0.5, |x| x.score);
            why.push(format!("reputation {score:.2} ≥ {min}"));
        }
        if q.q.is_some() {
            why.push("text".into());
        }
        Hit {
            entry: e.clone(),
            id: r.id.clone(),
            price,
            price_summary: c.price_summary,
            available: c.available,
            capacity: c.capacity,
            availability_summary: c.availability_summary,
            live: e.live.view(),
            reputation,
            provider_reputation,
            why: Why { matched: why, sorted_by: None, sort_value: c.key, relevance: c.relevance },
        }
    }
}

/// The resource's reputation score and confidence, neutral when it has none.
fn reputation_of(e: &Entry, scores: &dyn Scores) -> (f64, f64) {
    scores.resource(&e.record.id).map_or((0.5, 0.0), |x| (x.score, x.confidence))
}

/// The value a candidate is sorted by; `None` sorts last.
fn sort_key(e: &Entry, c: &Candidate, q: &Query, ctx: &Ctx<'_>, clock: &Clock) -> Option<f64> {
    let s = &e.record.spec;
    match &q.sort {
        Sort::Default | Sort::Relevance => c.relevance,
        Sort::Name => None,
        Sort::Price(rate) => match &c.price_summary {
            Some(sum) => sum.rates.get(rate).map(|r| r.avg).or(sum.free.then_some(0.0)),
            None => s.pricing.as_ref().and_then(|p| p.rate_at_clock(rate, clock)),
        },
        Sort::Ttft => e.live.ttft_ms().or_else(|| s.availability.latency.and_then(|l| l.ttft_ms)),
        Sort::Reputation => Some(reputation_of(e, ctx.scores).0),
    }
}

/// The cheap pass over candidates.
struct Pass<'a> {
    q: &'a Query,
    ctx: &'a Ctx<'a>,
    clock: &'a Clock,
    reqs: &'a [Option<semver::VersionReq>],
    gpu: Option<&'a str>,
}

impl Pass<'_> {
    fn check(&self, i: u32, e: &Entry) -> Option<Candidate> {
        let (q, ctx) = (self.q, self.ctx);
        let r = &e.record;
        let s = &r.spec;
        if e.live.is_expired(ctx.now) || !ctx.viewer.can_see(&s.access.visibility) {
            return None;
        }
        // Providers may keep resources from clients whose reputation is too low.
        if !ctx.viewer.reviewer
            && let Some(min) = s.access.min_client_reputation
        {
            let ok = match ctx.viewer.reputation {
                Some((score, conf)) if conf >= 0.3 => score >= min,
                _ => s.access.allow_new_clients,
            };
            if !ok {
                return None;
            }
        }
        if let Some(m) = &q.model {
            let info = s.model.as_ref()?;
            if m.family.as_ref().is_some_and(|f| info.family.as_deref() != Some(f.as_str()))
                || m.min_context_window.is_some_and(|min| info.context_window.is_none_or(|c| c < min))
                || m.min_output_tokens.is_some_and(|min| info.max_output_tokens.is_none_or(|c| c < min))
                || !m.input.iter().all(|x| info.modalities.input.contains(x))
                || !m.output.iter().all(|x| info.modalities.output.contains(x))
                || m.quantisation.as_ref().is_some_and(|qz| info.quantisation.as_deref() != Some(qz.as_str()))
            {
                return None;
            }
        }
        if !q.residency.is_empty()
            && (s.policy.residency.is_empty() || !s.policy.residency.iter().all(|x| q.residency.contains(x)))
        {
            return None;
        }
        if q.no_training_use && s.policy.training_use != Some(false) {
            return None;
        }
        if (q.hardware.is_some() || !q.software.is_empty())
            && e.inventory_visibility().is_none_or(|v| !ctx.viewer.can_see(v))
        {
            // An inventory the caller may not see cannot be matched on, or filtering would leak it.
            return None;
        }
        if let Some(h) = &q.hardware {
            let ok = e.hardware().any(|item| {
                let gpu = item.kind == HardwareType::Gpu;
                self.gpu.is_none_or(|g| gpu && item.model_key().as_deref() == Some(g))
                    && h.min_gpu_memory_gb.is_none_or(|m| gpu && item.memory_gb.is_some_and(|x| x >= m))
                    && h.min_gpus.is_none_or(|m| gpu && item.count >= m)
                    && h.r#type.is_none_or(|t| item.kind == t)
                    && h.min_memory_gb.is_none_or(|m| {
                        item.kind == HardwareType::Memory && item.memory_gb.is_some_and(|x| x * item.count as f64 >= m)
                    })
            });
            if !ok {
                return None;
            }
        }
        for (sq, req) in q.software.iter().zip(self.reqs) {
            if !e.software().any(|item| {
                item.name == sq.name && req.as_ref().is_none_or(|rq| item.semver().is_some_and(|v| rq.matches(&v)))
            }) {
                return None;
            }
        }

        // Availability, at the instant or over the span.
        let aq = q.availability.as_ref();
        let min_capacity = aq.and_then(|a| a.min_capacity);
        if aq.is_some_and(|a| a.exclude_degraded) && e.live.status() == LiveStatus::Degraded {
            return None;
        }
        let live = e.live.model(ctx.now);
        let (available, capacity, availability_summary) = match q.during {
            Some(span) => {
                let sum = s.availability.during(span, Some(live), min_capacity);
                let ok = match aq.map(|a| a.coverage).unwrap_or_default() {
                    Coverage::Full => sum.available_share >= 1.0 - 1e-9,
                    Coverage::Any => sum.available_share > 0.0,
                };
                (ok, sum.min_capacity, Some(sum))
            }
            None => {
                let a = s.availability.at_clock(self.clock, Some(live));
                let cap = match a {
                    At::Closed => Some(0),
                    At::Open(c) => c,
                };
                (a.meets(min_capacity), cap, None)
            }
        };
        if !available && !q.include_unavailable {
            return None;
        }

        // Prices.
        if let Some(cur) = &q.currency {
            match &s.pricing {
                Some(p) if p.free || p.currency.eq_ignore_ascii_case(cur) => {}
                _ => return None,
            }
        }
        let price_summary = match (q.during, &s.pricing) {
            (Some(span), Some(p)) => Some(p.during(span)),
            _ => None,
        };
        for (rate, max) in &q.max_price {
            let v = match (&price_summary, &s.pricing) {
                (Some(sum), _) => sum.rates.get(rate).map(|r| r.avg).or(sum.free.then_some(0.0)),
                (None, Some(p)) => p.rate_at_clock(rate, self.clock),
                _ => None,
            };
            match v {
                Some(v) if v <= *max => {}
                Some(_) => return None,
                None if q.include_unpriced => {}
                None => return None,
            }
        }

        // Reputation.
        if let Some(min) = q.min_reputation {
            let (score, conf) = reputation_of(e, ctx.scores);
            if score < min || conf < q.min_confidence.unwrap_or(0.3) {
                return None;
            }
        }
        Some(Candidate { i, key: None, available, capacity, availability_summary, price_summary, relevance: None })
    }
}

#[cfg(test)]
mod tests;
