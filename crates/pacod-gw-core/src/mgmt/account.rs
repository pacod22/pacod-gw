//! Account emails: email verification and password reset.
//!
//! Users ask for a link on the login page and use it on the console page it points to; a
//! platform admin can send the same emails from the user list. A link carries a one-time
//! token, of which only the hash is stored (`account_tokens`).
//!
//! The public request endpoints answer the same way whether or not an account exists: the
//! lookup, the token and the email happen after the answer, and the per-address limit drops
//! requests silently.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::Deserialize;

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use super::resources::access::MIN_PASSWORD_LEN;
use crate::crypto::{password, random_bytes, sha256};
use crate::mail;
use crate::model::{Doc, Obj, Status, User};
use crate::store::{AuditEntry, TokenPurpose};

/// Links one address may ask for, per purpose, within the window.
const MAX_REQUESTS: u32 = 3;
const REQUEST_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Keeps the public endpoints from being used to flood an inbox.
#[derive(Default)]
pub struct RequestGuard {
    seen: Mutex<HashMap<(TokenPurpose, String), (u32, Instant)>>,
}

impl RequestGuard {
    fn allow(&self, purpose: TokenPurpose, email: &str) -> bool {
        let mut map = self.seen.lock();
        if map.len() > 10_000 {
            map.retain(|_, (_, since)| since.elapsed() < REQUEST_WINDOW);
        }
        let entry = map.entry((purpose, email.to_owned())).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= REQUEST_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
        entry.0 <= MAX_REQUESTS
    }
}

fn no_email() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "email_unavailable",
        "this installation does not send email; an administrator can help instead",
    )
}

pub(crate) fn invalid_token() -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_token", "this link is invalid, was already used or has expired")
}

/// Creates a token for the user and sends the email with the link.
pub async fn send_account_mail(st: &AdminState, user: &Obj<User>, purpose: TokenPurpose) -> anyhow::Result<()> {
    let mailer = st.mailer.as_ref().ok_or_else(|| anyhow::anyhow!("email is not configured"))?;
    let settings = st.config.snapshot().settings.clone();
    let token = URL_SAFE_NO_PAD.encode(random_bytes::<32>());
    let (path, lifetime) = match purpose {
        TokenPurpose::VerifyEmail => {
            ("/verify-email", chrono::Duration::hours(i64::from(settings.verification_link_hours)))
        }
        TokenPurpose::ResetPassword => {
            ("/reset-password", chrono::Duration::minutes(i64::from(settings.reset_link_minutes)))
        }
    };
    st.store
        .account_token_create(&sha256(token.as_bytes()), purpose, &user.id, &user.doc.email, Utc::now() + lifetime)
        .await?;
    let link = mailer.link(path, &token);
    let message = match purpose {
        TokenPurpose::VerifyEmail => {
            mail::verification(&user.doc.display_name, &user.doc.email, &link, settings.verification_link_hours)
        }
        TokenPurpose::ResetPassword => mail::password_reset(&user.doc.display_name, &link, settings.reset_link_minutes),
    };
    mailer.send(&user.doc.email, message).await
}

/// Sends an account email without waiting for it; a failure is logged.
pub fn send_in_background(st: &AdminState, user: Obj<User>, purpose: TokenPurpose) {
    let st = st.clone();
    tokio::spawn(async move {
        match send_account_mail(&st, &user, purpose).await {
            Ok(()) => tracing::info!(user = %user.id, purpose = purpose.as_str(), "account email sent"),
            Err(e) => {
                tracing::warn!(user = %user.id, purpose = purpose.as_str(), error = %format!("{e:#}"), "account email not sent")
            }
        }
    });
}

/// Uses up a token and returns the user it belongs to, if the account still has the address
/// the link was sent to.
async fn redeem(st: &AdminState, token: &str, purpose: TokenPurpose) -> ApiResult<Obj<User>> {
    let (user_id, email) =
        st.store.account_token_consume(&sha256(token.trim().as_bytes()), purpose).await?.ok_or_else(invalid_token)?;
    let user = st.store.get_obj::<User>(&user_id).await.map_err(|_| invalid_token())?;
    if user.doc.email != email {
        return Err(invalid_token());
    }
    Ok(user)
}

/// Stores a changed user document, with an audit entry for `actor`.
async fn save(st: &AdminState, user: &Obj<User>, doc: &User, audit: AuditEntry) -> ApiResult<()> {
    let mut tx = st.store.begin().await?;
    tx.update(&user.id, None, Some(&doc.email), &serde_json::to_value(doc).map_err(ApiError::internal)?, audit).await?;
    st.commit(tx).await?;
    Ok(())
}

fn by_user(user: &Obj<User>, action: &str) -> AuditEntry {
    AuditEntry { actor: user.id.clone(), actor_kind: "user".into(), action: action.into(), ..Default::default() }
}

// --- public: the login page and the pages the links open ---------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EmailInput {
    pub email: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PasswordResetInput {
    /// The token from the link in the email.
    pub token: String,
    #[schemars(extend("writeOnly" = true))]
    pub password: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TokenInput {
    /// The token from the link in the email.
    pub token: String,
}

/// Starts the lookup and the email after answering, so that the answer looks the same
/// whether or not the account exists.
fn request_link(st: &AdminState, email: &str, purpose: TokenPurpose) -> ApiResult<StatusCode> {
    if st.mailer.is_none() {
        return Err(no_email());
    }
    let email = email.trim().to_lowercase();
    if !st.account_requests.allow(purpose, &email) {
        return Ok(StatusCode::ACCEPTED);
    }
    let snap = st.config.snapshot();
    let user = snap.user_emails.get(&email).and_then(|id| snap.users.get(id)).map(|u| (**u).clone());
    let wanted = user.filter(|u| {
        u.doc.status == Status::Active && (purpose == TokenPurpose::ResetPassword || !u.doc.email_verified)
    });
    if let Some(user) = wanted {
        send_in_background(st, user, purpose);
    }
    Ok(StatusCode::ACCEPTED)
}

async fn request_password_reset(State(st): State<AdminState>, Json(input): Json<EmailInput>) -> ApiResult<StatusCode> {
    super::moved_to_identity(&st)?;
    request_link(&st, &input.email, TokenPurpose::ResetPassword)
}

async fn request_verification(State(st): State<AdminState>, Json(input): Json<EmailInput>) -> ApiResult<StatusCode> {
    super::moved_to_identity(&st)?;
    request_link(&st, &input.email, TokenPurpose::VerifyEmail)
}

async fn reset_password(State(st): State<AdminState>, Json(input): Json<PasswordResetInput>) -> ApiResult<StatusCode> {
    super::moved_to_identity(&st)?;
    // Checked before the token is used up, so that a short password can be corrected.
    if input.password.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::invalid(format!("password must be at least {MIN_PASSWORD_LEN} characters")));
    }
    let user = redeem(&st, &input.token, TokenPurpose::ResetPassword).await?;
    if user.doc.status != Status::Active {
        return Err(invalid_token());
    }
    let pw = input.password;
    let hash = tokio::task::spawn_blocking(move || password::hash(&pw))
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
    let mut doc = user.doc.clone();
    doc.password_hash = hash;
    // The link reached the inbox, which confirms the address too.
    doc.email_verified = true;
    save(&st, &user, &doc, by_user(&user, "reset_password")).await?;
    // Whoever was signed in with the old password is signed out.
    st.store.sessions_delete_for_user(&user.id).await?;
    st.store.account_tokens_delete_for_user(&user.id, TokenPurpose::ResetPassword).await?;
    st.logins.succeeded(&doc.email);
    Ok(StatusCode::NO_CONTENT)
}

async fn verify_email(State(st): State<AdminState>, Json(input): Json<TokenInput>) -> ApiResult<StatusCode> {
    super::moved_to_identity(&st)?;
    let user = redeem(&st, &input.token, TokenPurpose::VerifyEmail).await?;
    if !user.doc.email_verified {
        let mut doc = user.doc.clone();
        doc.email_verified = true;
        save(&st, &user, &doc, by_user(&user, "verify_email")).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

// --- platform admins: from the user list ---------------------------------------------------

async fn admin_user(st: &AdminState, p: &Principal, id: &str) -> ApiResult<Obj<User>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    st.store.get_obj::<User>(id).await.map_err(|_| ApiError::not_found("user"))
}

/// Sends an email on an admin's behalf and waits for the mail server, so that the admin
/// learns about a failure.
async fn send_for_admin(
    st: &AdminState,
    p: &Principal,
    user: &Obj<User>,
    purpose: TokenPurpose,
) -> ApiResult<StatusCode> {
    if st.mailer.is_none() {
        return Err(no_email());
    }
    send_account_mail(st, user, purpose)
        .await
        .map_err(|e| ApiError::new(StatusCode::BAD_GATEWAY, "email_failed", format!("{e:#}")))?;
    let mut audit = p.audit(None, None);
    audit.action = match purpose {
        TokenPurpose::VerifyEmail => "send_verification",
        TokenPurpose::ResetPassword => "send_password_reset",
    }
    .into();
    audit.kind = User::KIND.into();
    audit.object_id = user.id.clone();
    let _ = st.store.audit(&audit).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn send_verification(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    super::moved_to_identity(&st)?;
    let user = admin_user(&st, &p, &id).await?;
    if user.doc.email_verified {
        return Err(ApiError::conflict("this user's email address is already confirmed"));
    }
    send_for_admin(&st, &p, &user, TokenPurpose::VerifyEmail).await
}

async fn send_password_reset(
    State(st): State<AdminState>,
    p: Principal,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    super::moved_to_identity(&st)?;
    let user = admin_user(&st, &p, &id).await?;
    if user.doc.status != Status::Active {
        return Err(ApiError::conflict("this user is disabled; enable the account first"));
    }
    send_for_admin(&st, &p, &user, TokenPurpose::ResetPassword).await
}

/// Marks an address as confirmed without an email, for example when sending fails.
async fn confirm_email(State(st): State<AdminState>, p: Principal, Path(id): Path<String>) -> ApiResult<StatusCode> {
    super::moved_to_identity(&st)?;
    let user = admin_user(&st, &p, &id).await?;
    if !user.doc.email_verified {
        let mut doc = user.doc.clone();
        doc.email_verified = true;
        let mut audit = p.audit(None, None);
        audit.action = "confirm_email".into();
        save(&st, &user, &doc, audit).await?;
        st.store.account_tokens_delete_for_user(&user.id, TokenPurpose::VerifyEmail).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let email = api.schema::<EmailInput>();
    let reset = api.schema::<PasswordResetInput>();
    let token = api.schema::<TokenInput>();
    let accepted = "Accepted. An email is sent if an account matches; the answer is the same either way";
    api.add(
        api.op("post", "/session/password-reset", "requestPasswordReset", "session", "Email a password-reset link")
            .public()
            .body(email.clone())
            .response("202", accepted, None),
    );
    api.add(
        api.op(
            "post",
            "/session/password-reset/confirm",
            "resetPassword",
            "session",
            "Set a new password with a reset link",
        )
        .public()
        .body(reset)
        .no_content(),
    );
    api.add(
        api.op("post", "/session/verification", "requestVerification", "session", "Email a new verification link")
            .public()
            .body(email)
            .response("202", accepted, None),
    );
    api.add(
        api.op(
            "post",
            "/session/verification/confirm",
            "verifyEmail",
            "session",
            "Confirm an email address with a link",
        )
        .public()
        .body(token)
        .no_content(),
    );
    api.add(
        api.op(
            "post",
            "/users/{id}/verification",
            "sendUserVerification",
            "users",
            "Email the user a verification link",
        )
        .no_content(),
    );
    api.add(
        api.op(
            "post",
            "/users/{id}/password-reset",
            "sendUserPasswordReset",
            "users",
            "Email the user a password-reset link",
        )
        .no_content(),
    );
    api.add(
        api.op(
            "post",
            "/users/{id}/email-confirmation",
            "confirmUserEmail",
            "users",
            "Mark the user's address as confirmed",
        )
        .no_content(),
    );
    router
        .route("/session/password-reset", post(request_password_reset))
        .route("/session/password-reset/confirm", post(reset_password))
        .route("/session/verification", post(request_verification))
        .route("/session/verification/confirm", post(verify_email))
        .route("/users/{id}/verification", post(send_verification))
        .route("/users/{id}/password-reset", post(send_password_reset))
        .route("/users/{id}/email-confirmation", post(confirm_email))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_limited_per_address_and_purpose() {
        let g = RequestGuard::default();
        for _ in 0..MAX_REQUESTS {
            assert!(g.allow(TokenPurpose::ResetPassword, "a@x"));
        }
        assert!(!g.allow(TokenPurpose::ResetPassword, "a@x"));
        assert!(g.allow(TokenPurpose::VerifyEmail, "a@x"), "the other purpose has its own count");
        assert!(g.allow(TokenPurpose::ResetPassword, "b@x"), "other addresses are unaffected");
    }
}
