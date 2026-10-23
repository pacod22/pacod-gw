//! The log of every pacod-gw binary, after the pacod logging convention
//! (pacod-demo/docs/LOGGING.md): one event per line, in JSON or text, with the same five keys
//! first everywhere (`timestamp`, `level`, `service`, `logger`, `message`) and the event's
//! fields flat after them.
//!
//! Each binary calls [`init`] (or builds [`Settings`] itself and calls [`install`]) once, before
//! it logs anything:
//!
//! - the level comes from `RUST_LOG`: a level (`debug`, `info`, `warn`, `error`, any case,
//!   `warning` for `warn`) or a filter directive (`info,sqlx=warn`). Anything else logs at the
//!   binary's default and says so in one warning.
//! - the format comes from the variable the binary has always read (`PGW_LOG_FORMAT`,
//!   `ANALYTICS_LOG_FORMAT`, `LOAD_AGENT_LOG_FORMAT`): `json` or `text`, any case. Unset or
//!   unknown: `json`.
//! - `service` comes from `SERVICE_NAME`, by default the binary's compose service name.
//!
//! The formatter writes what the code logs and nothing else: the binaries never log request
//! bodies or authorization headers (the minimal gateway's body line is the one exception an
//! operator switches on), and this crate adds no host name, process id or colour codes.

mod format;

use tracing::level_filters::LevelFilter;
use tracing_log::AsLog;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::fmt::format::JsonFields;

pub use format::EventFormat;

/// The variable that holds the level, for every Rust binary of the workspace.
pub const LEVEL_VAR: &str = "RUST_LOG";
/// The variable that names the process in its log.
pub const SERVICE_VAR: &str = "SERVICE_NAME";

/// How each event is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// One JSON object per line: the default, for log collectors.
    #[default]
    Json,
    /// One readable line per event, for a terminal.
    Text,
}

impl Format {
    /// `json` or `text`, in any case. `None` for anything else.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("json") {
            Some(Self::Json)
        } else if value.eq_ignore_ascii_case("text") {
            Some(Self::Text)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Text => "text",
        }
    }
}

/// What the log of one process looks like. Built from the environment by
/// [`Settings::from_env`], or step by step by a binary that reads its variables elsewhere
/// (the minimal gateway reads an env file as well).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// The `service` key of every event.
    pub service: String,
    /// The filter, as `tracing_subscriber::EnvFilter` reads it.
    pub filter: String,
    pub format: Format,
    /// Problems with the variables, logged as warnings once the log is set up.
    pub warnings: Vec<String>,
}

impl Settings {
    /// The binary's defaults: its compose service name, its filter (`info` for most) and JSON.
    pub fn new(default_service: &str, default_filter: &str) -> Self {
        Self {
            service: default_service.to_owned(),
            filter: default_filter.to_owned(),
            format: Format::Json,
            warnings: Vec::new(),
        }
    }

    /// `SERVICE_NAME`, `RUST_LOG` and the binary's format variable from the process
    /// environment.
    pub fn from_env(default_service: &str, default_filter: &str, format_var: &str) -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self::new(default_service, default_filter)
            .service(var(SERVICE_VAR).as_deref())
            .level(var(LEVEL_VAR).as_deref())
            .format(format_var, var(format_var).as_deref())
    }

    /// The value of `SERVICE_NAME`; unset or empty keeps the default.
    pub fn service(mut self, value: Option<&str>) -> Self {
        if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
            v.clone_into(&mut self.service);
        }
        self
    }

    /// The value of `RUST_LOG`; unset or empty keeps the default.
    pub fn level(mut self, value: Option<&str>) -> Self {
        match filter_of(value) {
            Ok(Some(filter)) => self.filter = filter,
            Ok(None) => {}
            Err(v) => self.warnings.push(format!(
                "{LEVEL_VAR}: \"{v}\" is neither a level (debug, info, warn, error) nor a filter directive; logging with \"{}\"",
                self.filter
            )),
        }
        self
    }

    /// The value of the binary's format variable, named `var` in the warning; unset or empty
    /// keeps JSON, and so does an unknown value.
    pub fn format(mut self, var: &str, value: Option<&str>) -> Self {
        if let Some(v) = value.map(str::trim).filter(|v| !v.is_empty()) {
            match Format::parse(v) {
                Some(f) => self.format = f,
                None => {
                    self.format = Format::Json;
                    self.warnings.push(format!("{var}: \"{v}\" is not one of json, text; writing json"));
                }
            }
        }
        self
    }

    /// A format the binary has read itself.
    pub fn with_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    fn env_filter(&self) -> EnvFilter {
        EnvFilter::try_new(&self.filter).unwrap_or_else(|_| EnvFilter::new("info"))
    }
}

/// The filter a `RUST_LOG` value stands for: `Ok(None)` when unset or empty, `Err` with the
/// value when it is neither a level nor a directive.
///
/// A single word must be a level: `EnvFilter` would take any other word for a target and log
/// almost nothing, which is the opposite of what someone who wrote `verbose` meant. Levels
/// inside a directive are read the same way (`warning` and `WARN` are `warn`).
fn filter_of(value: Option<&str>) -> Result<Option<String>, String> {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if !value.contains(['=', ',', '[', ':']) {
        return level_word(value).map(|l| Some(l.to_owned())).ok_or_else(|| value.to_owned());
    }
    let directives: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| match d.rsplit_once('=') {
            Some((target, level)) => match level_word(level) {
                Some(l) => format!("{target}={l}"),
                None => d.to_owned(),
            },
            None => level_word(d).map_or_else(|| d.to_owned(), str::to_owned),
        })
        .collect();
    let filter = directives.join(",");
    match EnvFilter::try_new(&filter) {
        Ok(_) => Ok(Some(filter)),
        Err(_) => Err(value.to_owned()),
    }
}

fn level_word(word: &str) -> Option<&'static str> {
    match word.trim().to_ascii_lowercase().as_str() {
        "trace" => Some("trace"),
        "debug" => Some("debug"),
        "info" => Some("info"),
        "warn" | "warning" => Some("warn"),
        "error" => Some("error"),
        "off" => Some("off"),
        _ => None,
    }
}

/// The type [`subscriber`] returns.
pub type Subscriber<W> = tracing_subscriber::fmt::Subscriber<JsonFields, EventFormat, EnvFilter, W>;

/// The subscriber `settings` describe, writing to `writer`. For tests, which read what it
/// writes; binaries call [`install`] or [`init`].
pub fn subscriber<W>(settings: &Settings, writer: W) -> Subscriber<W>
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .with_env_filter(settings.env_filter())
        // Span fields are kept as JSON, so that the formatter can put them flat into the event
        // with their types.
        .fmt_fields(JsonFields::new())
        .event_format(EventFormat::new(&settings.service, settings.format))
        .with_writer(writer)
        .finish()
}

/// Sets up the log of the process, then logs the warnings of `settings`. Returns false when a
/// log was set up before (tests start several gateways in one process): the first one stays.
pub fn install<W>(settings: Settings, writer: W) -> bool
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let max = settings.env_filter().max_level_hint().unwrap_or(LevelFilter::TRACE);
    // Records of the `log` facade become events, but only those the filter can let through.
    let _ = tracing_log::LogTracer::builder().with_max_level(max.as_log()).init();
    if tracing::subscriber::set_global_default(subscriber(&settings, writer)).is_err() {
        return false;
    }
    for w in &settings.warnings {
        tracing::warn!(target: "pacod_gw_logging", "{w}");
    }
    true
}

/// Sets up the log of the process on standard output from `SERVICE_NAME`, `RUST_LOG` and
/// `format_var`. `default_service` is the binary's compose service name.
pub fn init(default_service: &str, default_filter: &str, format_var: &str) -> bool {
    install(Settings::from_env(default_service, default_filter, format_var), std::io::stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_is_any_case_and_warning_is_warn() {
        assert_eq!(filter_of(Some("DEBUG")), Ok(Some("debug".into())));
        assert_eq!(filter_of(Some(" Warning ")), Ok(Some("warn".into())));
        assert_eq!(filter_of(Some("error")), Ok(Some("error".into())));
        assert_eq!(filter_of(None), Ok(None));
        assert_eq!(filter_of(Some("  ")), Ok(None), "compose passes an unset variable as an empty one");
    }

    #[test]
    fn a_directive_is_kept_with_its_levels_read_alike() {
        assert_eq!(filter_of(Some("info,sqlx=warn")), Ok(Some("info,sqlx=warn".into())));
        assert_eq!(filter_of(Some("Warning,pacod_gw_core=DEBUG")), Ok(Some("warn,pacod_gw_core=debug".into())));
        assert_eq!(filter_of(Some("pacod_gw_core::dataplane")), Ok(Some("pacod_gw_core::dataplane".into())));
    }

    #[test]
    fn an_unknown_level_keeps_the_default_with_one_warning() {
        let s = Settings::new("pacod-gw", "info").level(Some("verbose"));
        assert_eq!(s.filter, "info");
        assert_eq!(s.warnings.len(), 1);
        assert!(s.warnings[0].contains("\"verbose\""), "{:?}", s.warnings);
        let s = Settings::new("pacod-gw", "info").level(Some("info,sqlx=[bad"));
        assert_eq!((s.filter.as_str(), s.warnings.len()), ("info", 1));
    }

    #[test]
    fn the_format_is_any_case_and_unknown_means_json() {
        let s = Settings::new("x", "info").format("PGW_LOG_FORMAT", Some("TEXT"));
        assert_eq!((s.format, s.warnings.len()), (Format::Text, 0));
        let s = Settings::new("x", "info").format("PGW_LOG_FORMAT", Some("Json"));
        assert_eq!(s.format, Format::Json);
        let s = Settings::new("x", "info").format("PGW_LOG_FORMAT", None);
        assert_eq!((s.format, s.warnings.len()), (Format::Json, 0));
        let s = Settings::new("x", "info").format("ANALYTICS_LOG_FORMAT", Some("xml"));
        assert_eq!(s.format, Format::Json);
        assert!(s.warnings[0].starts_with("ANALYTICS_LOG_FORMAT: \"xml\""), "{:?}", s.warnings);
    }

    #[test]
    fn the_service_name_defaults_to_the_binarys() {
        assert_eq!(Settings::new("gw-mock", "info").service(None).service, "gw-mock");
        assert_eq!(Settings::new("gw-mock", "info").service(Some("")).service, "gw-mock");
        assert_eq!(Settings::new("gw-mock", "info").service(Some("gw-load-mock")).service, "gw-load-mock");
    }
}
