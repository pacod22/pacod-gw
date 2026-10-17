//! People's accounts (the gateway's `/users`). Needs `id.accounts.read` or
//! `id.accounts.manage`; in a realm with `accounts_managed_by`, its holders of those roles manage
//! the accounts that signed up in that realm, and only those.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use super::actor::{Actor, not_allowed};
use crate::authz::bindings;
use crate::config::Realm;
use crate::crypto::tokens;
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::mail::{self, Template};
use crate::oidc::account::plausible_email;
use crate::oidc::flows::{hash_password, send_verification};
use crate::policy;
use crate::store::accounts::account_set_status;
use crate::store::authz as db;
use crate::store::{Account, NewAccount, StoreError, changes};
use pacod_id_authz::ScopeRef;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/accounts", get(list).post(create))
        .route("/accounts/{id}", get(read).patch(update).delete(super::deletion::delete))
        .route("/accounts/{id}/verification", post(verification))
        .route("/accounts/{id}/password-reset", post(password_reset))
        .route("/accounts/{id}/email-confirmation", post(email_confirmation))
}

pub fn view(a: &Account) -> Value {
    json!({
        "id": a.id,
        "email": a.email,
        "display_name": a.display_name,
        "locale": a.locale,
        "status": a.status,
        "email_verified": a.is_verified(),
        "email_verified_source": a.email_verified_source,
        "has_password": a.password_hash.is_some(),
        "signup_realm": a.signup_realm,
        "last_realm": a.last_realm,
        "created_at": a.created_at,
        "last_login_at": a.last_login_at,
    })
}

/// The realm mail to an account goes out in: where it last signed in, where it signed up, or the
/// actor's.
pub fn mail_realm<'a>(st: &'a Shared, account: &Account, actor: &Actor) -> &'a Realm {
    [&account.last_realm, &account.signup_realm, &actor.realm]
        .into_iter()
        .find_map(|r| st.config.realms.get(r.as_str()))
        .expect("the actor's realm exists")
}

/// The account, when the actor may see it; 404 otherwise, so ids reveal nothing.
async fn visible(st: &Shared, actor: &Actor, id: &str) -> ApiResult<Account> {
    let catalog = st.catalog.get();
    let account = st.store.account(id).await?.ok_or_else(|| ApiError::not_found("no such account"))?;
    let sees = actor.reads_all_accounts(&catalog)
        || actor.manages_signups_of.as_deref() == Some(account.signup_realm.as_str());
    if sees { Ok(account) } else { Err(ApiError::not_found("no such account")) }
}

/// The account, when the actor may change it.
pub(crate) async fn managed(st: &Shared, actor: &Actor, id: &str) -> ApiResult<Account> {
    let account = visible(st, actor, id).await?;
    let catalog = st.catalog.get();
    if !actor.manages_account(&catalog, &account) {
        return Err(not_allowed());
    }
    // Through the realm policy (a product's admin managing its realm's sign-ups), only someone
    // the actor could have made: setting the password of an identity admin, or of a product
    // admin of more scopes, would make the actor them.
    if account.id != actor.id && !actor.allowed(&catalog, "id.accounts.manage", &[ScopeRef::platform()]) {
        let mut conn = st.store.pool().acquire().await.map_err(ApiError::internal)?;
        let held = bindings::subject_bindings(&mut conn, &account.id).await?;
        if held.iter().any(|b| actor.may_grant(&catalog, &b.role, &b.path).is_err()) {
            return Err(ApiError::forbidden("this account holds rights you cannot give")
                .with_details(json!({ "code": "holds_more_rights" })));
        }
    }
    Ok(account)
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    after: String,
    limit: Option<i64>,
}

async fn list(State(st): State<Shared>, actor: Actor, Query(q): Query<ListQuery>) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    if !actor.reads_accounts(&catalog) {
        return Err(not_allowed());
    }
    let realm = if actor.reads_all_accounts(&catalog) { None } else { actor.manages_signups_of.as_deref() };
    let limit = super::page(q.limit, 50);
    let accounts = st.store.accounts_list(q.q.trim(), realm, &q.after, limit).await?;
    let next = (accounts.len() as i64 == limit).then(|| accounts.last().map(|a| a.email.clone())).flatten();
    Ok(Json(json!({ "accounts": accounts.iter().map(view).collect::<Vec<_>>(), "next": next })).into_response())
}

async fn read(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<Response> {
    if !actor.reads_accounts(&st.catalog.get()) {
        return Err(not_allowed());
    }
    Ok(Json(view(&visible(&st, &actor, &id).await?)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAccountInput {
    email: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    locale: Option<String>,
    /// Without one, the person sets it with a reset link or an invitation.
    #[serde(default)]
    password: Option<String>,
}

async fn create(State(st): State<Shared>, actor: Actor, Json(body): Json<NewAccountInput>) -> ApiResult<Response> {
    if !actor.creates_accounts(&st.catalog.get()) {
        return Err(not_allowed());
    }
    let realm = &st.config.realms[&actor.realm];
    if !plausible_email(&body.email) {
        return Err(ApiError::invalid("that is not an email address").with_details(json!({ "code": "bad_email" })));
    }
    let password_hash = match &body.password {
        Some(p) => {
            policy::check(p, realm.password_min_length).map_err(|e| {
                ApiError::invalid("choose another password")
                    .with_details(json!({ "code": e.code(), "min_length": realm.password_min_length }))
            })?;
            Some(hash_password(&st, p).await?)
        }
        None => None,
    };
    // As in the gateway: proven by the admin unless the realm confirms addresses by mail.
    let verified = st.mail.is_none() || !realm.require_verified_email;
    let locale = body.locale.filter(|l| realm.locales.contains(l)).unwrap_or_else(|| realm.default_locale.clone());
    let account = match st
        .store
        .account_create(&NewAccount {
            id: tokens::new_id("usr"),
            email: body.email.trim().to_owned(),
            display_name: body.display_name.trim().chars().take(200).collect(),
            locale,
            password_hash,
            verified_by: verified.then(|| "admin".to_owned()),
            signup_realm: realm.name.clone(),
        })
        .await
    {
        Err(StoreError::Conflict(m)) => {
            return Err(ApiError::conflict(m).with_details(json!({ "code": "email_taken" })));
        }
        r => r?,
    };
    if !verified {
        send_verification(&st, realm, &account).await?;
    }
    st.store.audit(&actor.audit("account_created").subject(&account.id).detail(json!({ "how": "admin" }))).await?;
    Ok((StatusCode::CREATED, Json(view(&account))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    display_name: Option<String>,
    locale: Option<String>,
    email: Option<String>,
    password: Option<String>,
    status: Option<String>,
}

async fn update(
    State(st): State<Shared>,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<Patch>,
) -> ApiResult<Response> {
    let account = managed(&st, &actor, &id).await?;
    let realm = mail_realm(&st, &account, &actor);
    let mut changed = Vec::new();
    let mut seq = None;

    // Everything is checked before anything is saved: a refused field leaves the account as it was.
    if body.status.as_deref().is_some_and(|s| s != "active" && s != "disabled") {
        return Err(ApiError::invalid("status is active or disabled").with_details(json!({ "code": "bad_status" })));
    }
    if body.locale.as_deref().is_some_and(|l| !l.is_empty() && !realm.locales.iter().any(|x| x == l)) {
        return Err(ApiError::invalid("that language is not offered").with_details(json!({ "code": "bad_locale" })));
    }
    let new_email = body.email.as_deref().map(crate::store::normalize_email).filter(|e| *e != account.email);
    if let Some(email) = &new_email {
        if !plausible_email(email) {
            return Err(ApiError::invalid("that is not an email address").with_details(json!({ "code": "bad_email" })));
        }
        if st.store.account_by_email(email).await?.is_some() {
            return Err(
                ApiError::conflict("another account has this address").with_details(json!({ "code": "email_taken" }))
            );
        }
    }
    if let Some(password) = &body.password {
        let min = policy::min_length(&st.store, realm, Some(&account)).await?;
        policy::check(password, min).map_err(|e| {
            ApiError::invalid("choose another password").with_details(json!({ "code": e.code(), "min_length": min }))
        })?;
    }

    if let Some(status) = &body.status
        && *status != account.status
    {
        let kind = if status == "disabled" { "principal.disabled" } else { "principal.enabled" };
        let catalog = st.catalog.get();
        let mut tx = st.store.begin().await?;
        db::lock_head(&mut tx).await?;
        if status == "disabled" {
            bindings::check_leaving(&mut tx, &catalog, &actor.subject, &id).await?;
        }
        account_set_status(&mut tx, &id, status).await?;
        seq = Some(
            changes::record(&mut tx, kind, None, Some(&id), json!({ "sub": id, "principal_kind": "user" })).await?,
        );
        tx.commit().await?;
        if status == "disabled" {
            crate::oidc::logout::end_all(&st, &id, "account_disabled").await?;
        }
        changed.push("status");
    }
    let name: String =
        body.display_name.as_deref().map_or(account.display_name.clone(), |n| n.trim().chars().take(200).collect());
    let locale = body.locale.clone().unwrap_or_else(|| account.locale.clone());
    if name != account.display_name || locale != account.locale {
        st.store.account_set_profile(&id, &name, &locale).await?;
        // Products show names: they hear of the change on the feed.
        let mut tx = st.store.begin().await?;
        seq = Some(changes::record(&mut tx, "account.updated", None, Some(&id), json!({ "sub": id })).await?);
        tx.commit().await?;
        changed.push("profile");
    }
    if let Some(email) = &new_email {
        // Someone else's word does not prove an address, unless nothing can (no mail).
        let proven = (actor.id == id || st.mail.is_none()).then_some("admin");
        match st.store.account_set_email(&id, email, proven).await {
            Err(StoreError::Conflict(m)) => {
                return Err(ApiError::conflict(m).with_details(json!({ "code": "email_taken" })));
            }
            r => r?,
        }
        let params = BTreeMap::from([("new_email".to_owned(), email.clone())]);
        mail::enqueue(&st, Template::EmailChanged, &realm.name, &account.email, params).await?;
        let updated = st.store.account(&id).await?.ok_or(StoreError::NotFound)?;
        if !updated.is_verified() {
            send_verification(&st, realm, &updated).await?;
        }
        let mut tx = st.store.begin().await?;
        seq = Some(changes::record(&mut tx, "account.updated", None, Some(&id), json!({ "sub": id })).await?);
        tx.commit().await?;
        changed.push("email");
    }
    if let Some(password) = &body.password {
        let hash = hash_password(&st, password).await?;
        st.store.account_set_password(&id, &hash).await?;
        st.store.account_tokens_delete(&id, "reset_password").await?;
        crate::oidc::logout::end_all(&st, &id, "password_set_by_admin").await?;
        let params = BTreeMap::from([
            ("when".to_owned(), Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()),
            ("link".to_owned(), format!("{}/forgot-password", realm.issuer)),
        ]);
        mail::enqueue(&st, Template::PasswordChanged, &realm.name, &account.email, params).await?;
        changed.push("password");
    }
    if !changed.is_empty() {
        st.store.audit(&actor.audit("account_updated").subject(&id).detail(json!({ "changed": changed }))).await?;
    }
    let mut body = view(&st.store.account(&id).await?.ok_or(StoreError::NotFound)?);
    body["seq"] = json!(seq);
    Ok(Json(body).into_response())
}

fn no_mail() -> ApiError {
    ApiError::unavailable("this installation sends no mail").with_details(json!({ "code": "email_unavailable" }))
}

async fn verification(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let account = managed(&st, &actor, &id).await?;
    if account.is_verified() {
        return Err(
            ApiError::conflict("the address is confirmed already").with_details(json!({ "code": "already_verified" }))
        );
    }
    if st.mail.is_none() {
        return Err(no_mail());
    }
    send_verification(&st, mail_realm(&st, &account, &actor), &account).await?;
    st.store.audit(&actor.audit("verification_sent").subject(&id)).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn password_reset(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let account = managed(&st, &actor, &id).await?;
    if !account.is_active() {
        return Err(ApiError::conflict("the account is disabled").with_details(json!({ "code": "account_disabled" })));
    }
    if st.mail.is_none() {
        return Err(no_mail());
    }
    crate::oidc::account::send_reset(&st, mail_realm(&st, &account, &actor), &account).await?;
    st.store.audit(&actor.audit("password_reset_sent").subject(&id)).await?;
    Ok(StatusCode::ACCEPTED)
}

async fn email_confirmation(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let account = managed(&st, &actor, &id).await?;
    if !account.is_verified() {
        st.store.account_mark_verified(&id, "admin").await?;
        st.store.account_tokens_delete(&id, "verify_email").await?;
        st.store.audit(&actor.audit("email_confirmed").subject(&id)).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
