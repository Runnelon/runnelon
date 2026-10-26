//! Raw serde types for `runnelon.yaml` schema version 1.

use serde::Deserialize;
use std::collections::HashMap;

/// Default request-body limit when `limits` is omitted: 1 MiB.
pub const DEFAULT_MAX_REQUEST_BODY_BYTES: u64 = 1024 * 1024;

/// Top-level configuration document (schema version 1).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    /// Schema version; must be `1` for this release.
    pub version: u32,
    /// Downstream listen addresses.
    pub listeners: Vec<RawListener>,
    /// Route table (order matters for equal-length path ties).
    pub routes: Vec<RawRoute>,
    /// Named upstream pools.
    pub upstreams: HashMap<String, RawUpstream>,
    /// Optional global request limits.
    #[serde(default)]
    pub limits: Option<RawLimits>,
    /// Networks whose `X-Forwarded-For` chains are trusted (default: none).
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
    /// Optional management listener for `/healthz` and `/readyz`.
    #[serde(default)]
    pub management: Option<RawManagement>,
    /// Optional Docker discovery settings.
    #[serde(default)]
    pub docker: Option<RawDocker>,
}

/// Docker discovery configuration (read-only Engine API over a Unix socket).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawDocker {
    /// Unix socket path (default `/var/run/docker.sock`).
    #[serde(default = "default_docker_socket")]
    pub socket: String,
    /// Poll interval in milliseconds (default 2000).
    #[serde(default = "default_docker_poll_interval_ms")]
    pub poll_interval_ms: u64,
}

fn default_docker_socket() -> String {
    "/var/run/docker.sock".to_string()
}

fn default_docker_poll_interval_ms() -> u64 {
    2_000
}

/// Management listener configuration (loopback only).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawManagement {
    /// Bind address; must be a loopback socket address.
    pub address: String,
}

/// Global request limits.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawLimits {
    /// Maximum request body size in bytes (default 1 MiB; must be > 0).
    #[serde(default = "default_max_request_body_bytes")]
    pub max_request_body_bytes: u64,
}

fn default_max_request_body_bytes() -> u64 {
    DEFAULT_MAX_REQUEST_BODY_BYTES
}

/// A TCP listener bind address, optionally with TLS termination.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawListener {
    /// Bind address, e.g. `127.0.0.1:8080`.
    pub address: String,
    /// Optional TLS certificate and key.
    #[serde(default)]
    pub tls: Option<RawListenerTls>,
}

/// TLS material for a listener.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawListenerTls {
    /// PEM certificate chain path.
    pub cert_path: String,
    /// PEM private key path.
    pub key_path: String,
}

/// A host + path-prefix route to an upstream.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRoute {
    /// Exact host (case-insensitive) or `*` for any host.
    pub host: String,
    /// Path prefix; must start with `/`. Longer prefixes win.
    pub path_prefix: String,
    /// Name of an entry in `upstreams`.
    pub upstream: String,
}

/// Upstream pool definition.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawUpstream {
    /// One or more `host:port` endpoints. M1 selects the first endpoint.
    pub endpoints: Vec<String>,
    /// TCP connect timeout in milliseconds (default 5000).
    #[serde(default = "default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    /// Upstream read timeout in milliseconds (default 30000).
    #[serde(default = "default_read_timeout_ms")]
    pub read_timeout_ms: u64,
    /// Upstream write timeout in milliseconds (default 30000).
    #[serde(default = "default_write_timeout_ms")]
    pub write_timeout_ms: u64,
    /// How long an endpoint is skipped after a connect failure, in ms
    /// (default 5000; 0 disables passive health marking).
    #[serde(default = "default_unhealthy_cooldown_ms")]
    pub unhealthy_cooldown_ms: u64,
    /// Optional per-endpoint weights keyed by the exact endpoint string.
    /// Omitted endpoints default to weight 1.
    #[serde(default)]
    pub weights: HashMap<String, u32>,
}

fn default_connect_timeout_ms() -> u64 {
    5_000
}

fn default_read_timeout_ms() -> u64 {
    30_000
}

fn default_write_timeout_ms() -> u64 {
    30_000
}

fn default_unhealthy_cooldown_ms() -> u64 {
    5_000
}
