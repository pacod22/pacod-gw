//! Byte-level JSON handling for the paths that must not re-serialise conversation content.
//!
//! Proxy mode and the same-dialect managed path promise that `messages`, `system` and `tools`
//! reach the provider as the bytes the client sent. That rules out parsing the body into a
//! tree and writing it back. Instead [`scan`] locates the top-level members of the body in one
//! strict pass, and [`patch`] rebuilds the body from slices of the original bytes with only
//! parameter members replaced, added or removed.

mod patch;
mod scan;

pub use patch::{Op, Patch, PatchedBody, apply};
pub use scan::{Entry, Kind, ScanError, TopLevel, scan};
