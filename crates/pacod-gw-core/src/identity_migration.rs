//! Moving the gateway's people to pacod-id (runbook section 16): `pacod-gw-core identity
//! export | rollback | finalize`.
//!
//! 1. `export` writes the gateway's users, admin tokens, role bindings and invitations (raw, with
//!    their hashes), its organisations and workspaces, the session settings and the gateway's
//!    manifest to a file, for `pacod-id import gw`.
//! 2. The core then runs with `PGW_AUTH=oidc`. Its own documents stay, unused.
//! 3. `rollback` says whether returning to `PGW_AUTH=local` loses nothing: only while pacod-id
//!    recorded no identity write since the import.
//! 4. `finalize` deletes the documents for good, once every instance runs in OIDC mode and
//!    nothing was added here after the export; the core then refuses `PGW_AUTH=local`.

use std::io::Write as _;

use anyhow::{Context, bail};
use pacod_id_authz::Manifest;
use pacod_id_client::IdClient;
use serde_json::{Value, json};

use crate::model::{Doc, GlobalSettings, Organisation, SYSTEM_ORG_ID, SYSTEM_WS_ID, Workspace};
use crate::store::identity::IDENTITY_KINDS;
use crate::store::{AuditEntry, Store};

/// The export's format, as `pacod-id import gw` reads it.
pub const FORMAT: &str = "pacod-gw-identity/1";

/// The settings that concern signing in, carried along for the person setting up the realm.
const SESSION_SETTINGS: [&str; 6] = [
    "session_hours",
    "remember_me_days",
    "require_verified_email",
    "verification_link_hours",
    "reset_link_minutes",
    "invitation_link_days",
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The export document.
pub async fn export(store: &Store) -> anyhow::Result<Value> {
    let kinds = [Organisation::KIND, Workspace::KIND, GlobalSettings::KIND, "user", "admin_token", "role_binding"];
    let view = store.identity_export_view(&kinds).await?;
    let objects = &view.objects;
    let of = |kind: &'static str| objects.iter().filter(move |r| r.kind == kind);
    let raw = |kind: &'static str| -> Vec<Value> {
        of(kind)
            .map(|r| json!({ "id": r.id, "created_at": r.created_at, "updated_at": r.updated_at, "doc": r.doc }))
            .collect()
    };
    let settings = of(GlobalSettings::KIND)
        .next()
        .map(|r| SESSION_SETTINGS.iter().filter_map(|k| Some((k.to_string(), r.doc.get(*k)?.clone()))).collect())
        .unwrap_or_else(serde_json::Map::new);
    let manifest = Manifest::from_yaml(crate::identity::MANIFEST).context("authz/gw.yaml")?.source().clone();
    Ok(json!({
        "format": FORMAT,
        "exported_at": chrono::Utc::now(),
        "config_version": view.config_version,
        "manifest": manifest,
        "settings": settings,
        "organisations": of(Organisation::KIND).map(|r| json!({
            "id": r.id, "name": r.name.clone().unwrap_or_else(|| r.doc["name"].as_str().unwrap_or_default().to_owned()),
            "display_name": r.doc["display_name"], "system": r.id == SYSTEM_ORG_ID,
        })).collect::<Vec<_>>(),
        "workspaces": of(Workspace::KIND).map(|r| json!({
            "id": r.id, "organisation_id": r.parent, "name": r.name, "display_name": r.doc["display_name"],
            "system": r.id == SYSTEM_WS_ID,
        })).collect::<Vec<_>>(),
        "users": raw("user"),
        "admin_tokens": raw("admin_token"),
        "role_bindings": raw("role_binding"),
        "invitations": view.invitations.iter().map(|i| json!({
            "id": i.id, "token_hash": hex(&i.token_hash), "email": i.email, "role": i.role,
            "organisation_id": i.organisation_id, "workspace_id": i.workspace_id, "invited_by": i.invited_by,
            "emailed": i.emailed, "created_at": i.created_at, "expires_at": i.expires_at,
        })).collect::<Vec<_>>(),
    }))
}

/// Writes the export to a new file only its owner reads: it holds password and token hashes.
/// Answers what it holds, in a line.
pub async fn write_export(store: &Store, path: &std::path::Path) -> anyhow::Result<String> {
    let doc = export(store).await?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file =
        options.open(path).with_context(|| format!("creating {} (it must not exist yet)", path.display()))?;
    file.write_all(serde_json::to_string_pretty(&doc)?.as_bytes())?;
    file.sync_all()?;
    let n = |k: &str| doc[k].as_array().map_or(0, Vec::len);
    Ok(format!(
        "{}: {} users, {} admin tokens, {} role bindings, {} invitations, {} organisations, {} workspaces (configuration version {})",
        path.display(),
        n("users"),
        n("admin_tokens"),
        n("role_bindings"),
        n("invitations"),
        n("organisations"),
        n("workspaces"),
        doc["config_version"]
    ))
}

/// Whether the gateway may return to its own sign-in (`PGW_AUTH=local`) without losing anything:
/// pacod-id recorded no identity write since the import. The answer, or why not.
pub async fn rollback(store: &Store, client: &IdClient) -> anyhow::Result<String> {
    if let Some(at) = store.identity_finalized().await? {
        bail!(
            "identity finalize ran on {at}: the gateway's own users are gone, and PGW_AUTH=local is not possible any more"
        );
    }
    let Some(since) = client.last_import().await.context("asking pacod-id about the import")? else {
        return Ok("pacod-id has no import of the gateway: nothing moved, PGW_AUTH=local works as before".into());
    };
    let writes = since["writes"].as_i64().unwrap_or(0);
    if writes > 0 {
        let first: Vec<&str> = since["first"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        bail!(
            "pacod-id has {writes} identity changes since the import of {} that the gateway's own sign-in would not \
             know (the first: {}). Stay with PGW_AUTH=oidc",
            since["imported_at"].as_str().unwrap_or("?"),
            first.join("; ")
        );
    }
    Ok(format!(
        "nothing was written at pacod-id since the import of {}: set PGW_AUTH=local and restart every core instance",
        since["imported_at"].as_str().unwrap_or("?")
    ))
}

/// How long an instance counts as running after its last heartbeat, for finalize.
const LIVE_WITHIN_SECS: f64 = 60.0;

/// Deletes the gateway's own users, admin tokens, role bindings and invitations, which pacod-id
/// holds now, and with them the console sessions and one-time links. Refused while an instance
/// still signs people in itself, before pacod-id has the import, or while something was added
/// here after the export (it would be lost).
pub async fn finalize(store: &Store, client: &IdClient) -> anyhow::Result<String> {
    if let Some(at) = store.identity_finalized().await? {
        return Ok(format!("identity finalize ran already, on {at}"));
    }
    let Some(import) = client.last_import().await.context("asking pacod-id about the import")? else {
        bail!("pacod-id has no import of the gateway: run `pacod-id import gw` with this gateway's export first");
    };
    let local = store.local_auth_instances(LIVE_WITHIN_SECS).await?;
    if !local.is_empty() {
        bail!(
            "these instances still sign people in themselves (PGW_AUTH=local), or have not caught up with pacod-id's \
             feed yet: {}. Restart them with PGW_AUTH=oidc first",
            local.join(", ")
        );
    }
    let version = import["source_version"].as_i64().context("pacod-id's import names no configuration version")?;
    let exported_at = import["exported_at"]
        .as_str()
        .and_then(|s| s.parse().ok())
        .context("pacod-id's import names no export time")?;
    let newer = store.identity_written_after(version, exported_at).await?;
    if !newer.is_empty() {
        bail!(
            "changed here after the export, so pacod-id does not know: {}. Export and import again: the import \
             brings what is new and retires what the gateway no longer has",
            newer.join(", ")
        );
    }
    let mut tx = store.begin().await?;
    let mut deleted = 0;
    for kind in IDENTITY_KINDS {
        for row in tx.list(kind, None).await? {
            tx.delete(&row.id, AuditEntry::system("identity-finalize")).await?;
            deleted += 1;
        }
    }
    tx.identity_finalize(&import).await?;
    tx.commit().await?;
    Ok(format!(
        "{deleted} users, admin tokens and role bindings deleted, with the invitations, console sessions and links; \
         the core no longer starts with PGW_AUTH=local"
    ))
}
