//! ClickHouse over its HTTP interface.
//!
//! Three kinds of calls exist, each with its own settings:
//!
//! * [`ClickHouse::insert`] writes newline-delimited JSON rows into a table.
//! * [`ClickHouse::fetch`] runs a `SELECT` in read-only mode and decodes one JSON object per row.
//! * [`ClickHouse::command`] runs DDL and mutations.
//!
//! Values never become part of the SQL text. A value is registered with [`Binds`], which hands
//! back a `{name:Type}` placeholder for the SQL and sends the value as `param_name=` in the URL;
//! ClickHouse substitutes it after parsing the statement.

use std::time::Duration;

use bytes::Bytes;
use chrono::{DateTime, Utc};
use reqwest::header::HeaderMap;
use serde::de::DeserializeOwned;
use url::Url;
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// ClickHouse closes an idle HTTP connection after its `keep_alive_timeout` (10 or 30 seconds,
/// depending on the version). Dropping idle connections sooner keeps a request from being sent
/// on a connection the server is closing at that moment.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(5);
const QUERY_TIMEOUT: Duration = Duration::from_secs(65);
const INSERT_TIMEOUT: Duration = Duration::from_secs(120);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
const PING_TIMEOUT: Duration = Duration::from_secs(3);

/// Longest piece of a ClickHouse error message kept in an error value.
const MAX_ERROR_CHARS: usize = 400;

/// Settings of every `SELECT`.
const READ_SETTINGS: &[(&str, &str)] = &[
    // 64-bit integers as JSON numbers, time stamps as RFC 3339.
    ("output_format_json_quote_64bit_integers", "0"),
    ("date_time_output_format", "iso"),
    // FINAL only has to merge within a partition: copies of a row share the sorting key, the
    // partition key is derived from it, so copies are never spread over partitions.
    ("do_not_merge_across_partitions_select_final", "1"),
    // Safe with FINAL here because copies of a row are identical.
    ("use_skip_indexes_if_final", "1"),
    // Send nothing before the query has finished, so a failure is an HTTP error and never a
    // truncated result.
    ("wait_end_of_query", "1"),
    ("max_execution_time", "60"),
    // Reports cannot write, whatever the SQL says.
    ("readonly", "2"),
];

/// Settings of every `INSERT`.
const INSERT_SETTINGS: &[(&str, &str)] = &[
    // The insert must be on disk when ClickHouse answers, and deduplication by token does not
    // apply to asynchronous inserts.
    ("async_insert", "0"),
    ("insert_deduplicate", "1"),
    ("date_time_input_format", "best_effort"),
    // A float is stored with the bits it was sent with. ClickHouse 26.8 parses exactly even
    // without this; the setting pins that behaviour for versions whose default parser is the
    // faster, approximate one.
    ("precise_float_parsing", "1"),
    // A field without a column is an error, not silently dropped data.
    ("input_format_skip_unknown_fields", "0"),
];

#[derive(Debug, thiserror::Error)]
pub enum ClickHouseError {
    /// No answer: connection refused, time-out, broken connection.
    #[error("ClickHouse is unreachable: {0}")]
    Transport(String),
    /// ClickHouse answered with an error.
    #[error("ClickHouse error {code} (HTTP {status}): {message}")]
    Server {
        status: u16,
        /// ClickHouse's own error code, zero when the answer carried none.
        code: i32,
        message: String,
    },
    /// The answer was not what the query promised.
    #[error("unexpected ClickHouse output: {0}")]
    Decode(String),
}

impl ClickHouseError {
    fn transport(error: reqwest::Error) -> Self {
        // The URL carries the bound values; keep it out of the message. The causes below the
        // top-level error say what actually went wrong (refused, timed out, reset).
        let error = error.without_url();
        let mut message = error.to_string();
        let mut cause = std::error::Error::source(&error);
        while let Some(inner) = cause {
            message.push_str(": ");
            message.push_str(&inner.to_string());
            cause = inner.source();
        }
        Self::Transport(message)
    }
}

/// The values of one statement. See the module documentation.
#[derive(Debug, Clone, Default)]
pub struct Binds {
    params: Vec<(String, String)>,
}

impl Binds {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, kind: &str, value: String) -> String {
        let name = format!("p{}", self.params.len());
        let placeholder = format!("{{{name}:{kind}}}");
        self.params.push((format!("param_{name}"), value));
        placeholder
    }

    pub fn string(&mut self, value: &str) -> String {
        self.push("String", escape(value))
    }

    pub fn int64(&mut self, value: i64) -> String {
        self.push("Int64", value.to_string())
    }

    pub fn uint64(&mut self, value: u64) -> String {
        self.push("UInt64", value.to_string())
    }

    /// A list of strings, as an `Array(String)`.
    pub fn strings(&mut self, values: &[String]) -> String {
        let quoted: Vec<String> =
            values.iter().map(|v| format!("'{}'", v.replace('\\', "\\\\").replace('\'', "\\'"))).collect();
        self.push("Array(String)", escape(&format!("[{}]", quoted.join(","))))
    }

    pub fn uuid(&mut self, value: Uuid) -> String {
        self.push("UUID", value.to_string())
    }

    /// An instant, as a `DateTime64(9, 'UTC')` expression. Instants outside the range of a
    /// 64-bit nanosecond count are clamped to it.
    pub fn timestamp(&mut self, value: DateTime<Utc>) -> String {
        let nanos = value.timestamp_nanos_opt().unwrap_or(if value.timestamp() < 0 { i64::MIN } else { i64::MAX });
        format!("fromUnixTimestamp64Nano({}, 'UTC')", self.int64(nanos))
    }

    fn as_pairs(&self) -> &[(String, String)] {
        &self.params
    }
}

/// ClickHouse reads a parameter value in its tab-separated "escaped" form, where a backslash
/// starts an escape sequence and a raw tab or line break ends the value.
fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            other => out.push(other),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct ClickHouse {
    http: reqwest::Client,
    endpoint: Url,
    database: String,
    user: Option<String>,
    password: Option<String>,
}

impl ClickHouse {
    /// `database` must be a plain identifier; `Config::validate` checks that.
    pub fn new(url: &str, database: &str, user: Option<&str>, password: Option<&str>) -> anyhow::Result<Self> {
        let endpoint =
            Url::parse(url).map_err(|error| anyhow::anyhow!("CLICKHOUSE_URL is not a URL ({error}): {url}"))?;
        if !matches!(endpoint.scheme(), "http" | "https") {
            anyhow::bail!("CLICKHOUSE_URL must start with http:// or https://");
        }
        let http =
            reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT).pool_idle_timeout(POOL_IDLE_TIMEOUT).build()?;
        Ok(Self {
            http,
            endpoint,
            database: database.to_owned(),
            user: user.map(str::to_owned),
            password: password.map(str::to_owned),
        })
    }

    pub fn database(&self) -> &str {
        &self.database
    }

    fn request(&self, in_database: bool, timeout: Duration) -> reqwest::RequestBuilder {
        let mut request = self.http.post(self.endpoint.clone()).timeout(timeout);
        if in_database {
            request = request.query(&[("database", self.database.as_str())]);
        }
        if self.user.is_some() || self.password.is_some() {
            request = request
                .header("X-ClickHouse-User", self.user.as_deref().unwrap_or("default"))
                .header("X-ClickHouse-Key", self.password.as_deref().unwrap_or(""));
        }
        request
    }

    async fn send(request: reqwest::RequestBuilder) -> Result<reqwest::Response, ClickHouseError> {
        let response = request.send().await.map_err(ClickHouseError::transport)?;
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let code = exception_code(response.headers());
        let text = response.text().await.unwrap_or_default();
        Err(ClickHouseError::Server {
            status: status.as_u16(),
            code,
            message: text.trim().chars().take(MAX_ERROR_CHARS).collect(),
        })
    }

    /// Creates the configured database when it does not exist.
    pub async fn create_database(&self) -> Result<(), ClickHouseError> {
        let sql = format!("CREATE DATABASE IF NOT EXISTS `{}`", self.database);
        Self::send(self.request(false, COMMAND_TIMEOUT).body(sql)).await?;
        Ok(())
    }

    /// Runs a statement that returns no rows: DDL or a mutation.
    pub async fn command(&self, sql: &str, binds: &Binds, settings: &[(&str, &str)]) -> Result<(), ClickHouseError> {
        let request = self.request(true, COMMAND_TIMEOUT).query(settings).query(binds.as_pairs()).body(sql.to_owned());
        Self::send(request).await?;
        Ok(())
    }

    /// Runs a `SELECT` and decodes every row. The statement must not carry a `FORMAT` clause.
    pub async fn fetch<T: DeserializeOwned>(&self, sql: &str, binds: &Binds) -> Result<Vec<T>, ClickHouseError> {
        self.fetch_within(sql, binds, QUERY_TIMEOUT).await
    }

    async fn fetch_within<T: DeserializeOwned>(
        &self,
        sql: &str,
        binds: &Binds,
        timeout: Duration,
    ) -> Result<Vec<T>, ClickHouseError> {
        let request = self
            .request(true, timeout)
            .query(READ_SETTINGS)
            .query(binds.as_pairs())
            .body(format!("{sql}\nFORMAT JSONEachRow"));
        let body = Self::send(request).await?.bytes().await.map_err(ClickHouseError::transport)?;
        body.split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .enumerate()
            .map(|(index, line)| {
                serde_json::from_slice(line)
                    .map_err(|error| ClickHouseError::Decode(format!("row {}: {error}", index + 1)))
            })
            .collect()
    }

    /// Runs a `SELECT` that returns exactly one row, such as an aggregate without `GROUP BY`.
    pub async fn fetch_one<T: DeserializeOwned>(&self, sql: &str, binds: &Binds) -> Result<T, ClickHouseError> {
        let mut rows = self.fetch(sql, binds).await?;
        match rows.len() {
            1 => Ok(rows.remove(0)),
            other => Err(ClickHouseError::Decode(format!("expected one row, got {other}"))),
        }
    }

    /// Like [`fetch_one`](Self::fetch_one) with a short time-out, for the readiness probe.
    pub async fn probe<T: DeserializeOwned>(&self, sql: &str) -> Result<T, ClickHouseError> {
        let mut rows = self.fetch_within(sql, &Binds::new(), PING_TIMEOUT).await?;
        match rows.len() {
            1 => Ok(rows.remove(0)),
            other => Err(ClickHouseError::Decode(format!("expected one row, got {other}"))),
        }
    }

    /// Inserts newline-delimited JSON rows. Returns once ClickHouse has written them.
    ///
    /// `dedup_token` identifies the batch: ClickHouse drops an insert whose token it has seen
    /// among the table's recent inserts, so a retried batch is stored once.
    pub async fn insert(&self, table: &'static str, rows: Bytes, dedup_token: &str) -> Result<(), ClickHouseError> {
        let query = format!("INSERT INTO {table} FORMAT JSONEachRow");
        let request = self
            .request(true, INSERT_TIMEOUT)
            .query(&[("query", query.as_str()), ("insert_deduplication_token", dedup_token)])
            .query(INSERT_SETTINGS)
            .body(rows);
        Self::send(request).await?;
        Ok(())
    }
}

fn exception_code(headers: &HeaderMap) -> i32 {
    headers
        .get("x-clickhouse-exception-code")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_numbered_and_values_travel_separately() {
        let mut binds = Binds::new();
        assert_eq!(binds.string("a"), "{p0:String}");
        assert_eq!(binds.int64(-5), "{p1:Int64}");
        assert_eq!(binds.uint64(7), "{p2:UInt64}");
        assert_eq!(binds.uuid(Uuid::nil()), "{p3:UUID}");
        assert_eq!(
            binds.as_pairs(),
            &[
                ("param_p0".to_owned(), "a".to_owned()),
                ("param_p1".to_owned(), "-5".to_owned()),
                ("param_p2".to_owned(), "7".to_owned()),
                ("param_p3".to_owned(), "00000000-0000-0000-0000-000000000000".to_owned()),
            ]
        );
    }

    #[test]
    fn a_hostile_value_never_reaches_the_sql_text() {
        let mut binds = Binds::new();
        let placeholder = binds.string("x' OR 1=1; DROP TABLE usage_events --");
        assert_eq!(placeholder, "{p0:String}");
    }

    #[test]
    fn values_are_escaped_for_the_parameter_format() {
        assert_eq!(escape("plain 'quoted' \"text\""), "plain 'quoted' \"text\"");
        assert_eq!(escape("a\\b"), "a\\\\b");
        assert_eq!(escape("a\tb\nc\rd\0e"), "a\\tb\\nc\\rd\\0e");
        assert_eq!(escape("\u{8}\u{c}"), "\\b\\f");
    }

    #[test]
    fn instants_are_bound_as_nanoseconds() {
        let mut binds = Binds::new();
        let instant =
            DateTime::parse_from_rfc3339("2026-03-12T10:30:00.000000001Z").expect("valid").with_timezone(&Utc);
        assert_eq!(binds.timestamp(instant), "fromUnixTimestamp64Nano({p0:Int64}, 'UTC')");
        assert_eq!(binds.as_pairs()[0].1, "1773311400000000001");
    }

    #[test]
    fn only_http_urls_are_accepted() {
        assert!(ClickHouse::new("http://localhost:8123", "db", None, None).is_ok());
        assert!(ClickHouse::new("https://ch.internal:8443", "db", Some("u"), Some("p")).is_ok());
        assert!(ClickHouse::new("localhost:8123", "db", None, None).is_err());
        assert!(ClickHouse::new("not a url", "db", None, None).is_err());
    }
}
