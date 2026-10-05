//! Ingest: the core ships batches of usage events and audit records (concept section 14.3).
//!
//! A batch is newline-delimited JSON, one record per line. Every line is parsed into the
//! contract struct first, so a batch is stored whole or not at all, and the rows written are
//! built from the parsed records rather than passed through.
//!
//! The answer is `200` only after ClickHouse has written the batch. The core then advances its
//! checkpoint. Any ClickHouse failure is a `503`, and the core sends the same batch again with
//! the same batch id; ClickHouse drops the copy by that id.
//!
//! Bodies are never logged, and neither is anything derived from them: a parse error quotes the
//! offending value, so the log carries only the line number.

use std::time::Instant;

use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::CONTENT_ENCODING;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use bytes::Bytes;
use pacod_gw_events::ingest::{AUDIT_PATH, BATCH_ID_HEADER, IngestReply, USAGE_PATH};
use pacod_gw_events::{AuditRecord, UsageEvent};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::AppState;
use crate::error::ApiError;
use crate::storage::ClickHouseError;
use crate::storage::rows::{AUDIT_TABLE, AuditRow, USAGE_TABLE};

/// Longest batch id accepted.
const MAX_BATCH_ID_BYTES: usize = 200;

/// Longest parse error passed back to the core.
const MAX_PARSE_ERROR_CHARS: usize = 300;

pub fn routes(max_batch_bytes: usize) -> Router<AppState> {
    Router::new()
        .route(USAGE_PATH, post(usage))
        .route(AUDIT_PATH, post(audit))
        .layer(DefaultBodyLimit::max(max_batch_bytes))
}

async fn usage(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<IngestReply>, ApiError> {
    store(&state, USAGE_TABLE, &headers, body, Ok::<UsageEvent, _>).await
}

async fn audit(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> Result<Json<IngestReply>, ApiError> {
    store(&state, AUDIT_TABLE, &headers, body, |record: AuditRecord| AuditRow::from_record(record)).await
}

/// Parses a batch of `T`, turns every record into its row and inserts the rows under the batch
/// id.
async fn store<T, R>(
    state: &AppState,
    table: &'static str,
    headers: &HeaderMap,
    body: Result<Bytes, BytesRejection>,
    to_row: impl Fn(T) -> serde_json::Result<R>,
) -> Result<Json<IngestReply>, ApiError>
where
    T: DeserializeOwned,
    R: Serialize,
{
    let started = Instant::now();
    let batch_id = batch_id(headers)?;
    if let Some(encoding) = headers.get(CONTENT_ENCODING)
        && encoding.as_bytes() != b"identity"
    {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_encoding",
            "the batch body must not be compressed",
        ));
    }
    let body = body.map_err(|rejection| {
        if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "batch_too_large",
                "the batch is larger than this service accepts; send smaller batches",
            )
        } else {
            ApiError::bad_request("unreadable_body", "the batch body could not be read")
        }
    })?;
    let bytes = body.len();

    let records: Vec<T> = parse_batch(&body).inspect_err(|error| {
        tracing::warn!(table, batch_id, bytes, line = error.line, "batch rejected: malformed line");
    })?;
    drop(body);
    let rows = records.len();

    if rows > 0 {
        let payload = encode_rows(records, to_row).map_err(|_| {
            tracing::error!(table, batch_id, "a record could not be written as a row");
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "encoding_failed",
                "a record could not be written as a row",
            )
        })?;
        state
            .clickhouse()
            .insert(table, payload, batch_id)
            .await
            .map_err(|error| insert_failed(table, batch_id, rows, &error))?;
    }

    tracing::info!(table, batch_id, rows, bytes, elapsed_ms = started.elapsed().as_secs_f64() * 1000.0, "batch stored");
    Ok(Json(IngestReply { batch_id: batch_id.to_owned(), rows: rows as u64 }))
}

/// The batch id becomes ClickHouse's deduplication token, so it must be present and tame.
fn batch_id(headers: &HeaderMap) -> Result<&str, ApiError> {
    let invalid = |problem: &str| ApiError::bad_request("invalid_batch_id", format!("{BATCH_ID_HEADER}: {problem}"));
    let value = headers.get(BATCH_ID_HEADER).ok_or_else(|| invalid("the header is required"))?;
    let value = value.to_str().map_err(|_| invalid("only printable ASCII is allowed"))?;
    if value.is_empty() || value.len() > MAX_BATCH_ID_BYTES {
        return Err(invalid("must be 1 to 200 characters long"));
    }
    if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(invalid("only printable ASCII without spaces is allowed"));
    }
    Ok(value)
}

/// One record per line. Blank lines are skipped; line numbers count every line, from one.
fn parse_batch<T: DeserializeOwned>(body: &[u8]) -> Result<Vec<T>, ApiError> {
    let mut records = Vec::new();
    for (index, line) in body.split(|byte| *byte == b'\n').enumerate() {
        let line = line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_slice(line) {
            Ok(record) => records.push(record),
            Err(error) => {
                let number = index + 1;
                let problem: String = error.to_string().chars().take(MAX_PARSE_ERROR_CHARS).collect();
                let mut rejection = ApiError::bad_request("malformed_line", format!("line {number}: {problem}"));
                rejection.line = Some(number);
                return Err(rejection);
            }
        }
    }
    Ok(records)
}

fn encode_rows<T, R: Serialize>(
    records: Vec<T>,
    to_row: impl Fn(T) -> serde_json::Result<R>,
) -> serde_json::Result<Bytes> {
    let mut payload = Vec::new();
    for record in records {
        serde_json::to_writer(&mut payload, &to_row(record)?)?;
        payload.push(b'\n');
    }
    Ok(Bytes::from(payload))
}

/// ClickHouse quotes the data it could not read in its error message, so of a rejected insert
/// only the error code is logged.
fn insert_failed(table: &str, batch_id: &str, rows: usize, error: &ClickHouseError) -> ApiError {
    match error {
        ClickHouseError::Server { status, code, .. } => tracing::error!(
            table,
            batch_id,
            rows,
            clickhouse_status = status,
            clickhouse_code = code,
            "insert rejected by ClickHouse"
        ),
        other => tracing::error!(table, batch_id, rows, error = %other, "insert failed"),
    }
    ApiError::storage_unavailable()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Item {
        n: u32,
    }

    #[test]
    fn lines_are_parsed_and_blank_lines_skipped() {
        let items: Vec<Item> = parse_batch(b"{\"n\":1}\n\n{\"n\":2}\r\n  \n{\"n\":3}").expect("ok");
        assert_eq!(items, vec![Item { n: 1 }, Item { n: 2 }, Item { n: 3 }]);

        let none: Vec<Item> = parse_batch(b"").expect("ok");
        assert!(none.is_empty());
        let none: Vec<Item> = parse_batch(b"\n\n").expect("ok");
        assert!(none.is_empty());
    }

    #[test]
    fn a_malformed_line_is_named_by_its_number() {
        let error = parse_batch::<Item>(b"{\"n\":1}\n\n{\"n\":\"x\"}\n{\"n\":4}").unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.code, "malformed_line");
        assert_eq!(error.line, Some(3));
        assert!(error.message.starts_with("line 3: "), "{}", error.message);
    }

    #[test]
    fn a_long_parse_error_is_cut() {
        let long = format!("{{\"n\":\"{}\"}}", "x".repeat(5_000));
        let error = parse_batch::<Item>(long.as_bytes()).unwrap_err();
        assert!(error.message.chars().count() <= MAX_PARSE_ERROR_CHARS + "line 1: ".len());
    }

    #[test]
    fn rows_are_written_one_per_line() {
        let payload = encode_rows(vec![1u32, 2], |n| Ok(serde_json::json!({ "n": n }))).expect("ok");
        assert_eq!(&payload[..], b"{\"n\":1}\n{\"n\":2}\n");
    }

    #[test]
    fn the_batch_id_is_required_and_restricted() {
        let with = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(BATCH_ID_HEADER, HeaderValue::from_static(value));
            headers
        };
        assert_eq!(batch_id(&with("i-1:000042")).expect("ok"), "i-1:000042");
        assert!(batch_id(&HeaderMap::new()).is_err());
        assert!(batch_id(&with("")).is_err());
        assert!(batch_id(&with("has space")).is_err());

        let mut long = HeaderMap::new();
        long.insert(BATCH_ID_HEADER, HeaderValue::from_str(&"a".repeat(MAX_BATCH_ID_BYTES + 1)).expect("header"));
        assert!(batch_id(&long).is_err());
    }
}
