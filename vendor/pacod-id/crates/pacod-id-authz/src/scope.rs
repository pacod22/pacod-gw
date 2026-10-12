//! Scopes and paths: `platform`, `organisation:o1`, `acme.workspace:w1`, and a scope with all its
//! ancestors, root first.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The root of every path: the installation. It has no id.
pub const PLATFORM: &str = "platform";
/// The organisation, the one scope type pacod-id itself owns below the platform.
pub const ORGANISATION: &str = "organisation";

/// One scope: a type and, except for the platform, an id.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopeRef {
    pub kind: String,
    pub id: Option<String>,
}

impl ScopeRef {
    pub fn platform() -> Self {
        ScopeRef { kind: PLATFORM.to_owned(), id: None }
    }

    pub fn organisation(id: impl Into<String>) -> Self {
        ScopeRef { kind: ORGANISATION.to_owned(), id: Some(id.into()) }
    }

    /// A scope of a product type, e.g. `ScopeRef::of("gw.workspace", "ws_1")`.
    pub fn of(kind: impl Into<String>, id: impl Into<String>) -> Self {
        ScopeRef { kind: kind.into(), id: Some(id.into()) }
    }

    pub fn is_platform(&self) -> bool {
        self.kind == PLATFORM && self.id.is_none()
    }
}

impl fmt::Display for ScopeRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.id {
            None => f.write_str(&self.kind),
            Some(id) => write!(f, "{}:{id}", self.kind),
        }
    }
}

/// A scope written as text was not `platform` or `type:id`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a scope: {0:?} (expected `platform` or `type:id`)")]
pub struct BadScope(pub String);

impl FromStr for ScopeRef {
    type Err = BadScope;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once(':') {
            None if s == PLATFORM => Ok(ScopeRef::platform()),
            Some((kind, id)) if !kind.is_empty() && !id.is_empty() && kind != PLATFORM => {
                Ok(ScopeRef { kind: kind.to_owned(), id: Some(id.to_owned()) })
            }
            _ => Err(BadScope(s.to_owned())),
        }
    }
}

impl Serialize for ScopeRef {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ScopeRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

/// Whether `prefix` is `path` or one of its ancestors' paths.
pub(crate) fn is_prefix(prefix: &[ScopeRef], path: &[ScopeRef]) -> bool {
    !prefix.is_empty() && prefix.len() <= path.len() && prefix.iter().zip(path).all(|(a, b)| a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_read_and_write_as_text() {
        for text in ["platform", "organisation:o1", "acme.workspace:w1", "x.y:a:b"] {
            let scope: ScopeRef = text.parse().unwrap();
            assert_eq!(scope.to_string(), text);
        }
        assert_eq!("x.y:a:b".parse::<ScopeRef>().unwrap().id.as_deref(), Some("a:b"));
        for bad in ["", "organisation", "organisation:", ":o1", "platform:p1"] {
            assert!(bad.parse::<ScopeRef>().is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn prefixes() {
        let p = ScopeRef::platform();
        let o = ScopeRef::organisation("o1");
        assert!(is_prefix(std::slice::from_ref(&p), &[p.clone(), o.clone()]));
        assert!(is_prefix(&[p.clone(), o.clone()], &[p.clone(), o.clone()]));
        assert!(!is_prefix(&[p.clone(), o.clone()], std::slice::from_ref(&p)));
        assert!(!is_prefix(&[], &[p]));
    }
}
