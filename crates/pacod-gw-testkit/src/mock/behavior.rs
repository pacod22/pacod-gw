//! Scripted behaviours: the options of the `x-mock` request header and of queued scripts.

use std::{fmt, str::FromStr, time::Duration};

/// `fail=<status>x<times>`: fail the first `times` requests with `status`, then answer normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fail {
    /// The error status to answer with (400 to 599).
    pub status: u16,
    /// How many requests fail before one is answered normally.
    pub times: u32,
}

/// How the mock answers one request.
///
/// The default answers normally. Build one from an option list with [`Behavior::parse`] (the
/// syntax of the `x-mock` header, for example `delay=200,chunk_delay=20,chunks=8`) or fill the
/// fields directly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Behavior {
    /// `delay=<ms>`: wait before sending the response head.
    pub delay: Option<Duration>,
    /// `chunk_delay=<ms>`: wait between two written stream chunks.
    pub chunk_delay: Option<Duration>,
    /// `chunks=<n>`: split each streamed string (reply text, thinking text, tool arguments) into
    /// `n` content chunks. The default splits at word boundaries.
    pub chunks: Option<usize>,
    /// `split=<n>`: re-chunk the stream's bytes into pieces of `n` bytes.
    pub split: Option<usize>,
    /// `status=<code>`: answer with this error status in the provider's error envelope.
    pub status: Option<u16>,
    /// `fail=<code>x<n>`: fail `n` requests with `code`, then answer normally.
    pub fail: Option<Fail>,
    /// `retry_after=<value>`: the `retry-after` header of a scripted error response.
    pub retry_after: Option<String>,
    /// `reset_after=<n>`: abort the connection after `n` content chunks of a stream.
    pub reset_after: Option<usize>,
    /// `hang`: accept the request and never answer.
    pub hang: bool,
    /// `stall_after=<n>`: stop sending after `n` content chunks of a stream, without closing.
    pub stall_after: Option<usize>,
    /// `no_usage`: leave usage out of the response.
    pub no_usage: bool,
    /// `cache_read=<n>`: tokens reported as read from the prompt cache.
    pub cache_read: Option<u64>,
    /// `cache_write=<n>`: tokens reported as written to the prompt cache (Anthropic only).
    pub cache_write: Option<u64>,
    /// `reasoning=<n>`: reasoning tokens reported in usage.
    pub reasoning: Option<u64>,
    /// `thinking`: emit reasoning before the answer.
    pub thinking: bool,
    /// `tool=<name>`: answer with a call to this tool instead of text.
    pub tool: Option<String>,
    /// `stop=<reason>`: the finish or stop reason, written to the response as given.
    pub stop: Option<String>,
    /// `model=<id>`: the model the response reports as having served the request.
    pub model: Option<String>,
    /// `request_id=<id>`: the provider request-id response header.
    pub request_id: Option<String>,
    /// `tier=<name>`: the service tier the response reports (`service_tier`), for example
    /// `batch` or `priority`.
    pub tier: Option<String>,
    /// `code=<text>`: the business code of a scripted error, where the emulated provider has
    /// one (GLM `1113`, Kimi `exceeded_current_quota_error`).
    pub code: Option<String>,
    /// `reply=<base64url>`: the answer text instead of the echo, also where a JSON schema asks
    /// for structured output (a canned price extraction, for example).
    pub reply: Option<String>,
}

/// An option list that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BehaviorError(String);

impl fmt::Display for BehaviorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BehaviorError {}

impl Behavior {
    /// Parses a comma-separated option list such as `delay=200,chunks=8,thinking`.
    ///
    /// Option names are case-insensitive and `-` is accepted for `_`. An empty list is the
    /// default behaviour.
    pub fn parse(spec: &str) -> Result<Self, BehaviorError> {
        let mut behavior = Self::default();
        for item in spec.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (key, value) = match item.split_once('=') {
                Some((key, value)) => (key.trim(), Some(value.trim())),
                None => (item, None),
            };
            let key = key.to_ascii_lowercase().replace('-', "_");
            let option = Entry { key: &key, value };
            match key.as_str() {
                "delay" => behavior.delay = Some(Duration::from_millis(option.number()?)),
                "chunk_delay" => {
                    behavior.chunk_delay = Some(Duration::from_millis(option.number()?));
                }
                "chunks" => behavior.chunks = Some(option.at_least_one()?),
                "split" => behavior.split = Some(option.at_least_one()?),
                "status" => behavior.status = Some(error_status(&key, option.text()?)?),
                "fail" => behavior.fail = Some(option.fail()?),
                "retry_after" => behavior.retry_after = Some(option.header_value()?),
                "reset_after" => behavior.reset_after = Some(option.number()?),
                "hang" => behavior.hang = option.flag()?,
                "stall_after" => behavior.stall_after = Some(option.number()?),
                "no_usage" => behavior.no_usage = option.flag()?,
                "cache_read" => behavior.cache_read = Some(option.number()?),
                "cache_write" => behavior.cache_write = Some(option.number()?),
                "reasoning" => behavior.reasoning = Some(option.number()?),
                "thinking" => behavior.thinking = option.flag()?,
                "tool" => behavior.tool = Some(option.text()?.to_owned()),
                "stop" => behavior.stop = Some(option.text()?.to_owned()),
                "model" => behavior.model = Some(option.text()?.to_owned()),
                "tier" => behavior.tier = Some(option.text()?.to_owned()),
                "code" => behavior.code = Some(option.text()?.to_owned()),
                "reply" => behavior.reply = Some(option.base64url()?),
                "request_id" => behavior.request_id = Some(option.header_value()?),
                _ => return Err(BehaviorError(format!("unknown option `{key}`"))),
            }
        }
        Ok(behavior)
    }

    /// Parses one option list per request, for [`MockProvider::set_script`].
    ///
    /// [`MockProvider::set_script`]: super::MockProvider::set_script
    pub fn parse_all<I, S>(specs: I) -> Result<Vec<Self>, BehaviorError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        specs.into_iter().map(|spec| Self::parse(spec.as_ref())).collect()
    }

    /// Lays `over` on top of `self`: options set in `over` win, flags are or-ed.
    pub(crate) fn overlaid(mut self, over: &Self) -> Self {
        fn take<T: Clone>(base: &mut Option<T>, over: &Option<T>) {
            if over.is_some() {
                base.clone_from(over);
            }
        }
        take(&mut self.delay, &over.delay);
        take(&mut self.chunk_delay, &over.chunk_delay);
        take(&mut self.chunks, &over.chunks);
        take(&mut self.split, &over.split);
        take(&mut self.status, &over.status);
        take(&mut self.fail, &over.fail);
        take(&mut self.retry_after, &over.retry_after);
        take(&mut self.reset_after, &over.reset_after);
        take(&mut self.stall_after, &over.stall_after);
        take(&mut self.cache_read, &over.cache_read);
        take(&mut self.cache_write, &over.cache_write);
        take(&mut self.reasoning, &over.reasoning);
        take(&mut self.tool, &over.tool);
        take(&mut self.stop, &over.stop);
        take(&mut self.model, &over.model);
        take(&mut self.code, &over.code);
        take(&mut self.reply, &over.reply);
        take(&mut self.tier, &over.tier);
        take(&mut self.request_id, &over.request_id);
        self.hang |= over.hang;
        self.no_usage |= over.no_usage;
        self.thinking |= over.thinking;
        self
    }
}

impl FromStr for Behavior {
    type Err = BehaviorError;

    fn from_str(spec: &str) -> Result<Self, Self::Err> {
        Self::parse(spec)
    }
}

/// One `key[=value]` item of an option list.
struct Entry<'a> {
    key: &'a str,
    value: Option<&'a str>,
}

impl Entry<'_> {
    fn text(&self) -> Result<&str, BehaviorError> {
        match self.value {
            Some(value) if !value.is_empty() => Ok(value),
            _ => Err(BehaviorError(format!("option `{}` needs a value", self.key))),
        }
    }

    fn base64url(&self) -> Result<String, BehaviorError> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(self.text()?.trim_end_matches('='))
            .map_err(|_| BehaviorError(format!("option `{}` must be base64url", self.key)))?;
        String::from_utf8(bytes).map_err(|_| BehaviorError(format!("option `{}` must be UTF-8 text", self.key)))
    }

    fn number<T: FromStr>(&self) -> Result<T, BehaviorError> {
        let text = self.text()?;
        text.parse()
            .map_err(|_| BehaviorError(format!("option `{}` needs a non-negative number, got `{text}`", self.key)))
    }

    fn at_least_one(&self) -> Result<usize, BehaviorError> {
        match self.number()? {
            0 => Err(BehaviorError(format!("option `{}` must be at least 1", self.key))),
            number => Ok(number),
        }
    }

    fn flag(&self) -> Result<bool, BehaviorError> {
        match self.value {
            None | Some("true") => Ok(true),
            Some("false") => Ok(false),
            Some(value) => Err(BehaviorError(format!("option `{}` is a flag, got `{value}`", self.key))),
        }
    }

    /// A value that is sent as a response header.
    fn header_value(&self) -> Result<String, BehaviorError> {
        let text = self.text()?;
        if text.chars().all(|c| c.is_ascii_graphic() || c == ' ') {
            Ok(text.to_owned())
        } else {
            Err(BehaviorError(format!("option `{}` must be printable ASCII", self.key)))
        }
    }

    fn fail(&self) -> Result<Fail, BehaviorError> {
        let text = self.text()?;
        let malformed = || BehaviorError(format!("option `fail` has the form <status>x<times>, got `{text}`"));
        let (status, times) = text.split_once(['x', 'X']).ok_or_else(malformed)?;
        Ok(Fail {
            status: error_status(self.key, status.trim())?,
            times: times.trim().parse().map_err(|_| malformed())?,
        })
    }
}

fn error_status(key: &str, text: &str) -> Result<u16, BehaviorError> {
    match text.parse::<u16>() {
        Ok(status) if (400..=599).contains(&status) => Ok(status),
        _ => Err(BehaviorError(format!("option `{key}` needs an error status between 400 and 599, got `{text}`"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_option() {
        let behavior = Behavior::parse(
            "delay=200, chunk_delay=20,chunks=8,split=7,status=503,fail=429x2,retry-after=3,\
             reset_after=4,hang,stall_after=5,no_usage,cache_read=11,cache_write=12,reasoning=13,\
             thinking,tool=get_weather,stop=max_tokens,model=served-1,request_id=req_abc,tier=batch,code=1113,reply=aGk",
        )
        .unwrap();
        assert_eq!(
            behavior,
            Behavior {
                delay: Some(Duration::from_millis(200)),
                chunk_delay: Some(Duration::from_millis(20)),
                chunks: Some(8),
                split: Some(7),
                status: Some(503),
                fail: Some(Fail { status: 429, times: 2 }),
                retry_after: Some("3".into()),
                reset_after: Some(4),
                hang: true,
                stall_after: Some(5),
                no_usage: true,
                cache_read: Some(11),
                cache_write: Some(12),
                reasoning: Some(13),
                thinking: true,
                tool: Some("get_weather".into()),
                stop: Some("max_tokens".into()),
                model: Some("served-1".into()),
                request_id: Some("req_abc".into()),
                tier: Some("batch".into()),
                code: Some("1113".into()),
                reply: Some("hi".into()),
            }
        );
    }

    #[test]
    fn empty_list_is_the_default() {
        assert_eq!(Behavior::parse("").unwrap(), Behavior::default());
        assert_eq!(" , ".parse::<Behavior>().unwrap(), Behavior::default());
    }

    #[test]
    fn rejects_malformed_options() {
        for spec in [
            "nope",
            "delay",
            "delay=abc",
            "delay=-1",
            "chunks=0",
            "split=0",
            "status=200",
            "status=600",
            "fail=503",
            "fail=200x2",
            "fail=503xmany",
            "hang=maybe",
            "tool=",
            "request_id=caf\u{e9}",
        ] {
            assert!(Behavior::parse(spec).is_err(), "`{spec}` should not parse");
        }
    }

    #[test]
    fn parse_all_reports_the_first_error() {
        assert_eq!(Behavior::parse_all(["status=500", "hang"]).unwrap().len(), 2);
        let error = Behavior::parse_all(["status=500", "bogus=1"]).unwrap_err();
        assert_eq!(error.to_string(), "unknown option `bogus`");
    }

    #[test]
    fn overlay_prefers_the_upper_layer() {
        let base = Behavior::parse("delay=10,chunks=3,thinking,model=a").unwrap();
        let over = Behavior::parse("chunks=5,model=b,no_usage").unwrap();
        assert_eq!(base.overlaid(&over), Behavior::parse("delay=10,chunks=5,thinking,model=b,no_usage").unwrap());
    }
}
