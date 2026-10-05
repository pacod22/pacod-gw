//! Query-string parsing shared by the reports.
//!
//! Three rules hold for every report:
//!
//! * A parameter the endpoint does not know is an error. A misspelt scope filter must never be
//!   dropped silently, because the report would then cover more than the caller may see.
//! * Filter values only ever reach ClickHouse as bound parameters ([`Binds`]).
//! * Anything that becomes an identifier or an expression in the SQL (a group-by column, a
//!   bucket) is picked from a fixed list; the request only selects the entry.

use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use uuid::Uuid;

use crate::error::ApiError;
use crate::storage::Binds;

/// The parameters of one request. Every value must be taken before [`Params::finish`].
pub struct Params {
    pairs: Vec<Pair>,
}

struct Pair {
    name: String,
    value: String,
    taken: bool,
}

impl Params {
    pub fn parse(query: Option<&str>) -> Self {
        let pairs = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
            .map(|(name, value)| Pair { name: name.into_owned(), value: value.into_owned(), taken: false })
            .collect();
        Self { pairs }
    }

    /// Every value given for `name`, in order.
    pub fn all(&mut self, name: &str) -> Vec<String> {
        self.pairs
            .iter_mut()
            .filter(|pair| pair.name == name)
            .map(|pair| {
                pair.taken = true;
                pair.value.clone()
            })
            .collect()
    }

    /// The value of a parameter that may be given once.
    pub fn one(&mut self, name: &str) -> Result<Option<String>, ApiError> {
        let mut values = self.all(name);
        if values.len() > 1 {
            return Err(ApiError::invalid_parameter(name, "given more than once"));
        }
        Ok(values.pop())
    }

    pub fn parsed<T: FromStr>(&mut self, name: &str, expected: &str) -> Result<Option<T>, ApiError> {
        self.one(name)?
            .map(|value| value.parse().map_err(|_| ApiError::invalid_parameter(name, format!("expected {expected}"))))
            .transpose()
    }

    pub fn time(&mut self, name: &str) -> Result<Option<DateTime<Utc>>, ApiError> {
        self.one(name)?.map(|value| parse_time(name, &value)).transpose()
    }

    /// `from` and `to`. Without `to` the range ends now; without `from` it starts 24 hours
    /// before its end.
    pub fn range(&mut self) -> Result<Range, ApiError> {
        let to = self.time("to")?.unwrap_or_else(Utc::now);
        let from = self.time("from")?.unwrap_or(to - Duration::hours(24));
        if from >= to {
            return Err(ApiError::invalid_parameter("from", "must be earlier than `to`"));
        }
        Ok(Range { from, to })
    }

    /// `from` and `to` for lookups by id, where no bound is the default.
    pub fn optional_range(&mut self) -> Result<OpenRange, ApiError> {
        let from = self.time("from")?;
        let to = self.time("to")?;
        if let (Some(from), Some(to)) = (from, to)
            && from >= to
        {
            return Err(ApiError::invalid_parameter("from", "must be earlier than `to`"));
        }
        Ok(OpenRange { from, to })
    }

    /// `limit`, between 1 and `max`.
    pub fn limit(&mut self, default: u64, max: u64) -> Result<u64, ApiError> {
        let limit = self.parsed::<u64>("limit", "a whole number")?.unwrap_or(default);
        if limit == 0 || limit > max {
            return Err(ApiError::invalid_parameter("limit", format!("must be between 1 and {max}")));
        }
        Ok(limit)
    }

    pub fn format(&mut self) -> Result<Format, ApiError> {
        match self.one("format")?.as_deref() {
            None | Some("json") => Ok(Format::Json),
            Some("csv") => Ok(Format::Csv),
            Some(_) => Err(ApiError::invalid_parameter("format", "expected json or csv")),
        }
    }

    /// `true` or `false`.
    pub fn flag(&mut self, name: &str) -> Result<Option<bool>, ApiError> {
        match self.one(name)?.as_deref() {
            None => Ok(None),
            Some("true") => Ok(Some(true)),
            Some("false") => Ok(Some(false)),
            Some(_) => Err(ApiError::invalid_parameter(name, "expected true or false")),
        }
    }

    /// The `tag.<key>=<value>` parameters, grouped by key in order of first appearance.
    fn tags(&mut self) -> Result<Vec<(String, Vec<String>)>, ApiError> {
        let mut tags: Vec<(String, Vec<String>)> = Vec::new();
        for pair in &mut self.pairs {
            let Some(key) = pair.name.strip_prefix("tag.") else {
                continue;
            };
            pair.taken = true;
            if key.is_empty() {
                return Err(ApiError::invalid_parameter("tag.", "the tag key is missing"));
            }
            match tags.iter_mut().find(|(existing, _)| existing == key) {
                Some((_, values)) => values.push(pair.value.clone()),
                None => tags.push((key.to_owned(), vec![pair.value.clone()])),
            }
        }
        Ok(tags)
    }

    /// Fails when a parameter was given that the endpoint did not ask for.
    pub fn finish(self) -> Result<(), ApiError> {
        match self.pairs.iter().find(|pair| !pair.taken) {
            None => Ok(()),
            Some(pair) => Err(ApiError::bad_request(
                "unknown_parameter",
                format!("`{}` is not a parameter of this endpoint", pair.name.chars().take(80).collect::<String>()),
            )),
        }
    }
}

/// RFC 3339, within the years ClickHouse's `DateTime64(9)` can hold.
fn parse_time(name: &str, value: &str) -> Result<DateTime<Utc>, ApiError> {
    let time = DateTime::parse_from_rfc3339(value)
        .map_err(|_| {
            ApiError::invalid_parameter(
                name,
                "expected an RFC 3339 time such as 2026-03-12T10:30:00Z \
                 (a `+` in an offset must be sent as %2B)",
            )
        })?
        .with_timezone(&Utc);
    if !(1970..=2261).contains(&time.year()) {
        return Err(ApiError::invalid_parameter(name, "must lie between the years 1970 and 2261"));
    }
    Ok(time)
}

/// A half-open time range: `from <= timestamp < to`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// A time range in which either bound may be missing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpenRange {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Format {
    Json,
    Csv,
}

/// Which equality filters a table supports: request parameter and the column it compares.
pub struct FilterSpec {
    pub columns: &'static [(&'static str, &'static str)],
    /// Whether `tag.<key>` filters apply (the table has a `tags` column).
    pub tags: bool,
}

/// The filters of the reports on `usage_events`.
pub const USAGE_FILTERS: FilterSpec = FilterSpec {
    columns: &[
        ("organisation_id", "organisation_id"),
        ("workspace_id", "workspace_id"),
        ("provider", "provider"),
        ("model", "model_served"),
        ("template", "template"),
        ("key_id", "key_id"),
        ("end_user", "end_user"),
        ("status", "status"),
        ("mode", "mode"),
    ],
    tags: true,
};

/// Lookups by id are restricted by scope only.
pub const USAGE_SCOPE: FilterSpec =
    FilterSpec { columns: &[("organisation_id", "organisation_id"), ("workspace_id", "workspace_id")], tags: false };

/// The filters of the audit list: the columns an audit record carries.
pub const AUDIT_FILTERS: FilterSpec = FilterSpec {
    columns: &[
        ("organisation_id", "organisation_id"),
        ("workspace_id", "workspace_id"),
        ("model", "model"),
        ("template", "template"),
        ("key_id", "key_id"),
        ("end_user", "end_user"),
        ("mode", "mode"),
        ("session_id", "session_id"),
        ("trace_id", "trace_id"),
    ],
    tags: false,
};

pub const AUDIT_SCOPE: FilterSpec = USAGE_SCOPE;

/// The conditions of one query. Rendered into a `WHERE` clause with bound values.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    /// Column and the values it may have.
    equals: Vec<(&'static str, Vec<String>)>,
    /// Tag key and the values it may have.
    tags: Vec<(String, Vec<String>)>,
    /// Fixed conditions added by a report. Never built from request data.
    fixed: Vec<&'static str>,
}

impl Filters {
    /// Takes the filters `spec` allows from the request. A filter given several times matches
    /// any of its values.
    pub fn take(params: &mut Params, spec: &FilterSpec) -> Result<Self, ApiError> {
        let mut filters = Self::default();
        for (name, column) in spec.columns {
            let values = params.all(name);
            if !values.is_empty() {
                filters.equals.push((column, values));
            }
        }
        if spec.tags {
            filters.tags = params.tags()?;
        }
        Ok(filters)
    }

    pub fn within(mut self, range: Range) -> Self {
        self.from = Some(range.from);
        self.to = Some(range.to);
        self
    }

    pub fn between(mut self, range: OpenRange) -> Self {
        self.from = range.from;
        self.to = range.to;
        self
    }

    /// Adds `column = value`.
    pub fn equal(mut self, column: &'static str, value: impl Into<String>) -> Self {
        self.equals.push((column, vec![value.into()]));
        self
    }

    /// Adds a condition written in this crate, such as `status != 'ok'`.
    pub fn and(mut self, condition: &'static str) -> Self {
        self.fixed.push(condition);
        self
    }

    fn conditions(&self, binds: &mut Binds) -> Vec<String> {
        let mut conditions = Vec::new();
        if let Some(from) = self.from {
            conditions.push(format!("timestamp >= {}", binds.timestamp(from)));
        }
        if let Some(to) = self.to {
            conditions.push(format!("timestamp < {}", binds.timestamp(to)));
        }
        for (column, values) in &self.equals {
            conditions.push(any_of(column, values, binds));
        }
        for (key, values) in &self.tags {
            let tag = format!("tags[{}]", binds.string(key));
            conditions.push(any_of(&tag, values, binds));
        }
        conditions.extend(self.fixed.iter().map(|condition| (*condition).to_owned()));
        conditions
    }

    /// The conditions joined with `AND`, for a statement that brings its own `WHERE`.
    /// Empty when there is no condition.
    pub fn condition_sql(&self, binds: &mut Binds) -> String {
        self.conditions(binds).join(" AND ")
    }

    /// `WHERE ...`, or nothing when there is no condition.
    pub fn where_sql(&self, binds: &mut Binds) -> String {
        self.where_with(binds, None)
    }

    /// Like [`where_sql`](Self::where_sql) with one more condition whose values the caller has
    /// already bound, such as the position of a cursor.
    pub fn where_with(&self, binds: &mut Binds, extra: Option<String>) -> String {
        let mut conditions = self.conditions(binds);
        conditions.extend(extra);
        if conditions.is_empty() { String::new() } else { format!("WHERE {}", conditions.join(" AND ")) }
    }
}

fn any_of(expression: &str, values: &[String], binds: &mut Binds) -> String {
    match values {
        [value] => format!("{expression} = {}", binds.string(value)),
        values => {
            let list: Vec<String> = values.iter().map(|value| binds.string(value)).collect();
            format!("{expression} IN ({})", list.join(", "))
        }
    }
}

/// The width of a time bucket. Buckets are aligned to UTC; weeks start on Monday.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Bucket {
    Hour,
    Day,
    Week,
    Month,
}

impl Bucket {
    pub fn take(params: &mut Params, default: Self) -> Result<Self, ApiError> {
        match params.one("bucket")?.as_deref() {
            None => Ok(default),
            Some("hour") => Ok(Self::Hour),
            Some("day") => Ok(Self::Day),
            Some("week") => Ok(Self::Week),
            Some("month") => Ok(Self::Month),
            Some(_) => Err(ApiError::invalid_parameter("bucket", "expected hour, day, week or month")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    /// The start of the bucket a row falls into, as a `DateTime('UTC')` expression.
    pub fn sql(self) -> &'static str {
        match self {
            Self::Hour => "toStartOfHour(timestamp)",
            Self::Day => "toStartOfDay(timestamp)",
            Self::Week => "toDateTime(toStartOfWeek(timestamp, 1), 'UTC')",
            Self::Month => "toDateTime(toStartOfMonth(timestamp), 'UTC')",
        }
    }

    /// The start of the bucket that contains `time`. Matches [`sql`](Self::sql).
    pub fn start_of(self, time: DateTime<Utc>) -> DateTime<Utc> {
        let date = time.date_naive();
        let midnight = |date: chrono::NaiveDate| Utc.from_utc_datetime(&date.and_time(chrono::NaiveTime::MIN));
        match self {
            Self::Hour => {
                let seconds = time.timestamp();
                Utc.timestamp_opt(seconds - seconds.rem_euclid(3600), 0).single().unwrap_or(time)
            }
            Self::Day => midnight(date),
            Self::Week => {
                let since_monday = i64::from(date.weekday().num_days_from_monday());
                midnight(date - Duration::days(since_monday))
            }
            Self::Month => midnight(date.with_day(1).unwrap_or(date)),
        }
    }
}

/// What a report groups by.
#[derive(Debug, Clone, PartialEq)]
pub enum Group {
    None,
    Column { name: &'static str, column: &'static str },
    Tag(String),
}

/// Request value and the column it selects.
pub type GroupColumns = &'static [(&'static str, &'static str)];

impl Group {
    /// Reads the grouping from parameter `name`. `allowed` is the complete list of columns the
    /// report may group by; `tag:<key>` is accepted when `tags` is set.
    pub fn take(
        params: &mut Params,
        name: &str,
        allowed: GroupColumns,
        tags: bool,
        default: Option<Self>,
    ) -> Result<Self, ApiError> {
        let Some(value) = params.one(name)? else {
            return default.ok_or_else(|| ApiError::invalid_parameter(name, "the parameter is required"));
        };
        if value == "none" && default == Some(Self::None) {
            return Ok(Self::None);
        }
        if let Some(&(choice, column)) = allowed.iter().find(|(choice, _)| *choice == value) {
            return Ok(Self::Column { name: choice, column });
        }
        if tags && let Some(key) = value.strip_prefix("tag:") {
            if key.is_empty() {
                return Err(ApiError::invalid_parameter(name, "the tag key is missing"));
            }
            return Ok(Self::Tag(key.to_owned()));
        }
        let mut choices: Vec<&str> = Vec::new();
        if default == Some(Self::None) {
            choices.push("none");
        }
        choices.extend(allowed.iter().map(|(allowed, _)| *allowed));
        if tags {
            choices.push("tag:<key>");
        }
        Err(ApiError::invalid_parameter(name, format!("expected one of {}", choices.join(", "))))
    }

    /// The grouping expression. A tag key is bound, never written into the SQL.
    pub fn sql(&self, binds: &mut Binds) -> String {
        match self {
            Self::None => "''".to_owned(),
            Self::Column { column, .. } => format!("toString({column})"),
            Self::Tag(key) => format!("tags[{}]", binds.string(key)),
        }
    }

    /// The value as the request spelt it, echoed in the response.
    pub fn label(&self) -> String {
        match self {
            Self::None => "none".to_owned(),
            Self::Column { name, .. } => (*name).to_owned(),
            Self::Tag(key) => format!("tag:{key}"),
        }
    }

    pub fn is_none(&self) -> bool {
        *self == Self::None
    }
}

/// The position after the last row of a page: its time stamp and its id.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    pub nanos: i64,
    pub id: String,
}

impl Cursor {
    pub fn take(params: &mut Params) -> Result<Option<Self>, ApiError> {
        let Some(value) = params.one("cursor")? else {
            return Ok(None);
        };
        let invalid = || ApiError::invalid_parameter("cursor", "not a cursor returned by this endpoint");
        let (nanos, id) = value.split_once(':').ok_or_else(invalid)?;
        let nanos = nanos.parse().map_err(|_| invalid())?;
        if id.is_empty() {
            return Err(invalid());
        }
        Ok(Some(Self { nanos, id: id.to_owned() }))
    }

    pub fn encode(timestamp: DateTime<Utc>, id: &str) -> String {
        format!("{}:{id}", timestamp.timestamp_nanos_opt().unwrap_or(i64::MAX))
    }

    /// `(timestamp, <id_column>) < (cursor)` for a table whose id is a UUID.
    pub fn before_uuid(&self, id_column: &'static str, binds: &mut Binds) -> Result<String, ApiError> {
        let id = Uuid::parse_str(&self.id)
            .map_err(|_| ApiError::invalid_parameter("cursor", "not a cursor returned by this endpoint"))?;
        Ok(format!(
            "(timestamp, {id_column}) < (fromUnixTimestamp64Nano({}, 'UTC'), {})",
            binds.int64(self.nanos),
            binds.uuid(id)
        ))
    }

    /// `(timestamp, <id_column>) < (cursor)` for a table whose id is a string.
    pub fn before_string(&self, id_column: &'static str, binds: &mut Binds) -> String {
        format!(
            "(timestamp, {id_column}) < (fromUnixTimestamp64Nano({}, 'UTC'), {})",
            binds.int64(self.nanos),
            binds.string(&self.id)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value).expect("valid time").with_timezone(&Utc)
    }

    #[test]
    fn unknown_parameters_are_rejected() {
        let mut params = Params::parse(Some("organisation_id=a&organization_id=b"));
        let _ = Filters::take(&mut params, &USAGE_FILTERS).expect("filters");
        let error = params.finish().unwrap_err();
        assert_eq!(error.code, "unknown_parameter");
        assert!(error.message.contains("organization_id"));
    }

    #[test]
    fn a_single_valued_parameter_may_not_repeat() {
        let mut params = Params::parse(Some("bucket=hour&bucket=day"));
        assert!(Bucket::take(&mut params, Bucket::Hour).is_err());
    }

    #[test]
    fn the_range_defaults_to_the_last_24_hours() {
        let mut params = Params::parse(None);
        let range = params.range().expect("range");
        assert_eq!(range.to - range.from, Duration::hours(24));
        assert!((Utc::now() - range.to) < Duration::seconds(5));

        let mut params = Params::parse(Some("to=2026-03-12T10:30:00Z"));
        let range = params.range().expect("range");
        assert_eq!(range.from, time("2026-03-11T10:30:00Z"));

        let mut params = Params::parse(Some("from=2026-03-12T10:30:00Z&to=2026-03-12T10:30:00Z"));
        assert!(params.range().is_err());
    }

    #[test]
    fn times_must_be_rfc_3339() {
        for bad in ["yesterday", "2026-03-12", "2026-03-12 10:30:00", "1500-01-01T00:00:00Z"] {
            let query = format!("from={bad}");
            let mut params = Params::parse(Some(&query));
            assert!(params.range().is_err(), "{bad}");
        }
        let mut params = Params::parse(Some("from=2026-03-12T12:30:00%2B02:00&to=2026-03-12T11:00:00Z"));
        assert_eq!(params.range().expect("range").from, time("2026-03-12T10:30:00Z"));
    }

    #[test]
    fn filters_render_as_bound_conditions() {
        let mut params =
            Params::parse(Some("organisation_id=org-a&model=m1&model=m2&tag.purpose=plan&tag.team=x&tag.purpose=chat"));
        let filters = Filters::take(&mut params, &USAGE_FILTERS)
            .expect("filters")
            .within(Range { from: time("2026-03-01T00:00:00Z"), to: time("2026-03-02T00:00:00Z") })
            .and("status != 'ok'");
        params.finish().expect("all known");

        let mut binds = Binds::new();
        assert_eq!(
            filters.where_sql(&mut binds),
            "WHERE timestamp >= fromUnixTimestamp64Nano({p0:Int64}, 'UTC') \
             AND timestamp < fromUnixTimestamp64Nano({p1:Int64}, 'UTC') \
             AND organisation_id = {p2:String} \
             AND model_served IN ({p3:String}, {p4:String}) \
             AND tags[{p5:String}] IN ({p6:String}, {p7:String}) \
             AND tags[{p8:String}] = {p9:String} \
             AND status != 'ok'"
        );
    }

    #[test]
    fn no_filter_means_no_where_clause() {
        let mut binds = Binds::new();
        assert_eq!(Filters::default().where_sql(&mut binds), "");
        assert_eq!(Filters::default().where_with(&mut binds, Some("x = 1".to_owned())), "WHERE x = 1");
    }

    #[test]
    fn tag_filters_are_refused_where_the_table_has_no_tags() {
        let mut params = Params::parse(Some("tag.purpose=plan"));
        let _ = Filters::take(&mut params, &AUDIT_FILTERS).expect("filters");
        assert_eq!(params.finish().unwrap_err().code, "unknown_parameter");
    }

    const GROUPS: GroupColumns = &[("provider", "provider"), ("model", "model_served")];

    #[test]
    fn group_by_only_selects_from_the_list() {
        let take = |query: &str| {
            let mut params = Params::parse(Some(query));
            Group::take(&mut params, "group_by", GROUPS, true, Some(Group::None))
        };
        assert_eq!(take("").expect("default"), Group::None);
        assert_eq!(take("group_by=none").expect("none"), Group::None);
        assert_eq!(take("group_by=model").expect("model"), Group::Column { name: "model", column: "model_served" });
        assert_eq!(take("group_by=tag:purpose").expect("tag"), Group::Tag("purpose".to_owned()));
        for bad in ["model_served", "provider, id", "status); DROP TABLE usage_events --", "tag:", "PROVIDER"] {
            let query = format!("group_by={bad}");
            let error = take(&query).unwrap_err();
            assert_eq!(error.code, "invalid_parameter", "{bad}");
        }
    }

    #[test]
    fn a_tag_key_is_bound_and_not_written_into_the_sql() {
        let mut binds = Binds::new();
        let group = Group::Tag("x'] OR 1=1 --".to_owned());
        assert_eq!(group.sql(&mut binds), "tags[{p0:String}]");
        assert_eq!(group.label(), "tag:x'] OR 1=1 --");
    }

    #[test]
    fn a_required_grouping_has_no_none() {
        let mut params = Params::parse(Some("by=none"));
        assert!(Group::take(&mut params, "by", GROUPS, true, None).is_err());
        let mut params = Params::parse(None);
        assert!(Group::take(&mut params, "by", GROUPS, true, None).is_err());
    }

    #[test]
    fn bucket_starts_are_aligned_to_utc_and_monday() {
        let at = time("2026-03-12T10:30:45.5Z");
        assert_eq!(Bucket::Hour.start_of(at), time("2026-03-12T10:00:00Z"));
        assert_eq!(Bucket::Day.start_of(at), time("2026-03-12T00:00:00Z"));
        assert_eq!(Bucket::Week.start_of(at), time("2026-03-09T00:00:00Z"));
        assert_eq!(Bucket::Month.start_of(at), time("2026-03-01T00:00:00Z"));
        // A Monday is the start of its own week.
        assert_eq!(Bucket::Week.start_of(time("2026-03-09T00:00:00Z")), time("2026-03-09T00:00:00Z"));
    }

    #[test]
    fn cursors_round_trip_and_reject_garbage() {
        let at = time("2026-03-12T10:30:00.000000001Z");
        let encoded = Cursor::encode(at, "0199a8c0-0000-7000-8000-000000000001");
        let query = format!("cursor={encoded}");
        let mut params = Params::parse(Some(&query));
        let cursor = Cursor::take(&mut params).expect("ok").expect("present");
        assert_eq!(cursor.nanos, at.timestamp_nanos_opt().expect("in range"));
        assert_eq!(cursor.id, "0199a8c0-0000-7000-8000-000000000001");

        let mut binds = Binds::new();
        assert_eq!(
            cursor.before_uuid("id", &mut binds).expect("uuid"),
            "(timestamp, id) < (fromUnixTimestamp64Nano({p0:Int64}, 'UTC'), {p1:UUID})"
        );

        for bad in ["abc", "12", "12:", ":x", "1.5:x"] {
            let query = format!("cursor={bad}");
            let mut params = Params::parse(Some(&query));
            assert!(Cursor::take(&mut params).is_err(), "{bad}");
        }

        let not_a_uuid = Cursor { nanos: 1, id: "x' OR 1=1".to_owned() };
        assert!(not_a_uuid.before_uuid("id", &mut Binds::new()).is_err());
    }

    #[test]
    fn limits_are_bounded() {
        let limit = |query: &str| Params::parse(Some(query)).limit(50, 1000);
        assert_eq!(limit("").expect("default"), 50);
        assert_eq!(limit("limit=1000").expect("max"), 1000);
        assert!(limit("limit=0").is_err());
        assert!(limit("limit=1001").is_err());
        assert!(limit("limit=-1").is_err());
        assert!(limit("limit=10;DROP").is_err());
    }
}
