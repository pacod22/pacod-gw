//! Invitations: the token is stored as a hash
//! and bound to the invited address; one pending invitation per address and scope; inviting needs
//! the right to give the role there. A mailed invitation proves the address when it is accepted.
//! Without mail, the inviter gets the link once and hands it on.
//!
//! Accepting gives the role at the scope, replacing the roles of the same product the person held
//! there. Someone with an account accepts signed in to it; someone without one chooses a
//! password, long enough for a role holder.

use std::collections::BTreeMap;
use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use pacod_id_authz::{Catalog, Subject};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;

use super::actor::{Actor, not_allowed};
use crate::authz::bindings::{self, NewBinding, Protect, subject_bindings};
use crate::config::Realm;
use crate::crypto::{sha256, tokens};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::mail::{self, Template};
use crate::oidc::account::plausible_email;
use crate::oidc::flows::{hash_password, same_origin, send_verification};
use crate::oidc::{Caller, browser_session, realm_by_host};
use crate::store::accounts::account_insert;
use crate::store::authz::{self as db, InvitationRow, InvitationWrite};
use crate::store::{Account, AuditEntry, NewAccount, StoreError, normalize_email};

pub fn admin_routes() -> Router<Shared> {
    Router::new()
        .route("/invitations", get(list).post(create))
        .route("/invitations/{id}", delete(revoke))
        .route("/invitations/{id}/resend", post(resend))
}

pub fn public_routes() -> Router<Shared> {
    Router::new()
        .route("/ui-api/v1/invitations/lookup", post(lookup))
        .route("/ui-api/v1/invitations/accept", post(accept))
        .route("/ui-api/v1/account/invitations", get(mine))
        .route("/ui-api/v1/account/invitations/{id}/accept", post(accept_mine))
        .route("/ui-api/v1/account/invitations/{id}/decline", post(decline_mine))
}

fn product(role: &str) -> &str {
    role.split_once('.').map_or("", |(p, _)| p)
}

/// How the mail and the page name the role and the scope: the inviting product's labels, else
/// the role's own name and the scope's display name.
async fn labels(conn: &mut PgConnection, inv: &InvitationRow) -> ApiResult<(String, String)> {
    let role = inv.labels["role"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| inv.role.split_once('.').map_or(inv.role.as_str(), |(_, r)| r).replace('_', " "));
    if let Some(s) = inv.labels["scope"].as_str() {
        return Ok((role, s.to_owned()));
    }
    let scope = match inv.scope_type.as_str() {
        pacod_id_authz::ORGANISATION => db::organisation(conn, &inv.scope_id).await?.map(|o| o.display_name),
        pacod_id_authz::PLATFORM => None,
        kind => db::scope(conn, kind, &inv.scope_id).await?.map(|s| s.display_name),
    }
    .filter(|n| !n.is_empty())
    .unwrap_or_else(|| inv.scope_id.clone());
    Ok((role, scope))
}

pub async fn view(conn: &mut PgConnection, inv: &InvitationRow) -> ApiResult<Value> {
    let (role_label, scope_label) = labels(conn, inv).await?;
    // How the inviter is named (an account's display name or address, a service account's
    // name): those who manage invitations may not be allowed to read accounts.
    let inviter_name: Option<String> = sqlx::query_scalar(
        "SELECT coalesce(nullif(display_name, ''), email) FROM accounts WHERE id = $1
         UNION ALL SELECT name FROM service_accounts WHERE id = $1
         LIMIT 1",
    )
    .bind(&inv.invited_by)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(json!({
        "id": inv.id,
        "email": inv.email,
        "role": inv.role,
        "role_label": role_label,
        "scope": inv.scope().to_string(),
        "scope_label": scope_label,
        "realm": inv.realm,
        "invited_by": inv.invited_by,
        "inviter_name": inviter_name.unwrap_or_default(),
        "emailed": inv.emailed,
        "created_at": inv.created_at,
        "expires_at": inv.expires_at,
        "expired": inv.expired(),
    }))
}

/// How a mail names the inviter.
async fn inviter_name(st: &Shared, actor: &Actor) -> ApiResult<String> {
    if let Some(a) = &actor.account {
        return Ok(if a.display_name.is_empty() { a.email.clone() } else { a.display_name.clone() });
    }
    Ok(st.store.service_account(&actor.id).await?.map_or_else(|| actor.id.clone(), |s| s.name))
}

/// Mails the link, or returns it when the inviter hands it on.
async fn deliver(
    st: &Shared,
    conn: &mut PgConnection,
    realm: &Realm,
    inv: &InvitationRow,
    token: &str,
    inviter: &str,
) -> ApiResult<Option<String>> {
    let link = format!("{}/invite?token={token}", realm.issuer);
    if !inv.emailed {
        return Ok(Some(link));
    }
    let (role, scope) = labels(conn, inv).await?;
    let params = BTreeMap::from([
        ("inviter".to_owned(), inviter.to_owned()),
        ("scope".to_owned(), scope),
        ("role".to_owned(), role),
        ("days".to_owned(), realm.invitation_days.to_string()),
        ("link".to_owned(), link),
    ]);
    mail::enqueue(st, Template::Invitation, &realm.name, &inv.email, params).await?;
    Ok(None)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewInvitation {
    email: String,
    role: String,
    scope: String,
    /// `email` (the default, when the installation sends mail) or `link`: the link comes back
    /// once, to hand on.
    #[serde(default)]
    deliver: Option<String>,
    /// How the product names the role and the scope (`{"role": "Viewer", "scope": "Acme"}`).
    #[serde(default)]
    labels: Option<Value>,
}

fn by_mail(st: &Shared, deliver: Option<&str>) -> ApiResult<bool> {
    match deliver {
        None | Some("email") => Ok(st.mail.is_some()),
        Some("link") => Ok(false),
        Some(_) => Err(ApiError::invalid("deliver is email or link").with_details(json!({ "code": "bad_deliver" }))),
    }
}

async fn create(State(st): State<Shared>, actor: Actor, Json(body): Json<NewInvitation>) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    let email = normalize_email(&body.email);
    if !plausible_email(&email) {
        return Err(ApiError::invalid("that is not an email address").with_details(json!({ "code": "bad_email" })));
    }
    let scope = super::parse_scope(&body.scope)?;
    let emailed = by_mail(&st, body.deliver.as_deref())?;
    let realm = &st.config.realms[&actor.realm];
    let mut tx = st.store.begin().await?;
    let path = super::path_of(&mut tx, &scope).await?;
    actor.may_grant(&catalog, &body.role, &path)?;
    // One pending invitation per address and scope: a new one replaces the old only for someone
    // who could have sent the old one, of the same product.
    if let Some(old) = db::invitation_pending(&mut tx, &email, &scope).await?
        && (old.role.split('.').next() != body.role.split('.').next()
            || actor.may_grant(&catalog, &old.role, &path).is_err())
    {
        return Err(ApiError::conflict("an invitation for this address and scope is pending")
            .with_details(json!({ "code": "invitation_exists", "role": old.role })));
    }
    let token = tokens::opaque();
    let labels = body.labels.filter(Value::is_object).unwrap_or_else(|| json!({}));
    let inv = db::invitation_upsert(
        &mut tx,
        &tokens::new_id("inv"),
        &InvitationWrite {
            token_hash: &sha256(token.as_bytes()),
            email: &email,
            role: &body.role,
            scope: &scope,
            realm: &realm.name,
            invited_by: &actor.id,
            emailed,
            labels: &labels,
            expires_at: Utc::now() + Duration::days(i64::from(realm.invitation_days)),
        },
    )
    .await?;
    let inviter = inviter_name(&st, &actor).await?;
    let link = deliver(&st, &mut tx, realm, &inv, &token, &inviter).await?;
    let mut out = view(&mut tx, &inv).await?;
    tx.commit().await?;
    st.store
        .audit(
            &actor
                .audit("invitation_created")
                .subject(&email)
                .detail(json!({ "role": inv.role, "scope": body.scope, "id": inv.id })),
        )
        .await?;
    if let Some(link) = link {
        out["token"] = json!(token);
        out["link"] = json!(link);
    }
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

#[derive(Deserialize)]
struct ListQuery {
    scope: String,
    /// Also the invitations to scopes below it (an organisation's workspaces); the default.
    below: Option<bool>,
}

async fn list(State(st): State<Shared>, actor: Actor, Query(q): Query<ListQuery>) -> ApiResult<Response> {
    let scope = super::parse_scope(&q.scope)?;
    let mut conn = st.store.pool().acquire().await?;
    let path = super::path_of(&mut conn, &scope).await?;
    if !actor.administers(&st.catalog.get(), &path) {
        return Err(not_allowed());
    }
    let scopes = if q.below.unwrap_or(true) { db::subtree(&mut conn, &scope).await? } else { vec![scope] };
    let mut out = Vec::new();
    for inv in db::invitations_at(&mut conn, &scopes).await? {
        if actor.product.as_deref().is_none_or(|p| product(&inv.role) == p) {
            out.push(view(&mut conn, &inv).await?);
        }
    }
    Ok(Json(json!({ "invitations": out })).into_response())
}

/// The invitation, when the actor may give its role at its scope.
async fn administered(st: &Shared, actor: &Actor, conn: &mut PgConnection, id: &str) -> ApiResult<InvitationRow> {
    let inv = db::invitation(conn, id).await?.ok_or_else(|| ApiError::not_found("no such invitation"))?;
    let path = db::path_of(conn, &inv.scope()).await?.ok_or_else(|| ApiError::not_found("no such invitation"))?;
    actor.may_grant(&st.catalog.get(), &inv.role, &path).map_err(|_| ApiError::not_found("no such invitation"))?;
    Ok(inv)
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Resend {
    #[serde(default)]
    deliver: Option<String>,
}

/// Sends an invitation again with a new link (an expired one too); the old link stops working.
async fn resend(
    State(st): State<Shared>,
    actor: Actor,
    Path(id): Path<String>,
    body: Option<Json<Resend>>,
) -> ApiResult<Response> {
    let body = body.map(|Json(b)| b).unwrap_or_default();
    let emailed = by_mail(&st, body.deliver.as_deref())?;
    let mut tx = st.store.begin().await?;
    let inv = administered(&st, &actor, &mut tx, &id).await?;
    let realm = st.config.realms.get(&inv.realm).unwrap_or(&st.config.realms[&actor.realm]);
    let token = tokens::opaque();
    let expires = Utc::now() + Duration::days(i64::from(realm.invitation_days));
    let inv = db::invitation_renew(&mut tx, &id, &sha256(token.as_bytes()), &actor.id, emailed, expires).await?;
    let inviter = inviter_name(&st, &actor).await?;
    let link = deliver(&st, &mut tx, realm, &inv, &token, &inviter).await?;
    let mut out = view(&mut tx, &inv).await?;
    tx.commit().await?;
    st.store.audit(&actor.audit("invitation_resent").subject(&inv.email).detail(json!({ "id": id }))).await?;
    if let Some(link) = link {
        out["token"] = json!(token);
        out["link"] = json!(link);
    }
    Ok(Json(out).into_response())
}

async fn revoke(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let mut tx = st.store.begin().await?;
    let inv = administered(&st, &actor, &mut tx, &id).await?;
    db::invitation_delete(&mut tx, &id).await?;
    tx.commit().await?;
    st.store.audit(&actor.audit("invitation_revoked").subject(&inv.email).detail(json!({ "id": id }))).await?;
    Ok(StatusCode::NO_CONTENT)
}

// Accepting ------------------------------------------------------------------------------------

fn link_invalid() -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        "link_invalid",
        "the invitation is used, expired, revoked or replaced by a newer one",
    )
    .with_details(json!({ "code": "link_invalid" }))
}

/// Gives the invited role to `account` in `conn`'s transaction (holding `authz_head`), replacing
/// the roles of the same product it held at the scope. The last-admin rule holds, with the
/// person's own rights for the bypass.
async fn grant(
    conn: &mut PgConnection,
    catalog: &Catalog,
    inv: &InvitationRow,
    account: &str,
) -> ApiResult<Option<i64>> {
    let scope = inv.scope();
    db::path_of(conn, &scope).await?.ok_or_else(link_invalid)?;
    let held: Vec<_> = db::bindings_at(conn, std::slice::from_ref(&scope), false)
        .await?
        .into_iter()
        .filter(|b| b.principal_id == account && product(&b.role) == product(&inv.role))
        .collect();
    if held.iter().any(|b| b.role == inv.role) {
        return Ok(None);
    }
    let me = Subject::user(subject_bindings(conn, account).await?);
    let add = [NewBinding {
        principal: account.to_owned(),
        role: inv.role.clone(),
        scope,
        source: "invitation",
        created_by: inv.invited_by.clone(),
    }];
    Ok(bindings::apply(conn, catalog, Protect::As(&me), &held, &add).await?.seq)
}

/// Where the hosted UI sends people once they have accepted: the realm's first client that
/// says where its sign-in starts.
fn continue_to(st: &Shared, realm: &Realm) -> Option<String> {
    st.config.clients.values().filter(|c| c.realm == realm.name).find_map(|c| c.initiate_login_uri.clone())
}

#[derive(Deserialize)]
struct TokenOnly {
    token: String,
}

/// The invitation a link carries, for this realm's pages, while it can be accepted.
async fn by_link(conn: &mut PgConnection, realm: &Realm, token: &str) -> ApiResult<InvitationRow> {
    let inv = db::invitation_by_token(conn, &sha256(token.as_bytes())).await?.ok_or_else(link_invalid)?;
    if inv.expired() || inv.realm != realm.name || db::path_of(conn, &inv.scope()).await?.is_none() {
        return Err(link_invalid());
    }
    Ok(inv)
}

fn ui_realm<'a>(st: &'a Shared, headers: &HeaderMap) -> ApiResult<&'a Realm> {
    let realm = realm_by_host(st, headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    same_origin(realm, headers)?;
    Ok(realm)
}

/// What a link offers, before it is accepted.
async fn lookup(State(st): State<Shared>, headers: HeaderMap, Json(body): Json<TokenOnly>) -> ApiResult<Response> {
    let realm = ui_realm(&st, &headers)?;
    let mut conn = st.store.pool().acquire().await?;
    let inv = by_link(&mut conn, realm, &body.token).await?;
    let mut out = view(&mut conn, &inv).await?;
    out["account_exists"] = json!(st.store.account_by_email(&inv.email).await?.is_some());
    out["signed_in_as"] = json!(browser_session(&st, realm, &headers).await?.map(|(_, a)| a.email));
    Ok(Json(out).into_response())
}

#[derive(Deserialize)]
struct Accept {
    token: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

async fn accept(
    State(st): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Accept>,
) -> ApiResult<Response> {
    let realm = ui_realm(&st, &headers)?;
    let catalog = st.catalog.get();
    let caller = Caller::of(&st, peer, &headers);
    let existing = {
        let mut conn = st.store.pool().acquire().await?;
        let inv = by_link(&mut conn, realm, &body.token).await?;
        st.store.account_by_email(&inv.email).await?
    };
    let audit = |action: &str, account: &str, inv: &InvitationRow| AuditEntry {
        realm: Some(realm.name.clone()),
        ip: caller.ip.clone(),
        user_agent: caller.user_agent.clone(),
        ..AuditEntry::new(action)
            .by(account, "user")
            .subject(account)
            .detail(json!({ "role": inv.role, "scope": inv.scope().to_string(), "id": inv.id }))
    };

    if let Some(account) = existing {
        // An account: accepted signed in to it.
        let signed_in = browser_session(&st, realm, &headers).await?.filter(|(_, a)| a.id == account.id);
        if signed_in.is_none() || !account.is_active() {
            return Err(ApiError::unauthorized(format!("sign in as {} to accept", account.email))
                .with_details(json!({ "code": "sign_in_required", "email": account.email })));
        }
        let mut tx = st.store.begin().await?;
        db::lock_head(&mut tx).await?;
        let inv = by_link(&mut tx, realm, &body.token).await?;
        let seq = grant(&mut tx, &catalog, &inv, &account.id).await?;
        db::invitation_delete(&mut tx, &inv.id).await?;
        tx.commit().await?;
        if inv.emailed && !account.is_verified() {
            st.store.account_mark_verified(&account.id, "invitation").await?;
        }
        st.store.audit(&audit("invitation_accepted", &account.id, &inv)).await?;
        return Ok(Json(json!({ "next": "continue", "to": continue_to(&st, realm), "seq": seq })).into_response());
    }

    // No account yet: one is made, with a password long enough for a role holder.
    let min = realm.role_holder_password_min_length.max(realm.password_min_length);
    let password = body.password.as_deref().unwrap_or_default();
    crate::policy::check(password, min).map_err(|e| {
        ApiError::invalid("choose another password").with_details(json!({ "code": e.code(), "min_length": min }))
    })?;
    let hash = hash_password(&st, password).await?;
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let inv = by_link(&mut tx, realm, &body.token).await?;
    let account = match account_insert(
        &mut tx,
        &NewAccount {
            id: tokens::new_id("usr"),
            email: inv.email.clone(),
            display_name: body.display_name.as_deref().unwrap_or_default().trim().chars().take(200).collect(),
            locale: realm.default_locale.clone(),
            password_hash: Some(hash),
            verified_by: inv.emailed.then(|| "invitation".to_owned()),
            signup_realm: realm.name.clone(),
        },
    )
    .await
    {
        Err(StoreError::Conflict(_)) => {
            return Err(ApiError::conflict("an account was made for this address meanwhile; sign in to accept")
                .with_details(json!({ "code": "sign_in_required", "email": inv.email })));
        }
        r => r?,
    };
    let seq = grant(&mut tx, &catalog, &inv, &account.id).await?;
    db::invitation_delete(&mut tx, &inv.id).await?;
    tx.commit().await?;
    let confirm = !account.is_verified() && st.mail.is_some() && realm.require_verified_email;
    if !account.is_verified() && st.mail.is_some() {
        send_verification(&st, realm, &account).await?;
    }
    st.store.audit(&audit("account_created", &account.id, &inv)).await?;
    st.store.audit(&audit("invitation_accepted", &account.id, &inv)).await?;
    Ok(Json(json!({
        "next": "sign_in", "email": account.email, "needs_confirmation": confirm, "to": continue_to(&st, realm), "seq": seq,
    }))
    .into_response())
}

/// The signed-in person's account, for the account pages.
async fn me(st: &Shared, headers: &HeaderMap) -> ApiResult<Account> {
    let realm = realm_by_host(st, headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    let (_, account) = browser_session(st, realm, headers)
        .await?
        .ok_or_else(|| ApiError::unauthorized("sign in first").with_details(json!({ "code": "sign_in_required" })))?;
    Ok(account)
}

/// The signed-in person's pending invitations.
async fn mine(State(st): State<Shared>, headers: HeaderMap) -> ApiResult<Response> {
    let realm = realm_by_host(&st, &headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
    let account = me(&st, &headers).await?;
    let mut conn = st.store.pool().acquire().await?;
    let mut out = Vec::new();
    // Only a confirmed address shows what was sent to it: whoever registered someone else's
    // address must not see, or take, their invitations.
    if !account.is_verified() {
        return Ok(Json(json!({ "invitations": out })).into_response());
    }
    for inv in db::invitations_for_email(&mut conn, &account.email).await? {
        if inv.realm == realm.name && db::path_of(&mut conn, &inv.scope()).await?.is_some() {
            out.push(view(&mut conn, &inv).await?);
        }
    }
    Ok(Json(json!({ "invitations": out })).into_response())
}

/// One of the signed-in person's own invitations: sent to their confirmed address, in this realm.
async fn my_invitation(
    conn: &mut PgConnection,
    realm: &Realm,
    account: &Account,
    id: &str,
) -> ApiResult<InvitationRow> {
    db::invitation(conn, id)
        .await?
        .filter(|i| i.email == account.email && account.is_verified() && i.realm == realm.name && !i.expired())
        .ok_or_else(|| ApiError::not_found("no such invitation"))
}

async fn accept_mine(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<Response> {
    let realm = ui_realm(&st, &headers)?;
    let account = me(&st, &headers).await?;
    let catalog = st.catalog.get();
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let inv = my_invitation(&mut tx, realm, &account, &id).await?;
    let seq = grant(&mut tx, &catalog, &inv, &account.id).await?;
    db::invitation_delete(&mut tx, &id).await?;
    tx.commit().await?;
    st.store
        .audit(
            &AuditEntry {
                realm: Some(realm.name.clone()),
                ..AuditEntry::new("invitation_accepted").by(&account.id, "user").subject(&account.id)
            }
            .detail(json!({ "role": inv.role, "scope": inv.scope().to_string(), "id": id })),
        )
        .await?;
    Ok(Json(json!({ "seq": seq })).into_response())
}

async fn decline_mine(State(st): State<Shared>, headers: HeaderMap, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let realm = ui_realm(&st, &headers)?;
    let account = me(&st, &headers).await?;
    let mut conn = st.store.pool().acquire().await?;
    let inv = my_invitation(&mut conn, realm, &account, &id).await?;
    db::invitation_delete(&mut conn, &id).await?;
    st.store
        .audit(
            &AuditEntry {
                realm: Some(realm.name.clone()),
                ..AuditEntry::new("invitation_declined").by(&account.id, "user").subject(&account.id)
            }
            .detail(json!({ "role": inv.role, "scope": inv.scope().to_string(), "id": id })),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
