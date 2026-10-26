//! Pingora server bootstrap, configuration reload, and discovery wiring.

use crate::config::Snapshot;
use crate::docker::{self, DockerState};
use crate::management::ManagementService;
use crate::metrics::Metrics;
use crate::proxy::RunnelonProxy;
use crate::publisher::Publisher;
use crate::reload::{spawn_reloader, RELOAD_POLL_INTERVAL};
use log::info;
use pingora::proxy::http_proxy_service;
use pingora::server::Server;
use std::path::PathBuf;
use std::sync::Arc;

/// Build and run the Pingora server for `snapshot`, reloading `config_path`.
///
/// This function does not return on success; it blocks until the process exits.
pub fn run_proxy(snapshot: Snapshot, config_path: PathBuf) -> Result<(), String> {
    let listeners = snapshot.listeners.clone();
    if listeners.is_empty() {
        return Err("no listeners configured".to_string());
    }
    let management = snapshot.management;
    let docker_config = snapshot.docker.clone();
    let route_count = snapshot.routes.len();
    let upstream_count = snapshot.upstreams.len();

    let docker_state = match docker_config.as_ref() {
        Some(config) => docker::discover(config)
            .map_err(|err| format!("initial docker discovery failed: {err}"))?,
        None => DockerState::default(),
    };
    let publisher = Publisher::new(snapshot, docker_state);
    let live = publisher.live();
    let metrics = Arc::new(Metrics::new());

    let mut server =
        Server::new(None).map_err(|e| format!("failed to create Pingora server: {e}"))?;
    server.bootstrap();

    for listener in &listeners {
        let bind = listener.address.to_string();
        let scheme = if listener.tls.is_some() {
            "https"
        } else {
            "http"
        };
        let proxy = RunnelonProxy::new(Arc::clone(&live), Arc::clone(&metrics), scheme);
        let mut service = http_proxy_service(&server.configuration, proxy);

        match &listener.tls {
            Some(tls) => {
                let cert = tls.cert_path.to_string_lossy().into_owned();
                let key = tls.key_path.to_string_lossy().into_owned();
                service
                    .add_tls(&bind, &cert, &key)
                    .map_err(|err| format!("failed to configure TLS on {bind}: {err}"))?;
                info!("listening on https://{bind}");
            }
            None => {
                service.add_tcp(&bind);
                info!("listening on http://{bind}");
            }
        }

        server.add_service(service);
    }

    if let Some(addr) = management {
        let mut management_service =
            http_proxy_service(&server.configuration, ManagementService::new(metrics));
        management_service.add_tcp(&addr.to_string());
        server.add_service(management_service);
        info!("management endpoints on http://{addr} (/healthz, /readyz, /metrics)");
    }

    if let Some(config) = &docker_config {
        info!(
            "docker discovery via {} every {} ms",
            config.socket.display(),
            config.poll_interval.as_millis()
        );
    }
    let _docker_poller =
        docker_config.map(|config| docker::spawn_poller(config, Arc::clone(&publisher)));

    let _reloader = spawn_reloader(config_path.clone(), Arc::clone(&publisher));
    info!(
        "watching {} for changes every {} ms",
        config_path.display(),
        RELOAD_POLL_INTERVAL.as_millis()
    );

    info!("runnelon proxy ready ({route_count} route(s), {upstream_count} upstream(s))");
    server.run_forever();
}
