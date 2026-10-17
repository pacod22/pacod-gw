//! The first admin: made at start from PID_BOOTSTRAP_ADMIN_* when no account has
//! the address, with its roles at the platform. A binding to a role of a product whose catalogue
//! has not arrived waits, counting nowhere, until it does. Every instance may run this at once:
//! the feed's head lock lets one of them create the account.

use anyhow::Context;
use pacod_id_authz::ScopeRef;
use serde_json::json;

use super::bindings::{self, NewBinding, Protect};
use crate::http::Shared;
use crate::store::accounts::account_insert;
use crate::store::authz as db;
use crate::store::{AuditEntry, NewAccount, normalize_email};
use crate::{crypto, policy};

pub async fn ensure_admin(st: &Shared) -> anyhow::Result<()> {
    let Some(admin) = st.boot.bootstrap_admin.clone() else { return Ok(()) };
    let email = normalize_email(&admin.email);
    if st.store.account_by_email(&email).await?.is_some() {
        tracing::info!(%email, "the bootstrap admin's account exists; PID_BOOTSTRAP_ADMIN_* change nothing");
        return Ok(());
    }
    let realm = match &admin.realm {
        Some(name) => {
            st.config.realms.get(name).with_context(|| format!("PID_BOOTSTRAP_ADMIN_REALM: no realm {name}"))?
        }
        None if st.config.realms.len() == 1 => st.config.realms.values().next().expect("one realm"),
        None => anyhow::bail!("PID_BOOTSTRAP_ADMIN_REALM is needed: the configuration has more than one realm"),
    };
    let catalog = st.catalog.get();
    for role in &admin.roles {
        let (product, _) =
            role.split_once('.').with_context(|| format!("PID_BOOTSTRAP_ADMIN_ROLES: {role} is not product.role"))?;
        match catalog.manifest(product) {
            Some(m) => {
                let fits = m.role(role).is_some_and(|r| r.at.iter().any(|k| k == pacod_id_authz::PLATFORM));
                anyhow::ensure!(fits, "PID_BOOTSTRAP_ADMIN_ROLES: {role} is not a role held at the platform");
            }
            None => tracing::info!(%role, "the bootstrap admin's role waits for its product's catalogue"),
        }
    }
    let password = admin
        .password
        .clone()
        .context("PID_BOOTSTRAP_ADMIN_PASSWORD (or _FILE) is needed to create the bootstrap admin")?;
    let min = realm.role_holder_password_min_length.max(realm.password_min_length);
    if let Err(p) = policy::check(&password, min) {
        anyhow::bail!(
            "PID_BOOTSTRAP_ADMIN_PASSWORD is refused ({}); role holders need {min} characters or more",
            p.code()
        );
    }
    let cost = st.boot.password_cost;
    let hash = tokio::task::spawn_blocking(move || crypto::password::hash(&password, cost)).await??;

    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let taken = sqlx::query("SELECT 1 FROM accounts WHERE email = $1").bind(&email).fetch_optional(&mut *tx).await?;
    if taken.is_some() {
        return Ok(());
    }
    let account = account_insert(
        &mut tx,
        &NewAccount {
            id: crypto::tokens::new_id("usr"),
            email: email.clone(),
            display_name: String::new(),
            locale: realm.default_locale.clone(),
            password_hash: Some(hash),
            verified_by: Some("admin".into()),
            signup_realm: realm.name.clone(),
        },
    )
    .await?;
    let add: Vec<NewBinding> = admin
        .roles
        .iter()
        .map(|role| NewBinding {
            principal: account.id.clone(),
            role: role.clone(),
            scope: ScopeRef::platform(),
            source: "bootstrap",
            created_by: "bootstrap".into(),
        })
        .collect();
    bindings::apply(&mut tx, &catalog, Protect::Skip, &[], &add).await?;
    tx.commit().await?;
    st.store
        .audit(
            &AuditEntry {
                realm: Some(realm.name.clone()),
                ..AuditEntry::new("account_created").by("bootstrap", "system").subject(&account.id)
            }
            .detail(json!({ "how": "bootstrap", "roles": admin.roles })),
        )
        .await?;
    tracing::info!(%email, roles = ?admin.roles, "created the bootstrap admin");
    Ok(())
}
