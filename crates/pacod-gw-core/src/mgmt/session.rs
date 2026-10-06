//! Console login sessions.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use super::resources::access::MIN_PASSWORD_LEN;
use crate::crypto::keys::{SESSION_PREFIX, generate};
use crate::crypto::{password, random_bytes, sha256};
use crate::model::{Doc, RoleName, RoleScope, Status, User};
use crate::store::{AuditEntry, TokenPurpose};

/// Failed logins allowed per account within the window.
const MAX_FAILURES: u32 = 10;
const FAILURE_WINDOW: Duration = Duration::from_secs(15 * 60);

/// Slows down password guessing: after too many failures an account is locked for a while.
#[derive(Default)]
pub struct LoginGuard {
    failures: Mutex<HashMap<String, (u32, Instant)>>,
}

impl LoginGuard {
    fn locked(&self, account: &str) -> bool {
        let mut map = self.failures.lock();
        match map.get(account) {
            Some((n, since)) if since.elapsed() < FAILURE_WINDOW => *n >= MAX_FAILURES,
            Some(_) => {
                map.remove(account);
                false
            }
            None => false,
        }
    }

    fn failed(&self, account: &str) {
        let mut map = self.failures.lock();
        // Keep the table small: forget windows that have passed.
        if map.len() > 10_000 {
            map.retain(|_, (_, since)| since.elapsed() < FAILURE_WINDOW);
        }
        let entry = map.entry(account.to_owned()).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= FAILURE_WINDOW {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    pub(crate) fn succeeded(&self, account: &str) {
        self.failures.lock().remove(account);
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LoginInput {
    pub email: String,
    #[schemars(extend("writeOnly" = true))]
    pub password: String,
    /// "Keep me logged in": the session lasts `remember_me_days` instead of `session_hours`.
    #[serde(default)]
    pub remember_me: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RoleView {
    pub role: RoleName,
    pub scope: RoleScope,
}

/// Who the caller is.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PrincipalView {
    pub id: String,
    /// `user`, `token` or `internal`.
    pub kind: String,
    pub display: String,
    pub roles: Vec<RoleView>,
    pub platform_admin: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SessionView {
    /// Send this as a Bearer token. It is shown once.
    pub token: String,
    pub expires_at: DateTime<Utc>,
    pub principal: PrincipalView,
}

fn principal_view(p: &Principal) -> PrincipalView {
    PrincipalView {
        id: p.id.clone(),
        kind: p.kind.as_str().to_owned(),
        display: p.display.clone(),
        roles: p.roles.iter().map(|(role, scope)| RoleView { role: *role, scope: scope.clone() }).collect(),
        platform_admin: p.is_platform_admin(),
    }
}

async fn login(State(st): State<AdminState>, Json(input): Json<LoginInput>) -> ApiResult<Json<SessionView>> {
    let email = input.email.trim().to_lowercase();
    let denied = || ApiError::unauthorized("wrong email or password");
    if st.logins.locked(&email) {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "too many failed logins for this account; try again later",
        ));
    }
    let snap = st.config.snapshot();
    let user = snap.user_emails.get(&email).and_then(|id| snap.users.get(id)).cloned();

    // Hash verification is slow by design: keep it off the async workers. An unknown account
    // is verified against a fixed hash so that it takes the same time as a wrong password.
    let stored = user.as_ref().map(|u| u.doc.password_hash.clone());
    let pw = input.password;
    let ok = tokio::task::spawn_blocking(move || match stored {
        Some(h) => password::verify(&pw, &h),
        None => {
            password::verify(&pw, DUMMY_HASH);
            false
        }
    })
    .await
    .map_err(ApiError::internal)?;

    let Some(user) = user.filter(|u| ok && u.doc.status == Status::Active) else {
        st.logins.failed(&email);
        let _ = st
            .store
            .audit(&AuditEntry {
                actor: email.clone(),
                actor_kind: "user".into(),
                action: "login_failed".into(),
                kind: User::KIND.into(),
                object_id: String::new(),
                ..Default::default()
            })
            .await;
        return Err(denied());
    };
    st.logins.succeeded(&email);

    // Said only to someone who knows the password, so it reveals nothing about the account.
    if !user.doc.email_verified && snap.settings.require_verified_email && st.mailer.is_some() {
        let _ = st
            .store
            .audit(&AuditEntry {
                actor: user.id.clone(),
                actor_kind: "user".into(),
                action: "login_unverified".into(),
                kind: User::KIND.into(),
                object_id: user.id.clone(),
                ..Default::default()
            })
            .await;
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "email_not_verified",
            "confirm your email address first, with the link in the email you were sent",
        ));
    }

    let token = generate(SESSION_PREFIX, &hex::encode(random_bytes::<6>())).full;
    let hash = sha256(token.as_bytes());
    let lifetime = if input.remember_me {
        chrono::Duration::days(i64::from(snap.settings.remember_me_days.clamp(1, 365)))
    } else {
        chrono::Duration::hours(i64::from(snap.settings.session_hours.clamp(1, 24 * 30)))
    };
    let expires_at = Utc::now() + lifetime;
    st.store.session_create(&hash, &user.id, expires_at).await?;
    let _ = st
        .store
        .audit(&AuditEntry {
            actor: user.id.clone(),
            actor_kind: "user".into(),
            action: "login".into(),
            kind: User::KIND.into(),
            object_id: user.id.clone(),
            detail: serde_json::json!({ "remember_me": input.remember_me }),
            ..Default::default()
        })
        .await;

    let principal = super::auth::authenticate(&st, &token).await?;
    Ok(Json(SessionView { token, expires_at, principal: principal_view(&principal) }))
}

/// An Argon2id hash of a random password nobody knows; used to spend the same time on unknown
/// accounts as on known ones.
const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$Wk5Sy3ULQyWz7HhXkz0Q8a3H4mJm0h1o8o3k1nq5a2Y";

async fn whoami(p: Principal) -> Json<PrincipalView> {
    Json(principal_view(&p))
}

async fn logout(State(st): State<AdminState>, p: Principal) -> ApiResult<StatusCode> {
    if let Some(hash) = p.session_hash {
        st.store.session_delete(&hash).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PasswordChange {
    #[schemars(extend("writeOnly" = true))]
    pub current: String,
    #[schemars(extend("writeOnly" = true))]
    pub new: String,
}

async fn change_password(
    State(st): State<AdminState>,
    p: Principal,
    Json(input): Json<PasswordChange>,
) -> ApiResult<StatusCode> {
    let mut user = st.store.get_obj::<User>(&p.id).await.map_err(|_| ApiError::forbidden())?;
    if input.new.chars().count() < MIN_PASSWORD_LEN {
        return Err(ApiError::invalid(format!("password must be at least {MIN_PASSWORD_LEN} characters")));
    }
    let (current, stored, new) = (input.current, user.doc.password_hash.clone(), input.new);
    let new_hash =
        tokio::task::spawn_blocking(move || password::verify(&current, &stored).then(|| password::hash(&new)))
            .await
            .map_err(ApiError::internal)?
            // Not 401: the session is fine, and a client would take 401 for "log in again".
            .ok_or_else(|| {
                ApiError::invalid("the current password is wrong")
                    .with_details(serde_json::json!({ "field": "current" }))
            })?
            .map_err(ApiError::internal)?;
    user.doc.password_hash = new_hash;

    let mut tx = st.store.begin().await?;
    let mut audit = p.audit(None, None);
    audit.action = "change_password".into();
    tx.update(
        &user.id,
        None,
        Some(&user.doc.email),
        &serde_json::to_value(&user.doc).map_err(ApiError::internal)?,
        audit,
    )
    .await?;
    st.commit(tx).await?;
    // Every other session of the user ends; the caller logs in again. Reset links sent
    // before the change stop working.
    st.store.sessions_delete_for_user(&user.id).await?;
    st.store.account_tokens_delete_for_user(&user.id, TokenPurpose::ResetPassword).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let login_input = api.schema::<LoginInput>();
    let session = api.schema::<SessionView>();
    let principal = api.schema::<PrincipalView>();
    let pw = api.schema::<PasswordChange>();
    api.add(
        api.op("post", "/session", "login", "session", "Log in with email and password")
            .public()
            .body(login_input)
            .ok(session),
    );
    api.add(api.op("get", "/session", "whoami", "session", "The caller and their roles").ok(principal));
    api.add(api.op("delete", "/session", "logout", "session", "End the session").no_content());
    api.add(
        api.op("put", "/session/password", "changePassword", "session", "Change the caller's password")
            .body(pw)
            .no_content(),
    );
    router.route("/session", get(whoami).post(login).delete(logout)).route("/session/password", put(change_password))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_lock_after_repeated_failures_and_unlock_on_success() {
        let g = LoginGuard::default();
        for _ in 0..MAX_FAILURES - 1 {
            g.failed("a@x");
        }
        assert!(!g.locked("a@x"));
        g.failed("a@x");
        assert!(g.locked("a@x"));
        assert!(!g.locked("b@x"), "other accounts are unaffected");
        g.succeeded("a@x");
        assert!(!g.locked("a@x"));
    }

    #[test]
    fn dummy_hash_is_a_valid_hash_that_matches_nothing_guessable() {
        assert!(!password::verify("password", DUMMY_HASH));
        assert!(argon2::password_hash::PasswordHash::new(DUMMY_HASH).is_ok());
    }
}
