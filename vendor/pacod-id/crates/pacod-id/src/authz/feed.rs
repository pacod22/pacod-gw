//! The feed products follow: a snapshot of what they need, then the changes after
//! its `seq`, by long poll. One `LISTEN pid_authz` per instance wakes every waiting request; when
//! that connection is lost, the instance polls the head every second until it is back.

use std::collections::BTreeMap;
use std::time::Duration;

use pacod_id_authz::{ORGANISATION, PLATFORM, ScopeRef};
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::postgres::PgListener;
use tokio::sync::watch;

use super::path_json;
use crate::http::Shared;
use crate::store::StoreError;
use crate::store::authz::scope_ref;

/// Changes are kept this long; a follower further behind loads the snapshot again.
pub const KEEP_DAYS: u32 = 7;
/// Revoked sessions in the snapshot: the access tokens they issued live at most 15 minutes.
const REVOKED_SIDS_MINUTES: i32 = 15;
/// At most this many changes are read per answer.
const PAGE: i64 = 1000;
const CHANNEL: &str = "pid_authz";

/// The newest feed version this instance knows of.
pub struct Feed {
    head: watch::Sender<i64>,
}

impl Feed {
    pub fn new(head: i64) -> Self {
        Feed { head: watch::Sender::new(head) }
    }

    pub fn subscribe(&self) -> watch::Receiver<i64> {
        self.head.subscribe()
    }

    pub fn saw(&self, version: i64) {
        self.head.send_if_modified(|v| {
            let newer = version > *v;
            if newer {
                *v = version;
            }
            newer
        });
    }
}

/// Follows `pid_authz` until `stop`. Runs on every instance.
pub async fn listen(st: Shared, mut stop: watch::Receiver<bool>) {
    loop {
        match PgListener::connect_with(st.store.pool()).await {
            Ok(mut listener) => match listener.listen(CHANNEL).await {
                Ok(()) => {
                    // Whatever committed while nobody listened.
                    if let Ok(h) = st.store.authz_head().await {
                        st.feed.saw(h);
                    }
                    loop {
                        tokio::select! {
                            _ = stop.changed() => return,
                            n = listener.recv() => match n {
                                Ok(n) => {
                                    if let Ok(v) = n.payload().parse() {
                                        st.feed.saw(v);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "the feed's LISTEN connection is lost");
                                    break;
                                }
                            },
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "LISTEN pid_authz failed"),
            },
            Err(e) => tracing::warn!(error = %e, "the feed's LISTEN connection failed"),
        }
        // Until the next attempt: poll.
        for _ in 0..5 {
            tokio::select! {
                _ = stop.changed() => return,
                _ = tokio::time::sleep(Duration::from_secs(1)) => {
                    if let Ok(h) = st.store.authz_head().await {
                        st.feed.saw(h);
                    }
                }
            }
        }
    }
}

/// Whether an event concerns `product`: its own, pacod-id's, and those of no product (sessions,
/// principals, organisations).
fn concerns(event_product: Option<&str>, product: &str) -> bool {
    event_product.is_none_or(|p| p == product || p == "id")
}

#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("the changes after this seq are no longer kept; load the snapshot")]
    Gone,
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<sqlx::Error> for FeedError {
    fn from(e: sqlx::Error) -> Self {
        FeedError::Store(e.into())
    }
}

/// The changes after `after` that concern `product`, and the version the follower has seen
/// once it has read them: the last one scanned, so changes for other products do not hold it
/// back.
pub async fn changes(st: &Shared, product: &str, after: i64) -> Result<(i64, Vec<Value>), FeedError> {
    let head = st.store.authz_head().await?;
    if after > head {
        return Err(FeedError::Gone);
    }
    if after == head {
        return Ok((head, vec![]));
    }
    let oldest: Option<i64> =
        sqlx::query("SELECT min(version) AS v FROM authz_changes").fetch_one(st.store.pool()).await?.try_get("v")?;
    if oldest.is_none_or(|m| after < m - 1) {
        return Err(FeedError::Gone);
    }
    let rows = sqlx::query(
        "SELECT version, at, kind, product, payload FROM authz_changes WHERE version > $1 AND version <= $2
         ORDER BY version LIMIT $3",
    )
    .bind(after)
    .bind(head)
    .bind(PAGE)
    .fetch_all(st.store.pool())
    .await?;
    let seen = if rows.len() as i64 == PAGE { rows.last().map_or(head, |r| r.get("version")) } else { head };
    let mut events = Vec::new();
    for r in &rows {
        let p: Option<String> = r.try_get("product")?;
        if !concerns(p.as_deref(), product) {
            continue;
        }
        let mut e = json!({
            "seq": r.try_get::<i64, _>("version")?,
            "at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("at")?,
            "kind": r.try_get::<String, _>("kind")?,
        });
        if let (Value::Object(payload), Value::Object(event)) = (r.try_get::<Value, _>("payload")?, &mut e) {
            for (k, v) in payload {
                event.entry(k).or_insert(v);
            }
        }
        events.push(e);
    }
    Ok((seen, events))
}

/// Everything `product` needs to start following, as of one `seq` (one REPEATABLE READ
/// transaction, so no change is in the data without being before the `seq`, or after it without
/// being in the data).
pub async fn snapshot(st: &Shared, product: &str) -> Result<Value, StoreError> {
    let mut tx = st.store.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *tx).await?;
    let seq: i64 = sqlx::query("SELECT version FROM authz_head").fetch_one(&mut *tx).await?.try_get("version")?;

    let catalog = sqlx::query("SELECT version, manifest FROM catalog WHERE product = $1")
        .bind(product)
        .fetch_optional(&mut *tx)
        .await?
        .map(|r| -> Result<Value, StoreError> {
            Ok(json!({ "version": r.try_get::<i64, _>("version")?, "manifest": r.try_get::<Value, _>("manifest")? }))
        })
        .transpose()?;

    // Scopes: the organisations, and the product's own scopes with their paths.
    let mut parents: BTreeMap<String, ScopeRef> = BTreeMap::new();
    let mut scopes = Vec::new();
    for r in sqlx::query(
        "SELECT o.id, o.slug, o.display_name, o.status, o.system,
                coalesce(array_agg(p.product ORDER BY p.product) FILTER (WHERE p.product IS NOT NULL), '{}') AS products
         FROM organisations o LEFT JOIN organisation_products p ON p.organisation_id = o.id GROUP BY o.id ORDER BY o.id",
    )
    .fetch_all(&mut *tx)
    .await?
    {
        let id: String = r.try_get("id")?;
        let scope = ScopeRef::organisation(&id);
        parents.insert(scope.to_string(), ScopeRef::platform());
        scopes.push(json!({
            "type": ORGANISATION, "id": id, "slug": r.try_get::<String, _>("slug")?,
            "display_name": r.try_get::<String, _>("display_name")?, "status": r.try_get::<String, _>("status")?,
            "system": r.try_get::<bool, _>("system")?, "products": r.try_get::<Vec<String>, _>("products")?,
            "path": path_json(&[ScopeRef::platform(), scope]),
        }));
    }
    let product_scopes = sqlx::query("SELECT * FROM scopes WHERE product = $1 ORDER BY type, id")
        .bind(product)
        .fetch_all(&mut *tx)
        .await?;
    for r in &product_scopes {
        let scope = scope_ref(&r.try_get::<String, _>("type")?, &r.try_get::<String, _>("id")?);
        let parent = scope_ref(&r.try_get::<String, _>("parent_type")?, &r.try_get::<String, _>("parent_id")?);
        parents.insert(scope.to_string(), parent);
    }
    let path = |scope: &ScopeRef| -> Option<Vec<ScopeRef>> {
        let mut chain = vec![scope.clone()];
        let mut at = scope.clone();
        while at.kind != PLATFORM {
            at = parents.get(&at.to_string())?.clone();
            chain.push(at.clone());
            if chain.len() > 10 {
                return None;
            }
        }
        chain.reverse();
        Some(chain)
    };
    for r in &product_scopes {
        let scope = scope_ref(&r.try_get::<String, _>("type")?, &r.try_get::<String, _>("id")?);
        let Some(p) = path(&scope) else { continue };
        scopes.push(json!({
            "type": scope.kind, "id": scope.id, "parent": p[p.len() - 2].to_string(),
            "display_name": r.try_get::<String, _>("display_name")?, "system": r.try_get::<bool, _>("system")?,
            "path": path_json(&p),
        }));
    }

    let mut bindings = Vec::new();
    for r in sqlx::query(
        "SELECT id, principal_id, principal_kind, role, scope_type, scope_id, source FROM role_bindings
         WHERE starts_with(role, $1) OR starts_with(role, 'id.') ORDER BY id",
    )
    .bind(format!("{product}."))
    .fetch_all(&mut *tx)
    .await?
    {
        let scope = scope_ref(&r.try_get::<String, _>("scope_type")?, &r.try_get::<String, _>("scope_id")?);
        // A binding to a scope of another product (none today) or a deleted one counts nowhere.
        let Some(p) = path(&scope) else { continue };
        bindings.push(json!({
            "id": r.try_get::<String, _>("id")?, "principal": r.try_get::<String, _>("principal_id")?,
            "principal_kind": r.try_get::<String, _>("principal_kind")?, "role": r.try_get::<String, _>("role")?,
            "path": path_json(&p), "source": r.try_get::<String, _>("source")?,
        }));
    }

    let disabled: Vec<String> = sqlx::query(
        "SELECT id FROM accounts WHERE status <> 'active' UNION ALL SELECT id FROM service_accounts WHERE status <> 'active' ORDER BY 1",
    )
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| r.try_get("id"))
    .collect::<Result<_, _>>()?;
    let mut revoked_sids = Vec::new();
    for r in sqlx::query(
        "SELECT id, account_id FROM sso_sessions WHERE revoked_at > now() - make_interval(mins => $1) ORDER BY revoked_at",
    )
    .bind(REVOKED_SIDS_MINUTES)
    .fetch_all(&mut *tx)
    .await?
    {
        revoked_sids.push(json!({ "sid": r.try_get::<String, _>("id")?, "sub": r.try_get::<String, _>("account_id")? }));
    }
    let deleted: Vec<String> =
        sqlx::query("SELECT account_id FROM account_deletions WHERE $1 = ANY(pending_products) ORDER BY 1")
            .bind(product)
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(|r| r.try_get("account_id"))
            .collect::<Result<_, _>>()?;
    tx.commit().await?;
    Ok(json!({
        "seq": seq,
        "product": product,
        "catalog": catalog,
        "scopes": scopes,
        "bindings": bindings,
        "disabled": disabled,
        "deleted": deleted,
        "revoked_sids": revoked_sids,
    }))
}
