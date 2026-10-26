//! Runnelon library: configuration, routing, and Pingora proxy wiring.
//!
//! Milestone M1 provides a static HTTP reverse proxy driven by `runnelon.yaml`.

pub mod access;
pub mod cli;
pub mod config;
pub mod docker;
pub mod management;
pub mod metrics;
pub mod net;
pub mod proxy;
pub mod publisher;
pub mod reload;
pub mod routing;
pub mod server;

pub use cli::{execute, run, Cli};
pub use config::{load_snapshot_from_path, load_snapshot_from_str, ConfigError, Snapshot};
pub use net::IpNet;
