//! The log, in the shared pacod convention (pacod-demo/docs/LOGGING.md): one event per line on
//! standard output, as JSON or as text.
//!
//! - `PID_LOG`: a level (`debug`, `info`, `warn`, `error`; any case, `warning` is `warn`) or an
//!   `EnvFilter` directive (`info,sqlx=warn`, the default). A value that is neither gives the
//!   default, and the log says so in its first line.
//! - `PID_LOG_FORMAT`: `json` (the default) or `text`, any case; any other value gives JSON.
//! - `SERVICE_NAME`: the `service` of every event, `pacod-id` by default.
//!
//! JSON: `{"timestamp":"2026-10-23T06:41:07.695Z","level":"info","service":"pacod-id",
//! "logger":"pacod_id::app","message":"pacod-id is up","public":"0.0.0.0:8070"}`: these five keys
//! in this order, then the event's fields and those of its spans, flat.
//!
//! Text: `2026-10-23T06:41:07.695Z INFO  pacod-id pacod_id::app: pacod-id is up public=0.0.0.0:8070`.

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::{FormatEvent, FormatFields, JsonFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

/// `PID_LOG` when it is unset, empty or not understood.
pub const DEFAULT_FILTER: &str = "info,sqlx=warn";

/// `SERVICE_NAME` when it is unset or empty.
pub const DEFAULT_SERVICE: &str = "pacod-id";

/// The keys every event has; a field with one of these names gets a trailing underscore.
const RESERVED: [&str; 5] = ["timestamp", "level", "service", "logger", "message"];

const LEVELS: [&str; 6] = ["trace", "debug", "info", "warn", "error", "off"];

/// Whether `PID_LOG_FORMAT` asks for JSON: everything but `text` (any case) does.
pub fn json_format(value: Option<&str>) -> bool {
    !value.is_some_and(|v| v.trim().eq_ignore_ascii_case("text"))
}

/// A level word in any case, with `warning` for `warn`; `None` for anything else.
fn level_word(word: &str) -> Option<&'static str> {
    let word = word.trim().to_ascii_lowercase();
    let word = if word == "warning" { "warn" } else { word.as_str() };
    LEVELS.iter().copied().find(|l| *l == word)
}

/// The filter for a value of `PID_LOG`, and a warning when the value had to be replaced by the
/// default. Level words are taken in any case (`WARNING` is `warn`), also after a target
/// (`sqlx=WARN`); targets keep their case. A value without `=` or `,` must be a level: a bare word
/// would otherwise be read as a target, and silence everything else.
pub fn resolve_filter(value: Option<&str>) -> (String, Option<String>) {
    let Some(raw) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return (DEFAULT_FILTER.to_owned(), None);
    };
    let fallback = || {
        (
            DEFAULT_FILTER.to_owned(),
            Some(format!("PID_LOG {raw:?} is neither a level nor a filter directive; using {DEFAULT_FILTER}")),
        )
    };
    let directives: Vec<String> = raw
        .split(',')
        .map(|d| {
            let d = d.trim();
            match d.rsplit_once('=') {
                Some((target, level)) => match level_word(level) {
                    Some(level) => format!("{target}={level}"),
                    None => d.to_owned(),
                },
                None => level_word(d).map_or_else(|| d.to_owned(), str::to_owned),
            }
        })
        .collect();
    if !raw.contains([',', '=']) && level_word(raw).is_none() {
        return fallback();
    }
    let filter = directives.join(",");
    match tracing_subscriber::EnvFilter::builder().parse(&filter) {
        Ok(_) => (filter, None),
        Err(_) => fallback(),
    }
}

/// `SERVICE_NAME`, or `pacod-id`.
pub fn service_name(value: Option<&str>) -> String {
    value.map(str::trim).filter(|v| !v.is_empty()).unwrap_or(DEFAULT_SERVICE).to_owned()
}

/// Installs the log for the process from `PID_LOG` and `SERVICE_NAME`, as JSON or text.
pub fn init(json: bool) {
    let (filter, warning) = resolve_filter(std::env::var("PID_LOG").ok().as_deref());
    let service = service_name(std::env::var("SERVICE_NAME").ok().as_deref());
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .with_ansi(false)
        .fmt_fields(JsonFields::new())
        .event_format(EventFormat::new(service, json))
        .init();
    if let Some(warning) = warning {
        tracing::warn!("{warning}");
    }
}

/// The event formatter of the convention. Span fields are read as JSON, so the subscriber must
/// format them with [`JsonFields`] (`fmt_fields(JsonFields::new())`); others are left out.
#[derive(Debug, Clone)]
pub struct EventFormat {
    service: String,
    json: bool,
}

impl EventFormat {
    pub fn new(service: impl Into<String>, json: bool) -> Self {
        EventFormat { service: service.into(), json }
    }
}

/// An event's message and fields, as JSON values.
#[derive(Default)]
struct Fields {
    message: Option<String>,
    /// The target of an event from the `log` crate (field `log.target`).
    log_target: Option<String>,
    fields: Map<String, Value>,
}

impl Fields {
    fn put(&mut self, field: &Field, value: Value) {
        match field.name() {
            "message" => {
                self.message = Some(match value {
                    Value::String(s) => s,
                    other => other.to_string(),
                })
            }
            "log.target" => self.log_target = value.as_str().map(str::to_owned),
            // The rest of what the `log` bridge adds: where in the source, not part of the event.
            name if name.starts_with("log.") => {}
            name => {
                self.fields.insert(name.to_owned(), value);
            }
        }
    }
}

impl Visit for Fields {
    fn record_f64(&mut self, field: &Field, value: f64) {
        let v = serde_json::Number::from_f64(value).map_or_else(|| Value::String(value.to_string()), Value::Number);
        self.put(field, v);
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, Value::from(value));
    }
    fn record_i128(&mut self, field: &Field, value: i128) {
        let v = i64::try_from(value).map_or_else(|_| Value::String(value.to_string()), Value::from);
        self.put(field, v);
    }
    fn record_u128(&mut self, field: &Field, value: u128) {
        let v = u64::try_from(value).map_or_else(|_| Value::String(value.to_string()), Value::from);
        self.put(field, v);
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, Value::Bool(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, Value::String(value.to_owned()));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, Value::String(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.put(field, Value::String(format!("{value:?}")));
    }
}

/// A field's name as written: with a trailing underscore when it is one of the five keys.
fn key(name: &str) -> String {
    if RESERVED.contains(&name) { format!("{name}_") } else { name.to_owned() }
}

/// RFC 3339, UTC, milliseconds, `Z`.
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A value for a text line: strings bare unless they hold a space, a quote, `=` or a line break
/// (or are empty), then quoted and escaped; other values as JSON.
fn text_value(value: &Value) -> String {
    match value {
        Value::String(s) if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c == '"' || c == '=') => {
            Value::String(s.clone()).to_string()
        }
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl<S, N> FormatEvent<S, N> for EventFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(&self, ctx: &FmtContext<'_, S, N>, mut writer: Writer<'_>, event: &Event<'_>) -> fmt::Result {
        let meta = event.metadata();
        let mut own = Fields::default();
        event.record(&mut own);

        // The event's fields first, then those of its spans from the innermost out: the nearer
        // one wins.
        let mut fields = Map::new();
        for (name, value) in own.fields {
            fields.insert(key(&name), value);
        }
        if let Some(scope) = ctx.event_scope() {
            for span in scope {
                let ext = span.extensions();
                let Some(formatted) = ext.get::<FormattedFields<N>>() else { continue };
                let Ok(Value::Object(span_fields)) = serde_json::from_str::<Value>(formatted) else { continue };
                for (name, value) in span_fields {
                    fields.entry(key(&name)).or_insert(value);
                }
            }
        }

        let message = own.message.filter(|m| !m.is_empty()).unwrap_or_else(|| meta.name().to_owned());
        let logger = own.log_target.unwrap_or_else(|| meta.target().to_owned());
        let level = meta.level().as_str();

        if self.json {
            let mut object = Map::new();
            object.insert("timestamp".into(), Value::String(timestamp()));
            object.insert("level".into(), Value::String(level.to_ascii_lowercase()));
            object.insert("service".into(), Value::String(self.service.clone()));
            object.insert("logger".into(), Value::String(logger));
            object.insert("message".into(), Value::String(message));
            object.extend(fields);
            writeln!(writer, "{}", Value::Object(object))
        } else {
            // The message stays on its line: line breaks in it are written as \n.
            let message = message.replace('\r', "\\r").replace('\n', "\\n");
            write!(writer, "{} {level:<5} {} {logger}: {message}", timestamp(), self.service)?;
            for (name, value) in &fields {
                write!(writer, " {name}={}", text_value(value))?;
            }
            writeln!(writer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// The lines `f` logs, at trace and above.
    fn capture(json: bool, f: impl FnOnce()) -> Vec<String> {
        let buffer = Buffer::default();
        let out = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(true) // the formatter writes no colour codes even when asked to
            .fmt_fields(JsonFields::new())
            .event_format(EventFormat::new("id-test", json))
            .with_writer(move || out.clone())
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        let bytes = buffer.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap().lines().map(str::to_owned).collect()
    }

    fn parse(line: &str) -> Map<String, Value> {
        match serde_json::from_str(line).unwrap() {
            Value::Object(m) => m,
            other => panic!("not an object: {other}"),
        }
    }

    /// `2026-10-23T06:41:07.695Z`
    fn is_timestamp(s: &str) -> bool {
        let b = s.as_bytes();
        b.len() == 24
            && b.iter().enumerate().all(|(i, c)| match i {
                4 | 7 => *c == b'-',
                10 => *c == b'T',
                13 | 16 => *c == b':',
                19 => *c == b'.',
                23 => *c == b'Z',
                _ => c.is_ascii_digit(),
            })
    }

    #[test]
    fn json_has_the_five_keys_in_order_then_the_fields() {
        let lines = capture(true, || {
            tracing::info!(target: "pacod_id::app", public = "0.0.0.0:8070", n = 3, ok = true, ratio = 0.5, "pacod-id is up");
        });
        assert_eq!(lines.len(), 1);
        let event = parse(&lines[0]);
        let keys: Vec<&str> = event.keys().map(String::as_str).collect();
        assert_eq!(keys, ["timestamp", "level", "service", "logger", "message", "public", "n", "ok", "ratio"]);
        assert!(is_timestamp(event["timestamp"].as_str().unwrap()), "{}", event["timestamp"]);
        assert_eq!(event["level"], "info");
        assert_eq!(event["service"], "id-test");
        assert_eq!(event["logger"], "pacod_id::app");
        assert_eq!(event["message"], "pacod-id is up");
        assert_eq!(event["public"], "0.0.0.0:8070");
        assert_eq!(event["n"], 3);
        assert_eq!(event["ok"], true);
        assert_eq!(event["ratio"], 0.5);
        assert!(!lines[0].contains('\u{1b}'));
    }

    #[test]
    fn levels_are_lower_case() {
        let lines = capture(true, || {
            tracing::trace!("t");
            tracing::debug!("d");
            tracing::info!("i");
            tracing::warn!("w");
            tracing::error!("e");
        });
        let levels: Vec<Value> = lines.iter().map(|l| parse(l)["level"].clone()).collect();
        assert_eq!(levels, ["trace", "debug", "info", "warn", "error"]);
    }

    #[test]
    fn span_fields_are_flat_and_the_nearest_wins() {
        let lines = capture(true, || {
            let outer = tracing::info_span!("outer", realm = "shop", client = "outer", depth = 1);
            let _o = outer.enter();
            let inner = tracing::info_span!("inner", client = "inner", depth = 2);
            let _i = inner.enter();
            tracing::warn!(depth = 3, error = "boom", "a mail was not sent");
        });
        let event = parse(&lines[0]);
        assert_eq!(event["realm"], "shop");
        assert_eq!(event["client"], "inner");
        assert_eq!(event["depth"], 3);
        assert_eq!(event["error"], "boom");
        assert!(!event.contains_key("fields") && !event.contains_key("spans") && !event.contains_key("span"));
    }

    #[test]
    fn reserved_names_get_an_underscore() {
        let lines = capture(true, || {
            let span = tracing::info_span!("s", message = "from the span", service = "other");
            let _s = span.enter();
            tracing::info!(level = 5, logger = "x", timestamp = "y", "the message");
        });
        let event = parse(&lines[0]);
        assert_eq!(event["message"], "the message");
        assert_eq!(event["level"], "info");
        assert_eq!(event["service"], "id-test");
        assert_eq!(event["level_"], 5);
        assert_eq!(event["logger_"], "x");
        assert_eq!(event["timestamp_"], "y");
        assert_eq!(event["message_"], "from the span");
        assert_eq!(event["service_"], "other");
    }

    #[test]
    fn errors_and_debug_values_are_strings() {
        let e = std::io::Error::other("disk full");
        let lines = capture(true, || {
            tracing::error!(error = %e, list = ?[1, 2], nan = f64::NAN, "internal error");
        });
        let event = parse(&lines[0]);
        assert_eq!(event["error"], "disk full");
        assert_eq!(event["list"], "[1, 2]");
        assert_eq!(event["nan"], "NaN");
    }

    #[test]
    fn lines_from_the_log_crate_name_their_target() {
        let lines = capture(true, || {
            tracing::info!(
                "log.target" = "rustls::conn",
                "log.module_path" = "rustls::conn",
                "log.file" = "conn.rs",
                "log.line" = 1,
                "hi"
            );
        });
        let event = parse(&lines[0]);
        assert_eq!(event["logger"], "rustls::conn");
        assert_eq!(event["message"], "hi");
        assert!(event.keys().all(|k| !k.starts_with("log.")), "{}", lines[0]);
    }

    #[test]
    fn an_event_without_a_message_uses_its_name() {
        let lines = capture(true, || tracing::info!(n = 1));
        let message = parse(&lines[0])["message"].as_str().unwrap().to_owned();
        assert!(message.starts_with("event "), "{message}");
    }

    #[test]
    fn text_is_one_line_in_the_fixed_order() {
        let lines = capture(false, || {
            let span = tracing::info_span!("s", realm = "shop");
            let _s = span.enter();
            tracing::info!(target: "pacod_id::app", public = "0.0.0.0:8070", name = "two words", n = 7, "pacod-id is up");
            tracing::warn!(target: "pacod_id::mail", "line one\nline two");
        });
        assert_eq!(lines.len(), 2);
        let (ts, rest) = lines[0].split_once(' ').unwrap();
        assert!(is_timestamp(ts), "{ts}");
        assert_eq!(
            rest,
            r#"INFO  id-test pacod_id::app: pacod-id is up public=0.0.0.0:8070 name="two words" n=7 realm=shop"#
        );
        assert!(lines[1].ends_with(r"WARN  id-test pacod_id::mail: line one\nline two realm=shop"), "{}", lines[1]);
        assert!(lines.iter().all(|l| !l.contains('\u{1b}')));
    }

    #[test]
    fn the_format_switch() {
        assert!(json_format(None));
        assert!(json_format(Some("json")));
        assert!(json_format(Some("JSON")));
        assert!(json_format(Some("pretty")));
        assert!(!json_format(Some("text")));
        assert!(!json_format(Some(" Text ")));
    }

    #[test]
    fn the_level_switch() {
        assert_eq!(resolve_filter(None), (DEFAULT_FILTER.to_owned(), None));
        assert_eq!(resolve_filter(Some("  ")), (DEFAULT_FILTER.to_owned(), None));
        for (given, used) in [
            ("debug", "debug"),
            ("INFO", "info"),
            ("Warning", "warn"),
            ("WARN", "warn"),
            ("error", "error"),
            ("info,sqlx=warn", "info,sqlx=warn"),
            ("DEBUG,sqlx=Warning", "debug,sqlx=warn"),
            ("info,sqlx=warn,pacod_id=debug", "info,sqlx=warn,pacod_id=debug"),
        ] {
            assert_eq!(resolve_filter(Some(given)), (used.to_owned(), None), "{given}");
        }
        for bad in ["verbose", "warnings", "info,sqlx=loud"] {
            let (filter, warning) = resolve_filter(Some(bad));
            assert_eq!(filter, DEFAULT_FILTER, "{bad}");
            assert!(warning.unwrap().contains(bad), "{bad}");
        }
    }

    #[test]
    fn the_service_name() {
        assert_eq!(service_name(None), "pacod-id");
        assert_eq!(service_name(Some("")), "pacod-id");
        assert_eq!(service_name(Some("pacod-id-2")), "pacod-id-2");
    }
}
