//! What a plan must be before the agent makes a single call with it. The tester checks the
//! same and more; the agent checks again because it is the one that makes the load, and its
//! caps are its own.

use crate::agent::config::check_target;
use crate::contract::{CONTRACT, Caps, Mode, Prepare};

/// Headers the plan may not set: the key goes where `auth_header` says, and the rest belong
/// to the connection. The same list as the tester's message composer (`messages.py`).
pub const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "x-goog-api-key",
    "proxy-authorization",
    "cookie",
    "host",
    "content-length",
    "transfer-encoding",
    "connection",
    "upgrade",
    "te",
];

const MAX_STAGES: usize = 50;
const MAX_HEADERS: usize = 50;
const MAX_PATH: usize = 2000;
const MAX_TIMEOUT_S: u64 = 600;
const MAX_DRAIN_S: u64 = 600;
const LEASE_S: std::ops::RangeInclusive<u64> = 5..=600;

/// Checks a plan against the agent's caps; the answer is the base URL its calls go to.
pub fn check(p: &Prepare, caps: &Caps, own_target: &url::Url) -> Result<url::Url, String> {
    if p.contract != CONTRACT {
        return Err(format!("this agent speaks contract {CONTRACT}, the plan {}", p.contract));
    }
    if p.run_id.is_empty()
        || p.run_id.len() > 64
        || !p.run_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("the run id is letters, digits, - and _ only, at most 64".into());
    }
    if p.parts == 0 || p.part >= p.parts {
        return Err(format!("part {} of {} does not exist", p.part, p.parts));
    }
    if p.key.len() > 4096 || p.key.chars().any(|c| c.is_control()) {
        return Err("the key is not a valid header value".into());
    }
    let base = match &p.base_url {
        None => own_target.clone(),
        Some(text) => {
            let url = url::Url::parse(text).map_err(|e| format!("base_url: {e}"))?;
            check_target(&url).map_err(|e| format!("base_url: {e}"))?;
            url
        }
    };
    let r = &p.request;
    if !matches!(r.method.as_str(), "GET" | "POST") {
        return Err("the method is GET or POST".into());
    }
    check_path(&r.path)?;
    if r.headers.len() > MAX_HEADERS {
        return Err(format!("at most {MAX_HEADERS} headers"));
    }
    for (name, value) in &r.headers {
        let token = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_`|~-".contains(&b);
        if name.is_empty() || name.len() > 100 || !name.bytes().all(token) {
            return Err(format!("{name:?} is not a header name"));
        }
        if RESERVED_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!("the header {name} is set by the agent; choose where the key goes instead"));
        }
        if value.contains(['\r', '\n', '\0']) {
            return Err(format!("the value of {name} holds a line break"));
        }
    }
    if r.body.len() as u64 > caps.max_body_bytes {
        return Err(format!("the body is larger than this agent's {} bytes", caps.max_body_bytes));
    }

    let pr = &p.profile;
    if pr.stages.is_empty() || pr.stages.len() > MAX_STAGES {
        return Err(format!("1 to {MAX_STAGES} stages"));
    }
    if pr.stages.iter().any(|s| !s.target.is_finite() || s.target < 0.0) {
        return Err("a stage's target is a number of 0 or more".into());
    }
    let duration = pr.duration_s();
    if duration == 0 {
        return Err("the stages last no time at all".into());
    }
    if duration > caps.max_duration_s {
        return Err(format!("{duration} s is longer than this agent's {} s", caps.max_duration_s));
    }
    let peak = pr.peak();
    match pr.mode {
        Mode::Concurrency if peak > f64::from(caps.max_concurrency) => {
            return Err(format!("{peak} calls at a time is more than this agent's {}", caps.max_concurrency));
        }
        Mode::Rate if peak > caps.max_rate => {
            return Err(format!("{peak} calls a second is more than this agent's {}", caps.max_rate));
        }
        Mode::Rate => match pr.max_in_flight {
            Some(n) if n >= 1 && n <= caps.max_concurrency => {}
            _ => return Err(format!("rate mode needs max_in_flight, 1 to {}", caps.max_concurrency)),
        },
        Mode::Concurrency => {}
    }
    if !(1..=MAX_TIMEOUT_S).contains(&pr.timeout_s) {
        return Err(format!("the timeout is 1 to {MAX_TIMEOUT_S} s"));
    }
    if pr.drain_s > MAX_DRAIN_S {
        return Err(format!("the drain is at most {MAX_DRAIN_S} s"));
    }
    if pr.max_calls == Some(0) {
        return Err("max_calls is at least 1 when set".into());
    }
    if pr.warmup_connections > caps.max_concurrency {
        return Err("more warm-up connections than calls at a time".into());
    }
    if !LEASE_S.contains(&p.lease_s) {
        return Err(format!("the lease is {} to {} s", LEASE_S.start(), LEASE_S.end()));
    }
    Ok(base)
}

/// One `/`, then no scheme, backslash, space or control character: the path goes below the
/// base URL and must not lead anywhere else.
fn check_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') || path.starts_with("//") {
        return Err("the path starts with a single /".into());
    }
    if path.contains("://") || path.contains('\\') || path.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the path may not hold a scheme, backslashes, spaces or control characters".into());
    }
    if path.len() > MAX_PATH {
        return Err("the path is too long".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{AuthHeader, HttpVersion, Profile, RequestSpec, Stage};

    fn caps() -> Caps {
        Caps { max_concurrency: 100, max_rate: 50.0, max_duration_s: 120, max_body_bytes: 1000 }
    }

    fn plan() -> Prepare {
        Prepare {
            contract: CONTRACT,
            run_id: "20261015-101500-a1b2c3".into(),
            part: 0,
            parts: 1,
            key: "pgw_k_secret".into(),
            auth_header: AuthHeader::Authorization,
            base_url: None,
            request: RequestSpec {
                method: "POST".into(),
                path: "/v1/chat/completions".into(),
                headers: [("x-mock".to_owned(), "delay=10".to_owned())].into(),
                body: "{}".into(),
                stream: false,
            },
            profile: Profile {
                mode: Mode::Concurrency,
                stages: vec![Stage { duration_s: 10, target: 100.0 }],
                max_in_flight: None,
                timeout_s: 30,
                max_calls: None,
                drain_s: 10,
                http: HttpVersion::Http1,
                warmup_connections: 10,
            },
            lease_s: 30,
        }
    }

    fn own() -> url::Url {
        url::Url::parse("http://core:8080").unwrap()
    }

    #[test]
    fn a_good_plan_goes_to_the_agents_own_target_or_its_base_url() {
        assert_eq!(check(&plan(), &caps(), &own()).unwrap().as_str(), "http://core:8080/");
        let mut p = plan();
        p.base_url = Some("http://lite.internal:14080".into());
        assert_eq!(check(&p, &caps(), &own()).unwrap().host_str(), Some("lite.internal"));
    }

    #[test]
    fn plans_outside_the_caps_or_the_rules_are_refused() {
        let refused = |change: fn(&mut Prepare), says: &str| {
            let mut p = plan();
            change(&mut p);
            let e = check(&p, &caps(), &own()).unwrap_err();
            assert!(e.contains(says), "{e:?} does not say {says:?}");
        };
        refused(|p| p.contract = 2, "contract");
        refused(|p| p.part = 1, "does not exist");
        refused(|p| p.base_url = Some("http://u:p@lite".into()), "user name");
        refused(|p| p.base_url = Some("http://lite/?x=1".into()), "query");
        refused(|p| p.base_url = Some("file:///etc/passwd".into()), "not an http");
        refused(|p| p.request.method = "DELETE".into(), "GET or POST");
        refused(|p| p.request.path = "//evil.example/x".into(), "single /");
        refused(|p| p.request.path = "/a b".into(), "spaces");
        refused(|p| p.request.path = "/http://x".into(), "scheme");
        refused(|p| _ = p.request.headers.insert("Authorization".into(), "x".into()), "set by the agent");
        refused(|p| _ = p.request.headers.insert("x-a".into(), "a\r\nb".into()), "line break");
        refused(|p| p.request.body = "x".repeat(1001), "larger");
        refused(|p| p.profile.stages[0].target = 101.0, "at a time");
        refused(|p| p.profile.stages[0].duration_s = 121, "longer");
        refused(|p| p.profile.stages[0].duration_s = 0, "no time");
        refused(|p| p.profile.stages[0].target = f64::NAN, "0 or more");
        refused(
            |p| {
                p.profile.mode = Mode::Rate;
                p.profile.stages[0].target = 10.0;
            },
            "max_in_flight",
        );
        refused(
            |p| {
                p.profile.mode = Mode::Rate;
                p.profile.max_in_flight = Some(10);
            },
            "calls a second",
        );
        refused(|p| p.profile.timeout_s = 0, "timeout");
        refused(|p| p.profile.max_calls = Some(0), "max_calls");
        refused(|p| p.lease_s = 1, "lease");
    }
}
