//! Load for pacod-gw: the engine that makes it and measures it, and the agent that runs the
//! engine for the tester (`pacod-gw-load-agent`).
//!
//! The tester decides what a load test does and sets up what it needs; an agent only makes
//! calls and counts. The two speak the JSON of [`contract`], and the agent reports each
//! second as [`histogram::Histogram`]s, which add up across seconds and across agents.

pub mod agent;
pub mod client;
pub mod contract;
pub mod cpu;
pub mod engine;
pub mod histogram;
