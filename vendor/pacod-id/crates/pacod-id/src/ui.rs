//! The hosted pages: the SvelteKit build in `PID_UI_DIR`, read once at start.
//! Every page route answers the same page with the realm's brand, language and theme filled in,
//! so the right look shows from the first paint, under a strict Content-Security-Policy built
//! from the hashes of the page's inline scripts. Without a build the pages answer 503 and the
//! API works as before.
//!
//! The build is read from a directory rather than compiled into the binary, so that the crate
//! builds without it (products compile it for their tests from a vendored copy).

use std::collections::HashMap;
use std::path::Path;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::config::Realm;
use crate::error::ApiError;
use crate::http::Shared;
use crate::oidc::{read_cookie, realm_by_host};

/// Paths the pages never answer: the API keeps its JSON 404s.
const API_PREFIXES: &[&str] = &["/ui-api/", "/oidc/", "/federation/", "/.well-known/", "/internal/", "/admin/v1/"];
/// The cookies the pages set when a person picks a language or a theme.
pub const LANG_COOKIE: &str = "pid_lang";
pub const THEME_COOKIE: &str = "pid_theme";

struct Asset {
    body: Bytes,
    content_type: &'static str,
}

struct Page {
    template: String,
    csp: String,
}

/// The build, in memory.
#[derive(Default)]
pub struct Ui {
    files: HashMap<String, Asset>,
    page: Option<Page>,
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, e)| e) {
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        Some("webmanifest") => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

/// The sha256 hashes of the inline scripts of a page, as CSP sources.
fn script_hashes(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let Some(open_end) = after.find('>') else { break };
        let open = &after[..open_end];
        let body_start = open_end + 1;
        let Some(close) = after[body_start..].find("</script>") else { break };
        if !open.contains(" src=") {
            let body = &after[body_start..body_start + close];
            out.push(format!("'sha256-{}'", STANDARD.encode(crate::crypto::sha256(body.as_bytes()))));
        }
        rest = &after[body_start + close..];
    }
    out
}

impl Ui {
    /// Reads a build; an empty one when the directory has none (logged).
    pub fn load(dir: &Path) -> Self {
        let mut ui = Ui::default();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else { continue };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let (Ok(body), Ok(rel)) = (std::fs::read(&p), p.strip_prefix(dir)) {
                    let key = format!("/{}", rel.to_string_lossy().replace('\\', "/"));
                    ui.files.insert(key.clone(), Asset { content_type: content_type(&key), body: body.into() });
                }
            }
        }
        match ui.files.remove("/index.html").map(|a| String::from_utf8(a.body.to_vec())) {
            Some(Ok(template)) => {
                let scripts = script_hashes(&template).join(" ");
                let csp = format!(
                    "default-src 'none'; script-src 'self' {scripts}; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                     font-src 'self'; connect-src 'self'; manifest-src 'self'; form-action 'self'; frame-ancestors 'none'; \
                     base-uri 'none'; object-src 'none'"
                );
                tracing::info!(files = ui.files.len(), dir = %dir.display(), "the hosted pages are loaded");
                ui.page = Some(Page { template, csp });
            }
            _ => {
                tracing::warn!(dir = %dir.display(), "no hosted pages (PID_UI_DIR has no index.html): pages answer 503")
            }
        }
        ui
    }

    pub fn is_built(&self) -> bool {
        self.page.is_some()
    }
}

/// The language a page opens in: the person's choice, their browser's, or the realm's.
fn language(realm: &Realm, headers: &HeaderMap) -> String {
    if let Some(l) = read_cookie(headers, LANG_COOKIE).filter(|l| realm.locales.contains(l)) {
        return l;
    }
    let accept = headers.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()).unwrap_or("");
    let mut wanted: Vec<(f32, String)> = accept
        .split(',')
        .filter_map(|part| {
            let mut it = part.trim().split(';');
            let tag = it.next()?.trim().to_ascii_lowercase();
            let q = it.find_map(|p| p.trim().strip_prefix("q=")).and_then(|q| q.parse().ok()).unwrap_or(1.0);
            let primary = tag.split('-').next()?.to_owned();
            (!primary.is_empty() && q > 0.0).then_some((q, primary))
        })
        .collect();
    wanted.sort_by(|a, b| b.0.total_cmp(&a.0));
    wanted
        .into_iter()
        .map(|(_, l)| l)
        .find(|l| realm.locales.contains(l))
        .unwrap_or_else(|| realm.default_locale.clone())
}

fn page_response(st: &Shared, realm: &Realm, headers: &HeaderMap) -> Response {
    let Some(page) = &st.ui.page else {
        return (StatusCode::SERVICE_UNAVAILABLE, "The sign-in pages are not installed (PID_UI_DIR).").into_response();
    };
    let theme = match read_cookie(headers, THEME_COOKIE).as_deref() {
        Some("light") => " data-theme=\"light\"",
        Some("dark") => " data-theme=\"dark\"",
        _ => "",
    };
    let html = page
        .template
        .replacen("__PID_LANG__", &language(realm, headers), 1)
        .replacen("__PID_BRAND__", &realm.brand, 1)
        .replacen("__PID_THEME__", theme, 1);
    let mut r = ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response();
    let h = r.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&page.csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, v);
    }
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // The links from mails carry one-time tokens in the URL.
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    r
}

/// The public listener's fallback: a file of the build, the page, or the API's 404.
pub async fn serve(State(st): State<Shared>, method: Method, uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path();
    if API_PREFIXES.iter().any(|p| path.starts_with(p)) || !matches!(method, Method::GET | Method::HEAD) {
        return ApiError::not_found("no such endpoint").into_response();
    }
    if let Some(asset) = st.ui.files.get(path) {
        let cache = if path.starts_with("/_app/immutable/") {
            "public, max-age=31536000, immutable"
        } else {
            "public, max-age=300"
        };
        return (
            [(header::CONTENT_TYPE, asset.content_type), (header::CACHE_CONTROL, cache)],
            if method == Method::HEAD { Bytes::new() } else { asset.body.clone() },
        )
            .into_response();
    }
    // A missing file is not a page.
    if path.starts_with("/_app/") || path.rsplit('/').next().is_some_and(|last| last.contains('.')) {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let Some(realm) = realm_by_host(&st, &headers) else {
        return (StatusCode::NOT_FOUND, "No sign-in service is configured at this address.").into_response();
    };
    page_response(&st, realm, &headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_scripts_are_hashed_and_module_files_are_not() {
        let html = "<script src=\"/a.js\"></script><script>\n\tboot();\n</script><script type=\"module\">x()</script>";
        let hashes = script_hashes(html);
        assert_eq!(hashes.len(), 2);
        let expected = format!("'sha256-{}'", STANDARD.encode(crate::crypto::sha256(b"\n\tboot();\n")));
        assert_eq!(hashes[0], expected);
    }
}
