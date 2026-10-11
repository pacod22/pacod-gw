//! Ranking (feature catalog BRK-06): providers or models compared on how they serve. Every
//! metric is ranked on its own, and a preset's weights combine the metric scores into one
//! overall score. The score breakdown is part of the answer, and the order is total.
//!
//! Only calls that reached a provider count: the gateway's own rejections and the caller's
//! cancellations say nothing about the provider. Models are compared within one kind (chat or
//! speech), because a chat model and a speech model share no latency or price scale. The kind is
//! read from the call's path.

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::params::{Filters, Format, Params, USAGE_FILTERS};
use super::{EVENTS, csv};
use crate::AppState;
use crate::error::ApiError;
use crate::storage::Binds;

/// Largest `min_requests` accepted.
const MAX_MIN_REQUESTS: u64 = 100_000;

#[derive(Debug, Clone, Copy, PartialEq)]
enum By {
    Model,
    Provider,
}

impl By {
    fn take(params: &mut Params) -> Result<Self, ApiError> {
        match params.one("by")?.as_deref() {
            None | Some("model") => Ok(Self::Model),
            Some("provider") => Ok(Self::Provider),
            Some(_) => Err(ApiError::invalid_parameter("by", "expected model or provider")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Provider => "provider",
        }
    }

    /// The model part of the group key; empty when providers are ranked.
    fn model_sql(self) -> &'static str {
        match self {
            Self::Model => "toString(model_served)",
            Self::Provider => "''",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Chat,
    Speech,
    Embedding,
}

impl Kind {
    fn take(params: &mut Params) -> Result<Self, ApiError> {
        match params.one("kind")?.as_deref() {
            None | Some("chat") => Ok(Self::Chat),
            Some("speech") => Ok(Self::Speech),
            Some("embedding") => Ok(Self::Embedding),
            Some(_) => Err(ApiError::invalid_parameter("kind", "expected chat, speech or embedding")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Speech => "speech",
            Self::Embedding => "embedding",
        }
    }

    /// The calls of this kind, by path. A relayed call carries the provider's own path, so its
    /// ending decides: OpenAI chat completions and responses, Anthropic messages, Gemini
    /// `generateContent` and `streamGenerateContent`, Ollama `/api/chat` and `/api/generate`.
    /// Events written before the path was recorded have none and were chat. Token counting
    /// (`/v1/messages/count_tokens`, `:countTokens`) is neither.
    fn condition(self) -> &'static str {
        match self {
            Self::Chat => {
                "(path IN ('', '/v1/chat/completions', '/v1/messages', '/gw/v1/chat') \
                 OR endsWith(path, '/chat/completions') OR endsWith(path, '/messages') \
                 OR endsWith(path, '/responses') OR endsWith(path, '/api/chat') \
                 OR endsWith(path, '/api/generate') OR match(path, ':(streamG|g)enerateContent$'))"
            }
            Self::Speech => "(path IN ('/v1/audio/speech', '/gw/v1/speech') OR endsWith(path, '/audio/speech'))",
            Self::Embedding => {
                "(endsWith(path, '/embeddings') OR match(path, ':(embedContent|batchEmbedContents|predict)$'))"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Preset {
    Balanced,
    Speed,
    Cost,
    Reliability,
}

impl Preset {
    fn take(params: &mut Params) -> Result<Self, ApiError> {
        match params.one("preset")?.as_deref() {
            None | Some("balanced") => Ok(Self::Balanced),
            Some("speed") => Ok(Self::Speed),
            Some("cost") => Ok(Self::Cost),
            Some("reliability") => Ok(Self::Reliability),
            Some(_) => Err(ApiError::invalid_parameter("preset", "expected balanced, speed, cost or reliability")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Speed => "speed",
            Self::Cost => "cost",
            Self::Reliability => "reliability",
        }
    }

    /// The weight of each metric, in the order of [`METRICS`]. Each row adds up to 1.
    fn weights(self) -> [f64; 5] {
        match self {
            Self::Balanced => [0.30, 0.15, 0.15, 0.15, 0.25],
            Self::Speed => [0.15, 0.30, 0.20, 0.30, 0.05],
            Self::Cost => [0.20, 0.05, 0.05, 0.05, 0.65],
            Self::Reliability => [0.60, 0.05, 0.20, 0.05, 0.10],
        }
    }
}

/// The ranked metrics.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Metric {
    /// Successful calls of those that reached the provider. Higher is better.
    SuccessRate,
    /// Median time from the connection to the provider's response head, successful calls.
    FirstByteP50,
    /// 95th percentile of the same.
    FirstByteP95,
    /// Streaming time per output token, successful streamed calls. Chat only.
    OutputPerToken,
    /// Chat: cost per million tokens. Speech: cost per call. Priced calls only.
    Cost,
}

const METRICS: [Metric; 5] =
    [Metric::SuccessRate, Metric::FirstByteP50, Metric::FirstByteP95, Metric::OutputPerToken, Metric::Cost];

impl Metric {
    fn higher_is_better(self) -> bool {
        self == Self::SuccessRate
    }
}

/// One group as ClickHouse counts it. The aliases differ from the column names, because a
/// column read elsewhere in the `SELECT` would otherwise read the aggregate (see `mod.rs`).
#[derive(Debug, Clone, Default, Deserialize)]
struct Counts {
    provider_key: String,
    model_key: String,
    n_calls: u64,
    n_eligible: u64,
    n_ok: u64,
    n_429: u64,
    n_timeout: u64,
    fb_n: u64,
    fb_q: Vec<Option<f64>>,
    stream_ms_sum: f64,
    stream_out_tok: u64,
    priced_n: u64,
    priced_cost: f64,
    priced_tok: u64,
    cur_any: String,
    cur_n: u64,
    in_tok_sum: u64,
    cache_read_sum: u64,
    estimated_n: u64,
    attempts_sum: u64,
}

const COUNTS_SQL: &str = "WITH status = 'ok' AS ok, \
          attempts > 0 AND status NOT IN ('rejected', 'cancelled') AS eligible, \
          cost IS NOT NULL AND NOT price_missing AS priced, \
          if(isNull(cost_reporting), toString(currency), toString(reporting_currency)) AS cost_currency, \
          eligible AND ok AND upstream_first_byte_ms > 0 AS timed, \
          eligible AND ok AND stream AND output_tokens > 0 AND upstream_stream_ms > 0 AS streamed \
     SELECT count() AS n_calls, \
            countIf(eligible) AS n_eligible, \
            countIf(eligible AND ok) AS n_ok, \
            countIf(eligible AND http_status = 429) AS n_429, \
            countIf(eligible AND status = 'timeout') AS n_timeout, \
            countIf(timed) AS fb_n, \
            quantilesExactIf(0.5, 0.95)(toFloat64(upstream_first_byte_ms), timed) AS fb_q, \
            sumIf(toFloat64(upstream_stream_ms), streamed) AS stream_ms_sum, \
            sumIf(output_tokens, streamed) AS stream_out_tok, \
            countIf(eligible AND priced) AS priced_n, \
            sumIf(ifNull(cost_reporting, assumeNotNull(cost)), eligible AND priced) AS priced_cost, \
            sumIf(input_tokens + cache_read_tokens + cache_write_tokens + output_tokens, eligible AND priced) \
                AS priced_tok, \
            anyIf(cost_currency, eligible AND priced) AS cur_any, \
            uniqExactIf(cost_currency, eligible AND priced) AS cur_n, \
            sumIf(input_tokens, eligible) AS in_tok_sum, \
            sumIf(cache_read_tokens, eligible) AS cache_read_sum, \
            countIf(eligible AND usage_source != 'provider') AS estimated_n, \
            sumIf(attempts, eligible) AS attempts_sum";

/// A metric of one group: its value, its rank among the ranked groups that have one, and the
/// score that rank earns (100 for the best, 0 for the worst).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
struct Measure {
    value: Option<f64>,
    rank: Option<u64>,
    score: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
struct Metrics {
    success_rate: Measure,
    first_byte_p50_ms: Measure,
    first_byte_p95_ms: Measure,
    output_ms_per_token: Measure,
    cost: Measure,
}

impl Metrics {
    fn get_mut(&mut self, metric: Metric) -> &mut Measure {
        match metric {
            Metric::SuccessRate => &mut self.success_rate,
            Metric::FirstByteP50 => &mut self.first_byte_p50_ms,
            Metric::FirstByteP95 => &mut self.first_byte_p95_ms,
            Metric::OutputPerToken => &mut self.output_ms_per_token,
            Metric::Cost => &mut self.cost,
        }
    }

    fn get(&self, metric: Metric) -> Measure {
        match metric {
            Metric::SuccessRate => self.success_rate,
            Metric::FirstByteP50 => self.first_byte_p50_ms,
            Metric::FirstByteP95 => self.first_byte_p95_ms,
            Metric::OutputPerToken => self.output_ms_per_token,
            Metric::Cost => self.cost,
        }
    }
}

/// Figures shown beside the ranked metrics. Shares are of the calls that reached the provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
struct Info {
    rate_limited_share: f64,
    timeout_share: f64,
    /// Cache reads of all input tokens; `null` without input.
    cache_read_share: Option<f64>,
    /// Calls with a price.
    price_coverage: f64,
    /// Calls whose token counts did not come from the provider.
    estimated_share: f64,
    avg_attempts: f64,
    cost_per_request: Option<f64>,
    output_tokens_per_second: Option<f64>,
    /// The currency of the priced calls; `null` without priced calls or when they mix
    /// currencies.
    currency: Option<String>,
    mixed_currencies: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct Row {
    /// Position among the ranked groups; `null` for a group with too few calls.
    rank: Option<u64>,
    score: Option<f64>,
    ranked: bool,
    provider: String,
    /// `null` when providers are ranked.
    model: Option<String>,
    requests: u64,
    /// The calls that reached the provider.
    eligible: u64,
    metrics: Metrics,
    info: Info,
    /// The priced calls, for choosing the reference currency.
    #[serde(skip)]
    priced: u64,
}

#[derive(Serialize)]
struct Weights {
    success_rate: f64,
    first_byte_p50_ms: f64,
    first_byte_p95_ms: f64,
    output_ms_per_token: f64,
    cost: f64,
}

impl From<[f64; 5]> for Weights {
    fn from([success_rate, first_byte_p50_ms, first_byte_p95_ms, output_ms_per_token, cost]: [f64; 5]) -> Self {
        Self { success_rate, first_byte_p50_ms, first_byte_p95_ms, output_ms_per_token, cost }
    }
}

#[derive(Serialize)]
pub struct Ranking {
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    kind: &'static str,
    by: &'static str,
    preset: &'static str,
    min_requests: u64,
    /// The currency costs are ranked in: the one with the most priced calls among the ranked
    /// groups. `null` when none of them has a price.
    currency: Option<String>,
    weights: Weights,
    rows: Vec<Row>,
}

/// One line of the CSV download: the rows flattened.
#[derive(Default, Serialize)]
struct CsvRow {
    rank: Option<u64>,
    score: Option<f64>,
    ranked: bool,
    provider: String,
    model: Option<String>,
    requests: u64,
    eligible: u64,
    success_rate: Option<f64>,
    success_rate_rank: Option<u64>,
    first_byte_p50_ms: Option<f64>,
    first_byte_p50_ms_rank: Option<u64>,
    first_byte_p95_ms: Option<f64>,
    first_byte_p95_ms_rank: Option<u64>,
    output_ms_per_token: Option<f64>,
    output_ms_per_token_rank: Option<u64>,
    cost: Option<f64>,
    cost_rank: Option<u64>,
    currency: Option<String>,
    rate_limited_share: f64,
    timeout_share: f64,
    cache_read_share: Option<f64>,
    price_coverage: f64,
    estimated_share: f64,
    avg_attempts: f64,
    cost_per_request: Option<f64>,
    output_tokens_per_second: Option<f64>,
}

impl From<Row> for CsvRow {
    fn from(row: Row) -> Self {
        let m = &row.metrics;
        Self {
            rank: row.rank,
            score: row.score,
            ranked: row.ranked,
            success_rate: m.success_rate.value,
            success_rate_rank: m.success_rate.rank,
            first_byte_p50_ms: m.first_byte_p50_ms.value,
            first_byte_p50_ms_rank: m.first_byte_p50_ms.rank,
            first_byte_p95_ms: m.first_byte_p95_ms.value,
            first_byte_p95_ms_rank: m.first_byte_p95_ms.rank,
            output_ms_per_token: m.output_ms_per_token.value,
            output_ms_per_token_rank: m.output_ms_per_token.rank,
            cost: m.cost.value,
            cost_rank: m.cost.rank,
            currency: row.info.currency,
            rate_limited_share: row.info.rate_limited_share,
            timeout_share: row.info.timeout_share,
            cache_read_share: row.info.cache_read_share,
            price_coverage: row.info.price_coverage,
            estimated_share: row.info.estimated_share,
            avg_attempts: row.info.avg_attempts,
            cost_per_request: row.info.cost_per_request,
            output_tokens_per_second: row.info.output_tokens_per_second,
            provider: row.provider,
            model: row.model,
            requests: row.requests,
            eligible: row.eligible,
        }
    }
}

pub async fn report(State(state): State<AppState>, RawQuery(query): RawQuery) -> Result<Response, ApiError> {
    let mut params = Params::parse(query.as_deref());
    let range = params.range()?;
    let by = By::take(&mut params)?;
    let kind = Kind::take(&mut params)?;
    let preset = Preset::take(&mut params)?;
    let min_requests = params.parsed::<u64>("min_requests", "a whole number")?.unwrap_or(20);
    if min_requests == 0 || min_requests > MAX_MIN_REQUESTS {
        return Err(ApiError::invalid_parameter("min_requests", format!("must be between 1 and {MAX_MIN_REQUESTS}")));
    }
    let filters = Filters::take(&mut params, &USAGE_FILTERS)?.within(range).and(kind.condition());
    let format = params.format()?;
    params.finish()?;

    let mut binds = Binds::new();
    // Groups whose calls never reached a provider (all rejected, say) have nothing to compare.
    let sql = format!(
        "{COUNTS_SQL}, toString(provider) AS provider_key, {model} AS model_key \
         FROM {EVENTS} {filter} \
         GROUP BY provider_key, model_key \
         HAVING n_eligible > 0",
        model = by.model_sql(),
        filter = filters.where_sql(&mut binds),
    );
    let counts: Vec<Counts> = state.clickhouse().fetch(&sql, &binds).await?;
    let (currency, rows) = rank(counts, by, kind, preset, min_requests);

    if format == Format::Csv {
        let rows: Vec<CsvRow> = rows.into_iter().map(CsvRow::from).collect();
        return csv::download("ranking", &CsvRow::default(), &rows);
    }
    Ok(Json(Ranking {
        from: range.from,
        to: range.to,
        kind: kind.name(),
        by: by.name(),
        preset: preset.name(),
        min_requests,
        currency,
        weights: preset.weights().into(),
        rows,
    })
    .into_response())
}

fn ratio(part: u64, whole: u64) -> Option<f64> {
    (whole > 0).then(|| part as f64 / whole as f64)
}

/// The values of one group, before any ranking.
fn measure(counts: &Counts, by: By, kind: Kind) -> Row {
    let eligible = counts.n_eligible;
    let share = |part: u64| ratio(part, eligible).unwrap_or(0.0);
    let one_currency = counts.priced_n > 0 && counts.cur_n == 1;
    let first_byte = |index: usize| {
        if counts.fb_n == 0 { None } else { counts.fb_q.get(index).copied().flatten() }
    };
    let per_token = (kind == Kind::Chat && counts.stream_out_tok > 0 && counts.stream_ms_sum > 0.0)
        .then(|| counts.stream_ms_sum / counts.stream_out_tok as f64);
    let cost =
        match kind {
            Kind::Chat => (one_currency && counts.priced_tok > 0)
                .then(|| counts.priced_cost / counts.priced_tok as f64 * 1_000_000.0),
            Kind::Speech => one_currency.then(|| counts.priced_cost / counts.priced_n as f64),
            // Embeddings are priced by their input tokens: per million, like chat.
            Kind::Embedding => (one_currency && counts.priced_tok > 0)
                .then(|| counts.priced_cost / counts.priced_tok as f64 * 1_000_000.0),
        };

    let value = |value: Option<f64>| Measure { value, ..Measure::default() };
    Row {
        rank: None,
        score: None,
        ranked: false,
        provider: counts.provider_key.clone(),
        model: (by == By::Model).then(|| counts.model_key.clone()),
        requests: counts.n_calls,
        eligible,
        metrics: Metrics {
            success_rate: value(ratio(counts.n_ok, eligible)),
            first_byte_p50_ms: value(first_byte(0)),
            first_byte_p95_ms: value(first_byte(1)),
            output_ms_per_token: value(per_token),
            cost: value(cost),
        },
        info: Info {
            rate_limited_share: share(counts.n_429),
            timeout_share: share(counts.n_timeout),
            cache_read_share: ratio(counts.cache_read_sum, counts.in_tok_sum + counts.cache_read_sum),
            price_coverage: share(counts.priced_n),
            estimated_share: share(counts.estimated_n),
            avg_attempts: share(counts.attempts_sum),
            cost_per_request: one_currency.then(|| counts.priced_cost / counts.priced_n as f64),
            output_tokens_per_second: per_token.map(|ms| 1000.0 / ms),
            currency: one_currency.then(|| counts.cur_any.clone()),
            mixed_currencies: counts.cur_n > 1,
        },
        priced: counts.priced_n,
    }
}

/// The currency with the most priced calls among the ranked rows; the smaller code on a tie.
fn reference_currency(rows: &[Row]) -> Option<String> {
    let mut calls: Vec<(&str, u64)> = Vec::new();
    for row in rows.iter().filter(|row| row.ranked) {
        let Some(currency) = row.info.currency.as_deref() else {
            continue;
        };
        match calls.iter_mut().find(|(existing, _)| *existing == currency) {
            Some((_, total)) => *total += row.priced,
            None => calls.push((currency, row.priced)),
        }
    }
    calls.sort_by(|(a, x), (b, y)| y.cmp(x).then_with(|| a.cmp(b)));
    calls.first().map(|(currency, _)| (*currency).to_owned())
}

/// Competition ranks (equal values share a rank) and their scores, for `values` in any order.
/// The best value ranks 1 and scores 100, the worst scores 0; a lone value scores 100.
fn ranks(values: &[f64], higher_is_better: bool) -> Vec<(u64, f64)> {
    let n = values.len();
    values
        .iter()
        .map(|value| {
            let better =
                values.iter().filter(|other| if higher_is_better { *other > value } else { *other < value }).count();
            let rank = better + 1;
            let score = if n == 1 { 100.0 } else { 100.0 * (n - rank) as f64 / (n - 1) as f64 };
            (rank as u64, score)
        })
        .collect()
}

/// Two decimals: enough to tell scores apart, and equal-looking scores fall to the tie-break.
fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Ranks the groups. Returns the reference currency and the rows: the ranked ones best first,
/// then those with too few calls, most calls first.
fn rank(counts: Vec<Counts>, by: By, kind: Kind, preset: Preset, min_requests: u64) -> (Option<String>, Vec<Row>) {
    let mut rows: Vec<Row> = counts.iter().map(|counts| measure(counts, by, kind)).collect();
    for row in &mut rows {
        row.ranked = row.eligible >= min_requests;
    }
    let currency = reference_currency(&rows);

    for metric in METRICS {
        let comparable = |row: &Row| {
            row.ranked
                && row.metrics.get(metric).value.is_some()
                && (metric != Metric::Cost || row.info.currency == currency)
        };
        let indices: Vec<usize> = (0..rows.len()).filter(|&index| comparable(&rows[index])).collect();
        let values: Vec<f64> = indices.iter().filter_map(|&index| rows[index].metrics.get(metric).value).collect();
        for (&index, (rank, score)) in indices.iter().zip(ranks(&values, metric.higher_is_better())) {
            let measure = rows[index].metrics.get_mut(metric);
            measure.rank = Some(rank);
            measure.score = Some(round2(score));
        }
    }

    let weights = preset.weights();
    for row in rows.iter_mut().filter(|row| row.ranked) {
        let (weighted, total) = METRICS
            .iter()
            .zip(weights)
            .filter_map(|(metric, weight)| row.metrics.get(*metric).score.map(|score| (score * weight, weight)))
            .fold((0.0, 0.0), |(sum, total), (part, weight)| (sum + part, total + weight));
        row.score = (total > 0.0).then(|| round2(weighted / total));
    }

    rows.sort_by(|a, b| {
        b.ranked
            .cmp(&a.ranked)
            .then_with(|| b.score.unwrap_or(-1.0).total_cmp(&a.score.unwrap_or(-1.0)))
            .then_with(|| b.eligible.cmp(&a.eligible))
            .then_with(|| a.provider.cmp(&b.provider))
            .then_with(|| a.model.cmp(&b.model))
    });
    for (position, row) in rows.iter_mut().filter(|row| row.ranked).enumerate() {
        row.rank = Some(position as u64 + 1);
    }
    (currency, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(provider: &str, model: &str, eligible: u64, ok: u64) -> Counts {
        Counts {
            provider_key: provider.to_owned(),
            model_key: model.to_owned(),
            n_calls: eligible,
            n_eligible: eligible,
            n_ok: ok,
            attempts_sum: eligible,
            ..Counts::default()
        }
    }

    fn priced(mut counts: Counts, cost: f64, tokens: u64, currency: &str) -> Counts {
        counts.priced_n = counts.n_eligible;
        counts.priced_cost = cost;
        counts.priced_tok = tokens;
        counts.cur_any = currency.to_owned();
        counts.cur_n = 1;
        counts
    }

    fn timed(mut counts: Counts, p50: f64, p95: f64) -> Counts {
        counts.fb_n = counts.n_ok;
        counts.fb_q = vec![Some(p50), Some(p95)];
        counts
    }

    #[test]
    fn competition_ranks_share_a_place_and_score_from_best_to_worst() {
        assert_eq!(ranks(&[3.0, 1.0, 1.0, 2.0], false), vec![(4, 0.0), (1, 100.0), (1, 100.0), (3, 100.0 / 3.0)]);
        assert_eq!(ranks(&[0.5, 0.9], true), vec![(2, 0.0), (1, 100.0)]);
        assert_eq!(ranks(&[7.0], false), vec![(1, 100.0)]);
        assert!(ranks(&[], true).is_empty());
    }

    #[test]
    fn the_preset_weights_combine_the_metric_scores() {
        // a is more reliable, b is faster: the preset decides.
        let a = timed(group("p", "a", 10, 10), 200.0, 400.0);
        let b = timed(group("p", "b", 10, 8), 100.0, 200.0);
        let (_, reliability) = rank(vec![a.clone(), b.clone()], By::Model, Kind::Chat, Preset::Reliability, 1);
        assert_eq!(reliability[0].model.as_deref(), Some("a"));
        // a: success 100 × 0.60, both latencies 0 → 60 / 0.85.
        assert_eq!(reliability[0].score, Some(round2(60.0 / 0.85)));
        assert_eq!(reliability[0].rank, Some(1));

        let (_, speed) = rank(vec![a, b], By::Model, Kind::Chat, Preset::Speed, 1);
        assert_eq!(speed[0].model.as_deref(), Some("b"));
        assert_eq!(
            speed[0].metrics.first_byte_p50_ms,
            Measure { value: Some(100.0), rank: Some(1), score: Some(100.0) }
        );
        assert_eq!(speed[1].metrics.success_rate, Measure { value: Some(1.0), rank: Some(1), score: Some(100.0) });
    }

    #[test]
    fn groups_with_too_few_calls_are_listed_unranked_after_the_others() {
        let (_, rows) = rank(
            vec![group("p", "few", 3, 3), group("p", "many", 30, 15)],
            By::Model,
            Kind::Chat,
            Preset::Balanced,
            20,
        );
        assert_eq!(rows[0].model.as_deref(), Some("many"));
        assert_eq!((rows[0].rank, rows[0].ranked), (Some(1), true));
        assert_eq!((rows[1].rank, rows[1].score, rows[1].ranked), (None, None, false));
        assert_eq!(rows[1].metrics.success_rate, Measure { value: Some(1.0), rank: None, score: None });
    }

    #[test]
    fn equal_scores_fall_to_more_calls_then_to_the_name() {
        let (_, rows) = rank(
            vec![group("q", "m", 10, 10), group("p", "m", 10, 10), group("z", "m", 20, 20)],
            By::Model,
            Kind::Chat,
            Preset::Balanced,
            1,
        );
        let order: Vec<&str> = rows.iter().map(|row| row.provider.as_str()).collect();
        assert_eq!(order, ["z", "p", "q"]);
        assert_eq!(rows.iter().map(|row| row.rank).collect::<Vec<_>>(), [Some(1), Some(2), Some(3)]);
    }

    #[test]
    fn cost_is_ranked_in_the_reference_currency_only() {
        let usd_cheap = priced(group("p", "a", 10, 10), 1.0, 1_000_000, "USD");
        let usd_dear = priced(group("p", "b", 10, 10), 4.0, 1_000_000, "USD");
        let eur = priced(group("p", "c", 5, 5), 0.5, 1_000_000, "EUR");
        let mut mixed = priced(group("p", "d", 10, 10), 0.1, 1_000_000, "USD");
        mixed.cur_n = 2;
        let unpriced = group("p", "e", 10, 10);

        let (currency, rows) =
            rank(vec![usd_cheap, usd_dear, eur, mixed, unpriced], By::Model, Kind::Chat, Preset::Cost, 1);
        assert_eq!(currency.as_deref(), Some("USD"));
        let cost = |model: &str| rows.iter().find(|row| row.model.as_deref() == Some(model)).unwrap().metrics.cost;
        assert_eq!(cost("a"), Measure { value: Some(1.0), rank: Some(1), score: Some(100.0) });
        assert_eq!(cost("b"), Measure { value: Some(4.0), rank: Some(2), score: Some(0.0) });
        // Shown in its own currency, not ranked against dollars.
        assert_eq!(cost("c"), Measure { value: Some(0.5), rank: None, score: None });
        // A sum over two currencies means nothing.
        assert_eq!(cost("d"), Measure::default());
        // No price is not the cheapest price.
        assert_eq!(cost("e"), Measure::default());
    }

    #[test]
    fn speech_costs_per_call_and_has_no_output_speed() {
        let mut counts = priced(group("p", "voice", 4, 4), 2.0, 0, "USD");
        counts.stream_out_tok = 10;
        counts.stream_ms_sum = 100.0;
        let row = measure(&counts, By::Model, Kind::Speech);
        assert_eq!(row.metrics.cost.value, Some(0.5));
        assert_eq!(row.metrics.output_ms_per_token.value, None);

        let chat = measure(&counts, By::Provider, Kind::Chat);
        assert_eq!(chat.metrics.output_ms_per_token.value, Some(10.0));
        assert_eq!(chat.info.output_tokens_per_second, Some(100.0));
        assert_eq!(chat.model, None);
    }

    #[test]
    fn every_preset_adds_up_to_one() {
        for preset in [Preset::Balanced, Preset::Speed, Preset::Cost, Preset::Reliability] {
            let total: f64 = preset.weights().iter().sum();
            assert!((total - 1.0).abs() < 1e-9, "{}: {total}", preset.name());
        }
    }
}
