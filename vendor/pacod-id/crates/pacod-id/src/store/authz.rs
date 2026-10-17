//! Authorization data: the catalogue, organisations, product scopes and role
//! bindings. Functions that take a connection run inside the caller's transaction, which holds
//! `authz_head` (see `lock_head`) whenever it changes bindings.

use chrono::{DateTime, Utc};
use pacod_id_authz::{ORGANISATION, PLATFORM, ScopeRef};
use sqlx::{PgConnection, Row};

use super::{Store, StoreError};

/// How deep product scope types may nest below an organisation or the platform.
const MAX_DEPTH: usize = 8;

/// Takes the one row every authorization change goes through. Held until the transaction ends,
/// so checks that read bindings (the last-admin rule) see no change happen under them.
pub async fn lock_head(conn: &mut PgConnection) -> Result<i64, StoreError> {
    Ok(sqlx::query("SELECT version FROM authz_head FOR UPDATE").fetch_one(conn).await?.try_get("version")?)
}

/// A stored role binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingRow {
    pub id: String,
    pub principal_id: String,
    pub principal_kind: String,
    pub role: String,
    pub scope_type: String,
    /// '' for the platform.
    pub scope_id: String,
    pub source: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

impl BindingRow {
    pub fn scope(&self) -> ScopeRef {
        scope_ref(&self.scope_type, &self.scope_id)
    }

    /// The product whose role this is.
    pub fn product(&self) -> &str {
        self.role.split_once('.').map_or("", |(p, _)| p)
    }

    fn from_row(r: &sqlx::postgres::PgRow) -> Result<Self, StoreError> {
        Ok(BindingRow {
            id: r.try_get("id")?,
            principal_id: r.try_get("principal_id")?,
            principal_kind: r.try_get("principal_kind")?,
            role: r.try_get("role")?,
            scope_type: r.try_get("scope_type")?,
            scope_id: r.try_get("scope_id")?,
            source: r.try_get("source")?,
            created_by: r.try_get("created_by")?,
            created_at: r.try_get("created_at")?,
        })
    }
}

/// A scope from its stored type and id ('' for the platform).
pub fn scope_ref(kind: &str, id: &str) -> ScopeRef {
    if kind == PLATFORM { ScopeRef::platform() } else { ScopeRef::of(kind, id) }
}

fn parts(scope: &ScopeRef) -> (&str, &str) {
    (scope.kind.as_str(), scope.id.as_deref().unwrap_or(""))
}

/// The path from the platform to a scope, or `None` when the scope is not registered.
pub async fn path_of(conn: &mut PgConnection, scope: &ScopeRef) -> Result<Option<Vec<ScopeRef>>, StoreError> {
    let mut chain = vec![scope.clone()];
    let mut at = scope.clone();
    for _ in 0..MAX_DEPTH {
        let (kind, id) = parts(&at);
        if kind == PLATFORM {
            chain.reverse();
            return Ok(Some(chain));
        }
        if kind == ORGANISATION {
            let known =
                sqlx::query("SELECT 1 FROM organisations WHERE id = $1").bind(id).fetch_optional(&mut *conn).await?;
            if known.is_none() {
                return Ok(None);
            }
            chain.push(ScopeRef::platform());
            chain.reverse();
            return Ok(Some(chain));
        }
        let Some(r) = sqlx::query("SELECT parent_type, parent_id FROM scopes WHERE type = $1 AND id = $2")
            .bind(kind)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
        else {
            return Ok(None);
        };
        at = scope_ref(&r.try_get::<String, _>("parent_type")?, &r.try_get::<String, _>("parent_id")?);
        chain.push(at.clone());
    }
    Err(StoreError::Corrupt(format!("the scope {scope} nests deeper than {MAX_DEPTH}")))
}

/// A scope and every scope below it, the deepest first.
pub async fn subtree(conn: &mut PgConnection, root: &ScopeRef) -> Result<Vec<ScopeRef>, StoreError> {
    let (kind, id) = parts(root);
    let rows = sqlx::query(
        "WITH RECURSIVE sub(type, id, depth) AS (
             SELECT $1::text, $2::text, 0
             UNION ALL
             SELECT s.type, s.id, sub.depth + 1 FROM scopes s JOIN sub ON s.parent_type = sub.type AND s.parent_id = sub.id
             WHERE sub.depth < $3
         ) SELECT type, id FROM sub ORDER BY depth DESC, type, id",
    )
    .bind(kind)
    .bind(id)
    .bind(MAX_DEPTH as i32)
    .fetch_all(conn)
    .await?;
    rows.iter().map(|r| Ok(scope_ref(&r.try_get::<String, _>("type")?, &r.try_get::<String, _>("id")?))).collect()
}

fn unnest(scopes: &[ScopeRef]) -> (Vec<String>, Vec<String>) {
    scopes.iter().map(|s| (s.kind.clone(), s.id.clone().unwrap_or_default())).unzip()
}

/// The bindings at exactly these scopes; with `active_only`, only those of principals that are
/// not disabled (the holders that count for the last-admin rule).
pub async fn bindings_at(
    conn: &mut PgConnection,
    scopes: &[ScopeRef],
    active_only: bool,
) -> Result<Vec<BindingRow>, StoreError> {
    let (types, ids) = unnest(scopes);
    let rows = sqlx::query(
        "SELECT b.* FROM role_bindings b
         WHERE (b.scope_type, b.scope_id) IN (SELECT * FROM unnest($1::text[], $2::text[]))
           AND (NOT $3 OR (
                NOT EXISTS (SELECT 1 FROM accounts a WHERE a.id = b.principal_id AND a.status <> 'active')
            AND NOT EXISTS (SELECT 1 FROM service_accounts s WHERE s.id = b.principal_id AND s.status <> 'active')))
         ORDER BY b.created_at, b.id",
    )
    .bind(&types)
    .bind(&ids)
    .bind(active_only)
    .fetch_all(conn)
    .await?;
    rows.iter().map(BindingRow::from_row).collect()
}

/// A principal's bindings.
pub async fn bindings_of(conn: &mut PgConnection, principal: &str) -> Result<Vec<BindingRow>, StoreError> {
    let rows = sqlx::query("SELECT * FROM role_bindings WHERE principal_id = $1 ORDER BY created_at, id")
        .bind(principal)
        .fetch_all(conn)
        .await?;
    rows.iter().map(BindingRow::from_row).collect()
}

/// The bindings of a product's roles (`<product>.…`).
pub async fn bindings_of_product(conn: &mut PgConnection, product: &str) -> Result<Vec<BindingRow>, StoreError> {
    let rows = sqlx::query("SELECT * FROM role_bindings WHERE starts_with(role, $1) ORDER BY created_at, id")
        .bind(format!("{product}."))
        .fetch_all(conn)
        .await?;
    rows.iter().map(BindingRow::from_row).collect()
}

pub async fn binding(conn: &mut PgConnection, id: &str) -> Result<Option<BindingRow>, StoreError> {
    let r = sqlx::query("SELECT * FROM role_bindings WHERE id = $1").bind(id).fetch_optional(conn).await?;
    r.as_ref().map(BindingRow::from_row).transpose()
}

/// Inserts a binding unless the same one exists; whether it was new.
pub async fn binding_insert(conn: &mut PgConnection, b: &BindingRow) -> Result<bool, StoreError> {
    let r = sqlx::query(
        "INSERT INTO role_bindings (id, principal_id, principal_kind, role, scope_type, scope_id, source, created_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (principal_id, role, scope_type, scope_id) DO NOTHING",
    )
    .bind(&b.id)
    .bind(&b.principal_id)
    .bind(&b.principal_kind)
    .bind(&b.role)
    .bind(&b.scope_type)
    .bind(&b.scope_id)
    .bind(&b.source)
    .bind(&b.created_by)
    .execute(conn)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Deletes bindings by id; the ids that were there.
pub async fn bindings_delete(conn: &mut PgConnection, ids: &[String]) -> Result<Vec<String>, StoreError> {
    let rows =
        sqlx::query("DELETE FROM role_bindings WHERE id = ANY($1) RETURNING id").bind(ids).fetch_all(conn).await?;
    rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
}

/// Whether a principal exists, and of which kind.
pub async fn principal_kind(conn: &mut PgConnection, id: &str) -> Result<Option<&'static str>, StoreError> {
    let r = sqlx::query(
        "SELECT CASE WHEN EXISTS (SELECT 1 FROM accounts WHERE id = $1) THEN 'user'
                     WHEN EXISTS (SELECT 1 FROM service_accounts WHERE id = $1) THEN 'service' END AS kind",
    )
    .bind(id)
    .fetch_one(conn)
    .await?;
    Ok(match r.try_get::<Option<String>, _>("kind")?.as_deref() {
        Some("user") => Some("user"),
        Some("service") => Some("service"),
        _ => None,
    })
}

// The catalogue ------------------------------------------------------------------------------

/// A product's stored manifest (as pushed, local names) and its version.
#[derive(Debug, Clone)]
pub struct CatalogRow {
    pub product: String,
    pub version: i64,
    pub manifest: serde_json::Value,
    pub pushed_at: DateTime<Utc>,
}

impl Store {
    pub async fn catalog_rows(&self) -> Result<Vec<CatalogRow>, StoreError> {
        let rows = sqlx::query("SELECT product, version, manifest, pushed_at FROM catalog ORDER BY product")
            .fetch_all(self.pool())
            .await?;
        rows.iter()
            .map(|r| {
                Ok(CatalogRow {
                    product: r.try_get("product")?,
                    version: r.try_get("version")?,
                    manifest: r.try_get("manifest")?,
                    pushed_at: r.try_get("pushed_at")?,
                })
            })
            .collect()
    }

    /// The feed's current version.
    pub async fn authz_head(&self) -> Result<i64, StoreError> {
        Ok(sqlx::query("SELECT version FROM authz_head").fetch_one(self.pool()).await?.try_get("version")?)
    }

    /// Forgets changes older than `days`; followers behind them load the snapshot again.
    pub async fn authz_changes_prune(&self, days: u32) -> Result<u64, StoreError> {
        let r = sqlx::query("DELETE FROM authz_changes WHERE at < now() - make_interval(days => $1)")
            .bind(days as i32)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected())
    }
}

/// The product's catalogue row, locked for an update.
pub async fn catalog_row_for_update(conn: &mut PgConnection, product: &str) -> Result<Option<CatalogRow>, StoreError> {
    let r = sqlx::query("SELECT product, version, manifest, pushed_at FROM catalog WHERE product = $1 FOR UPDATE")
        .bind(product)
        .fetch_optional(conn)
        .await?;
    r.map(|r| {
        Ok(CatalogRow {
            product: r.try_get("product")?,
            version: r.try_get("version")?,
            manifest: r.try_get("manifest")?,
            pushed_at: r.try_get("pushed_at")?,
        })
    })
    .transpose()
}

/// Stores a product's manifest; its new version.
pub async fn catalog_put(
    conn: &mut PgConnection,
    product: &str,
    manifest: &serde_json::Value,
) -> Result<i64, StoreError> {
    let r = sqlx::query(
        "INSERT INTO catalog (product, manifest) VALUES ($1, $2)
         ON CONFLICT (product) DO UPDATE SET manifest = $2, version = catalog.version + 1, pushed_at = now()
         RETURNING version",
    )
    .bind(product)
    .bind(manifest)
    .fetch_one(conn)
    .await?;
    Ok(r.try_get("version")?)
}

/// The scope types of a product that have registered scopes, with how many.
pub async fn scope_types_in_use(conn: &mut PgConnection, product: &str) -> Result<Vec<(String, i64)>, StoreError> {
    let rows = sqlx::query("SELECT type, count(*) AS n FROM scopes WHERE product = $1 GROUP BY type ORDER BY type")
        .bind(product)
        .fetch_all(conn)
        .await?;
    rows.iter().map(|r| Ok((r.try_get("type")?, r.try_get("n")?))).collect()
}

pub async fn custom_roles_count(conn: &mut PgConnection, product: &str) -> Result<i64, StoreError> {
    Ok(sqlx::query("SELECT count(*) AS n FROM custom_roles WHERE product = $1")
        .bind(product)
        .fetch_one(conn)
        .await?
        .try_get("n")?)
}

// Organisations and product scopes -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganisationRow {
    pub id: String,
    pub slug: String,
    pub display_name: String,
    pub status: String,
    pub system: bool,
    pub products: Vec<String>,
}

pub async fn organisation(conn: &mut PgConnection, id: &str) -> Result<Option<OrganisationRow>, StoreError> {
    let r = sqlx::query(
        "SELECT o.*, coalesce(array_agg(p.product ORDER BY p.product) FILTER (WHERE p.product IS NOT NULL), '{}') AS products
         FROM organisations o LEFT JOIN organisation_products p ON p.organisation_id = o.id
         WHERE o.id = $1 GROUP BY o.id",
    )
    .bind(id)
    .fetch_optional(conn)
    .await?;
    r.map(|r| {
        Ok(OrganisationRow {
            id: r.try_get("id")?,
            slug: r.try_get("slug")?,
            display_name: r.try_get("display_name")?,
            status: r.try_get("status")?,
            system: r.try_get("system")?,
            products: r.try_get("products")?,
        })
    })
    .transpose()
}

/// Creates or renames an organisation and registers `product` with it. Fails with `Conflict`
/// when another organisation has the slug.
pub async fn organisation_upsert(
    conn: &mut PgConnection,
    id: &str,
    slug: &str,
    display_name: &str,
    system: bool,
    product: &str,
) -> Result<(), StoreError> {
    let r = sqlx::query(
        "INSERT INTO organisations (id, slug, display_name, system) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO UPDATE SET slug = $2, display_name = $3, system = organisations.system OR $4, updated_at = now()",
    )
    .bind(id)
    .bind(slug)
    .bind(display_name)
    .bind(system)
    .execute(&mut *conn)
    .await;
    match r {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(StoreError::Conflict(format!("another organisation has the slug {slug:?}")));
        }
        r => r?,
    };
    sqlx::query("INSERT INTO organisation_products (organisation_id, product) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(product)
        .execute(conn)
        .await?;
    Ok(())
}

/// Unregisters a product from an organisation; the products still registered.
pub async fn organisation_unregister(
    conn: &mut PgConnection,
    id: &str,
    product: &str,
) -> Result<Vec<String>, StoreError> {
    sqlx::query("DELETE FROM organisation_products WHERE organisation_id = $1 AND product = $2")
        .bind(id)
        .bind(product)
        .execute(&mut *conn)
        .await?;
    let rows = sqlx::query("SELECT product FROM organisation_products WHERE organisation_id = $1 ORDER BY product")
        .bind(id)
        .fetch_all(conn)
        .await?;
    rows.iter().map(|r| Ok(r.try_get("product")?)).collect()
}

/// A registered product scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeRow {
    pub kind: String,
    pub id: String,
    pub parent: ScopeRef,
    pub product: String,
    pub display_name: String,
    pub system: bool,
}

pub async fn scope(conn: &mut PgConnection, kind: &str, id: &str) -> Result<Option<ScopeRow>, StoreError> {
    let r = sqlx::query("SELECT * FROM scopes WHERE type = $1 AND id = $2")
        .bind(kind)
        .bind(id)
        .fetch_optional(conn)
        .await?;
    r.map(|r| {
        Ok(ScopeRow {
            kind: r.try_get("type")?,
            id: r.try_get("id")?,
            parent: scope_ref(&r.try_get::<String, _>("parent_type")?, &r.try_get::<String, _>("parent_id")?),
            product: r.try_get("product")?,
            display_name: r.try_get("display_name")?,
            system: r.try_get("system")?,
        })
    })
    .transpose()
}

pub async fn scope_upsert(conn: &mut PgConnection, s: &ScopeRow) -> Result<(), StoreError> {
    let (parent_type, parent_id) = parts(&s.parent);
    sqlx::query(
        "INSERT INTO scopes (type, id, parent_type, parent_id, product, display_name, system)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (type, id) DO UPDATE SET display_name = $6, system = scopes.system OR $7, updated_at = now()",
    )
    .bind(&s.kind)
    .bind(&s.id)
    .bind(parent_type)
    .bind(parent_id)
    .bind(&s.product)
    .bind(&s.display_name)
    .bind(s.system)
    .execute(conn)
    .await?;
    Ok(())
}

/// Deletes a product's invitations to one scope (its roles start with `<product>.`).
pub async fn invitations_delete_of_product(
    conn: &mut PgConnection,
    scope: &ScopeRef,
    product: &str,
) -> Result<u64, StoreError> {
    let (kind, id) = parts(scope);
    Ok(sqlx::query("DELETE FROM invitations WHERE scope_type = $1 AND scope_id = $2 AND starts_with(role, $3)")
        .bind(kind)
        .bind(id)
        .bind(format!("{product}."))
        .execute(conn)
        .await?
        .rows_affected())
}

/// Deletes these scopes (an organisation from `organisations`, the others from `scopes`) and the
/// invitations to them.
pub async fn scopes_delete(conn: &mut PgConnection, scopes: &[ScopeRef]) -> Result<(), StoreError> {
    let (types, ids) = unnest(scopes);
    sqlx::query(
        "DELETE FROM invitations WHERE (scope_type, scope_id) IN (SELECT * FROM unnest($1::text[], $2::text[]))",
    )
    .bind(&types)
    .bind(&ids)
    .execute(&mut *conn)
    .await?;
    sqlx::query("DELETE FROM scopes WHERE (type, id) IN (SELECT * FROM unnest($1::text[], $2::text[]))")
        .bind(&types)
        .bind(&ids)
        .execute(&mut *conn)
        .await?;
    let orgs: Vec<&str> = scopes.iter().filter(|s| s.kind == ORGANISATION).filter_map(|s| s.id.as_deref()).collect();
    sqlx::query("DELETE FROM organisations WHERE id = ANY($1)").bind(&orgs).execute(conn).await?;
    Ok(())
}

// Invitations --------------------------------------------------------------------------------

/// A stored invitation; the token itself is never stored.
#[derive(Debug, Clone, PartialEq)]
pub struct InvitationRow {
    pub id: String,
    pub email: String,
    pub role: String,
    pub scope_type: String,
    pub scope_id: String,
    pub realm: String,
    pub invited_by: String,
    pub emailed: bool,
    pub labels: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl InvitationRow {
    pub fn scope(&self) -> ScopeRef {
        scope_ref(&self.scope_type, &self.scope_id)
    }

    pub fn expired(&self) -> bool {
        self.expires_at <= Utc::now()
    }

    fn from_row(r: &sqlx::postgres::PgRow) -> Result<Self, StoreError> {
        Ok(InvitationRow {
            id: r.try_get("id")?,
            email: r.try_get("email")?,
            role: r.try_get("role")?,
            scope_type: r.try_get("scope_type")?,
            scope_id: r.try_get("scope_id")?,
            realm: r.try_get("realm")?,
            invited_by: r.try_get("invited_by")?,
            emailed: r.try_get("emailed")?,
            labels: r.try_get("labels")?,
            created_at: r.try_get("created_at")?,
            expires_at: r.try_get("expires_at")?,
        })
    }
}

/// What an invitation is made or renewed with.
pub struct InvitationWrite<'a> {
    pub token_hash: &'a [u8],
    pub email: &'a str,
    pub role: &'a str,
    pub scope: &'a ScopeRef,
    pub realm: &'a str,
    pub invited_by: &'a str,
    pub emailed: bool,
    pub labels: &'a serde_json::Value,
    pub expires_at: DateTime<Utc>,
}

/// One pending invitation per address and scope: a new one replaces the old, whose link stops
/// working.
pub async fn invitation_upsert(
    conn: &mut PgConnection,
    id: &str,
    w: &InvitationWrite<'_>,
) -> Result<InvitationRow, StoreError> {
    let (kind, scope_id) = parts(w.scope);
    let r = sqlx::query(
        "INSERT INTO invitations (id, token_hash, email, role, scope_type, scope_id, realm, invited_by, emailed, labels, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (email, scope_type, scope_id) DO UPDATE SET token_hash = $2, role = $4, realm = $7, invited_by = $8,
           emailed = $9, labels = $10, created_at = now(), expires_at = $11
         RETURNING *",
    )
    .bind(id)
    .bind(w.token_hash)
    .bind(w.email)
    .bind(w.role)
    .bind(kind)
    .bind(scope_id)
    .bind(w.realm)
    .bind(w.invited_by)
    .bind(w.emailed)
    .bind(w.labels)
    .bind(w.expires_at)
    .fetch_one(conn)
    .await?;
    InvitationRow::from_row(&r)
}

/// The pending invitation of an address at a scope, if any. An expired one is not pending: a
/// new invitation simply replaces it.
pub async fn invitation_pending(
    conn: &mut PgConnection,
    email: &str,
    scope: &ScopeRef,
) -> Result<Option<InvitationRow>, StoreError> {
    let (kind, scope_id) = parts(scope);
    let r = sqlx::query(
        "SELECT * FROM invitations WHERE email = $1 AND scope_type = $2 AND scope_id = $3 AND expires_at > now()
         FOR UPDATE",
    )
    .bind(email)
    .bind(kind)
    .bind(scope_id)
    .fetch_optional(conn)
    .await?;
    r.as_ref().map(InvitationRow::from_row).transpose()
}

pub async fn invitation(conn: &mut PgConnection, id: &str) -> Result<Option<InvitationRow>, StoreError> {
    let r = sqlx::query("SELECT * FROM invitations WHERE id = $1").bind(id).fetch_optional(conn).await?;
    r.as_ref().map(InvitationRow::from_row).transpose()
}

pub async fn invitation_by_token(conn: &mut PgConnection, hash: &[u8]) -> Result<Option<InvitationRow>, StoreError> {
    let r = sqlx::query("SELECT * FROM invitations WHERE token_hash = $1 FOR UPDATE")
        .bind(hash)
        .fetch_optional(conn)
        .await?;
    r.as_ref().map(InvitationRow::from_row).transpose()
}

pub async fn invitations_at(conn: &mut PgConnection, scopes: &[ScopeRef]) -> Result<Vec<InvitationRow>, StoreError> {
    let (types, ids) = unnest(scopes);
    let rows = sqlx::query(
        "SELECT * FROM invitations WHERE (scope_type, scope_id) IN (SELECT * FROM unnest($1::text[], $2::text[]))
         ORDER BY created_at DESC, id",
    )
    .bind(&types)
    .bind(&ids)
    .fetch_all(conn)
    .await?;
    rows.iter().map(InvitationRow::from_row).collect()
}

pub async fn invitations_for_email(conn: &mut PgConnection, email: &str) -> Result<Vec<InvitationRow>, StoreError> {
    let rows =
        sqlx::query("SELECT * FROM invitations WHERE email = $1 AND expires_at > now() ORDER BY created_at DESC, id")
            .bind(email)
            .fetch_all(conn)
            .await?;
    rows.iter().map(InvitationRow::from_row).collect()
}

pub async fn invitation_delete(conn: &mut PgConnection, id: &str) -> Result<bool, StoreError> {
    Ok(sqlx::query("DELETE FROM invitations WHERE id = $1").bind(id).execute(conn).await?.rows_affected() == 1)
}

/// A new link for an invitation (sent again): the old link stops working.
pub async fn invitation_renew(
    conn: &mut PgConnection,
    id: &str,
    token_hash: &[u8],
    invited_by: &str,
    emailed: bool,
    expires_at: DateTime<Utc>,
) -> Result<InvitationRow, StoreError> {
    let r = sqlx::query(
        "UPDATE invitations SET token_hash = $2, invited_by = $3, emailed = $4, created_at = now(), expires_at = $5
         WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(token_hash)
    .bind(invited_by)
    .bind(emailed)
    .bind(expires_at)
    .fetch_one(conn)
    .await?;
    InvitationRow::from_row(&r)
}
