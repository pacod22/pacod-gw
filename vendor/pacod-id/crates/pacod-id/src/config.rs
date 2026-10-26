//! Realms and clients: the file PID_CONFIG names, in TOML.
//!
//! ```toml
//! [[realm]]
//! name = "gateway"
//! issuer = "https://id.example.com"
//! brand = "gateway"
//!
//! [[client]]
//! id = "pacod-gw-console"
//! realm = "gateway"
//! kind = "confidential"
//! secret_env = "PGW_UI_CLIENT_SECRET"
//! redirect_uris = ["https://console.example.com/auth/callback"]
//! audience = "pacod-gw"
//! ```

use std::collections::{BTreeMap, BTreeSet};

use pacod_id_jose::Alg;
use serde::Deserialize;
use url::Url;

/// The built-in brands; they choose the hosted UI's theme and the mail layout.
pub const BRANDS: [&str; 5] = ["gateway", "cms", "kb", "shop", "bazaar"];
/// The hosted UI's languages.
pub const LOCALES: [&str; 4] = ["en", "bg", "de", "ru"];

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default, rename = "realm")]
    realms: Vec<Realm>,
    #[serde(default, rename = "client")]
    clients: Vec<Client>,
}

fn default_locales() -> Vec<String> {
    LOCALES.iter().map(|s| (*s).to_owned()).collect()
}
fn en() -> String {
    "en".into()
}
fn bg_en() -> Vec<String> {
    vec!["bg".into(), "en".into()]
}
fn twelve() -> usize {
    12
}
fn twelve_hours() -> u32 {
    12
}
fn seven() -> u32 {
    7
}
fn day() -> u32 {
    24
}
fn hour() -> u32 {
    60
}

/// One issuer host with its brand and policies.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Realm {
    pub name: String,
    /// The public URL, without a trailing slash: the `iss` of every token of the realm.
    pub issuer: String,
    pub brand: String,
    #[serde(default = "default_locales")]
    pub locales: Vec<String>,
    #[serde(default = "en")]
    pub default_locale: String,
    /// The languages of the realm's mails, all in one mail (by default Bulgarian, then English).
    #[serde(default = "bg_en")]
    pub mail_locales: Vec<String>,
    /// Whether anyone may create an account here (clients may turn it off).
    #[serde(default)]
    pub self_signup: bool,
    /// The shortest password set in this realm.
    #[serde(default = "twelve")]
    pub password_min_length: usize,
    /// The shortest password of an account that holds any role beyond the implicit ones,
    /// wherever it sets it.
    #[serde(default = "twelve")]
    pub role_holder_password_min_length: usize,
    /// A browser's sign-in without "keep me signed in".
    #[serde(default = "twelve_hours")]
    pub session_hours: u32,
    /// "Keep me signed in", in days.
    #[serde(default = "seven")]
    pub remember_me_days: u32,
    #[serde(default = "day")]
    pub verification_link_hours: u32,
    #[serde(default = "hour")]
    pub reset_link_minutes: u32,
    #[serde(default = "seven")]
    pub invitation_days: u32,
    /// Roles that let their holders create, disable and reset accounts in this realm (in the
    /// gateway realm: gw.platform_admin).
    #[serde(default)]
    pub accounts_managed_by: Vec<String>,
    #[serde(default)]
    pub terms_url: Option<String>,
    /// The version of the terms a sign-up accepts (recorded in consents).
    #[serde(default = "first")]
    pub terms_version: String,
    #[serde(default)]
    pub privacy_url: Option<String>,
    /// Whether an account must confirm its address before it signs in. Only when mail can be
    /// sent: without SMTP no confirmation is possible, and none is asked for.
    #[serde(default = "yes")]
    pub require_verified_email: bool,
    /// The realm's Google client: its own, so Google's consent screen shows the
    /// realm's brand.
    #[serde(default)]
    pub google: Option<Upstream>,
}

/// An OpenID provider people sign in with: Google, or a stand-in in tests and development.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub client_id: String,
    /// The variable (or `<NAME>_FILE`, or a file in PID_SECRETS_DIR) holding the client secret.
    pub secret_env: String,
    #[serde(default = "google_issuer")]
    pub issuer: String,
    /// Where pacod-id itself reaches the provider (discovery, token and keys) when that is not
    /// the address browsers use, for a provider in a container next to it.
    #[serde(default)]
    pub server_url: Option<String>,
}

fn google_issuer() -> String {
    "https://accounts.google.com".into()
}

fn first() -> String {
    "1".into()
}
fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// A server-side application (a BFF) that keeps a secret.
    Confidential,
    /// An app that cannot keep a secret (Android); PKCE alone protects its codes.
    Public,
    /// A product's backend: client credentials and token exchange, no people.
    Service,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grant {
    AuthorizationCode,
    RefreshToken,
    ClientCredentials,
    TokenExchange,
}

/// For one client only (Open WebUI), a `roles` claim in the id_token and userinfo
///: each value whose permission the account holds, else `default`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RolesClaim {
    pub values: BTreeMap<String, String>,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub id: String,
    pub realm: String,
    pub kind: ClientKind,
    /// The variable (or `<NAME>_FILE`, or a file in PID_SECRETS_DIR) holding the client secret.
    /// Without it, the secret set by `pacod-id clients secret` counts.
    #[serde(default)]
    pub secret_env: Option<String>,
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    #[serde(default)]
    pub post_logout_redirect_uris: Vec<String>,
    #[serde(default)]
    pub backchannel_logout_uri: Option<String>,
    /// The product the client's access tokens are for (their `aud`).
    #[serde(default)]
    pub audience: Option<String>,
    /// For a service client: the product it speaks for (its manifest, its scopes, its feed).
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub grants: Option<Vec<Grant>>,
    #[serde(default = "five_minutes")]
    pub access_ttl_seconds: u32,
    #[serde(default = "rs256")]
    pub id_token_alg: Alg,
    #[serde(default = "rs256")]
    pub access_token_alg: Alg,
    /// Overrides the realm's sign-up policy for this client.
    #[serde(default)]
    pub self_signup: Option<bool>,
    #[serde(default)]
    pub roles_claim: Option<RolesClaim>,
    /// Where the product starts a sign-in (OIDC client metadata): the hosted UI sends people
    /// there after they accept an invitation.
    #[serde(default)]
    pub initiate_login_uri: Option<String>,
    /// For a native app: how long a sign-in lasts from its last use, and at most.
    #[serde(default)]
    pub refresh_idle_days: Option<u32>,
    #[serde(default)]
    pub refresh_absolute_days: Option<u32>,
}

fn five_minutes() -> u32 {
    300
}
fn rs256() -> Alg {
    Alg::RS256
}

impl Client {
    /// The grants the client may use: as configured, or the usual ones for its kind.
    pub fn grants(&self) -> BTreeSet<Grant> {
        match &self.grants {
            Some(g) => g.iter().copied().collect(),
            None => match self.kind {
                ClientKind::Confidential | ClientKind::Public => [Grant::AuthorizationCode, Grant::RefreshToken].into(),
                ClientKind::Service => [Grant::ClientCredentials, Grant::TokenExchange].into(),
            },
        }
    }
}

/// The checked configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub realms: BTreeMap<String, Realm>,
    pub clients: BTreeMap<String, Client>,
}

fn ident(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some('a'..='z'))
        && c.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// An absolute http(s) URL without a fragment.
fn absolute(s: &str) -> Result<Url, String> {
    let u = Url::parse(s).map_err(|e| format!("{s:?} is not a URL: {e}"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() || u.fragment().is_some() {
        return Err(format!("{s:?} must be an http or https URL without a fragment"));
    }
    Ok(u)
}

/// The `host[:port]` a request to this URL carries in its Host header.
pub fn authority(u: &Url) -> String {
    match u.port() {
        Some(p) => format!("{}:{p}", u.host_str().unwrap_or_default()),
        None => u.host_str().unwrap_or_default().to_owned(),
    }
}

/// Puts the environment's values in for `${NAME}` (a name of letters, digits and `_`), so that one
/// file serves installations that differ only in their host names (a compose stack's
/// `issuer = "https://${PGW_ID_HOST}"`). `$${` stands for a literal `${`. A name that is not set,
/// or set empty, is an error that names it. Comment lines are left as they are.
pub fn expand_env(text: &str, var: impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('#') {
            out.push_str(line);
        } else {
            out.push_str(&expand_line(line, &var)?);
        }
    }
    Ok(out)
}

fn expand_line(text: &str, var: &impl Fn(&str) -> Option<String>) -> anyhow::Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        if rest[..at].ends_with('$') {
            out.push_str(&rest[..at - 1]);
            out.push_str("${");
            rest = &rest[at + 2..];
            continue;
        }
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let end = after.find('}').ok_or_else(|| anyhow::anyhow!("an unclosed ${{ in the configuration"))?;
        let name = &after[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            anyhow::bail!("${{{name}}} is not a variable name");
        }
        match var(name) {
            Some(value) if !value.is_empty() => out.push_str(&value),
            _ => anyhow::bail!("the configuration uses ${{{name}}}, which is not set"),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Config {
    pub fn from_toml(text: &str) -> anyhow::Result<Self> {
        let file: File = toml::from_str(text)?;
        Self::check(file.realms, file.clients).map_err(|e| anyhow::anyhow!(e))
    }

    pub fn check(realms: Vec<Realm>, clients: Vec<Client>) -> Result<Self, String> {
        let mut out = Config::default();
        let mut hosts = BTreeSet::new();
        for r in realms {
            if !ident(&r.name) {
                return Err(format!("realm {:?}: names are lowercase letters, digits, - and _", r.name));
            }
            let issuer = absolute(&r.issuer).map_err(|e| format!("realm {}: issuer {e}", r.name))?;
            if r.issuer.ends_with('/') || issuer.path() != "/" || issuer.query().is_some() {
                return Err(format!("realm {}: the issuer is a scheme and host, without path or slash", r.name));
            }
            if !hosts.insert(authority(&issuer)) {
                return Err(format!("realm {}: another realm has the same host", r.name));
            }
            if !BRANDS.contains(&r.brand.as_str()) {
                return Err(format!("realm {}: brand must be one of {BRANDS:?}", r.name));
            }
            for l in r.locales.iter().chain(&r.mail_locales).chain([&r.default_locale]) {
                if !LOCALES.contains(&l.as_str()) {
                    return Err(format!("realm {}: unknown language {l:?} (known: {LOCALES:?})", r.name));
                }
            }
            if !r.locales.contains(&r.default_locale) {
                return Err(format!("realm {}: default_locale must be one of its locales", r.name));
            }
            if !(8..=128).contains(&r.password_min_length)
                || !(r.password_min_length..=128).contains(&r.role_holder_password_min_length)
            {
                return Err(format!(
                    "realm {}: password minimums must be 8 to 128 (the common-password list starts at 8), role holders' at least the realm's",
                    r.name
                ));
            }
            if r.session_hours == 0 || r.remember_me_days == 0 {
                return Err(format!("realm {}: session lifetimes must be positive", r.name));
            }
            if let Some(g) = &r.google {
                absolute(&g.issuer).map_err(|e| format!("realm {}: google.issuer {e}", r.name))?;
                if let Some(s) = &g.server_url {
                    absolute(s).map_err(|e| format!("realm {}: google.server_url {e}", r.name))?;
                }
            }
            if out.realms.insert(r.name.clone(), r.clone()).is_some() {
                return Err(format!("realm {} appears twice", r.name));
            }
        }
        for c in clients {
            if !ident(&c.id) {
                return Err(format!("client {:?}: ids are lowercase letters, digits, - and _", c.id));
            }
            if !out.realms.contains_key(&c.realm) {
                return Err(format!("client {}: unknown realm {}", c.id, c.realm));
            }
            for u in c
                .redirect_uris
                .iter()
                .chain(&c.post_logout_redirect_uris)
                .chain(c.backchannel_logout_uri.iter())
                .chain(c.initiate_login_uri.iter())
            {
                absolute(u).map_err(|e| format!("client {}: {e}", c.id))?;
            }
            let grants = c.grants();
            match c.kind {
                ClientKind::Confidential | ClientKind::Public => {
                    if c.audience.as_deref().is_none_or(|a| !ident(a)) {
                        return Err(format!("client {}: an audience (the product, e.g. pacod-gw) is required", c.id));
                    }
                    if grants.contains(&Grant::AuthorizationCode) && c.redirect_uris.is_empty() {
                        return Err(format!("client {}: needs at least one redirect URI", c.id));
                    }
                    if grants.contains(&Grant::ClientCredentials) || grants.contains(&Grant::TokenExchange) {
                        return Err(format!(
                            "client {}: only service clients use client credentials or token exchange",
                            c.id
                        ));
                    }
                }
                ClientKind::Service => {
                    // Without a product a service only exchanges tokens (a product's tester,
                    // say): the internal API is for products' services.
                    if c.product.as_deref().is_some_and(|p| !ident(p)) {
                        return Err(format!("client {}: a service client's product is a short name", c.id));
                    }
                    if grants.contains(&Grant::AuthorizationCode) || grants.contains(&Grant::RefreshToken) {
                        return Err(format!("client {}: service clients do not sign people in", c.id));
                    }
                    // Token exchange takes only tokens issued for the product's own audience
                    //: without one, any product's tokens would do.
                    if grants.contains(&Grant::TokenExchange) && c.audience.as_deref().is_none_or(|a| !ident(a)) {
                        return Err(format!(
                            "client {}: a service client that exchanges tokens names the audience of its \
                             product's tokens (e.g. pacod-gw)",
                            c.id
                        ));
                    }
                }
            }
            if c.kind == ClientKind::Public && c.secret_env.is_some() {
                return Err(format!("client {}: a public client has no secret", c.id));
            }
            if !(120..=900).contains(&c.access_ttl_seconds) {
                return Err(format!("client {}: access_ttl_seconds must be 120 to 900", c.id));
            }
            if out.clients.insert(c.id.clone(), c.clone()).is_some() {
                return Err(format!("client {} appears twice", c.id));
            }
        }
        Ok(out)
    }

    /// The realm whose issuer has this Host.
    pub fn realm_for_host(&self, host: &str) -> Option<&Realm> {
        let host = host.to_ascii_lowercase();
        self.realms.values().find(|r| Url::parse(&r.issuer).is_ok_and(|u| authority(&u) == host))
    }
}

#[cfg(test)]
mod expand {
    use super::expand_env;

    #[test]
    fn variables_are_put_in_and_a_missing_one_is_named() {
        let var = |n: &str| match n {
            "HOST" => Some("id.example.com".to_owned()),
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        assert_eq!(
            expand_env(r#"issuer = "https://${HOST}/x""#, var).unwrap(),
            r#"issuer = "https://id.example.com/x""#
        );
        assert_eq!(expand_env("a $${HOST} b", var).unwrap(), "a ${HOST} b");
        assert_eq!(expand_env("no variables, $5", var).unwrap(), "no variables, $5");
        assert_eq!(
            expand_env("# ${NAME} in a comment\nx = \"${HOST}\"\n", var).unwrap(),
            "# ${NAME} in a comment\nx = \"id.example.com\"\n"
        );
        for (text, says) in [
            ("${NOPE}", "${NOPE}, which is not set"),
            ("${EMPTY}", "${EMPTY}, which is not set"),
            ("${HOST", "unclosed"),
            ("${A B}", "not a variable name"),
        ] {
            assert!(expand_env(text, var).unwrap_err().to_string().contains(says), "{text}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[[realm]]
name = "gateway"
issuer = "http://127.0.0.1:13070"
brand = "gateway"
accounts_managed_by = ["gw.platform_admin"]

[[realm]]
name = "bazaar"
issuer = "https://id.bazaar.example"
brand = "bazaar"
password_min_length = 8
self_signup = true

[[client]]
id = "pacod-gw-console"
realm = "gateway"
kind = "confidential"
secret_env = "PGW_UI_CLIENT_SECRET"
redirect_uris = ["http://127.0.0.1:13000/auth/callback"]
audience = "pacod-gw"

[[client]]
id = "pacod-gw-core"
realm = "gateway"
kind = "service"
product = "gw"
audience = "pacod-gw"
"#;

    #[test]
    fn a_sample_reads() {
        let c = Config::from_toml(SAMPLE).unwrap();
        let g = &c.realms["gateway"];
        assert_eq!((g.remember_me_days, g.session_hours, g.password_min_length), (7, 12, 12));
        assert_eq!(g.mail_locales, ["bg", "en"]);
        assert_eq!(c.realms["bazaar"].password_min_length, 8);
        assert_eq!(c.realm_for_host("127.0.0.1:13070").unwrap().name, "gateway");
        assert_eq!(c.realm_for_host("ID.BAZAAR.EXAMPLE").unwrap().name, "bazaar");
        assert!(c.realm_for_host("127.0.0.1:13000").is_none());
        assert_eq!(c.clients["pacod-gw-core"].grants(), [Grant::ClientCredentials, Grant::TokenExchange].into());
        assert_eq!(c.clients["pacod-gw-console"].access_ttl_seconds, 300);
    }

    #[test]
    fn mistakes_are_refused_with_the_place() {
        let bad = [
            (
                SAMPLE.replace("issuer = \"https://id.bazaar.example\"", "issuer = \"https://id.bazaar.example/\""),
                "realm bazaar",
            ),
            (SAMPLE.replace("brand = \"bazaar\"", "brand = \"nope\""), "brand"),
            (SAMPLE.replace("audience = \"pacod-gw\"", ""), "audience"),
            (SAMPLE.replace("product = \"gw\"", "product = \"G W\""), "product is a short name"),
            (SAMPLE.replace("product = \"gw\"\naudience = \"pacod-gw\"", "product = \"gw\""), "exchanges tokens"),
            (SAMPLE.replace("password_min_length = 8", "password_min_length = 3"), "password minimums"),
            (
                SAMPLE.replace("realm = \"gateway\"\nkind = \"service\"", "realm = \"nope\"\nkind = \"service\""),
                "unknown realm",
            ),
            (
                SAMPLE.replace("issuer = \"https://id.bazaar.example\"", "issuer = \"http://127.0.0.1:13070\""),
                "same host",
            ),
            (format!("{SAMPLE}\nsurprise = 1"), "unknown field"),
        ];
        for (text, needle) in bad {
            let err = Config::from_toml(&text).unwrap_err().to_string();
            assert!(err.contains(needle), "{needle}: {err}");
        }
    }
}
