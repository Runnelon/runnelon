//! Single writer for the published snapshot: merges static config with discovery.
//!
//! The reloader and the Docker poller both go through this publisher, so
//! concurrent updates cannot publish a snapshot that drops the other source.
//! Requests read the result lock-free through `ArcSwap`.

use crate::config::{CompiledRoute, CompiledUpstream, ConfigError, Snapshot};
use crate::docker::DockerState;
use arc_swap::ArcSwap;
use std::sync::{Arc, Mutex, MutexGuard};

struct State {
    base: Snapshot,
    docker: DockerState,
}

/// Publishes `merge(static, docker)` snapshots.
pub struct Publisher {
    live: Arc<ArcSwap<Snapshot>>,
    state: Mutex<State>,
}

impl Publisher {
    /// Create a publisher for `base` merged with the initial `docker` state.
    pub fn new(base: Snapshot, docker: DockerState) -> Arc<Self> {
        let merged = merge(&base, &docker);
        Arc::new(Self {
            live: Arc::new(ArcSwap::from_pointee(merged)),
            state: Mutex::new(State { base, docker }),
        })
    }

    /// Lock-free handle for the request path.
    pub fn live(&self) -> Arc<ArcSwap<Snapshot>> {
        Arc::clone(&self.live)
    }

    /// Replace the static base snapshot and republish the merge.
    ///
    /// Listener, management and Docker settings are bound at startup; changing
    /// them requires a restart and is rejected.
    pub fn replace_base(&self, candidate: Snapshot) -> Result<(), ConfigError> {
        let mut state = self.lock();
        if candidate.listeners != state.base.listeners
            || candidate.management != state.base.management
            || candidate.docker != state.base.docker
        {
            return Err(ConfigError::validation(
                "listener, management or docker changes require a restart; reload only updates routes, upstreams, limits and trusted_proxies",
            ));
        }
        let merged = merge(&candidate, &state.docker);
        state.base = candidate;
        self.live.store(Arc::new(merged));
        Ok(())
    }

    /// Replace the discovered Docker state and republish the merge.
    pub fn replace_docker(&self, docker: DockerState) {
        let mut state = self.lock();
        let merged = merge(&state.base, &docker);
        state.docker = docker;
        self.live.store(Arc::new(merged));
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Merge discovered services into the static snapshot.
///
/// Static configuration wins: a Docker upstream whose name already exists is
/// ignored, and so is a Docker route that duplicates an existing
/// `host + path_prefix`. Remaining routes are appended and re-sorted so the
/// documented precedence still applies.
pub fn merge(base: &Snapshot, docker: &DockerState) -> Snapshot {
    let mut merged = base.clone();

    for (name, endpoints) in &docker.upstreams {
        if merged.upstreams.contains_key(name) {
            continue;
        }
        if let Some(upstream) = CompiledUpstream::from_endpoints(endpoints.clone()) {
            merged.upstreams.insert(name.clone(), upstream);
        }
    }

    let mut next_order = merged.routes.len();
    for route in &docker.routes {
        if !merged.upstreams.contains_key(&route.upstream) {
            continue;
        }
        let conflicts = merged.routes.iter().any(|existing| {
            existing.host == route.host && existing.path_prefix == route.path_prefix
        });
        if conflicts {
            continue;
        }
        merged.routes.push(CompiledRoute {
            host: route.host.clone(),
            path_prefix: route.path_prefix.clone(),
            upstream: route.upstream.clone(),
            order: next_order,
        });
        next_order += 1;
    }

    merged.routes.sort_by(|left, right| {
        right
            .path_prefix
            .len()
            .cmp(&left.path_prefix.len())
            .then_with(|| left.order.cmp(&right.order))
    });

    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_snapshot_from_str;
    use crate::docker::DiscoveredRoute;
    use std::collections::HashMap;
    use std::net::SocketAddr;

    fn base() -> Snapshot {
        load_snapshot_from_str(
            r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "static"
upstreams:
  static:
    endpoints: ["127.0.0.1:3001"]
"#,
        )
        .expect("valid base")
    }

    fn docker_state() -> DockerState {
        let mut upstreams = HashMap::new();
        upstreams.insert(
            "api".to_string(),
            vec!["127.0.0.1:9001".parse::<SocketAddr>().unwrap()],
        );
        DockerState {
            routes: vec![
                DiscoveredRoute {
                    host: crate::config::HostMatch::Any,
                    path_prefix: "/api".to_string(),
                    upstream: "api".to_string(),
                },
                // Conflicts with the static wildcard route and is skipped.
                DiscoveredRoute {
                    host: crate::config::HostMatch::Any,
                    path_prefix: "/".to_string(),
                    upstream: "api".to_string(),
                },
            ],
            upstreams,
        }
    }

    #[test]
    fn merges_discovery_without_overriding_static() {
        let merged = merge(&base(), &docker_state());
        assert!(merged.upstreams.contains_key("api"));
        assert_eq!(merged.routes.len(), 2, "conflicting docker route skipped");
        assert_eq!(merged.routes[0].path_prefix, "/api");
        assert_eq!(merged.routes[0].upstream, "api");
        assert_eq!(merged.routes[1].upstream, "static");
    }

    #[test]
    fn static_upstream_name_wins() {
        let mut docker = docker_state();
        docker.upstreams.insert(
            "static".to_string(),
            vec!["127.0.0.1:9999".parse().unwrap()],
        );
        let merged = merge(&base(), &docker);
        assert_eq!(
            merged.upstreams["static"].endpoints[0].to_string(),
            "127.0.0.1:3001"
        );
    }

    #[test]
    fn publisher_republishes_on_base_and_docker_changes() {
        let publisher = Publisher::new(base(), DockerState::default());
        let live = publisher.live();
        assert_eq!(live.load().routes.len(), 1);

        publisher.replace_docker(docker_state());
        assert_eq!(live.load().routes.len(), 2);
        assert_eq!(live.load().routes[0].path_prefix, "/api");

        let mut updated = base();
        updated.max_request_body_bytes = 2048;
        publisher.replace_base(updated).expect("compatible reload");
        assert_eq!(live.load().max_request_body_bytes, 2048);
        assert_eq!(live.load().routes.len(), 2, "docker routes survive reload");

        let mut moved = base();
        moved.listeners = vec![crate::config::ListenerConfig {
            address: "127.0.0.1:9999".parse().unwrap(),
            tls: None,
        }];
        assert!(publisher.replace_base(moved).is_err());
        assert_eq!(live.load().listeners[0].address.port(), 8080);
    }
}
