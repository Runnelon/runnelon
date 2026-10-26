//! Validate raw config and compile an immutable routing snapshot.

use super::error::ConfigError;
use super::raw::RawConfig;
use crate::net::IpNet;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How a route matches the HTTP `Host` header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HostMatch {
    /// Match any host.
    Any,
    /// Exact match after normalization (lowercase, no port).
    Exact(String),
}

/// A compiled route entry used on the request path.
#[derive(Debug, Clone)]
pub struct CompiledRoute {
    /// Host matcher.
    pub host: HostMatch,
    /// Path prefix (as configured; must start with `/`).
    pub path_prefix: String,
    /// Upstream name in [`Snapshot::upstreams`].
    pub upstream: String,
    /// Declaration order (lower wins ties of equal path length).
    pub order: usize,
}

/// A compiled upstream with timeouts and endpoints.
#[derive(Debug, Clone)]
pub struct CompiledUpstream {
    /// Endpoint addresses referenced by [`Self::schedule`].
    pub endpoints: Vec<SocketAddr>,
    /// Weighted selection schedule: endpoint indices repeated by weight.
    pub schedule: Vec<usize>,
    /// Connect timeout.
    pub connect_timeout: Duration,
    /// Read timeout.
    pub read_timeout: Duration,
    /// Write timeout.
    pub write_timeout: Duration,
    /// Next schedule slot; shared by all requests using this upstream.
    next_slot: Arc<AtomicUsize>,
    /// Per-endpoint cooldown deadline (ms since `started_at`).
    unhealthy_until: Arc<Vec<AtomicU64>>,
    /// Reference instant for cooldown deadlines.
    started_at: Instant,
    /// Cooldown applied after a connect failure (0 disables marking).
    unhealthy_cooldown_ms: u64,
}

impl CompiledUpstream {
    /// Build an upstream from discovered endpoints using default tuning.
    ///
    /// Used by discovery providers: 5 s connect, 30 s read/write, round-robin
    /// schedule, 5 s passive-health cooldown.
    pub fn from_endpoints(endpoints: Vec<SocketAddr>) -> Option<Self> {
        let count = endpoints.len();
        if count == 0 {
            return None;
        }
        Some(Self {
            schedule: (0..count).collect(),
            endpoints,
            connect_timeout: Duration::from_millis(5_000),
            read_timeout: Duration::from_millis(30_000),
            write_timeout: Duration::from_millis(30_000),
            next_slot: Arc::new(AtomicUsize::new(0)),
            unhealthy_until: Arc::new((0..count).map(|_| AtomicU64::new(0)).collect()),
            started_at: Instant::now(),
            unhealthy_cooldown_ms: 5_000,
        })
    }

    /// Pick an endpoint index, skipping endpoints inside their cooldown.
    ///
    /// Selection starts at the next weighted schedule slot and scans forward.
    /// If every endpoint is cooling down, the first slot is returned so traffic
    /// still attempts an upstream instead of failing without a connection try.
    pub fn select_endpoint(&self) -> usize {
        let start = self.next_slot_start();
        match self.find_endpoint(start, &[]) {
            Some(endpoint) => endpoint,
            None => self.schedule[start],
        }
    }

    /// Pick an endpoint that is neither cooling down nor in `avoid`.
    ///
    /// Advances the weighted schedule by one slot, like [`Self::select_endpoint`].
    pub fn select_endpoint_avoiding(&self, avoid: &[usize]) -> Option<usize> {
        let start = self.next_slot_start();
        self.find_endpoint(start, avoid)
    }

    /// Whether an endpoint is selectable right now without advancing the schedule.
    pub fn has_endpoint_avoiding(&self, avoid: &[usize]) -> bool {
        let start = self.next_slot.load(Ordering::Relaxed) % self.schedule.len();
        self.find_endpoint(start, avoid).is_some()
    }

    /// Record a connect failure so the endpoint is skipped for the cooldown.
    pub fn mark_unhealthy(&self, endpoint: usize) {
        if let Some(until) = self.unhealthy_until.get(endpoint) {
            until.store(
                self.millis_since_start() + self.unhealthy_cooldown_ms,
                Ordering::Relaxed,
            );
        }
    }

    fn next_slot_start(&self) -> usize {
        self.next_slot.fetch_add(1, Ordering::Relaxed) % self.schedule.len()
    }

    fn find_endpoint(&self, start: usize, avoid: &[usize]) -> Option<usize> {
        let now = self.millis_since_start();
        for offset in 0..self.schedule.len() {
            let endpoint = self.schedule[(start + offset) % self.schedule.len()];
            if avoid.contains(&endpoint) {
                continue;
            }
            if now >= self.unhealthy_until[endpoint].load(Ordering::Relaxed) {
                return Some(endpoint);
            }
        }
        None
    }

    fn millis_since_start(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}

/// A listener with optional TLS termination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerConfig {
    /// Bind address.
    pub address: SocketAddr,
    /// TLS material, when terminating TLS on this listener.
    pub tls: Option<TlsConfig>,
}

/// TLS certificate and key paths for a listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsConfig {
    /// PEM certificate chain.
    pub cert_path: std::path::PathBuf,
    /// PEM private key.
    pub key_path: std::path::PathBuf,
}

/// Immutable configuration snapshot published to the data plane.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Listener bind addresses and TLS settings.
    pub listeners: Vec<ListenerConfig>,
    /// Routes sorted for matching: longer `path_prefix` first, then lower `order`.
    pub routes: Vec<CompiledRoute>,
    /// Named upstreams.
    pub upstreams: HashMap<String, CompiledUpstream>,
    /// Maximum accepted request body size in bytes.
    pub max_request_body_bytes: u64,
    /// Trusted proxy networks; inbound `X-Forwarded-For` chains are preserved
    /// (and appended to) only when the peer falls inside one of these.
    pub trusted_proxies: Vec<IpNet>,
    /// Optional loopback management listener (`/healthz`, `/readyz`).
    pub management: Option<SocketAddr>,
    /// Optional Docker discovery settings.
    pub docker: Option<DockerDiscovery>,
}

/// Docker discovery settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerDiscovery {
    /// Unix socket path for the Docker Engine API.
    pub socket: std::path::PathBuf,
    /// How often to re-list containers.
    pub poll_interval: Duration,
}

/// Load, parse, validate, and compile a config file from disk.
pub fn load_snapshot_from_path(path: impl AsRef<Path>) -> Result<Snapshot, ConfigError> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    load_snapshot_from_str(&contents)
}

/// Parse, validate, and compile a config document from a YAML string.
pub fn load_snapshot_from_str(yaml: &str) -> Result<Snapshot, ConfigError> {
    let raw: RawConfig = serde_yaml::from_str(yaml)?;
    compile(raw)
}

/// Maximum number of slots in a compiled weighted schedule.
const MAX_WEIGHT_SCHEDULE: usize = 1024;

fn compile(raw: RawConfig) -> Result<Snapshot, ConfigError> {
    if raw.version != 1 {
        return Err(ConfigError::validation(format!(
            "unsupported version {}; only version 1 is supported",
            raw.version
        )));
    }
    if raw.listeners.is_empty() {
        return Err(ConfigError::validation("at least one listener is required"));
    }

    let docker = match &raw.docker {
        Some(docker) => {
            let socket = docker.socket.trim();
            if socket.is_empty() {
                return Err(ConfigError::validation("docker.socket must be non-empty"));
            }
            if docker.poll_interval_ms == 0 {
                return Err(ConfigError::validation(
                    "docker.poll_interval_ms must be greater than zero",
                ));
            }
            Some(DockerDiscovery {
                socket: std::path::PathBuf::from(socket),
                poll_interval: Duration::from_millis(docker.poll_interval_ms),
            })
        }
        None => None,
    };

    if raw.routes.is_empty() && docker.is_none() {
        return Err(ConfigError::validation(
            "at least one route is required unless docker discovery is configured",
        ));
    }
    if raw.upstreams.is_empty() && docker.is_none() {
        return Err(ConfigError::validation(
            "at least one upstream is required unless docker discovery is configured",
        ));
    }

    let max_request_body_bytes = match &raw.limits {
        Some(limits) => {
            if limits.max_request_body_bytes == 0 {
                return Err(ConfigError::validation(
                    "limits.max_request_body_bytes must be greater than zero",
                ));
            }
            limits.max_request_body_bytes
        }
        None => super::raw::DEFAULT_MAX_REQUEST_BODY_BYTES,
    };

    let mut trusted_proxies = Vec::with_capacity(raw.trusted_proxies.len());
    let mut seen_trusted = HashSet::with_capacity(raw.trusted_proxies.len());
    for (idx, entry) in raw.trusted_proxies.iter().enumerate() {
        let network = entry
            .parse::<IpNet>()
            .map_err(|err| ConfigError::validation(format!("trusted_proxies[{idx}]: {err}")))?;
        if !seen_trusted.insert(network) {
            return Err(ConfigError::validation(format!(
                "trusted_proxies[{idx}]: duplicate network '{entry}'"
            )));
        }
        trusted_proxies.push(network);
    }

    let management = match &raw.management {
        Some(management) => {
            let addr = parse_socket_addr(&management.address)
                .map_err(|err| ConfigError::validation(format!("management.address: {err}")))?;
            if !addr.ip().is_loopback() {
                return Err(ConfigError::validation(format!(
                    "management.address '{addr}': management endpoints must bind to loopback"
                )));
            }
            Some(addr)
        }
        None => None,
    };

    let mut listeners = Vec::with_capacity(raw.listeners.len());
    let mut seen_listeners = HashSet::with_capacity(raw.listeners.len());
    for (idx, listener) in raw.listeners.iter().enumerate() {
        let addr = parse_socket_addr(&listener.address)
            .map_err(|e| ConfigError::validation(format!("listeners[{idx}].address: {e}")))?;
        if !seen_listeners.insert(addr) {
            return Err(ConfigError::validation(format!(
                "listeners[{idx}].address: duplicate listener address '{addr}'"
            )));
        }

        let tls = match &listener.tls {
            Some(tls) => {
                let cert_path = tls.cert_path.trim();
                let key_path = tls.key_path.trim();
                for (field, path) in [("cert_path", cert_path), ("key_path", key_path)] {
                    if path.is_empty() {
                        return Err(ConfigError::validation(format!(
                            "listeners[{idx}].tls.{field} must be non-empty"
                        )));
                    }
                    std::fs::File::open(path).map_err(|err| {
                        ConfigError::validation(format!(
                            "listeners[{idx}].tls.{field}: cannot read '{path}': {err}"
                        ))
                    })?;
                }
                Some(TlsConfig {
                    cert_path: PathBuf::from(cert_path),
                    key_path: PathBuf::from(key_path),
                })
            }
            None => None,
        };

        listeners.push(ListenerConfig { address: addr, tls });
    }

    if let Some(addr) = management {
        if listeners.iter().any(|listener| listener.address == addr) {
            return Err(ConfigError::validation(format!(
                "management.address '{addr}' conflicts with a configured listener"
            )));
        }
    }

    let mut upstreams = HashMap::with_capacity(raw.upstreams.len());
    for (name, upstream) in &raw.upstreams {
        if name.trim().is_empty() {
            return Err(ConfigError::validation("upstream names must be non-empty"));
        }
        if upstream.endpoints.is_empty() {
            return Err(ConfigError::validation(format!(
                "upstream '{name}' must declare at least one endpoint"
            )));
        }
        if upstream.connect_timeout_ms == 0
            || upstream.read_timeout_ms == 0
            || upstream.write_timeout_ms == 0
        {
            return Err(ConfigError::validation(format!(
                "upstream '{name}': timeouts must be greater than zero"
            )));
        }

        let mut endpoints = Vec::with_capacity(upstream.endpoints.len());
        let mut seen_endpoints = HashSet::with_capacity(upstream.endpoints.len());
        let mut schedule = Vec::new();
        let mut total_weight: usize = 0;
        for (idx, endpoint) in upstream.endpoints.iter().enumerate() {
            let addr = parse_socket_addr(endpoint).map_err(|e| {
                ConfigError::validation(format!("upstream '{name}' endpoints[{idx}]: {e}"))
            })?;
            if !seen_endpoints.insert(addr) {
                return Err(ConfigError::validation(format!(
                    "upstream '{name}' endpoints[{idx}]: duplicate endpoint '{addr}'"
                )));
            }
            let weight = upstream.weights.get(endpoint).copied().unwrap_or(1);
            if weight == 0 {
                return Err(ConfigError::validation(format!(
                    "upstream '{name}' endpoint '{endpoint}': weight must be greater than zero"
                )));
            }
            total_weight += weight as usize;
            if total_weight > MAX_WEIGHT_SCHEDULE {
                return Err(ConfigError::validation(format!(
                    "upstream '{name}': total weight {total_weight} exceeds limit {MAX_WEIGHT_SCHEDULE}"
                )));
            }
            for _ in 0..weight {
                schedule.push(idx);
            }
            endpoints.push(addr);
        }
        for key in upstream.weights.keys() {
            if !upstream.endpoints.contains(key) {
                return Err(ConfigError::validation(format!(
                    "upstream '{name}' weights: unknown endpoint '{key}'"
                )));
            }
        }

        let unhealthy_until = (0..endpoints.len())
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>();

        upstreams.insert(
            name.clone(),
            CompiledUpstream {
                endpoints,
                schedule,
                connect_timeout: Duration::from_millis(upstream.connect_timeout_ms),
                read_timeout: Duration::from_millis(upstream.read_timeout_ms),
                write_timeout: Duration::from_millis(upstream.write_timeout_ms),
                next_slot: Arc::new(AtomicUsize::new(0)),
                unhealthy_until: Arc::new(unhealthy_until),
                started_at: Instant::now(),
                unhealthy_cooldown_ms: upstream.unhealthy_cooldown_ms,
            },
        );
    }

    let mut routes = Vec::with_capacity(raw.routes.len());
    let mut seen_routes: HashSet<(HostMatch, String)> = HashSet::with_capacity(raw.routes.len());
    for (order, route) in raw.routes.into_iter().enumerate() {
        let host = normalize_config_host(&route.host)?;
        let path_prefix = validate_path_prefix(&route.path_prefix)?;
        if !upstreams.contains_key(&route.upstream) {
            return Err(ConfigError::validation(format!(
                "routes[{order}]: unknown upstream '{}'",
                route.upstream
            )));
        }
        if !seen_routes.insert((host.clone(), path_prefix.clone())) {
            return Err(ConfigError::validation(format!(
                "routes[{order}]: duplicate route for host '{}' and path_prefix '{path_prefix}'",
                host_label(&host)
            )));
        }
        routes.push(CompiledRoute {
            host,
            path_prefix,
            upstream: route.upstream,
            order,
        });
    }

    // Longer path prefixes first; equal length → declaration order.
    routes.sort_by(|a, b| {
        b.path_prefix
            .len()
            .cmp(&a.path_prefix.len())
            .then_with(|| a.order.cmp(&b.order))
    });

    Ok(Snapshot {
        listeners,
        routes,
        upstreams,
        max_request_body_bytes,
        trusted_proxies,
        management,
        docker,
    })
}

fn host_label(host: &HostMatch) -> &str {
    match host {
        HostMatch::Any => "*",
        HostMatch::Exact(name) => name,
    }
}

fn parse_socket_addr(value: &str) -> Result<SocketAddr, String> {
    value
        .parse::<SocketAddr>()
        .map_err(|_| format!("invalid socket address '{value}' (expected host:port)"))
}

pub(crate) fn normalize_config_host(host: &str) -> Result<HostMatch, ConfigError> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::validation(
            "route host must be non-empty (use '*' for any host)",
        ));
    }
    if trimmed == "*" {
        return Ok(HostMatch::Any);
    }
    if trimmed.contains('/') {
        return Err(ConfigError::validation(format!(
            "route host '{trimmed}' must not contain '/'"
        )));
    }
    // Reject host:port in config; listeners define ports separately.
    if trimmed.contains(':') {
        return Err(ConfigError::validation(format!(
            "route host '{trimmed}' must not include a port; use the hostname only"
        )));
    }
    Ok(HostMatch::Exact(trimmed.to_ascii_lowercase()))
}

pub(crate) fn validate_path_prefix(path: &str) -> Result<String, ConfigError> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(ConfigError::validation(format!(
            "path_prefix '{path}' must be non-empty and start with '/'"
        )));
    }
    if path.chars().any(char::is_control) {
        return Err(ConfigError::validation(format!(
            "path_prefix '{path}' must not contain control characters"
        )));
    }
    if path.chars().any(char::is_whitespace) {
        return Err(ConfigError::validation(format!(
            "path_prefix '{path}' must not contain whitespace"
        )));
    }
    if path.contains('?') || path.contains('#') {
        return Err(ConfigError::validation(format!(
            "path_prefix '{path}' must not contain '?' or '#'"
        )));
    }
    if path.contains('%') {
        return Err(ConfigError::validation(format!(
            "path_prefix '{path}' must be a decoded path without percent escapes"
        )));
    }
    Ok(path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "App.Local"
    path_prefix: "/api"
    upstream: "api"
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  api:
    endpoints: ["127.0.0.1:3001"]
    connect_timeout_ms: 1000
  web:
    endpoints: ["127.0.0.1:3002"]
"#;

    #[test]
    fn compiles_valid_config_and_sorts_routes() {
        let snap = load_snapshot_from_str(VALID).expect("valid");
        assert_eq!(snap.listeners.len(), 1);
        assert_eq!(snap.routes.len(), 2);
        assert_eq!(snap.routes[0].path_prefix, "/api");
        assert_eq!(snap.routes[1].path_prefix, "/");
        assert_eq!(
            snap.routes[0].host,
            HostMatch::Exact("app.local".to_string())
        );
        assert_eq!(
            snap.upstreams["api"].connect_timeout,
            Duration::from_millis(1000)
        );
        assert_eq!(
            snap.upstreams["api"].read_timeout,
            Duration::from_millis(30_000)
        );
    }

    #[test]
    fn rejects_unknown_fields() {
        let yaml = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
    unexpected: true
upstreams:
  web:
    endpoints: ["127.0.0.1:3000"]
"#;
        let err = load_snapshot_from_str(yaml).expect_err("unknown field");
        assert!(
            err.to_string().contains("unexpected")
                || err.to_string().contains("unknown")
                || err.to_string().contains("YAML"),
            "{err}"
        );
    }

    #[test]
    fn rejects_bad_version_and_missing_upstream() {
        let bad_version = VALID.replace("version: 1", "version: 99");
        assert!(load_snapshot_from_str(&bad_version)
            .unwrap_err()
            .to_string()
            .contains("version"));

        let missing = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "missing"
upstreams:
  web:
    endpoints: ["127.0.0.1:3000"]
"#;
        assert!(load_snapshot_from_str(missing)
            .unwrap_err()
            .to_string()
            .contains("unknown upstream"));
    }

    #[test]
    fn rejects_invalid_path_and_zero_timeout() {
        let bad_path = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "*"
    path_prefix: "no-slash"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:3000"]
"#;
        assert!(load_snapshot_from_str(bad_path)
            .unwrap_err()
            .to_string()
            .contains("path_prefix"));

        let zero = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:3000"]
    connect_timeout_ms: 0
"#;
        assert!(load_snapshot_from_str(zero)
            .unwrap_err()
            .to_string()
            .contains("timeout"));
    }

    #[test]
    fn equal_path_length_preserves_declaration_order() {
        let yaml = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "a.local"
    path_prefix: "/x"
    upstream: "one"
  - host: "b.local"
    path_prefix: "/y"
    upstream: "two"
upstreams:
  one:
    endpoints: ["127.0.0.1:1"]
  two:
    endpoints: ["127.0.0.1:2"]
"#;
        let snap = load_snapshot_from_str(yaml).unwrap();
        // Equal length: order by declaration index.
        assert_eq!(snap.routes[0].upstream, "one");
        assert_eq!(snap.routes[1].upstream, "two");
    }

    fn expect_invalid(yaml: &str) -> String {
        match load_snapshot_from_str(yaml) {
            Ok(_) => panic!("config should be rejected:\n{yaml}"),
            Err(err) => err.to_string(),
        }
    }

    #[test]
    fn rejects_duplicate_listeners_routes_and_endpoints() {
        let duplicate_listener = VALID.replace(
            "  - address: \"127.0.0.1:8080\"",
            "  - address: \"127.0.0.1:8080\"\n  - address: \"127.0.0.1:8080\"",
        );
        assert!(expect_invalid(&duplicate_listener).contains("duplicate listener"));

        let duplicate_route = VALID.replace(
            "  - host: \"*\"\n    path_prefix: \"/\"\n    upstream: \"web\"",
            "  - host: \"*\"\n    path_prefix: \"/\"\n    upstream: \"web\"\n  - host: \"*\"\n    path_prefix: \"/\"\n    upstream: \"web\"",
        );
        assert!(expect_invalid(&duplicate_route).contains("duplicate route"));

        // Host comparison uses the normalized (lowercase) form.
        let case_insensitive_route = VALID.replace(
            "  - host: \"*\"\n    path_prefix: \"/\"\n    upstream: \"web\"",
            "  - host: \"APP.LOCAL\"\n    path_prefix: \"/api\"\n    upstream: \"web\"",
        );
        assert!(expect_invalid(&case_insensitive_route).contains("duplicate route"));

        let duplicate_endpoint = VALID.replace(
            "endpoints: [\"127.0.0.1:3002\"]",
            "endpoints: [\"127.0.0.1:3002\", \"127.0.0.1:3002\"]",
        );
        assert!(expect_invalid(&duplicate_endpoint).contains("duplicate endpoint"));
    }

    #[test]
    fn rejects_unmatchable_path_prefix() {
        for prefix in [
            "/api v1",
            "/api?x=1",
            "/api#frag",
            "/api%2Fv1",
            "/api\u{a0}v1",
        ] {
            let yaml = VALID.replace(
                "path_prefix: \"/api\"",
                &format!("path_prefix: \"{prefix}\""),
            );
            let err = expect_invalid(&yaml);
            assert!(err.contains("path_prefix"), "prefix {prefix:?}: {err}");
        }
    }

    #[test]
    fn rejects_whitespace_only_upstream_name() {
        let yaml = VALID.replace("  web:", "  \" \":");
        assert!(expect_invalid(&yaml).contains("upstream names"));
    }

    #[test]
    fn compiles_weighted_schedules_and_rejects_invalid_weights() {
        let base = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:3001", "127.0.0.1:3002"]
"#;
        let snap = load_snapshot_from_str(base).expect("valid");
        assert_eq!(snap.upstreams["api"].schedule, vec![0, 1]);

        let weighted = format!(
            "{base}    weights:\n      \"127.0.0.1:3001\": 3\n      \"127.0.0.1:3002\": 1\n"
        );
        let snap = load_snapshot_from_str(&weighted).expect("valid");
        assert_eq!(snap.upstreams["api"].schedule, vec![0, 0, 0, 1]);

        let zero = format!("{base}    weights:\n      \"127.0.0.1:3001\": 0\n");
        assert!(expect_invalid(&zero).contains("weight must be greater than zero"));

        let unknown = format!("{base}    weights:\n      \"127.0.0.1:3099\": 2\n");
        assert!(expect_invalid(&unknown).contains("unknown endpoint"));

        let oversized = format!("{base}    weights:\n      \"127.0.0.1:3001\": 1025\n");
        assert!(expect_invalid(&oversized).contains("exceeds limit"));
    }

    #[test]
    fn skips_unhealthy_endpoints_until_cooldown_expires() {
        let yaml = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:3001", "127.0.0.1:3002"]
    unhealthy_cooldown_ms: 20
"#;
        let snap = load_snapshot_from_str(yaml).expect("valid");
        let upstream = &snap.upstreams["api"];

        assert_eq!(upstream.select_endpoint(), 0);
        upstream.mark_unhealthy(0);
        assert_eq!(
            upstream.select_endpoint_avoiding(&[1]),
            None,
            "cooling endpoint plus avoided endpoint leaves nothing"
        );
        for _ in 0..4 {
            assert_eq!(
                upstream.select_endpoint(),
                1,
                "cooling endpoint must be skipped"
            );
        }

        std::thread::sleep(Duration::from_millis(40));
        let selected: Vec<usize> = (0..4).map(|_| upstream.select_endpoint()).collect();
        assert!(
            selected.contains(&0),
            "expired cooldown must be eligible: {selected:?}"
        );
    }

    #[test]
    fn parses_and_validates_management_address() {
        let snap = load_snapshot_from_str(VALID).expect("valid");
        assert!(snap.management.is_none());

        let yaml = format!("{VALID}\nmanagement:\n  address: \"127.0.0.1:9090\"\n");
        let snap = load_snapshot_from_str(&yaml).expect("valid");
        assert_eq!(
            snap.management.map(|addr| addr.to_string()).as_deref(),
            Some("127.0.0.1:9090")
        );

        let public = format!("{VALID}\nmanagement:\n  address: \"0.0.0.0:9090\"\n");
        assert!(expect_invalid(&public).contains("loopback"));

        let conflict = format!("{VALID}\nmanagement:\n  address: \"127.0.0.1:8080\"\n");
        assert!(expect_invalid(&conflict).contains("conflicts"));
    }

    #[test]
    fn parses_docker_settings_and_relaxes_empty_static_tables() {
        let yaml = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes: []
upstreams: {}
docker:
  socket: "/tmp/docker.sock"
  poll_interval_ms: 250
"#;
        let snap = load_snapshot_from_str(yaml).expect("docker-only config");
        let docker = snap.docker.expect("docker settings");
        assert_eq!(docker.socket, std::path::PathBuf::from("/tmp/docker.sock"));
        assert_eq!(docker.poll_interval, Duration::from_millis(250));
        assert!(snap.routes.is_empty());
        assert!(snap.upstreams.is_empty());

        let defaults = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes: []
upstreams: {}
docker: {}
"#;
        let snap = load_snapshot_from_str(defaults).expect("defaults");
        let docker = snap.docker.expect("docker settings");
        assert_eq!(
            docker.socket,
            std::path::PathBuf::from("/var/run/docker.sock")
        );
        assert_eq!(docker.poll_interval, Duration::from_millis(2_000));

        let empty_static = r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes: []
upstreams: {}
"#;
        assert!(expect_invalid(empty_static).contains("route"));

        let zero_poll = yaml.replace("poll_interval_ms: 250", "poll_interval_ms: 0");
        assert!(expect_invalid(&zero_poll).contains("poll_interval_ms"));
    }

    #[test]
    fn parses_tls_listeners_and_rejects_missing_material() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        std::fs::write(&cert, "dummy cert").unwrap();
        std::fs::write(&key, "dummy key").unwrap();

        let yaml = format!(
            r#"
version: 1
listeners:
  - address: "127.0.0.1:8443"
    tls:
      cert_path: "{}"
      key_path: "{}"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:3000"]
"#,
            cert.display(),
            key.display()
        );
        let snap = load_snapshot_from_str(&yaml).expect("tls listener");
        assert!(snap.listeners[0].tls.is_some());

        let missing = yaml.replace(&cert.display().to_string(), "/nonexistent/cert.pem");
        assert!(expect_invalid(&missing).contains("cert_path"));
    }

    #[test]
    fn applies_default_and_explicit_request_body_limits() {
        let snap = load_snapshot_from_str(VALID).expect("valid");
        assert_eq!(snap.max_request_body_bytes, 1_048_576);

        let explicit = format!("{VALID}\nlimits:\n  max_request_body_bytes: 2048\n");
        let snap = load_snapshot_from_str(&explicit).expect("valid");
        assert_eq!(snap.max_request_body_bytes, 2048);

        let zero = format!("{VALID}\nlimits:\n  max_request_body_bytes: 0\n");
        assert!(expect_invalid(&zero).contains("max_request_body_bytes"));

        let unknown = format!("{VALID}\nlimits:\n  surprise: 1\n");
        assert!(expect_invalid(&unknown).contains("unknown"));
    }

    #[test]
    fn parses_and_validates_trusted_proxies() {
        let snap = load_snapshot_from_str(VALID).expect("valid");
        assert!(snap.trusted_proxies.is_empty());

        let yaml = format!("{VALID}\ntrusted_proxies: [\"10.0.0.0/8\", \"192.0.2.7\"]\n");
        let snap = load_snapshot_from_str(&yaml).expect("valid");
        assert_eq!(snap.trusted_proxies.len(), 2);
        assert!(snap.trusted_proxies[0].contains("10.1.2.3".parse().expect("ip")));
        assert!(snap.trusted_proxies[1].contains("192.0.2.7".parse().expect("ip")));

        let invalid = format!("{VALID}\ntrusted_proxies: [\"10.0.0.0/33\"]\n");
        assert!(expect_invalid(&invalid).contains("trusted_proxies[0]"));

        let duplicate = format!("{VALID}\ntrusted_proxies: [\"10.0.0.0/8\", \"10.1.2.3/8\"]\n");
        assert!(expect_invalid(&duplicate).contains("duplicate network"));
    }
}
