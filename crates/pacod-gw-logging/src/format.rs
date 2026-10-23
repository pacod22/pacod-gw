//! The event of the pacod logging convention, in JSON and in text.
//!
//! ```text
//! {"timestamp":"2026-10-23T06:41:07.695Z","level":"info","service":"pacod-gw","logger":"pacod_gw_core::app","message":"listening","addr":"0.0.0.0:8080"}
//! 2026-10-23T06:41:07.695Z INFO  pacod-gw pacod_gw_core::app: listening addr=0.0.0.0:8080
//! ```

use std::fmt::{self, Write as _};

use chrono::{SecondsFormat, Utc};
use serde_json::{Number, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_log::NormalizeEvent;
use tracing_subscriber::fmt::format::{JsonFields, Writer};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormattedFields};
use tracing_subscriber::registry::LookupSpan;

use crate::Format;

/// The keys every event starts with. A field of one of these names is written with a trailing
/// underscore (`level_`), so that it cannot hide or repeat them.
const RESERVED: [&str; 5] = ["timestamp", "level", "service", "logger", "message"];

/// Writes each event as one line of the convention, in the format of the settings.
#[derive(Debug, Clone)]
pub struct EventFormat {
    service: String,
    format: Format,
}

impl EventFormat {
    pub fn new(service: &str, format: Format) -> Self {
        Self { service: service.to_owned(), format }
    }
}

impl<S> FormatEvent<S, JsonFields> for EventFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, JsonFields>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        // Records of the `log` facade carry their real target and level here.
        let normalized = event.normalized_metadata();
        let meta = normalized.as_ref().unwrap_or_else(|| event.metadata());

        let mut fields = Fields::default();
        event.record(&mut fields);
        // The fields of the enclosing spans, innermost first: an inner span's field wins over an
        // outer one's, and the event's own over both.
        if let Some(scope) = ctx.event_scope() {
            for span in scope {
                let extensions = span.extensions();
                let Some(recorded) = extensions.get::<FormattedFields<JsonFields>>() else { continue };
                if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&recorded.fields) {
                    for (name, value) in map {
                        fields.add_context(&name, value);
                    }
                }
            }
        }
        let message = fields.message.take().filter(|m| !m.is_empty()).unwrap_or_else(|| meta.name().to_owned());
        let line = Line {
            timestamp: &Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            level: meta.level(),
            service: &self.service,
            logger: meta.target(),
            message: &message,
            fields: &fields.list,
        };
        let text = match self.format {
            Format::Json => line.json(),
            Format::Text => line.text(),
        };
        writer.write_str(&text)
    }
}

/// One event, ready to be written.
pub(crate) struct Line<'a> {
    pub timestamp: &'a str,
    pub level: &'a Level,
    pub service: &'a str,
    pub logger: &'a str,
    pub message: &'a str,
    pub fields: &'a [(String, Value)],
}

impl Line<'_> {
    /// One JSON object and a newline.
    pub fn json(&self) -> String {
        let mut out = String::with_capacity(160 + self.message.len());
        out.push('{');
        let key = |out: &mut String, k: &str, v: &str| {
            if out.len() > 1 {
                out.push(',');
            }
            push_json_str(out, k);
            out.push(':');
            push_json_str(out, v);
        };
        key(&mut out, "timestamp", self.timestamp);
        key(&mut out, "level", lower(self.level));
        key(&mut out, "service", self.service);
        key(&mut out, "logger", self.logger);
        key(&mut out, "message", self.message);
        for (k, v) in self.fields {
            out.push(',');
            push_json_str(&mut out, k);
            out.push(':');
            // A Value always serialises; the fallback only keeps the line whole.
            out.push_str(&serde_json::to_string(v).unwrap_or_else(|_| "null".into()));
        }
        out.push_str("}\n");
        out
    }

    /// `<timestamp> <LEVEL> <service> <logger>: <message> key=value ...` and a newline.
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(120 + self.message.len());
        let _ = write!(
            out,
            "{} {:<5} {} {}: {}",
            self.timestamp,
            upper(self.level),
            self.service,
            self.logger,
            self.message
        );
        for (k, v) in self.fields {
            out.push(' ');
            out.push_str(k);
            out.push('=');
            match v {
                Value::String(s) => push_text_value(&mut out, s),
                Value::Number(_) | Value::Bool(_) | Value::Null => out.push_str(&v.to_string()),
                other => push_text_value(&mut out, &other.to_string()),
            }
        }
        out.push('\n');
        out
    }
}

fn lower(level: &Level) -> &'static str {
    match *level {
        Level::TRACE => "trace",
        Level::DEBUG => "debug",
        Level::INFO => "info",
        Level::WARN => "warn",
        Level::ERROR => "error",
    }
}

fn upper(level: &Level) -> &'static str {
    match *level {
        Level::TRACE => "TRACE",
        Level::DEBUG => "DEBUG",
        Level::INFO => "INFO",
        Level::WARN => "WARN",
        Level::ERROR => "ERROR",
    }
}

fn push_json_str(out: &mut String, s: &str) {
    out.push_str(&serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into()));
}

/// A value as it is, or quoted and escaped when it would not read back as one value: empty,
/// with spaces, quotes, `=` or line breaks (a body logged by the minimal gateway has them).
fn push_text_value(out: &mut String, s: &str) {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"' || c == '=') {
        push_json_str(out, s);
    } else {
        out.push_str(s);
    }
}

/// The fields of one event, in the order they were declared, with the message apart.
#[derive(Default)]
pub(crate) struct Fields {
    pub message: Option<String>,
    pub list: Vec<(String, Value)>,
}

impl Fields {
    /// A field of the event itself: a later field of the same name replaces an earlier one.
    fn add(&mut self, name: &str, value: Value) {
        let name = name.strip_prefix("r#").unwrap_or(name);
        // The `log` facade's own metadata (`log.target`, `log.file`, ...), already in the
        // event's logger.
        if name.starts_with("log.") {
            return;
        }
        if name == "message" {
            let text = match value {
                Value::String(s) => s,
                other => other.to_string(),
            };
            // A field named message beside the message itself (`message = %x, "text"`): the
            // text, which the macros record first, is the message.
            if self.message.is_some() {
                self.set("message_".to_owned(), Value::String(text));
            } else {
                self.message = Some(text);
            }
            return;
        }
        self.set(rename(name), value);
    }

    /// A field of an enclosing span: kept only when the event has no field of that name.
    fn add_context(&mut self, name: &str, value: Value) {
        let name = if name == "message" { "message_".to_owned() } else { rename(name) };
        if !self.list.iter().any(|(k, _)| *k == name) {
            self.list.push((name, value));
        }
    }

    fn set(&mut self, name: String, value: Value) {
        match self.list.iter_mut().find(|(k, _)| *k == name) {
            Some(slot) => slot.1 = value,
            None => self.list.push((name, value)),
        }
    }
}

fn rename(name: &str) -> String {
    if RESERVED.contains(&name) { format!("{name}_") } else { name.to_owned() }
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.add(field.name(), Value::String(format!("{value:?}")));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.add(field.name(), Value::String(value.to_owned()));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.add(field.name(), Value::Number(value.into()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.add(field.name(), Value::Number(value.into()));
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        let v = i64::try_from(value).map_or_else(|_| Value::String(value.to_string()), |v| Value::Number(v.into()));
        self.add(field.name(), v);
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        let v = u64::try_from(value).map_or_else(|_| Value::String(value.to_string()), |v| Value::Number(v.into()));
        self.add(field.name(), v);
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        // JSON has no NaN or infinity.
        let v = Number::from_f64(value).map_or_else(|| Value::String(value.to_string()), Value::Number);
        self.add(field.name(), v);
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.add(field.name(), Value::Bool(value));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        // The error and its causes on one line, as `{:#}` of anyhow writes them.
        let mut text = value.to_string();
        let mut source = value.source();
        while let Some(s) = source {
            let _ = write!(text, ": {s}");
            source = s.source();
        }
        self.add(field.name(), Value::String(text));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::json;

    use super::*;
    use crate::{Settings, subscriber};

    /// A writer the tests read back.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    /// What `log` writes in `format` for service `svc`.
    fn capture(format: Format, filter: &str, log: impl FnOnce()) -> String {
        let buffer = Buffer::default();
        let writer = buffer.clone();
        let settings = Settings::new("gw-test", filter).with_format(format);
        tracing::subscriber::with_default(subscriber(&settings, move || writer.clone()), log);
        buffer.text()
    }

    fn json_lines(text: &str) -> Vec<serde_json::Map<String, Value>> {
        text.lines()
            .map(|l| match serde_json::from_str(l).expect("every line is one JSON object") {
                Value::Object(m) => m,
                other => panic!("not an object: {other}"),
            })
            .collect()
    }

    #[test]
    fn json_starts_with_the_five_keys_in_order_and_has_the_fields_flat() {
        let text = capture(Format::Json, "info", || {
            tracing::info!(target: "pacod_gw_core::app", addr = "0.0.0.0:8080", workers = 4_u64, ratio = 0.5, ok = true, "listening");
        });
        let lines = json_lines(&text);
        assert_eq!(lines.len(), 1, "{text}");
        let keys: Vec<&str> = lines[0].keys().map(String::as_str).collect();
        assert_eq!(keys, ["timestamp", "level", "service", "logger", "message", "addr", "workers", "ratio", "ok"]);
        let l = &lines[0];
        assert_eq!(l["level"], "info");
        assert_eq!(l["service"], "gw-test");
        assert_eq!(l["logger"], "pacod_gw_core::app");
        assert_eq!(l["message"], "listening");
        assert_eq!((l["workers"].clone(), l["ratio"].clone(), l["ok"].clone()), (json!(4), json!(0.5), json!(true)));
        assert!(!text.contains("\"fields\"") && !text.contains("\"spans\"") && !text.contains("\"target\""));
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn the_timestamp_is_utc_with_milliseconds() {
        let text = capture(Format::Json, "info", || tracing::info!("x"));
        let ts = json_lines(&text)[0]["timestamp"].as_str().unwrap().to_owned();
        // 2026-10-23T06:41:07.695Z
        assert_eq!(ts.len(), 24, "{ts}");
        let b = ts.as_bytes();
        assert_eq!((b[4], b[7], b[10], b[13], b[16], b[19], b[23]), (b'-', b'-', b'T', b':', b':', b'.', b'Z'), "{ts}");
        assert!(ts[20..23].bytes().all(|c| c.is_ascii_digit()), "{ts}");
        assert!(chrono::DateTime::parse_from_rfc3339(&ts).is_ok(), "{ts}");
    }

    #[test]
    fn levels_are_lower_case_in_json_and_padded_upper_case_in_text() {
        let text = capture(Format::Json, "trace", || {
            tracing::trace!("t");
            tracing::debug!("d");
            tracing::info!("i");
            tracing::warn!("w");
            tracing::error!("e");
        });
        let levels: Vec<String> = json_lines(&text).iter().map(|l| l["level"].as_str().unwrap().to_owned()).collect();
        assert_eq!(levels, ["trace", "debug", "info", "warn", "error"]);

        let text = capture(Format::Text, "info", || {
            tracing::info!(target: "a", "i");
            tracing::warn!(target: "a", "w");
            tracing::error!(target: "a", "e");
        });
        let after_ts: Vec<&str> = text.lines().map(|l| &l[25..]).collect();
        assert_eq!(after_ts, ["INFO  gw-test a: i", "WARN  gw-test a: w", "ERROR gw-test a: e"]);
    }

    #[test]
    fn span_fields_are_flat_and_the_events_own_field_wins() {
        let text = capture(Format::Json, "info", || {
            let outer = tracing::info_span!("request", request_id = "gw_1", key = "outer", depth = 1_u64);
            let _o = outer.enter();
            let inner = tracing::info_span!("attempt", key = "inner", provider = "openai");
            let _i = inner.enter();
            tracing::info!(key = "event", "sent");
        });
        let l = &json_lines(&text)[0];
        assert_eq!(l["key"], "event");
        assert_eq!(l["provider"], "openai");
        assert_eq!(l["request_id"], "gw_1");
        assert_eq!(l["depth"], 1, "span fields keep their JSON type");
        let keys: Vec<&str> = l.keys().map(String::as_str).collect();
        assert_eq!(keys[..6], ["timestamp", "level", "service", "logger", "message", "key"]);
        assert_eq!(keys.iter().filter(|k| **k == "key").count(), 1);
    }

    #[test]
    fn a_field_with_a_reserved_name_gets_a_trailing_underscore() {
        let text = capture(Format::Json, "info", || {
            tracing::info!(level = "high", service = "other", logger = "x", timestamp = 5_u64, "event");
            tracing::info!(message = "the field", "the text");
        });
        let lines = json_lines(&text);
        let l = &lines[0];
        assert_eq!((l["level"].as_str(), l["level_"].as_str()), (Some("info"), Some("high")));
        assert_eq!((l["service"].as_str(), l["service_"].as_str()), (Some("gw-test"), Some("other")));
        assert_eq!(l["logger_"], "x");
        assert_eq!(l["timestamp_"], 5);
        assert_eq!(
            (lines[1]["message"].as_str(), lines[1]["message_"].as_str()),
            (Some("the text"), Some("the field"))
        );
    }

    #[test]
    fn an_event_without_a_message_uses_its_name() {
        let text = capture(Format::Json, "info", || tracing::info!(name: "settled", n = 1_u64));
        assert_eq!(json_lines(&text)[0]["message"], "settled");
    }

    #[test]
    fn text_has_the_fixed_order_quotes_where_needed_and_no_colour() {
        let text = capture(Format::Text, "info", || {
            tracing::warn!(
                target: "pacod_gw_lite::call",
                request_id = "gw_1",
                http = 502_u64,
                error = "upstream said no",
                empty = "",
                body = "line one\nline \"two\"",
                cost = 0.25,
                "call"
            );
        });
        assert!(!text.contains('\u{1b}'), "no ANSI codes: {text:?}");
        assert_eq!(text.lines().count(), 1, "one line per event, whatever the values: {text:?}");
        let (ts, rest) = text.split_once(' ').unwrap();
        assert_eq!(ts.len(), 24);
        assert_eq!(
            rest,
            "WARN  gw-test pacod_gw_lite::call: call request_id=gw_1 http=502 error=\"upstream said no\" empty=\"\" body=\"line one\\nline \\\"two\\\"\" cost=0.25\n"
        );
    }

    #[test]
    fn display_and_debug_values_are_strings_and_errors_carry_their_causes() {
        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl fmt::Display for Outer {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("could not connect")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let err = Outer(std::io::Error::other("refused"));
        let addr: std::net::SocketAddr = "127.0.0.1:9".parse().unwrap();
        let text = capture(Format::Json, "info", || {
            tracing::error!(%addr, list = ?["a", "b"], error = &err as &(dyn std::error::Error + 'static), "failed");
        });
        let l = &json_lines(&text)[0];
        assert_eq!(l["addr"], "127.0.0.1:9");
        assert_eq!(l["list"], "[\"a\", \"b\"]");
        assert_eq!(l["error"], "could not connect: refused");
    }

    #[test]
    fn the_filter_applies() {
        let text = capture(Format::Json, "warn,chatty=error", || {
            tracing::info!("dropped");
            tracing::warn!("kept");
            tracing::warn!(target: "chatty", "dropped too");
        });
        let lines = json_lines(&text);
        assert_eq!(lines.len(), 1, "{text}");
        assert_eq!(lines[0]["message"], "kept");
    }

    #[test]
    fn non_finite_numbers_stay_valid_json() {
        let text = capture(Format::Json, "info", || tracing::info!(x = f64::NAN, big = u128::MAX, "n"));
        let l = &json_lines(&text)[0];
        assert_eq!(l["x"], "NaN");
        assert_eq!(l["big"], u128::MAX.to_string());
    }
}
