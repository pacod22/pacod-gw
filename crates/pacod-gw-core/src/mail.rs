//! Outgoing email for account messages: email verification and password reset.
//!
//! Each message has a plain-text part and an HTML alternative. Links point at the console
//! (`PGW_CONSOLE_URL`) and carry a one-time token; the token never appears in logs.

use std::time::Duration;

use anyhow::Context;
use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

use crate::bootstrap::{EmailConfig, SmtpTls};

/// The product name the emails are signed with.
const BRAND: &str = "PACOD AI Gateway";

pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    console_url: String,
}

/// One message, before it is addressed.
#[derive(Debug, Clone)]
pub struct Mail {
    pub subject: String,
    pub text: String,
    pub html: String,
}

impl Mailer {
    pub fn new(cfg: &EmailConfig) -> anyhow::Result<Self> {
        let host = cfg.smtp_host.as_str();
        let builder = match cfg.smtp_tls {
            SmtpTls::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?,
            SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host)?,
            SmtpTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
        };
        let mut builder = builder.port(cfg.smtp_port).timeout(Some(Duration::from_secs(cfg.smtp_timeout_seconds)));
        if let Some(user) = &cfg.smtp_username {
            builder =
                builder.credentials(Credentials::new(user.clone(), cfg.smtp_password.clone().unwrap_or_default()));
        }
        let from = Mailbox::new(
            Some(cfg.from_name.clone()),
            cfg.from_address.parse().with_context(|| format!("the sender address \"{}\"", cfg.from_address))?,
        );
        Ok(Self { transport: builder.build(), from, console_url: cfg.console_url.clone() })
    }

    /// A console link with a one-time token, for example `/reset-password`.
    pub fn link(&self, path: &str, token: &str) -> String {
        format!("{}{path}?token={token}", self.console_url)
    }

    pub async fn send(&self, to: &str, mail: Mail) -> anyhow::Result<()> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(to.parse().with_context(|| format!("the recipient address \"{to}\""))?)
            .subject(mail.subject)
            .multipart(MultiPart::alternative_plain_html(mail.text, mail.html))?;
        self.transport.send(message).await.context("the mail server refused the message")?;
        Ok(())
    }

    /// Connects, secures the connection and signs in, without sending anything.
    pub async fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.transport.test_connection().await.context("connecting to the mail server")?,
            "the mail server did not accept the connection"
        );
        Ok(())
    }

    /// A test message, for `pacod-gw-core email-check --send-to`.
    pub async fn send_test(&self, to: &str) -> anyhow::Result<()> {
        let text = format!(
            "This is a test message from {BRAND}.\n\nThe console links in account emails point to:\n{}\n\n— {BRAND}",
            self.console_url
        );
        let message = Message::builder()
            .from(self.from.clone())
            .to(to.parse().with_context(|| format!("the recipient address \"{to}\""))?)
            .subject(format!("Test message — {BRAND}"))
            .singlepart(SinglePart::builder().header(ContentType::TEXT_PLAIN).body(text))?;
        self.transport.send(message).await.context("the mail server refused the message")?;
        Ok(())
    }
}

/// Asks the recipient to confirm their address.
pub fn verification(display_name: &str, email: &str, link: &str, valid_hours: u32) -> Mail {
    let valid = plural(valid_hours, "hour");
    let text = format!(
        "Hello {display_name},\n\n\
         An account for the {BRAND} console was set up for {email}. Please confirm that this \
         address is yours by opening the link below:\n{link}\n\n\
         The link is valid for {valid}. If you did not expect this email, you can ignore it.\n\n— {BRAND}"
    );
    let html = layout(
        "Confirm your email address",
        &format!(
            "<p>Hello {name},</p><p>An account for the {BRAND} console was set up for <strong>{email}</strong>. \
             Please confirm that this address is yours.</p>",
            name = escape(display_name),
            email = escape(email),
        ),
        "Verify email",
        link,
        &format!("The link is valid for {valid}. If you did not expect this email, you can ignore it."),
    );
    Mail { subject: format!("Verify your email — {BRAND}"), text, html }
}

/// Carries the link to choose a new password.
pub fn password_reset(display_name: &str, link: &str, valid_minutes: u32) -> Mail {
    let valid = if valid_minutes.is_multiple_of(60) {
        plural(valid_minutes / 60, "hour")
    } else {
        plural(valid_minutes, "minute")
    };
    let text = format!(
        "Hello {display_name},\n\n\
         We received a request to reset the password of your {BRAND} console account. Open the \
         link below to choose a new password:\n{link}\n\n\
         The link is valid for {valid} and works once. If you did not ask for this, you can \
         ignore this email: your password stays as it is.\n\n— {BRAND}"
    );
    let html = layout(
        "Reset your password",
        &format!(
            "<p>Hello {name},</p><p>We received a request to reset the password of your {BRAND} console \
             account. Choose a new password with the button below.</p>",
            name = escape(display_name),
        ),
        "Reset password",
        link,
        &format!(
            "The link is valid for {valid} and works once. If you did not ask for this, you can ignore \
             this email: your password stays as it is."
        ),
    );
    Mail { subject: format!("Reset your password — {BRAND}"), text, html }
}

fn plural(n: u32, unit: &str) -> String {
    if n == 1 { format!("1 {unit}") } else { format!("{n} {unit}s") }
}

/// The HTML frame of every message, in the console's colours. Inline styles only: mail
/// clients drop style sheets.
fn layout(heading: &str, intro_html: &str, button: &str, link: &str, note: &str) -> String {
    let link = escape(link);
    format!(
        r#"<!doctype html>
<html lang="en"><body style="margin:0;padding:24px 12px;background:#fbfbf8;color:#13211d;font-family:Inter,'Segoe UI',Roboto,Helvetica,Arial,sans-serif;font-size:15px;line-height:1.5">
<table role="presentation" width="100%" cellspacing="0" cellpadding="0"><tr><td align="center">
<table role="presentation" width="100%" cellspacing="0" cellpadding="0" style="max-width:480px;background:#ffffff;border:1px solid #e8e7e0;border-radius:18px">
<tr><td style="padding:32px">
<div style="display:inline-block;width:44px;height:44px;line-height:44px;border-radius:11px;background:#0e6e57;color:#ffffff;font-weight:800;font-size:16px;text-align:center">GW</div>
<h1 style="margin:20px 0 12px;font-size:20px;line-height:1.3;color:#13211d">{heading}</h1>
{intro_html}
<p style="margin:24px 0"><a href="{link}" style="display:inline-block;padding:10px 22px;border-radius:999px;background:#0e6e57;color:#ffffff;font-weight:600;text-decoration:none">{button}</a></p>
<p style="color:#5e6e68;font-size:13px">{note}</p>
<p style="color:#5e6e68;font-size:13px;word-break:break-all">If the button does not work, copy this address into your browser:<br><a href="{link}" style="color:#0e6e57">{link}</a></p>
</td></tr></table>
<p style="margin-top:16px;color:#8a978f;font-size:11px;letter-spacing:0.12em;text-transform:uppercase">{BRAND}</p>
</td></tr></table>
</body></html>"#,
        heading = escape(heading),
        button = escape(button),
        note = escape(note),
    )
}

/// Escapes text for HTML: names and addresses come from users.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_escaped_in_html_but_not_in_text() {
        let m = verification("<b>Eve</b> & co", "eve@example.com", "https://c.example/verify-email?token=abc", 24);
        assert!(m.html.contains("&lt;b&gt;Eve&lt;/b&gt; &amp; co"));
        assert!(!m.html.contains("<b>Eve"));
        assert!(m.text.contains("Hello <b>Eve</b> & co,"));
        assert!(m.text.contains("https://c.example/verify-email?token=abc"));
        assert!(m.text.contains("24 hours"));
    }

    #[test]
    fn link_lifetimes_follow_the_settings() {
        assert!(password_reset("A", "x", 60).text.contains("valid for 1 hour "));
        assert!(password_reset("A", "x", 90).text.contains("valid for 90 minutes"));
        assert!(verification("A", "a@b", "x", 1).text.contains("valid for 1 hour."));
    }
}
