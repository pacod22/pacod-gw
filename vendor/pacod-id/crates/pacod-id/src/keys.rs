//! Signing keys and the JWKS.
//!
//! A key is `next` (published, not yet signing), `active` (signs; one per algorithm) or
//! `retired` (still published, so tokens it signed keep verifying). The maintenance job makes
//! the next key 24 hours before it is needed, promotes it after 90 days, and deletes retired
//! keys two days and an hour after they stopped signing. Every instance reloads the keys twice
//! a minute. RS256 always has a key; ES256 one only once a client asks for it.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use arc_swap::ArcSwap;
use chrono::{DateTime, Duration, Utc};
use pacod_id_jose::{Alg, JwkSet, SigningKey, Verifier};

use crate::crypto::envelope::{self, Kek};
use crate::crypto::tokens::base62;
use crate::store::{KeyRow, Store};

/// How long a key signs before the next one takes over.
pub const ACTIVE_FOR: Duration = Duration::days(90);
/// How long before taking over the next key is published, so caches have it in time.
pub const PUBLISHED_AHEAD: Duration = Duration::hours(24);
/// How long a retired key stays in the JWKS: the longest token lifetime (15 minutes) and two
/// days, rounded up.
pub const RETIRED_FOR: Duration = Duration::hours(49);

/// The keys an instance signs and verifies with.
pub struct Keys {
    signing: HashMap<Alg, Arc<SigningKey>>,
    jwks: JwkSet,
    verifier: Arc<Verifier>,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys").field("signing", &self.signing.keys().collect::<Vec<_>>()).finish_non_exhaustive()
    }
}

/// The current keys, swapped as a whole when they change.
#[derive(Debug)]
pub struct KeyRing(ArcSwap<Keys>);

fn alg_of(s: &str) -> anyhow::Result<Alg> {
    match s {
        "RS256" => Ok(Alg::RS256),
        "ES256" => Ok(Alg::ES256),
        other => anyhow::bail!("unknown key algorithm {other}"),
    }
}

fn keys_from(rows: &[KeyRow], keks: &[Kek]) -> anyhow::Result<Keys> {
    let mut signing = HashMap::new();
    let mut jwks = JwkSet::default();
    for row in rows {
        jwks.keys.push(serde_json::from_value(row.public_jwk.clone()).context("a stored public key")?);
        if row.state == "active" {
            let der = envelope::open(keks, &row.kid, &row.private_enc)
                .with_context(|| format!("signing key {}: cannot decrypt (is PID_KEK right?)", row.kid))?;
            signing.insert(alg_of(&row.alg)?, Arc::new(SigningKey::from_pkcs8(&row.kid, alg_of(&row.alg)?, &der)?));
        }
    }
    let verifier = Arc::new(Verifier::from_jwks(&jwks));
    Ok(Keys { signing, jwks, verifier })
}

/// A new key pair of `alg`, as a row to store (the private key sealed under `kek`).
async fn new_key(alg: Alg, kek: &Kek, state: &str, activates_at: DateTime<Utc>) -> anyhow::Result<KeyRow> {
    let kid = format!("{}-{}-{}", alg.as_str().to_lowercase(), Utc::now().format("%Y%m%d"), base62(8));
    let der = match alg {
        // RSA generation takes a moment: off the async threads.
        Alg::RS256 => {
            tokio::task::spawn_blocking(|| -> anyhow::Result<Vec<u8>> {
                use rsa::pkcs8::EncodePrivateKey;
                let key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)?;
                Ok(key.to_pkcs8_der()?.as_bytes().to_vec())
            })
            .await??
        }
        Alg::ES256 => SigningKey::generate_es256(&kid)?.1,
    };
    let key = SigningKey::from_pkcs8(&kid, alg, &der)?;
    Ok(KeyRow {
        kid: kid.clone(),
        alg: alg.as_str().to_owned(),
        state: state.to_owned(),
        public_jwk: serde_json::to_value(key.public_jwk())?,
        private_enc: envelope::seal(kek, &kid, &der),
        created_at: Utc::now(),
        activates_at,
        retires_at: None,
    })
}

impl KeyRing {
    /// Loads the keys, making the first RS256 key when there is none.
    pub async fn load(store: &Store, keks: &[Kek]) -> anyhow::Result<Self> {
        ensure_active(store, &keks[0], Alg::RS256).await?;
        let ring = KeyRing(ArcSwap::from_pointee(keys_from(&store.keys().await?, keks)?));
        Ok(ring)
    }

    pub async fn reload(&self, store: &Store, keks: &[Kek]) -> anyhow::Result<()> {
        self.0.store(Arc::new(keys_from(&store.keys().await?, keks)?));
        Ok(())
    }

    /// The key that signs with `alg`, if there is one.
    pub fn signer(&self, alg: Alg) -> Option<Arc<SigningKey>> {
        self.0.load().signing.get(&alg).cloned()
    }

    /// The published keys: next, active and retired.
    pub fn jwks(&self) -> JwkSet {
        self.0.load().jwks.clone()
    }

    /// Checks tokens pacod-id signed itself (id_token_hint, its own access tokens).
    pub fn verifier(&self) -> Arc<Verifier> {
        self.0.load().verifier.clone()
    }
}

/// Makes an active key of `alg` when there is none.
pub async fn ensure_active(store: &Store, kek: &Kek, alg: Alg) -> anyhow::Result<()> {
    if store.keys().await?.iter().any(|k| k.state == "active" && k.alg == alg.as_str()) {
        return Ok(());
    }
    // Made before taking the lock: generating an RSA key takes a moment.
    let row = new_key(alg, kek, "active", Utc::now()).await?;
    let mut tx = store.begin().await?;
    Store::lock_keys(&mut tx).await?;
    if !Store::keys_in(&mut tx).await?.iter().any(|k| k.state == "active" && k.alg == alg.as_str()) {
        Store::key_insert(&mut tx, &row).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// What one maintenance pass changed, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rotation {
    pub published: Vec<String>,
    pub promoted: Vec<String>,
    pub deleted: Vec<String>,
}

/// The scheduled rotation: publish the next key ahead of time, promote it when due, delete
/// retired keys whose time is up. Run by the maintenance job's holder; `now` for tests.
pub async fn rotate(store: &Store, kek: &Kek, now: DateTime<Utc>) -> anyhow::Result<Rotation> {
    let mut done = Rotation::default();
    for alg in [Alg::RS256, Alg::ES256] {
        let keys = store.keys().await?;
        let Some(active) = keys.iter().find(|k| k.state == "active" && k.alg == alg.as_str()) else { continue };
        let due = active.activates_at + ACTIVE_FOR;
        if !keys.iter().any(|k| k.state == "next" && k.alg == alg.as_str()) && now >= due - PUBLISHED_AHEAD {
            let row = new_key(alg, kek, "next", due.max(now + PUBLISHED_AHEAD)).await?;
            let mut tx = store.begin().await?;
            Store::lock_keys(&mut tx).await?;
            if !Store::keys_in(&mut tx).await?.iter().any(|k| k.state == "next" && k.alg == alg.as_str()) {
                Store::key_insert(&mut tx, &row).await?;
                done.published.push(row.kid.clone());
            }
            tx.commit().await?;
        }
    }
    let mut tx = store.begin().await?;
    Store::lock_keys(&mut tx).await?;
    let keys = Store::keys_in(&mut tx).await?;
    for next in keys.iter().filter(|k| k.state == "next" && k.activates_at <= now) {
        for old in keys.iter().filter(|k| k.state == "active" && k.alg == next.alg) {
            Store::key_set_state(&mut tx, &old.kid, "retired", Some(now + RETIRED_FOR)).await?;
        }
        Store::key_set_state(&mut tx, &next.kid, "active", None).await?;
        done.promoted.push(next.kid.clone());
    }
    for old in keys.iter().filter(|k| k.state == "retired" && k.retires_at.is_some_and(|t| t <= now)) {
        Store::key_delete(&mut tx, &old.kid).await?;
        done.deleted.push(old.kid.clone());
    }
    tx.commit().await?;
    Ok(done)
}

/// Rotation at once (an emergency): a new active key of `alg` replaces the current one, which
/// is retired, or deleted when it is `revoke` (then tokens it signed stop verifying as soon as
/// instances reload, within 30 seconds).
pub async fn rotate_now(store: &Store, kek: &Kek, alg: Alg, revoke: Option<&str>) -> anyhow::Result<String> {
    let row = new_key(alg, kek, "active", Utc::now()).await?;
    let mut tx = store.begin().await?;
    Store::lock_keys(&mut tx).await?;
    let keys = Store::keys_in(&mut tx).await?;
    // A mistyped key id would leave the suspect key published: refuse rather than rotate.
    if let Some(kid) = revoke {
        anyhow::ensure!(keys.iter().any(|k| k.kid == kid), "no key {kid}: see `pacod-id keys list`; nothing changed");
    }
    for k in keys {
        if Some(k.kid.as_str()) == revoke {
            Store::key_delete(&mut tx, &k.kid).await?;
        } else if k.alg == alg.as_str() && k.state == "active" {
            Store::key_set_state(&mut tx, &k.kid, "retired", Some(Utc::now() + RETIRED_FOR)).await?;
        }
    }
    Store::key_insert(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(row.kid)
}

/// Moves every sealed private key under the current key-encryption key. Returns how many moved.
pub async fn rewrap(store: &Store, keks: &[Kek]) -> anyhow::Result<usize> {
    let mut tx = store.begin().await?;
    Store::lock_keys(&mut tx).await?;
    let mut moved = 0;
    for k in Store::keys_in(&mut tx).await? {
        if let Some(sealed) = envelope::rewrap(keks, &keks[0], &k.kid, &k.private_enc)? {
            Store::key_set_private(&mut tx, &k.kid, &sealed).await?;
            moved += 1;
        }
    }
    tx.commit().await?;
    Ok(moved)
}
