//! pacod-id: one place where people and machines sign in, and where it is recorded what they
//! may do, for every product that uses it.

pub mod admin;
pub mod app;
pub mod authz;
pub mod bootstrap;
pub mod config;
pub mod crypto;
pub mod error;
pub mod federation;
pub mod http;
pub mod import;
pub mod keys;
pub mod logging;
pub mod mail;
pub mod me;
pub mod net;
pub mod oidc;
pub mod policy;
pub mod store;
pub mod ui;

pub use app::{Listeners, Running, start};
pub use bootstrap::Bootstrap;
pub use config::Config;
