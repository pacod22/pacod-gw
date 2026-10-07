//! Who this instance is: its id, kept in its spool directory, and a lock on that directory.
//!
//! Every instance needs a spool directory of its own. Two processes writing into one would
//! produce batches with the same ids, and the analytics service would drop one side's events
//! as duplicates without anyone noticing. The lock makes the second process refuse to start.
//!
//! The id stays the same across restarts: `PGW_INSTANCE_ID` when it is set, otherwise the id
//! stored in the directory the first time. `home_id` tells a restart of this instance (same
//! directory) from another process that was given the same id.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write as _;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

const LOCK_FILE: &str = ".lock";
const ID_FILE: &str = "instance.json";

pub struct Identity {
    pub id: String,
    /// Random id stored next to the instance id; `None` when the directory is not writable.
    pub home_id: Option<String>,
    /// New for every process.
    pub boot_id: String,
    /// Held for as long as the process runs.
    _lock: Option<File>,
}

#[derive(Serialize, Deserialize)]
struct Stored {
    id: String,
    home_id: String,
}

/// Whether `id` is usable as an instance id: it appears in alert ids, metric labels and URLs.
pub fn valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// An id derived from the host name, for an instance that was given none.
fn generated_id(host: &str) -> String {
    let mut base: String =
        host.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') { c } else { '-' }).collect();
    base = base.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).chars().take(50).collect();
    if base.is_empty() {
        base = "gw".into();
    }
    format!("{base}-{}", hex::encode(crate::crypto::random_bytes::<3>()))
}

fn new_uuid() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Takes the spool directory for this process and settles the instance id.
pub fn acquire(dir: &Path, explicit: Option<&str>, host: &str) -> anyhow::Result<Identity> {
    let boot_id = new_uuid();
    if let Some(id) = explicit {
        anyhow::ensure!(
            valid_id(id),
            "PGW_INSTANCE_ID \"{id}\" is not usable: use up to 64 letters, digits, '.', '_' or '-', starting with a letter or digit"
        );
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::warn!(dir = %dir.display(), error = %e, "the spool directory cannot be created; the instance id will change at every start");
        let id = explicit.map_or_else(|| generated_id(host), str::to_owned);
        return Ok(Identity { id, home_id: None, boot_id, _lock: None });
    }

    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))
        .with_context(|| format!("opening {}", dir.join(LOCK_FILE).display()))?;
    let lock = match lock.try_lock() {
        Ok(()) => Some(lock),
        Err(TryLockError::WouldBlock) => anyhow::bail!(
            "another process uses the spool directory {}; every instance needs a spool directory of its own (PGW_SPOOL_DIR)",
            dir.display()
        ),
        Err(TryLockError::Error(e)) => {
            tracing::warn!(dir = %dir.display(), error = %e, "the spool directory cannot be locked; make sure no other instance uses it");
            None
        }
    };

    let path = dir.join(ID_FILE);
    let stored: Option<Stored> = std::fs::read_to_string(&path).ok().and_then(|raw| serde_json::from_str(&raw).ok());
    let id = match (explicit, &stored) {
        (Some(id), _) => id.to_owned(),
        (None, Some(s)) if valid_id(&s.id) => s.id.clone(),
        (None, _) => generated_id(host),
    };
    let home_id = stored.as_ref().map_or_else(new_uuid, |s| s.home_id.clone());
    let unchanged = stored.as_ref().is_some_and(|s| s.id == id && s.home_id == home_id);
    let home_id = if unchanged {
        Some(home_id)
    } else {
        match write_atomically(dir, &path, &Stored { id: id.clone(), home_id: home_id.clone() }) {
            Ok(()) => Some(home_id),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "the instance id cannot be stored; it will change at every start");
                None
            }
        }
    };
    Ok(Identity { id, home_id, boot_id, _lock: lock })
}

fn write_atomically(dir: &Path, path: &Path, value: &Stored) -> std::io::Result<()> {
    let tmp = dir.join(format!("{ID_FILE}.tmp"));
    let mut f = File::create(&tmp)?;
    f.write_all(&serde_json::to_vec(value).map_err(std::io::Error::other)?)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("pgw-identity-{}", new_uuid()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_id_survives_a_restart() {
        let d = dir();
        let first = acquire(&d, None, "host-a").unwrap();
        assert!(first.id.starts_with("host-a-"), "{}", first.id);
        let (id, home) = (first.id.clone(), first.home_id.clone());
        drop(first);
        let again = acquire(&d, None, "another-host").unwrap();
        assert_eq!(again.id, id);
        assert_eq!(again.home_id, home);
        assert_ne!(again.boot_id, "");
    }

    #[test]
    fn an_explicit_id_wins_and_keeps_the_home() {
        let d = dir();
        let home = acquire(&d, None, "h").unwrap().home_id;
        let named = acquire(&d, Some("gateway-1"), "h").unwrap();
        assert_eq!(named.id, "gateway-1");
        assert_eq!(named.home_id, home);
        drop(named);
        assert_eq!(acquire(&d, None, "h").unwrap().id, "gateway-1", "the explicit id is remembered");
    }

    #[test]
    fn a_second_process_on_the_same_directory_is_refused() {
        let d = dir();
        let _held = acquire(&d, Some("a"), "h").unwrap();
        let err = acquire(&d, Some("b"), "h").err().expect("refused").to_string();
        assert!(err.contains("spool directory"), "{err}");
    }

    #[test]
    fn ids_are_checked() {
        assert!(valid_id("gateway-1") && valid_id("node.eu_2") && valid_id("a"));
        assert!(!valid_id("") && !valid_id("-x") && !valid_id("a b") && !valid_id("a/b") && !valid_id(&"x".repeat(65)));
        assert!(acquire(&dir(), Some("bad id"), "h").is_err());
        assert!(valid_id(&generated_id("weird host!")));
        assert!(valid_id(&generated_id("")));
    }

    #[test]
    fn an_unwritable_directory_still_gives_an_id() {
        let file = dir().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let id = acquire(&file.join("spool"), None, "h").unwrap();
        assert!(id.home_id.is_none());
        assert!(valid_id(&id.id));
    }
}
