//! Imports (concept 17): the people, admin tokens, roles and invitations a product kept itself
//! before it signed people in through pacod-id. `pacod-id import gw <file>` reads the gateway's
//! `pacod-gw-core identity export`.
//!
//! The import keeps ids (`usr_…` accounts, `tok_…` service accounts, `rb_…` bindings, `inv_…`
//! invitations), copies password and link hashes as they are, and runs in one transaction: all
//! or nothing. A dry run makes the same changes and rolls them back, so its report is exactly
//! what the import would do. An address pacod-id has under another id is a conflict, reported
//! and refused: a standalone gateway never merges accounts (concept 12.5).
//!
//! Each import records where the feed and the audit log stood. Rolling the product back to its
//! own sign-in is safe only while nothing was written at pacod-id since ([`writes_since`]); for
//! the same reason a second import is refused once something was.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use pacod_id_authz::{Catalog, Manifest, ScopeRef};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, Row as _};

use crate::authz::bindings::{self, NewBinding};
use crate::authz::{base_catalog, path_json, scopes};
use crate::store::accounts::normalize_email;
use crate::store::authz::{self as db, ScopeRow};
use crate::store::{Store, StoreError, changes};

/// The format `pacod-gw-core identity export` writes.
pub const GW_FORMAT: &str = "pacod-gw-identity/1";

/// The gateway's export (`pacod-gw-core identity export`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GwExport {
    pub format: String,
    pub exported_at: DateTime<Utc>,
    /// The gateway's configuration version the export was read at.
    pub config_version: i64,
    /// The gateway's manifest, pushed when pacod-id has none of the gateway's yet.
    pub manifest: Value,
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub organisations: Vec<GwOrganisation>,
    #[serde(default)]
    pub workspaces: Vec<GwWorkspace>,
    #[serde(default)]
    pub users: Vec<GwObject>,
    #[serde(default)]
    pub admin_tokens: Vec<GwObject>,
    #[serde(default)]
    pub role_bindings: Vec<GwObject>,
    #[serde(default)]
    pub invitations: Vec<GwInvitation>,
}

#[derive(Debug, Deserialize)]
pub struct GwOrganisation {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub system: bool,
}

#[derive(Debug, Deserialize)]
pub struct GwWorkspace {
    pub id: String,
    pub organisation_id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub system: bool,
}

/// A configuration document as the gateway stores it: the raw `doc`, so that a field left out
/// (an unproven address) stays told apart from one set.
#[derive(Debug, Deserialize)]
pub struct GwObject {
    pub id: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    pub doc: Value,
}

#[derive(Debug, Deserialize)]
pub struct GwInvitation {
    pub id: String,
    /// SHA-256 of the link's token, hex.
    pub token_hash: String,
    pub email: String,
    pub role: String,
    pub organisation_id: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub invited_by: String,
    #[serde(default)]
    pub emailed: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// How many of a kind were created, changed and found as they are.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Counts {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
}

/// What an import did, or would do.
#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub dry_run: bool,
    pub applied: bool,
    /// `pushed`: pacod-id had no catalogue of the gateway's; `present`: the stored one is used.
    pub catalog: &'static str,
    pub organisations: Counts,
    pub workspaces: Counts,
    pub accounts: Counts,
    pub service_accounts: Counts,
    pub bindings: Counts,
    pub invitations: Counts,
    /// What cannot be imported as it is; the import is refused while there are any.
    pub conflicts: Vec<String>,
    /// What was left out, and why (a role binding whose principal is gone, an invitation to a
    /// place the export does not have).
    pub skipped: Vec<String>,
    /// The feed's version after the import: rolling back is safe while nothing touching the
    /// product's people came after it.
    pub feed_seq: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0}")]
    Invalid(String),
    #[error("pacod-id has changes since the last import of {product} ({count}), which an import would undo: {first}")]
    WrittenSince { product: String, count: i64, first: String },
    #[error("the import has conflicts")]
    Conflicts(Box<Report>),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Change(#[from] bindings::ChangeError),
}

impl From<sqlx::Error> for ImportError {
    fn from(e: sqlx::Error) -> Self {
        ImportError::Store(e.into())
    }
}

/// The audit actions that change nothing a rollback would lose: signing in and out, sessions,
/// mail sent, the service's own housekeeping.
const NOT_WRITES: &[&str] = &[
    "logout",
    "session_ended",
    "other_sessions_ended",
    "session_revoked",
    "refresh_token_reused",
    "code_replayed",
    "keys_rotated",
    "instance_started",
    "verification_sent",
    "password_reset_sent",
    "email_change_requested",
    "import",
];

/// The last import of a product, and what was written at pacod-id since.
#[derive(Debug, Serialize)]
pub struct Since {
    pub imported_at: DateTime<Utc>,
    pub feed_seq: i64,
    /// The product's configuration version the import came from.
    pub source_version: i64,
    pub exported_at: DateTime<Utc>,
    /// Identity writes since: the audit log's (in the product's realm) and the feed's changes
    /// to people and their roles.
    pub writes: i64,
    /// The first of them, for the person deciding.
    pub first: Vec<String>,
}

/// The product's last import and the writes since, or `None` when it never imported.
pub async fn writes_since(conn: &mut PgConnection, product: &str, realm: &str) -> Result<Option<Since>, StoreError> {
    let Some(row) = sqlx::query(
        "SELECT at, feed_seq, audit_id, source_version, exported_at FROM imports WHERE product = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(product)
    .fetch_optional(&mut *conn)
    .await?
    else {
        return Ok(None);
    };
    let feed_seq: i64 = row.try_get("feed_seq")?;
    let audit_id: i64 = row.try_get("audit_id")?;
    let audited = sqlx::query(
        "SELECT at, action, subject FROM audit_log
         WHERE id > $1 AND coalesce(realm, '') IN ('', $2) AND NOT action LIKE 'login%' AND action <> ALL($3)
         ORDER BY id",
    )
    .bind(audit_id)
    .bind(realm)
    .bind(NOT_WRITES.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    .fetch_all(&mut *conn)
    .await?;
    let changed = sqlx::query(
        "SELECT at, kind, coalesce(principal_id, '') AS principal_id FROM authz_changes
         WHERE version > $1 AND (product IS NULL OR product = $2)
           AND (kind LIKE 'binding.%' OR kind LIKE 'principal.%' OR kind IN ('account.updated', 'credential.revoked'))
         ORDER BY version",
    )
    .bind(feed_seq)
    .bind(product)
    .fetch_all(&mut *conn)
    .await?;
    let mut first = Vec::new();
    for r in audited.iter().take(10) {
        let at: DateTime<Utc> = r.try_get("at")?;
        first.push(format!(
            "{} {} {}",
            at.to_rfc3339(),
            r.try_get::<String, _>("action")?,
            r.try_get::<String, _>("subject")?
        ));
    }
    for r in changed.iter().take(10usize.saturating_sub(first.len())) {
        let at: DateTime<Utc> = r.try_get("at")?;
        first.push(format!(
            "{} {} {}",
            at.to_rfc3339(),
            r.try_get::<String, _>("kind")?,
            r.try_get::<String, _>("principal_id")?
        ));
    }
    Ok(Some(Since {
        imported_at: row.try_get("at")?,
        feed_seq,
        source_version: row.try_get("source_version")?,
        exported_at: row.try_get("exported_at")?,
        writes: (audited.len() + changed.len()) as i64,
        first,
    }))
}

fn plain_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| rest.starts_with('_'))
        && id.len() <= 100
        && !id.chars().any(|c| c == ':' || c == '/' || c.is_whitespace())
}

/// The gateway's role (`org_admin`) as pacod-id names it, and its scope.
fn binding_of(doc: &Value) -> Option<(String, ScopeRef)> {
    let role = format!("gw.{}", doc["role"].as_str()?);
    let scope = match (doc["scope"]["type"].as_str()?, doc["scope"]["id"].as_str()) {
        ("platform", _) => ScopeRef::platform(),
        ("organisation", Some(o)) => ScopeRef::organisation(o),
        ("workspace", Some(w)) => ScopeRef::of("gw.workspace", w),
        _ => return None,
    };
    Some((role, scope))
}

fn hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Imports the gateway's export into `realm` (the gateway's, `gateway` by default).
pub async fn gw(store: &Store, realm: &str, export: &GwExport, dry_run: bool) -> Result<Report, ImportError> {
    const PRODUCT: &str = "gw";
    if export.format != GW_FORMAT {
        return Err(ImportError::Invalid(format!("this is not a gateway identity export ({GW_FORMAT})")));
    }
    let manifest = serde_json::from_value(export.manifest.clone())
        .map_err(|e| e.to_string())
        .and_then(|source| Manifest::from_source(source).map_err(|e| e.to_string()))
        .map_err(|e| ImportError::Invalid(format!("the export's manifest does not read: {e}")))?;
    if manifest.product() != PRODUCT {
        return Err(ImportError::Invalid("the export's manifest is not the gateway's".into()));
    }

    let mut tx = store.begin().await?;
    db::lock_head(&mut tx).await?;
    if let Some(since) = writes_since(&mut tx, PRODUCT, realm).await?
        && since.writes > 0
    {
        return Err(ImportError::WrittenSince {
            product: PRODUCT.into(),
            count: since.writes,
            first: since.first.join("; "),
        });
    }
    let mut report = Report { dry_run, ..Default::default() };

    // The catalogue: the one the gateway pushed when it ran with pacod-id already, else the
    // export's.
    let mut catalog = base_catalog();
    let mut have_gw = false;
    for r in sqlx::query("SELECT product, manifest FROM catalog").fetch_all(&mut *tx).await? {
        let product: String = r.try_get("product")?;
        if let Ok(m) = serde_json::from_value(r.try_get("manifest")?)
            .map_err(|e| e.to_string())
            .and_then(|s| Manifest::from_source(s).map_err(|e| e.to_string()))
        {
            have_gw |= product == PRODUCT;
            catalog.insert(m);
        }
    }
    if have_gw {
        report.catalog = "present";
    } else {
        let version = db::catalog_put(&mut tx, PRODUCT, &export.manifest).await?;
        changes::record(
            &mut tx,
            "catalog.updated",
            Some(PRODUCT),
            None,
            json!({ "product": PRODUCT, "version": version }),
        )
        .await?;
        catalog.insert(manifest);
        report.catalog = "pushed";
    }

    organisations(&mut tx, export, &mut report).await?;
    workspaces(&mut tx, export, &mut report).await?;
    let mut principals = accounts(&mut tx, realm, export, &mut report).await?;
    principals.extend(service_accounts(&mut tx, export, &mut report).await?);
    role_bindings(&mut tx, &catalog, export, &principals, &mut report).await?;
    invitations(&mut tx, realm, export, &principals, &mut report).await?;

    if !report.conflicts.is_empty() {
        return Err(ImportError::Conflicts(Box::new(report)));
    }
    let summary = json!({
        "accounts": report.accounts, "service_accounts": report.service_accounts, "bindings": report.bindings,
        "invitations": report.invitations, "organisations": report.organisations, "workspaces": report.workspaces,
        "skipped": report.skipped.len(),
    });
    store_audit(&mut tx, realm, PRODUCT, &summary).await?;
    report.feed_seq = sqlx::query_scalar("SELECT version FROM authz_head").fetch_one(&mut *tx).await?;
    let audit_id: i64 = sqlx::query_scalar("SELECT coalesce(max(id), 0) FROM audit_log").fetch_one(&mut *tx).await?;
    sqlx::query(
        "INSERT INTO imports (product, feed_seq, audit_id, source_version, exported_at, report)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(PRODUCT)
    .bind(report.feed_seq)
    .bind(audit_id)
    .bind(export.config_version)
    .bind(export.exported_at)
    .bind(&summary)
    .execute(&mut *tx)
    .await?;
    if dry_run {
        tx.rollback().await?;
    } else {
        tx.commit().await?;
        report.applied = true;
    }
    Ok(report)
}

async fn store_audit(tx: &mut PgConnection, realm: &str, product: &str, summary: &Value) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO audit_log (actor, actor_kind, action, subject, realm, detail)
         VALUES ('cli', 'operator', 'import', $1, $2, $3)",
    )
    .bind(product)
    .bind(realm)
    .bind(summary)
    .execute(tx)
    .await?;
    Ok(())
}

async fn organisations(tx: &mut PgConnection, export: &GwExport, report: &mut Report) -> Result<(), ImportError> {
    for o in &export.organisations {
        if !plain_id(&o.id, "org") {
            report.conflicts.push(format!("organisation {:?}: not an id pacod-id takes", o.id));
            continue;
        }
        let taken: Option<String> = sqlx::query_scalar("SELECT id FROM organisations WHERE slug = $1 AND id <> $2")
            .bind(&o.name)
            .bind(&o.id)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(other) = taken {
            report
                .conflicts
                .push(format!("organisation {}: the name {:?} belongs to {other} at pacod-id", o.id, o.name));
            continue;
        }
        let before = db::organisation(tx, &o.id).await?;
        db::organisation_upsert(tx, &o.id, &o.name, &o.display_name, o.system, "gw").await?;
        let after = db::organisation(tx, &o.id).await?.ok_or(StoreError::NotFound)?;
        if before.as_ref() == Some(&after) {
            report.organisations.unchanged += 1;
        } else {
            scopes::organisation_upserted(tx, &after).await?;
            if before.is_some() { report.organisations.updated += 1 } else { report.organisations.created += 1 }
        }
    }
    Ok(())
}

async fn workspaces(tx: &mut PgConnection, export: &GwExport, report: &mut Report) -> Result<(), ImportError> {
    for w in &export.workspaces {
        if !plain_id(&w.id, "ws") {
            report.conflicts.push(format!("workspace {:?}: not an id pacod-id takes", w.id));
            continue;
        }
        let parent = ScopeRef::organisation(&w.organisation_id);
        let Some(mut path) = db::path_of(tx, &parent).await? else {
            report
                .skipped
                .push(format!("workspace {}: its organisation {} is not in the export", w.id, w.organisation_id));
            continue;
        };
        let row = ScopeRow {
            kind: "gw.workspace".into(),
            id: w.id.clone(),
            parent: parent.clone(),
            product: "gw".into(),
            display_name: w.display_name.clone(),
            system: w.system,
        };
        let before = db::scope(tx, "gw.workspace", &w.id).await?;
        if before.as_ref().is_some_and(|b| b.parent != parent) {
            report.conflicts.push(format!("workspace {}: pacod-id has it under another organisation", w.id));
            continue;
        }
        db::scope_upsert(tx, &row).await?;
        let after = db::scope(tx, "gw.workspace", &w.id).await?.ok_or(StoreError::NotFound)?;
        if before.as_ref() == Some(&after) {
            report.workspaces.unchanged += 1;
            continue;
        }
        path.push(ScopeRef::of("gw.workspace", &w.id));
        changes::record(
            tx,
            "scope.upserted",
            Some("gw"),
            None,
            json!({
                "type": "gw.workspace", "id": w.id, "parent": parent.to_string(), "display_name": after.display_name,
                "system": after.system, "path": path_json(&path),
            }),
        )
        .await?;
        if before.is_some() { report.workspaces.updated += 1 } else { report.workspaces.created += 1 }
    }
    Ok(())
}

/// The gateway's users as accounts. Returns the principals the export brings, by id.
async fn accounts(
    tx: &mut PgConnection,
    realm: &str,
    export: &GwExport,
    report: &mut Report,
) -> Result<BTreeMap<String, &'static str>, ImportError> {
    let mut principals = BTreeMap::new();
    for u in &export.users {
        let d = &u.doc;
        let email = normalize_email(d["email"].as_str().unwrap_or_default());
        if !plain_id(&u.id, "usr") || !crate::oidc::account::plausible_email(&email) {
            report.conflicts.push(format!("user {}: the id or the address {email:?} is not one pacod-id takes", u.id));
            continue;
        }
        let taken: Option<String> = sqlx::query_scalar("SELECT id FROM accounts WHERE email = $1 AND id <> $2")
            .bind(&email)
            .bind(&u.id)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(other) = taken {
            report.conflicts.push(format!(
                "user {}: pacod-id has the address {email} as account {other}; a gateway's accounts are not merged",
                u.id
            ));
            continue;
        }
        // Proof of the address (concept 12.5): `email_verified: true` written explicitly is
        // proof; left out (accounts from before confirmation existed) it is assumed; false is none.
        let (verified_at, source): (Option<DateTime<Utc>>, &str) = match d.get("email_verified") {
            Some(Value::Bool(true)) => (Some(u.updated_at.unwrap_or(u.created_at)), "verified"),
            None => (Some(u.created_at), "legacy_assumed"),
            Some(_) => (None, ""),
        };
        let status = if d["status"] == "disabled" { "disabled" } else { "active" };
        let display_name: String = d["display_name"].as_str().unwrap_or_default().trim().chars().take(200).collect();
        let hash = d["password_hash"].as_str().filter(|h| h.starts_with('$')).map(str::to_owned);
        let before = sqlx::query(
            "SELECT email, display_name, status, password_hash, email_verified_source FROM accounts WHERE id = $1",
        )
        .bind(&u.id)
        .fetch_optional(&mut *tx)
        .await?;
        match before {
            None => {
                sqlx::query(
                    "INSERT INTO accounts (id, email, email_verified_at, email_verified_source, display_name, status,
                                           password_hash, password_changed_at, signup_realm, last_realm, created_at, updated_at)
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9, $10, now())",
                )
                .bind(&u.id)
                .bind(&email)
                .bind(verified_at)
                .bind(source)
                .bind(&display_name)
                .bind(status)
                .bind(&hash)
                .bind(hash.as_ref().map(|_| u.updated_at.unwrap_or(u.created_at)))
                .bind(realm)
                .bind(u.created_at)
                .execute(&mut *tx)
                .await?;
                if status == "disabled" {
                    changes::record(
                        tx,
                        "principal.disabled",
                        None,
                        Some(&u.id),
                        json!({ "sub": u.id, "principal_kind": "user" }),
                    )
                    .await?;
                }
                report.accounts.created += 1;
            }
            Some(b) => {
                let old_status: String = b.try_get("status")?;
                let same = b.try_get::<String, _>("email")? == email
                    && b.try_get::<String, _>("display_name")? == display_name
                    && old_status == status
                    && b.try_get::<Option<String>, _>("password_hash")? == hash
                    && b.try_get::<String, _>("email_verified_source")? == source;
                if same {
                    report.accounts.unchanged += 1;
                } else {
                    sqlx::query(
                        "UPDATE accounts SET email = $2, display_name = $3, status = $4, password_hash = $5,
                                email_verified_at = $6, email_verified_source = $7, updated_at = now() WHERE id = $1",
                    )
                    .bind(&u.id)
                    .bind(&email)
                    .bind(&display_name)
                    .bind(status)
                    .bind(&hash)
                    .bind(verified_at)
                    .bind(source)
                    .execute(&mut *tx)
                    .await?;
                    if old_status != status {
                        let kind = if status == "disabled" { "principal.disabled" } else { "principal.enabled" };
                        changes::record(tx, kind, None, Some(&u.id), json!({ "sub": u.id, "principal_kind": "user" }))
                            .await?;
                    }
                    report.accounts.updated += 1;
                }
            }
        }
        // Where the account came from, for the next import's report.
        sqlx::query(
            "INSERT INTO account_external_ids (source, external_id, account_id) VALUES ('gw', $1, $1) ON CONFLICT DO NOTHING",
        )
        .bind(&u.id)
        .execute(&mut *tx)
        .await?;
        principals.insert(u.id.clone(), "user");
    }
    Ok(principals)
}

/// The gateway's admin tokens as platform service accounts, each with its token kept as a
/// legacy one: pacod-id checks `pgwa_…` under the gateway's pepper (`PID_LEGACY_PEPPERS`).
async fn service_accounts(
    tx: &mut PgConnection,
    export: &GwExport,
    report: &mut Report,
) -> Result<BTreeMap<String, &'static str>, ImportError> {
    let mut principals = BTreeMap::new();
    for t in &export.admin_tokens {
        let d = &t.doc;
        let name: String = d["name"].as_str().unwrap_or_default().trim().to_owned();
        let Some(hmac) = d["secret_hmac"].as_str().and_then(hex).filter(|h| h.len() == 32) else {
            report.conflicts.push(format!("admin token {}: its secret_hmac is not 32 bytes in hex", t.id));
            continue;
        };
        if !plain_id(&t.id, "tok") || name.is_empty() || name.chars().count() > 100 {
            report
                .conflicts
                .push(format!("admin token {}: the id or the name {name:?} is not one pacod-id takes", t.id));
            continue;
        }
        let taken: Option<String> = sqlx::query_scalar(
            "SELECT id FROM service_accounts WHERE lower(name) = lower($1) AND id <> $2 AND organisation_id IS NULL",
        )
        .bind(&name)
        .bind(&t.id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(other) = taken {
            report.conflicts.push(format!("admin token {}: the name {name:?} belongs to {other} at pacod-id", t.id));
            continue;
        }
        let status = if d["status"] == "disabled" { "disabled" } else { "active" };
        let expires_at: Option<DateTime<Utc>> = d["expires_at"].as_str().and_then(|s| s.parse().ok());
        let hint = d["hint"].as_str().unwrap_or_default();
        let before = sqlx::query("SELECT name, status FROM service_accounts WHERE id = $1")
            .bind(&t.id)
            .fetch_optional(&mut *tx)
            .await?;
        let mut changed = match &before {
            None => {
                sqlx::query(
                    "INSERT INTO service_accounts (id, name, organisation_id, status, created_by, created_at)
                     VALUES ($1, $2, NULL, $3, 'import', $4)",
                )
                .bind(&t.id)
                .bind(&name)
                .bind(status)
                .bind(t.created_at)
                .execute(&mut *tx)
                .await?;
                if status == "disabled" {
                    changes::record(
                        tx,
                        "principal.disabled",
                        None,
                        Some(&t.id),
                        json!({ "sub": t.id, "principal_kind": "service" }),
                    )
                    .await?;
                }
                true
            }
            Some(b) => {
                let old_status: String = b.try_get("status")?;
                if b.try_get::<String, _>("name")? == name && old_status == status {
                    false
                } else {
                    sqlx::query("UPDATE service_accounts SET name = $2, status = $3 WHERE id = $1")
                        .bind(&t.id)
                        .bind(&name)
                        .bind(status)
                        .execute(&mut *tx)
                        .await?;
                    if old_status != status {
                        let kind = if status == "disabled" { "principal.disabled" } else { "principal.enabled" };
                        changes::record(
                            tx,
                            kind,
                            None,
                            Some(&t.id),
                            json!({ "sub": t.id, "principal_kind": "service" }),
                        )
                        .await?;
                    }
                    true
                }
            }
        };
        let has_token: Option<String> = sqlx::query_scalar(
            "SELECT service_account_id FROM service_tokens WHERE legacy_pepper_id = 'gw' AND legacy_hmac = $1",
        )
        .bind(&hmac)
        .fetch_optional(&mut *tx)
        .await?;
        match has_token {
            Some(owner) if owner != t.id => {
                report.conflicts.push(format!("admin token {}: its token belongs to {owner} at pacod-id", t.id));
                continue;
            }
            Some(_) => {}
            None => {
                sqlx::query(
                    "INSERT INTO service_tokens (id, service_account_id, legacy_hmac, legacy_pepper_id, hint, expires_at, created_at)
                     VALUES ($1, $2, $3, 'gw', $4, $5, $6)",
                )
                .bind(crate::crypto::tokens::new_id("stk"))
                .bind(&t.id)
                .bind(&hmac)
                .bind(hint)
                .bind(expires_at)
                .bind(t.created_at)
                .execute(&mut *tx)
                .await?;
                changed = true;
            }
        }
        match (before.is_some(), changed) {
            (false, _) => report.service_accounts.created += 1,
            (true, true) => report.service_accounts.updated += 1,
            (true, false) => report.service_accounts.unchanged += 1,
        }
        principals.insert(t.id.clone(), "service");
    }
    Ok(principals)
}

async fn role_bindings(
    tx: &mut PgConnection,
    catalog: &Catalog,
    export: &GwExport,
    principals: &BTreeMap<String, &'static str>,
    report: &mut Report,
) -> Result<(), ImportError> {
    let mut add = Vec::new();
    for b in &export.role_bindings {
        let Some((role, scope)) = binding_of(&b.doc) else {
            report.conflicts.push(format!("role binding {}: not a role and scope pacod-id reads", b.id));
            continue;
        };
        let principal = b.doc["principal"].as_str().unwrap_or_default();
        if !plain_id(&b.id, "rb") {
            report.conflicts.push(format!("role binding {:?}: not an id pacod-id takes", b.id));
            continue;
        }
        if !principals.contains_key(principal) {
            report.skipped.push(format!("role binding {}: its principal {principal} is not in the export", b.id));
            continue;
        }
        if db::path_of(tx, &scope).await?.is_none() {
            report.skipped.push(format!("role binding {}: its scope {scope} is not in the export", b.id));
            continue;
        }
        let fits = catalog.manifest("gw").and_then(|m| m.role(&role)).is_some_and(|r| r.at.contains(&scope.kind));
        if !fits {
            report.conflicts.push(format!("role binding {}: {role} cannot be held at {scope}", b.id));
            continue;
        }
        let existing: Option<(String, String, String, String)> =
            sqlx::query_as("SELECT principal_id, role, scope_type, scope_id FROM role_bindings WHERE id = $1")
                .bind(&b.id)
                .fetch_optional(&mut *tx)
                .await?;
        let scope_id = scope.id.clone().unwrap_or_default();
        match existing {
            Some(e) if e == (principal.to_owned(), role.clone(), scope.kind.clone(), scope_id) => {
                report.bindings.unchanged += 1;
            }
            Some(_) => report.conflicts.push(format!("role binding {}: pacod-id has another binding by this id", b.id)),
            None => add.push((
                b.id.clone(),
                NewBinding {
                    principal: principal.to_owned(),
                    role,
                    scope,
                    source: "import",
                    created_by: "import".into(),
                },
            )),
        }
    }
    let applied = bindings::import(tx, catalog, &add).await?;
    report.bindings.created += applied.added.len();
    // Held already under another id: the same role, nothing to add.
    report.bindings.unchanged += add.len() - applied.added.len();
    Ok(())
}

async fn invitations(
    tx: &mut PgConnection,
    realm: &str,
    export: &GwExport,
    principals: &BTreeMap<String, &'static str>,
    report: &mut Report,
) -> Result<(), ImportError> {
    for i in &export.invitations {
        let Some(hash) = hex(&i.token_hash).filter(|h| h.len() == 32) else {
            report.conflicts.push(format!("invitation {}: its token_hash is not 32 bytes in hex", i.id));
            continue;
        };
        if !plain_id(&i.id, "inv") {
            report.conflicts.push(format!("invitation {:?}: not an id pacod-id takes", i.id));
            continue;
        }
        let scope = match &i.workspace_id {
            Some(w) => ScopeRef::of("gw.workspace", w),
            None => ScopeRef::organisation(&i.organisation_id),
        };
        if db::path_of(tx, &scope).await?.is_none() {
            report.skipped.push(format!("invitation {}: its scope {scope} is not in the export", i.id));
            continue;
        }
        let email = normalize_email(&i.email);
        let role = format!("gw.{}", i.role);
        let (kind, scope_id) = (scope.kind.clone(), scope.id.clone().unwrap_or_default());
        let other: Option<String> = sqlx::query_scalar(
            "SELECT id FROM invitations WHERE email = $1 AND scope_type = $2 AND scope_id = $3 AND id <> $4",
        )
        .bind(&email)
        .bind(&kind)
        .bind(&scope_id)
        .bind(&i.id)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(other) = other {
            report.skipped.push(format!("invitation {}: pacod-id has {other} for {email} there already", i.id));
            continue;
        }
        // The inviter, when the export brings them; otherwise the invitation names nobody.
        let invited_by = if principals.contains_key(&i.invited_by) { i.invited_by.as_str() } else { "" };
        let r = sqlx::query(
            "INSERT INTO invitations (id, token_hash, email, role, scope_type, scope_id, realm, invited_by, emailed, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             ON CONFLICT (id) DO UPDATE SET token_hash = $2, role = $4, invited_by = $8, emailed = $9, expires_at = $11
             WHERE invitations.token_hash <> $2 OR invitations.role <> $4 OR invitations.expires_at <> $11
             RETURNING (xmax = 0) AS created",
        )
        .bind(&i.id)
        .bind(&hash)
        .bind(&email)
        .bind(&role)
        .bind(&kind)
        .bind(&scope_id)
        .bind(realm)
        .bind(invited_by)
        .bind(i.emailed)
        .bind(i.created_at)
        .bind(i.expires_at)
        .fetch_optional(&mut *tx)
        .await?;
        match r.map(|r| r.try_get::<bool, _>("created")).transpose()? {
            Some(true) => report.invitations.created += 1,
            Some(false) => report.invitations.updated += 1,
            None => report.invitations.unchanged += 1,
        }
    }
    Ok(())
}
