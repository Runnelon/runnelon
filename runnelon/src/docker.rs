//! Read-only Docker discovery over the Engine API (Unix socket).

use crate::config::{DockerDiscovery, HostMatch};
use crate::publisher::Publisher;
use log::warn;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

const LABEL_UPSTREAM: &str = "runnelon.upstream";
const LABEL_HOST: &str = "runnelon.host";
const LABEL_PATH_PREFIX: &str = "runnelon.path_prefix";
const LABEL_PORT: &str = "runnelon.port";
const LABEL_NETWORK: &str = "runnelon.network";

/// One route discovered from container labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredRoute {
    /// Host matcher.
    pub host: HostMatch,
    /// Path prefix.
    pub path_prefix: String,
    /// Upstream pool name.
    pub upstream: String,
}

/// Snapshot of everything Docker discovery currently provides.
#[derive(Debug, Clone, Default)]
pub struct DockerState {
    /// Discovered routes.
    pub routes: Vec<DiscoveredRoute>,
    /// Discovered upstream pools (endpoint lists are deduplicated).
    pub upstreams: HashMap<String, Vec<SocketAddr>>,
}

/// Poll Docker once and return the discovered state.
pub fn discover(config: &DockerDiscovery) -> Result<DockerState, String> {
    let body = fetch_containers(&config.socket)?;
    parse_containers(&body)
}

/// Start the reconciliation loop; failures keep the last published state.
pub fn spawn_poller(config: DockerDiscovery, publisher: Arc<Publisher>) -> JoinHandle<()> {
    thread::spawn(move || loop {
        thread::sleep(config.poll_interval);
        match discover(&config) {
            Ok(state) => publisher.replace_docker(state),
            Err(err) => warn!("docker discovery failed, keeping last-known-good: {err}"),
        }
    })
}

fn fetch_containers(socket: &Path) -> Result<Vec<u8>, String> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|err| format!("connect {}: {err}", socket.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|err| format!("set read timeout: {err}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|err| format!("set write timeout: {err}"))?;
    stream
        .write_all(b"GET /containers/json HTTP/1.1\r\nHost: docker\r\nConnection: close\r\n\r\n")
        .map_err(|err| format!("write request: {err}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|err| format!("read response: {err}"))?;
    parse_http_body(&response)
}

fn parse_http_body(response: &[u8]) -> Result<Vec<u8>, String> {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "malformed Docker response: missing header terminator".to_string())?
        + 4;
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|_| "malformed Docker response: non-UTF-8 headers".to_string())?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "malformed Docker response: missing status".to_string())?;
    if status != 200 {
        return Err(format!("Docker API returned HTTP {status}"));
    }

    let body = &response[header_end..];
    if headers
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        decode_chunked(body)
    } else {
        Ok(body.to_vec())
    }
}

fn decode_chunked(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| "truncated chunked response".to_string())?;
        let size_text =
            std::str::from_utf8(&data[..line_end]).map_err(|_| "invalid chunk size".to_string())?;
        let size_text = size_text.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| format!("invalid chunk size '{size_text}'"))?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size + 2 {
            return Err("truncated chunk body".to_string());
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[derive(Debug, Deserialize)]
struct Container {
    #[serde(rename = "Id", default)]
    id: String,
    #[serde(rename = "Labels", default)]
    labels: HashMap<String, String>,
    #[serde(rename = "Ports", default)]
    ports: Vec<PortMapping>,
    #[serde(rename = "NetworkSettings", default)]
    network_settings: NetworkSettings,
}

#[derive(Debug, Deserialize)]
struct PortMapping {
    #[serde(rename = "PrivatePort")]
    private_port: u16,
    #[serde(rename = "Type", default)]
    kind: String,
}

#[derive(Debug, Default, Deserialize)]
struct NetworkSettings {
    #[serde(rename = "Networks", default)]
    networks: BTreeMap<String, Network>,
}

#[derive(Debug, Default, Deserialize)]
struct Network {
    #[serde(rename = "IPAddress", default)]
    ip_address: String,
}

/// Parse a `/containers/json` response body into routes and upstream pools.
pub fn parse_containers(body: &[u8]) -> Result<DockerState, String> {
    let containers: Vec<Container> =
        serde_json::from_slice(body).map_err(|err| format!("invalid Docker JSON: {err}"))?;

    let mut state = DockerState::default();
    for container in containers {
        let short_id = &container.id[..container.id.len().min(12)];

        let Some(upstream) = label(&container.labels, LABEL_UPSTREAM) else {
            // Not enrolled in discovery.
            continue;
        };
        let Some(host_label) = label(&container.labels, LABEL_HOST) else {
            warn!("docker container {short_id}: missing {LABEL_HOST}");
            continue;
        };
        let host = match crate::config::normalize_config_host(host_label) {
            Ok(host) => host,
            Err(err) => {
                warn!("docker container {short_id}: {err}");
                continue;
            }
        };
        let path_prefix = label(&container.labels, LABEL_PATH_PREFIX).unwrap_or("/");
        let path_prefix = match crate::config::validate_path_prefix(path_prefix) {
            Ok(prefix) => prefix,
            Err(err) => {
                warn!("docker container {short_id}: {err}");
                continue;
            }
        };

        let port = match label(&container.labels, LABEL_PORT) {
            Some(value) => match value.parse::<u16>() {
                Ok(port) if port != 0 => port,
                _ => {
                    warn!("docker container {short_id}: invalid {LABEL_PORT} '{value}'");
                    continue;
                }
            },
            None => {
                match container
                    .ports
                    .iter()
                    .filter(|port| port.kind == "tcp")
                    .map(|port| port.private_port)
                    .min()
                {
                    Some(port) => port,
                    None => {
                        warn!("docker container {short_id}: no TCP port exposed and {LABEL_PORT} unset");
                        continue;
                    }
                }
            }
        };

        let preferred_network = label(&container.labels, LABEL_NETWORK);
        let Some(ip) = select_ip(&container.network_settings.networks, preferred_network) else {
            warn!(
                "docker container {short_id}: no usable IP address (network label: {preferred_network:?})"
            );
            continue;
        };

        let endpoint = SocketAddr::new(ip, port);
        let endpoints = state.upstreams.entry(upstream.to_string()).or_default();
        endpoints.push(endpoint);

        let route = DiscoveredRoute {
            host,
            path_prefix,
            upstream: upstream.to_string(),
        };
        if !state.routes.contains(&route) {
            state.routes.push(route);
        }
    }

    for endpoints in state.upstreams.values_mut() {
        endpoints.sort();
        endpoints.dedup();
    }

    Ok(state)
}

fn label<'a>(labels: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    labels
        .get(key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
}

fn select_ip(networks: &BTreeMap<String, Network>, preferred: Option<&str>) -> Option<IpAddr> {
    let parse = |network: &Network| network.ip_address.trim().parse::<IpAddr>().ok();
    match preferred {
        Some(name) => networks.get(name).and_then(parse),
        None => networks.values().find_map(parse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn container_json(id: &str, labels: &str, ports: &str, networks: &str) -> String {
        format!(
            r#"{{"Id":"{id}","Labels":{{{labels}}},"Ports":[{ports}],"NetworkSettings":{{"Networks":{{{networks}}}}}}}"#
        )
    }

    #[test]
    fn maps_labels_to_routes_and_pools() {
        let body = format!(
            "[{},{}]",
            container_json(
                "aaaaaaaaaaaaaaaa",
                r#""runnelon.upstream":"api","runnelon.host":"app.local","runnelon.path_prefix":"/api""#,
                r#"{"PrivatePort":8080,"Type":"tcp"},{"PrivatePort":80,"Type":"udp"}"#,
                r#""bridge":{"IPAddress":"127.0.0.1"}"#
            ),
            container_json(
                "bbbbbbbbbbbbbbbb",
                r#""runnelon.upstream":"api","runnelon.host":"app.local","runnelon.path_prefix":"/api""#,
                r#"{"PrivatePort":8080,"Type":"tcp"}"#,
                r#""bridge":{"IPAddress":"127.0.0.1"}"#
            ),
        );

        let state = parse_containers(body.as_bytes()).expect("valid JSON");
        assert_eq!(state.routes.len(), 1);
        assert_eq!(state.routes[0].path_prefix, "/api");
        assert_eq!(
            state.upstreams["api"].len(),
            1,
            "duplicate endpoint removed"
        );
        assert_eq!(state.upstreams["api"][0].to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn honours_port_and_network_labels() {
        let body = format!(
            "[{}]",
            container_json(
                "cccccccccccccccc",
                r#""runnelon.upstream":"web","runnelon.host":"*","runnelon.port":"3000","runnelon.network":"app""#,
                r#"{"PrivatePort":8080,"Type":"tcp"}"#,
                r#""bridge":{"IPAddress":"172.17.0.2"},"app":{"IPAddress":"172.18.0.5"}"#
            )
        );
        let state = parse_containers(body.as_bytes()).expect("valid JSON");
        assert_eq!(state.upstreams["web"][0].to_string(), "172.18.0.5:3000");
    }

    #[test]
    fn skips_containers_without_required_labels_or_ports() {
        let body = format!(
            "[{},{},{}]",
            container_json(
                "dddddddddddddddd",
                r#""other":"x""#,
                "",
                r#""bridge":{"IPAddress":"127.0.0.1"}"#
            ),
            container_json(
                "eeeeeeeeeeeeeeee",
                r#""runnelon.upstream":"api""#,
                r#"{"PrivatePort":8080,"Type":"tcp"}"#,
                r#""bridge":{"IPAddress":"127.0.0.1"}"#
            ),
            container_json(
                "ffffffffffffffff",
                r#""runnelon.upstream":"api","runnelon.host":"*""#,
                "",
                r#""bridge":{"IPAddress":"127.0.0.1"}"#
            ),
        );
        let state = parse_containers(body.as_bytes()).expect("valid JSON");
        assert!(state.routes.is_empty());
        assert!(state.upstreams.is_empty());
    }

    #[test]
    fn parses_content_length_and_chunked_responses() {
        let json = br#"[{"Id":"x"}]"#;
        let plain = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            json.len(),
            std::str::from_utf8(json).unwrap()
        );
        assert_eq!(parse_http_body(plain.as_bytes()).expect("plain body"), json);

        let chunked = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            json.len(),
            std::str::from_utf8(json).unwrap()
        );
        assert_eq!(
            parse_http_body(chunked.as_bytes()).expect("chunked body"),
            json
        );

        let error = b"HTTP/1.1 500 Internal Server Error\r\n\r\n";
        assert!(parse_http_body(error).is_err());
    }
}
