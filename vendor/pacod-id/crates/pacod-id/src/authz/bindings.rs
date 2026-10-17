//! Changing role bindings. Every change goes through `apply`, inside a
//! transaction that holds `authz_head` from before it reads anything: the last-admin rule is
//! checked on the bindings as they are, and two removals cannot each leave the other's holder as
//! the last one.

use std::collections::BTreeMap;

use pacod_id_authz::{Catalog, HeldBinding, ScopeRef, Subject, Violation};
use serde_json::json;
use sqlx::PgConnection;

use super::path_json;
use crate::crypto::tokens;
use crate::store::authz::{self as db, BindingRow};
use crate::store::{StoreError, changes};

/// Whose rights may let a change leave a protected role's last holder go.
#[derive(Debug, Clone, Copy)]
pub enum Protect<'a> {
    /// The rule holds, with the bypass the role's manifest gives this actor.
    As(&'a Subject),
    /// The rule does not apply: the scope itself goes, or the change is pacod-id's own
    /// (bootstrap, import, a product's catalogue migration).
    Skip,
}

/// A binding to add.
#[derive(Debug, Clone)]
pub struct NewBinding {
    pub principal: String,
    pub role: String,
    pub scope: ScopeRef,
    /// manual, invitation, bootstrap, import or policy.
    pub source: &'static str,
    pub created_by: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ChangeError {
    #[error("the scope {0} is not registered")]
    ScopeUnknown(String),
    #[error("no account or service account {0}")]
    PrincipalUnknown(String),
    #[error("the change leaves {} without a {}", .0.scope, .0.role)]
    LastHolder(Violation),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<sqlx::Error> for ChangeError {
    fn from(e: sqlx::Error) -> Self {
        ChangeError::Store(e.into())
    }
}

/// Paths of scopes, looked up once per change.
#[derive(Default)]
struct Paths(BTreeMap<String, Option<Vec<ScopeRef>>>);

impl Paths {
    async fn of(&mut self, conn: &mut PgConnection, scope: &ScopeRef) -> Result<Option<Vec<ScopeRef>>, StoreError> {
        let key = scope.to_string();
        if let Some(p) = self.0.get(&key) {
            return Ok(p.clone());
        }
        let p = db::path_of(conn, scope).await?;
        self.0.insert(key, p.clone());
        Ok(p)
    }
}

/// What a change did: the version of its last event, or `None` when nothing changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Applied {
    pub seq: Option<i64>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// Removes `remove` and adds `add` in `conn`'s transaction, which must hold `authz_head`
/// (`db::lock_head`). Refuses a change that leaves a scope without a holder of a protected role,
/// unless `protect` allows it. Adding a binding that exists already changes nothing.
pub async fn apply(
    conn: &mut PgConnection,
    catalog: &Catalog,
    protect: Protect<'_>,
    remove: &[BindingRow],
    add: &[NewBinding],
) -> Result<Applied, ChangeError> {
    let add: Vec<(Option<&str>, &NewBinding)> = add.iter().map(|b| (None, b)).collect();
    apply_ids(conn, catalog, protect, remove, &add).await
}

/// Adds bindings that keep the ids they had in another system (an import), as `apply` does
/// otherwise; nothing is protected, nothing removed.
pub async fn import(
    conn: &mut PgConnection,
    catalog: &Catalog,
    add: &[(String, NewBinding)],
) -> Result<Applied, ChangeError> {
    let add: Vec<(Option<&str>, &NewBinding)> = add.iter().map(|(id, b)| (Some(id.as_str()), b)).collect();
    apply_ids(conn, catalog, Protect::Skip, &[], &add).await
}

async fn apply_ids(
    conn: &mut PgConnection,
    catalog: &Catalog,
    protect: Protect<'_>,
    remove: &[BindingRow],
    add: &[(Option<&str>, &NewBinding)],
) -> Result<Applied, ChangeError> {
    let mut paths = Paths::default();
    let mut new_rows = Vec::new();
    for &(id, b) in add {
        let path = paths.of(conn, &b.scope).await?.ok_or_else(|| ChangeError::ScopeUnknown(b.scope.to_string()))?;
        let kind = db::principal_kind(conn, &b.principal)
            .await?
            .ok_or_else(|| ChangeError::PrincipalUnknown(b.principal.clone()))?;
        let scope_id = b.scope.id.clone().unwrap_or_default();
        new_rows.push((
            BindingRow {
                id: id.map_or_else(|| tokens::new_id("rb"), str::to_owned),
                principal_id: b.principal.clone(),
                principal_kind: kind.to_owned(),
                role: b.role.clone(),
                scope_type: b.scope.kind.clone(),
                scope_id,
                source: b.source.to_owned(),
                created_by: b.created_by.clone(),
                created_at: chrono::Utc::now(),
            },
            path,
        ));
    }

    if let Protect::As(actor) = protect
        && !remove.is_empty()
    {
        let scopes: Vec<ScopeRef> = remove.iter().map(BindingRow::scope).collect();
        let mut before = Vec::new();
        for b in db::bindings_at(conn, &scopes, true).await? {
            if let Some(path) = paths.of(conn, &b.scope()).await? {
                before.push(HeldBinding { principal: b.principal_id.clone(), role: b.role.clone(), path });
            }
        }
        let removed: Vec<&str> = remove.iter().map(|b| b.id.as_str()).collect();
        let kept =
            db::bindings_at(conn, &scopes, true).await?.into_iter().filter(|b| !removed.contains(&b.id.as_str()));
        let mut after = Vec::new();
        for b in kept {
            if let Some(path) = paths.of(conn, &b.scope()).await? {
                after.push(HeldBinding { principal: b.principal_id.clone(), role: b.role.clone(), path });
            }
        }
        for (row, path) in &new_rows {
            after.push(HeldBinding { principal: row.principal_id.clone(), role: row.role.clone(), path: path.clone() });
        }
        catalog.check_protected(actor, &before, &after).map_err(ChangeError::LastHolder)?;
    }

    let mut applied = Applied::default();
    let ids: Vec<String> = remove.iter().map(|b| b.id.clone()).collect();
    let gone = db::bindings_delete(conn, &ids).await?;
    for b in remove.iter().filter(|b| gone.contains(&b.id)) {
        let path = paths.of(conn, &b.scope()).await?.unwrap_or_default();
        let seq = changes::record(
            conn,
            "binding.removed",
            Some(b.product()),
            Some(&b.principal_id),
            json!({ "id": b.id, "principal": b.principal_id, "principal_kind": b.principal_kind, "role": b.role, "path": path_json(&path), "source": b.source }),
        )
        .await?;
        applied.seq = Some(seq);
        applied.removed.push(b.id.clone());
    }
    for (row, path) in new_rows {
        if !db::binding_insert(conn, &row).await? {
            continue;
        }
        let seq = changes::record(
            conn,
            "binding.added",
            Some(row.product()),
            Some(&row.principal_id),
            json!({
                "id": row.id, "principal": row.principal_id, "principal_kind": row.principal_kind, "role": row.role,
                "path": path_json(&path), "source": row.source,
            }),
        )
        .await?;
        applied.seq = Some(seq);
        applied.added.push(row.id);
    }
    Ok(applied)
}

/// A principal's bindings as the engine reads them (bindings whose scope is gone are left out).
pub async fn subject_bindings(
    conn: &mut PgConnection,
    principal: &str,
) -> Result<Vec<pacod_id_authz::Binding>, StoreError> {
    let mut paths = Paths::default();
    let mut out = Vec::new();
    for b in db::bindings_of(conn, principal).await? {
        if let Some(path) = paths.of(conn, &b.scope()).await? {
            out.push(pacod_id_authz::Binding::new(b.role, path));
        }
    }
    Ok(out)
}

/// Refuses when `principal` going away (disabled or deleted) would leave a scope without a
/// holder of a protected role, unless `actor`'s rights let it. Call it holding `authz_head`.
pub async fn check_leaving(
    conn: &mut PgConnection,
    catalog: &Catalog,
    actor: &Subject,
    principal: &str,
) -> Result<(), ChangeError> {
    let own = db::bindings_of(conn, principal).await?;
    if own.is_empty() {
        return Ok(());
    }
    let scopes: Vec<ScopeRef> = own.iter().map(BindingRow::scope).collect();
    let mut paths = Paths::default();
    let (mut before, mut after) = (Vec::new(), Vec::new());
    for b in db::bindings_at(conn, &scopes, true).await? {
        if let Some(path) = paths.of(conn, &b.scope()).await? {
            let held = HeldBinding { principal: b.principal_id.clone(), role: b.role.clone(), path };
            if b.principal_id != principal {
                after.push(held.clone());
            }
            before.push(held);
        }
    }
    catalog.check_protected(actor, &before, &after).map_err(ChangeError::LastHolder)
}
