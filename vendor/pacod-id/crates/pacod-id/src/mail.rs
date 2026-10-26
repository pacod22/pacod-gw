//! Account mail: one layout per brand, the strings of the hosted UI's
//! catalogues (ui/messages/*.json, keys mail_*), every mail in the realm's mail languages at once
//! (by default Bulgarian, then English), queued in mail_outbox and sent by whichever instance claims it first (row locks).
//!
//! The outbox keeps a mail's parameters sealed under the key-encryption key (they hold links
//! with one-time tokens) and blanks them once the mail is sent.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::Context;
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde_json::Value;

use crate::config::Realm;
use crate::crypto::envelope;
use crate::http::Shared;

/// The mails pacod-id sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    VerifyEmail,
    ResetPassword,
    AccountExists,
    PasswordChanged,
    EmailChanged,
    NewSignIn,
    SessionRevokedReuse,
    Invitation,
}

impl Template {
    pub fn name(self) -> &'static str {
        match self {
            Template::VerifyEmail => "verify_email",
            Template::ResetPassword => "reset_password",
            Template::AccountExists => "account_exists",
            Template::PasswordChanged => "password_changed",
            Template::EmailChanged => "email_changed",
            Template::NewSignIn => "new_sign_in",
            Template::SessionRevokedReuse => "session_revoked_reuse",
            Template::Invitation => "invitation",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        ALL.iter().copied().find(|t| t.name() == name)
    }

    /// The catalogue keys of the paragraphs, in order, after `mail_<name>_`.
    fn lines(self) -> &'static [&'static str] {
        match self {
            Template::VerifyEmail | Template::ResetPassword | Template::Invitation => &["intro", "expiry", "ignore"],
            Template::AccountExists => &["intro", "hint"],
            Template::PasswordChanged | Template::EmailChanged | Template::NewSignIn => &["intro", "not_you"],
            Template::SessionRevokedReuse => &["intro", "advice"],
        }
    }

    /// Whether the mail has a button (to the `link` parameter).
    fn has_button(self) -> bool {
        !matches!(self, Template::EmailChanged | Template::NewSignIn)
    }
}

const ALL: [Template; 8] = [
    Template::VerifyEmail,
    Template::ResetPassword,
    Template::AccountExists,
    Template::PasswordChanged,
    Template::EmailChanged,
    Template::NewSignIn,
    Template::SessionRevokedReuse,
    Template::Invitation,
];

/// The hosted UI's message catalogues, by language.
static CATALOG: LazyLock<HashMap<&'static str, HashMap<String, String>>> = LazyLock::new(|| {
    let read = |text: &str| -> HashMap<String, String> {
        let v: Value = serde_json::from_str(text).expect("a message catalogue is JSON");
        v.as_object()
            .expect("a message catalogue is a map")
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
            .collect()
    };
    HashMap::from([
        ("en", read(include_str!("../../../ui/messages/en.json"))),
        ("bg", read(include_str!("../../../ui/messages/bg.json"))),
        ("de", read(include_str!("../../../ui/messages/de.json"))),
        ("ru", read(include_str!("../../../ui/messages/ru.json"))),
    ])
});

fn message(locale: &str, key: &str) -> String {
    CATALOG
        .get(locale)
        .and_then(|m| m.get(key))
        .or_else(|| CATALOG["en"].get(key))
        .cloned()
        .unwrap_or_else(|| key.to_owned())
}

/// A brand's colours for mail, from the design package's tokens (light values).
struct Palette {
    bg: String,
    surface: String,
    border: String,
    text: String,
    muted: String,
    accent: String,
    monogram: String,
}

static PALETTES: LazyLock<HashMap<String, Palette>> = LazyLock::new(|| {
    let color: Value =
        serde_json::from_str(include_str!("../../../packages/ui/tokens/color.json")).expect("color.json");
    let brands: Value =
        serde_json::from_str(include_str!("../../../packages/ui/tokens/brands.json")).expect("brands.json");
    let light = |overrides: &Value, name: &str| -> String {
        overrides["color"][name][0].as_str().or_else(|| color[name][0].as_str()).unwrap_or("#000000").to_owned()
    };
    let mut out = HashMap::new();
    for (brand, b) in brands.as_object().expect("brands.json is a map") {
        if brand.starts_with('$') {
            continue;
        }
        out.insert(
            brand.clone(),
            Palette {
                bg: light(b, "bg"),
                surface: light(b, "surface"),
                border: light(b, "border"),
                text: light(b, "text"),
                muted: light(b, "text-muted"),
                accent: light(b, "accent"),
                monogram: b["monogram"].as_str().unwrap_or("ID").to_owned(),
            },
        );
    }
    out
});

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Replaces `{name}` with the parameter, escaped for HTML when `html`.
fn fill(text: &str, params: &BTreeMap<String, String>, html: bool) -> String {
    let mut out = text.to_owned();
    for (k, v) in params {
        let v = if html { escape(v) } else { v.clone() };
        out = out.replace(&format!("{{{k}}}"), &v);
    }
    if html { out } else { out.replace("&nbsp;", " ") }
}

/// A rendered mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// Renders a mail in the realm's mail languages, in their order.
pub fn render(template: Template, realm: &Realm, params: &BTreeMap<String, String>) -> Rendered {
    let locales: Vec<&str> = if realm.mail_locales.is_empty() {
        vec!["en"]
    } else {
        realm.mail_locales.iter().map(String::as_str).collect()
    };
    let palette = &PALETTES[realm.brand.as_str()];
    let brand_key = format!("mail_brand_{}", realm.brand);
    let brands: Vec<String> = locales.iter().map(|l| message(l, &brand_key)).collect();
    let mut brand_names = brands.clone();
    brand_names.dedup();
    let key = |part: &str| format!("mail_{}_{part}", template.name());
    let with_brand = |l: usize| {
        let mut p = params.clone();
        p.insert("brand".into(), brands[l].clone());
        p
    };

    let subjects: Vec<String> =
        locales.iter().enumerate().map(|(i, l)| fill(&message(l, &key("subject")), &with_brand(i), false)).collect();
    let subject = format!("{} — {}", subjects.join(" · "), brand_names.join(" · "));
    let link = params.get("link").cloned().unwrap_or_default();

    let mut text = String::new();
    let mut sections = String::new();
    for (i, l) in locales.iter().enumerate() {
        let p = with_brand(i);
        if i > 0 {
            text.push_str("\n\n— — —\n\n");
            sections.push_str(&format!(
                r#"<tr><td style="padding:0 28px"><hr style="border:0;border-top:1px solid {};margin:8px 0"></td></tr>"#,
                palette.border
            ));
        }
        text.push_str(&subjects[i]);
        text.push('\n');
        text.push_str(&"=".repeat(subjects[i].chars().count()));
        text.push_str("\n\n");
        let mut body = format!(
            r#"<h1 style="margin:0 0 14px;font-size:20px;line-height:1.35;font-weight:700">{}</h1>"#,
            escape(&subjects[i])
        );
        for (n, part) in template.lines().iter().enumerate() {
            let line = message(l, &key(part));
            text.push_str(&fill(&line, &p, false));
            text.push_str("\n\n");
            body.push_str(&format!(
                r#"<p style="margin:0 0 14px;font-size:15px;line-height:1.55;color:{}">{}</p>"#,
                if n == 0 { &palette.text } else { &palette.muted },
                fill(&line, &p, true)
            ));
            if n == 0 && template.has_button() && !link.is_empty() {
                let label = message(l, &key("button"));
                text.push_str(&format!("{label}: {link}\n\n"));
                body.push_str(&format!(
                    r#"<p style="margin:22px 0"><a href="{href}" style="display:inline-block;background:{accent};color:#ffffff;padding:12px 22px;border-radius:999px;text-decoration:none;font-weight:600;font-size:15px">{label}</a></p>"#,
                    href = escape(&link),
                    accent = palette.accent,
                    label = escape(&label)
                ));
            }
        }
        if template.has_button() && !link.is_empty() {
            body.push_str(&format!(
                r#"<p style="margin:0 0 14px;font-size:12px;line-height:1.5;color:{muted}">{fallback}<br><a href="{href}" style="color:{muted};word-break:break-all">{href}</a></p>"#,
                muted = palette.muted,
                fallback = escape(&message(l, "mail_link_fallback")),
                href = escape(&link)
            ));
        }
        let footer = fill(&message(l, "mail_footer"), &p, false);
        text.push_str(&footer);
        body.push_str(&format!(
            r#"<p style="margin:18px 0 0;font-size:12px;line-height:1.5;color:{}">{}</p>"#,
            palette.muted,
            escape(&footer)
        ));
        sections.push_str(&format!(r#"<tr><td lang="{l}" style="padding:12px 28px 20px">{body}</td></tr>"#));
    }

    let html = format!(
        r#"<!doctype html>
<html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title}</title></head>
<body style="margin:0;padding:0;background:{bg};color:{text};font-family:Inter,'Segoe UI',Roboto,Helvetica,Arial,sans-serif">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="background:{bg}"><tr><td align="center" style="padding:32px 16px">
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" style="max-width:540px;background:{surface};border:1px solid {border};border-radius:18px">
<tr><td style="padding:28px 28px 8px"><span style="display:inline-block;min-width:40px;height:40px;line-height:40px;padding:0 6px;text-align:center;border-radius:10px;background:{accent};color:#ffffff;font-weight:800;font-size:15px">{monogram}</span></td></tr>
{sections}
</table></td></tr></table></body></html>
"#,
        lang = locales[0],
        title = escape(&subject),
        bg = palette.bg,
        text = palette.text,
        surface = palette.surface,
        border = palette.border,
        accent = palette.accent,
        monogram = escape(&palette.monogram),
    );
    Rendered { subject, text, html }
}

/// How mail leaves: SMTP, or (in tests) a list in memory.
#[derive(Clone)]
pub enum Transport {
    Smtp(Box<(AsyncSmtpTransport<Tokio1Executor>, Mailbox)>),
    Memory(Arc<Mutex<Vec<Sent>>>),
}

impl std::fmt::Debug for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Smtp(s) => write!(f, "Transport::Smtp({})", s.1),
            Transport::Memory(_) => f.write_str("Transport::Memory"),
        }
    }
}

/// A mail the memory transport kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub to: String,
    pub template: String,
    pub mail: Rendered,
}

/// SMTP settings (PID_SMTP_*, PID_EMAIL_FROM_*).
#[derive(Debug, Clone)]
pub struct Smtp {
    pub host: String,
    pub port: Option<u16>,
    /// starttls (the default), tls or none.
    pub tls: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub from_address: String,
    pub from_name: String,
}

impl Transport {
    pub fn smtp(s: &Smtp) -> anyhow::Result<Self> {
        let builder = match s.tls.as_str() {
            "tls" => AsyncSmtpTransport::<Tokio1Executor>::relay(&s.host)?,
            "none" => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&s.host),
            _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&s.host)?,
        };
        let mut builder = builder.timeout(Some(Duration::from_secs(20)));
        if let Some(port) = s.port {
            builder = builder.port(port);
        }
        if let (Some(u), Some(p)) = (&s.username, &s.password) {
            builder = builder.credentials(Credentials::new(u.clone(), p.clone()));
        }
        let from = Mailbox::new(Some(s.from_name.clone()), s.from_address.parse().context("PID_EMAIL_FROM_ADDRESS")?);
        Ok(Transport::Smtp(Box::new((builder.build(), from))))
    }

    async fn send(&self, to: &str, template: &str, mail: Rendered) -> anyhow::Result<()> {
        match self {
            Transport::Memory(sent) => {
                sent.lock().expect("the mail list").push(Sent {
                    to: to.to_owned(),
                    template: template.to_owned(),
                    mail,
                });
                Ok(())
            }
            Transport::Smtp(smtp) => {
                let (transport, from) = &**smtp;
                let message = Message::builder()
                    .from(from.clone())
                    .to(to.parse().context("the recipient's address")?)
                    .subject(mail.subject)
                    .multipart(MultiPart::alternative_plain_html(mail.text, mail.html))?;
                transport.send(message).await?;
                Ok(())
            }
        }
    }
}

/// Queues a mail. Its parameters are sealed, bound to the recipient.
pub async fn enqueue(
    st: &Shared,
    template: Template,
    realm: &str,
    to: &str,
    params: BTreeMap<String, String>,
) -> anyhow::Result<()> {
    if st.mail.is_none() {
        return Ok(());
    }
    let sealed = envelope::seal(st.boot.kek(), to, &serde_json::to_vec(&params)?);
    st.store.mail_enqueue(template.name(), realm, to, &serde_json::to_value(sealed)?).await?;
    Ok(())
}

/// Sends what is due. Returns how many mails went out. Every instance runs this every few
/// seconds, the outbox's row locks keeping a mail from going twice; tests call it directly.
pub async fn deliver_due(st: &Shared) -> anyhow::Result<usize> {
    let Some(transport) = &st.mail else { return Ok(0) };
    let mut sent = 0;
    for row in st.store.mail_due(20).await? {
        let result: anyhow::Result<()> = async {
            let sealed: envelope::Sealed = serde_json::from_value(row.params.clone())?;
            let params: BTreeMap<String, String> =
                serde_json::from_slice(&envelope::open(&st.boot.keks, &row.to_address, &sealed)?)?;
            let realm = st.config.realms.get(&row.realm).context("the mail's realm is no longer configured")?;
            let template = Template::from_name(&row.template).context("unknown template")?;
            transport.send(&row.to_address, &row.template, render(template, realm, &params)).await
        }
        .await;
        match result {
            Ok(()) => {
                st.store.mail_sent(row.id).await?;
                sent += 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, template = %row.template, "a mail was not sent");
                st.store.mail_failed(row.id, &e.to_string()).await?;
            }
        }
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn realm(brand: &str, locales: &[&str]) -> Realm {
        let mut c = Config::from_toml(&format!(
            "[[realm]]\nname = \"r\"\nissuer = \"https://id.example\"\nbrand = \"{brand}\"\n"
        ))
        .unwrap();
        let mut r = c.realms.remove("r").unwrap();
        r.mail_locales = locales.iter().map(|s| (*s).to_owned()).collect();
        r
    }

    fn params(link: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("email".into(), "ada@example.com".into()),
            ("hours".into(), "24".into()),
            ("link".into(), link.into()),
        ])
    }

    #[test]
    fn every_catalogue_has_every_mail_string() {
        for t in ALL {
            for l in ["en", "bg", "de", "ru"] {
                for part in t.lines().iter().copied().chain(["subject"]).chain(t.has_button().then_some("button")) {
                    let k = format!("mail_{}_{part}", t.name());
                    assert!(CATALOG[l].contains_key(&k), "{l}: {k}");
                }
            }
        }
    }

    #[test]
    fn every_brand_has_a_palette_and_a_name_in_every_catalogue() {
        for brand in crate::config::BRANDS {
            assert!(PALETTES.contains_key(brand), "{brand}: not in packages/ui/tokens/brands.json");
            for l in ["en", "bg", "de", "ru"] {
                assert!(CATALOG[l].contains_key(&format!("mail_brand_{brand}")), "{l}: mail_brand_{brand}");
            }
        }
    }

    #[test]
    fn the_cms_names_itself_in_the_base_colours() {
        let m =
            render(Template::VerifyEmail, &realm("cms", &["en"]), &params("https://id.example/verify-email?token=x"));
        assert!(m.subject.ends_with("— pacod-cms"), "{}", m.subject);
        assert!(m.html.contains("#0e6e57"), "the base teal");
    }

    #[test]
    fn the_kb_names_itself_in_the_base_colours() {
        let m =
            render(Template::VerifyEmail, &realm("kb", &["en"]), &params("https://id.example/verify-email?token=x"));
        assert!(m.subject.ends_with("— pacod-kb"), "{}", m.subject);
        assert!(m.html.contains("#0e6e57"), "the base teal");
    }

    #[test]
    fn bulgarian_comes_first_then_english() {
        let m = render(
            Template::VerifyEmail,
            &realm("gateway", &["bg", "en"]),
            &params("https://id.example/verify-email?token=abc"),
        );
        assert_eq!(m.subject, "Потвърдете имейл адреса си · Confirm your email address — pacod-gw");
        let bg = m.text.find("Потвърдете, че ada@example.com").unwrap();
        let en = m.text.find("Confirm that ada@example.com").unwrap();
        assert!(bg < en);
        assert!(m.text.contains("Confirm email address: https://id.example/verify-email?token=abc"));
        assert!(m.html.contains(r#"lang="bg""#) && m.html.contains(r#"lang="en""#));
        assert!(m.html.contains("#0e6e57"), "the gateway's teal");
    }

    #[test]
    fn bazaar_names_itself_per_language_and_uses_its_colours() {
        let m = render(
            Template::ResetPassword,
            &realm("bazaar", &["bg", "en"]),
            &params("https://id.bazaar.example/reset-password?token=x"),
        );
        assert!(m.subject.ends_with("— Базар · Bazaar"), "{}", m.subject);
        assert!(m.html.contains("#a2542a"), "Bazaar's deep ember");
    }

    #[test]
    fn values_are_escaped_in_html() {
        let mut p = params("https://id.example/x?a=1&b=2");
        p.insert("email".into(), "<script>@example.com".into());
        let m = render(Template::VerifyEmail, &realm("shop", &["en"]), &p);
        assert!(!m.html.contains("<script>") && m.html.contains("&lt;script&gt;"));
        assert!(m.html.contains("a=1&amp;b=2"));
        assert!(m.text.contains("<script>@example.com"), "plain text stays plain");
    }
}
