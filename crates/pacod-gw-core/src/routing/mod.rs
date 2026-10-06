//! Routing among the models of a template (concept section 7.5).
//!
//! A scheme is compiled once per snapshot ([`compile`]) and bound to each template's pool
//! ([`select::bind`]), so that a call only evaluates a few precompiled conditions and picks
//! from precomputed candidate lists: no lookup by name, no lock and no allocation.

#[cfg(feature = "routing-cel")]
pub mod cel;
pub mod compile;
pub mod live;
pub mod select;

use std::collections::BTreeMap;

use crate::model::Complexity;
use crate::model::template::MAX_POOL;

pub use compile::{CompileError, CompiledScheme, compile};
pub use live::{BreakerState, InFlight, RouteRegistry, TargetState, TargetView};
pub use select::{BoundAction, BoundRoute, PoolMember, bind};

/// What a call says about itself, as far as routing looks at it.
#[derive(Debug, Clone, Copy)]
pub struct RouteInput<'a> {
    pub complexity: Option<Complexity>,
    pub agent_id: Option<&'a str>,
    pub task: Option<&'a str>,
    pub user_id: Option<&'a str>,
    pub end_user: Option<&'a str>,
    pub session_id: Option<&'a str>,
    /// The template's name.
    pub template: &'a str,
    pub tags: &'a BTreeMap<String, String>,
    /// `openai`, `anthropic`, `native` or `proxy`.
    pub interface: &'a str,
    pub stream: bool,
    /// A rough count of the input tokens, from the body size.
    pub input_tokens: u32,
}

static NO_TAGS: BTreeMap<String, String> = BTreeMap::new();

impl RouteInput<'static> {
    /// A call that says nothing about itself.
    pub fn empty() -> Self {
        Self {
            complexity: None,
            agent_id: None,
            task: None,
            user_id: None,
            end_user: None,
            session_id: None,
            template: "",
            tags: &NO_TAGS,
            interface: "",
            stream: false,
            input_tokens: 0,
        }
    }
}

/// Positions in a template's pool, in order. Fixed size: a pool holds at most 16 models.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Candidates {
    idx: [u8; MAX_POOL],
    len: u8,
}

impl Candidates {
    pub fn push(&mut self, i: usize) {
        if (self.len as usize) < MAX_POOL {
            self.idx[self.len as usize] = i as u8;
            self.len += 1;
        }
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.idx[..self.len as usize]
    }

    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.as_slice().iter().map(|&i| i as usize)
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn contains(&self, i: usize) -> bool {
        self.as_slice().contains(&(i as u8))
    }

    pub fn get(&self, n: usize) -> Option<usize> {
        self.as_slice().get(n).map(|&i| i as usize)
    }

    pub fn truncate(&mut self, n: usize) {
        self.len = self.len.min(n as u8);
    }
}

impl std::fmt::Debug for Candidates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.as_slice()).finish()
    }
}

impl FromIterator<usize> for Candidates {
    fn from_iter<T: IntoIterator<Item = usize>>(iter: T) -> Self {
        let mut c = Self::default();
        for i in iter {
            c.push(i);
        }
        c
    }
}

/// FNV-1a: a fast hash that is the same on every instance, so that session affinity sends a
/// session to the same model whichever instance takes the call.
pub fn stable_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
