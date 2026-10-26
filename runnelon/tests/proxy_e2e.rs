//! Integration tests: two mock upstreams routed through a live Runnelon process.

use assert_cmd::cargo::cargo_bin;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

struct MockUpstream {
    port: u16,
    _join: JoinHandle<()>,
    hits: Arc<Mutex<Vec<MockHit>>>,
}

#[derive(Debug, Clone)]
struct MockHit {
    path: String,
    host: Option<String>,
    xff: Option<String>,
    xfp: Option<String>,
    xfh: Option<String>,
    request_id: Option<String>,
    body: Vec<u8>,
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_mock(label: &'static str) -> MockUpstream {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let hits_thread = Arc::clone(&hits);

    let join = thread::spawn(move || {
        listener.set_nonblocking(false).ok();
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let mut request = Vec::new();
            // Read until headers end or buffer fills.
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        request.extend_from_slice(&buf[..n]);
                        if request.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                        if request.len() > 16_384 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            let text = String::from_utf8_lossy(&request);
            let mut lines = text.split("\r\n");
            let request_line = lines.next().unwrap_or("");
            let path = request_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("/")
                .to_string();

            let mut host = None;
            let mut xff = None;
            let mut xfp = None;
            let mut xfh = None;
            let mut request_id = None;
            let mut content_length = 0usize;
            for line in lines {
                if line.is_empty() {
                    break;
                }
                let lower = line.to_ascii_lowercase();
                if let Some(v) = line.split_once(':').map(|(_, v)| v.trim().to_string()) {
                    if lower.starts_with("host:") {
                        host = Some(v);
                    } else if lower.starts_with("x-forwarded-for:") {
                        xff = Some(v);
                    } else if lower.starts_with("x-forwarded-proto:") {
                        xfp = Some(v);
                    } else if lower.starts_with("x-forwarded-host:") {
                        xfh = Some(v);
                    } else if lower.starts_with("x-request-id:") {
                        request_id = Some(v);
                    } else if lower.starts_with("content-length:") {
                        content_length = v.parse().unwrap_or(0);
                    }
                }
            }

            // Drain body if present (best-effort for POST tests).
            let header_end = request
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| i + 4)
                .unwrap_or(request.len());
            let mut body = request[header_end..].to_vec();
            while body.len() < content_length {
                match stream.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => body.extend_from_slice(&buf[..n]),
                    Err(_) => break,
                }
            }

            hits_thread.lock().unwrap().push(MockHit {
                path,
                host,
                xff,
                xfp,
                xfh,
                request_id,
                body,
            });

            let payload = format!("upstream={label}");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.shutdown(Shutdown::Both);
        }
    });

    MockUpstream {
        port,
        _join: join,
        hits,
    }
}

/// Upstream that accepts connections and never sends a response.
///
/// Returns the port plus the number of accepted connections so tests can assert
/// that the proxy did not retry.
fn spawn_silent_upstream() -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent upstream");
    let port = listener.local_addr().unwrap().port();
    let accepts = Arc::new(AtomicUsize::new(0));
    let accepted = Arc::clone(&accepts);

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            accepted.fetch_add(1, Ordering::SeqCst);
            // Hold the socket open without writing anything.
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(5));
                drop(stream);
            });
        }
    });

    (port, accepts)
}

/// Upstream that completes a WebSocket (HTTP/1.1 101) handshake and then echoes
/// raw bytes, which is enough to observe upgrade tunnelling.
fn spawn_websocket_echo_upstream() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind websocket upstream");
    let port = listener.local_addr().unwrap().port();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            thread::spawn(move || {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 1024];
                let mut request = Vec::new();
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            request.extend_from_slice(&buf[..n]);
                            if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                break;
                            }
                        }
                    }
                }

                // Accept value for the RFC 6455 sample key.
                let response = "HTTP/1.1 101 Switching Protocols\r\n\
                                Upgrade: websocket\r\n\
                                Connection: Upgrade\r\n\
                                Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\r\n";
                if stream.write_all(response.as_bytes()).is_err() {
                    return;
                }

                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => {
                            if stream.write_all(&buf[..n]).is_err() {
                                return;
                            }
                        }
                    }
                }
            });
        }
    });

    port
}

/// Perform a WebSocket upgrade through `proxy` and echo `payload` through the tunnel.
fn websocket_tunnel(
    proxy: SocketAddr,
    host: &str,
    path: &str,
    payload: &[u8],
) -> (u16, String, Vec<u8>) {
    let mut stream =
        TcpStream::connect_timeout(&proxy, Duration::from_secs(2)).expect("connect proxy");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();

    // Read the handshake byte-by-byte so no tunnelled payload is consumed.
    let mut headers = Vec::new();
    let mut byte = [0u8; 1];
    while !headers.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => headers.push(byte[0]),
            _ => break,
        }
    }
    let text = String::from_utf8_lossy(&headers).into_owned();
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);

    stream.write_all(payload).unwrap();
    let mut echoed = vec![0u8; payload.len()];
    stream.read_exact(&mut echoed).unwrap();
    let _ = stream.shutdown(Shutdown::Both);

    (status, text, echoed)
}

/// Fake Docker Engine API over a Unix socket with a swappable response body.
struct FakeDocker {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    body: Arc<Mutex<String>>,
    requests: Arc<AtomicUsize>,
}

impl FakeDocker {
    fn spawn() -> Self {
        let dir = tempfile::tempdir().expect("docker tempdir");
        let socket = dir.path().join("docker.sock");
        let listener = UnixListener::bind(&socket).expect("bind fake docker socket");
        let body = Arc::new(Mutex::new("[]".to_string()));
        let shared = Arc::clone(&body);
        let requests = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&requests);

        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                count.fetch_add(1, Ordering::SeqCst);
                let mut request = [0u8; 1024];
                let _ = stream.read(&mut request);
                let body = shared.lock().unwrap().clone();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });

        Self {
            _dir: dir,
            socket,
            body,
            requests,
        }
    }

    fn set_body(&self, body: &str) {
        *self.body.lock().unwrap() = body.to_string();
    }

    fn container(&self, port: u16) -> String {
        format!(
            r#"[{{"Id":"1111111111111111","Labels":{{"runnelon.upstream":"docker-api","runnelon.host":"*"}},"Ports":[{{"PrivatePort":{port},"Type":"tcp"}}],"NetworkSettings":{{"Networks":{{"bridge":{{"IPAddress":"127.0.0.1"}}}}}}}}]"#
        )
    }
}

struct ProxyProcess {
    child: Child,
    addr: SocketAddr,
    config: NamedTempFile,
}

impl ProxyProcess {
    fn listen_port(&self) -> u16 {
        self.addr.port()
    }

    /// Replace the config file; the reloader polls it.
    fn write_config(&self, yaml: &str) {
        std::fs::write(self.config.path(), yaml).expect("rewrite config");
    }
}

impl Drop for ProxyProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_proxy(config_yaml: &str) -> ProxyProcess {
    let listen_port = free_port();
    let yaml = config_yaml.replace("__LISTEN__", &listen_port.to_string());

    let mut file = NamedTempFile::new().expect("config tempfile");
    write!(file, "{yaml}").unwrap();
    file.flush().unwrap();

    let bin = cargo_bin!("runnelon");
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(file.path())
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn runnelon");

    // Keep the file alive for reloads; the child reads it again on change.
    let addr: SocketAddr = format!("127.0.0.1:{listen_port}").parse().unwrap();
    if let Err(err) = wait_for_listen(addr, Duration::from_secs(20)) {
        let _ = child.kill();
        let _ = child.wait();
        panic!("{err}");
    }

    ProxyProcess {
        child,
        addr,
        config: file,
    }
}

fn start_proxy_with_management(config_yaml: &str) -> (ProxyProcess, SocketAddr) {
    let management_port = free_port();
    let yaml = config_yaml.replace("__MGMT__", &management_port.to_string());
    let proxy = start_proxy(&yaml);
    let management = format!("127.0.0.1:{management_port}")
        .parse()
        .expect("management addr");
    (proxy, management)
}

fn wait_for_listen(addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(());
        }
        if start.elapsed() > timeout {
            return Err(format!(
                "proxy did not accept connections at {addr} within {timeout:?}"
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn http_get(proxy: SocketAddr, host: &str, path: &str) -> (u16, String) {
    http_exchange(proxy, "GET", host, path, &[], &[])
}

/// Case-insensitive response-header lookup on raw response text.
fn header_value(response: &str, name: &str) -> Option<String> {
    response.lines().find_map(|line| {
        let (header, value) = line.split_once(':')?;
        header
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

/// Generate a self-signed certificate for the TLS e2e test.
fn self_signed_cert(dir: &std::path::Path) -> (PathBuf, PathBuf) {
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let status = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
        ])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run openssl req");
    assert!(status.success(), "openssl could not generate a certificate");
    (cert, key)
}

/// HTTPS request through the proxy using `curl` (test-only dependency).
fn https_get(proxy: SocketAddr, host: &str, path: &str) -> (u16, String) {
    let output = Command::new("curl")
        .args(["-sk", "-o", "-", "-w", "\n%{http_code}"])
        .arg(format!("https://{proxy}{path}"))
        .arg("-H")
        .arg(format!("Host: {host}"))
        .output()
        .expect("run curl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let (body, status) = stdout.rsplit_once('\n').unwrap_or((&stdout, "0"));
    (status.trim().parse().unwrap_or(0), body.to_string())
}

fn http_exchange(
    proxy: SocketAddr,
    method: &str,
    host: &str,
    path: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> (u16, String) {
    let mut stream =
        TcpStream::connect_timeout(&proxy, Duration::from_secs(2)).expect("connect proxy");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (name, value) in extra_headers {
        req.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    if !body.is_empty() {
        stream.write_all(body).unwrap();
    }

    read_response(stream)
}

/// Read a full HTTP/1 response from `stream` and return status + raw text.
fn read_response(mut stream: TcpStream) -> (u16, String) {
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).unwrap();
    let text = String::from_utf8_lossy(&resp);
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, text.into_owned())
}

/// Send a chunked `POST` with no `Content-Length` header.
fn http_post_chunked(proxy: SocketAddr, host: &str, path: &str, chunks: &[&[u8]]) -> (u16, String) {
    let mut stream =
        TcpStream::connect_timeout(&proxy, Duration::from_secs(2)).expect("connect proxy");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).unwrap();
    for chunk in chunks {
        stream
            .write_all(format!("{:x}\r\n", chunk.len()).as_bytes())
            .unwrap();
        stream.write_all(chunk).unwrap();
        stream.write_all(b"\r\n").unwrap();
    }
    stream.write_all(b"0\r\n\r\n").unwrap();

    read_response(stream)
}

#[test]
fn routes_two_backends_by_host_and_path() {
    let api = spawn_mock("api");
    let web = spawn_mock("web");

    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "app.local"
    path_prefix: "/api"
    upstream: "api"
  - host: "app.local"
    path_prefix: "/"
    upstream: "web"
  - host: "other.local"
    path_prefix: "/"
    upstream: "web"
upstreams:
  api:
    endpoints: ["127.0.0.1:{api_port}"]
    connect_timeout_ms: 2000
    read_timeout_ms: 5000
    write_timeout_ms: 5000
  web:
    endpoints: ["127.0.0.1:{web_port}"]
    connect_timeout_ms: 2000
    read_timeout_ms: 5000
    write_timeout_ms: 5000
"#,
        api_port = api.port,
        web_port = web.port
    );

    let proxy = start_proxy(&config);

    let (status, body) = http_get(proxy.addr, "app.local", "/api/users");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=api"), "{body}");

    let (status, body) = http_get(proxy.addr, "app.local", "/");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=web"), "{body}");

    let (status, body) = http_get(proxy.addr, "other.local", "/x");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=web"), "{body}");

    // Forwarding headers observed by upstream.
    thread::sleep(Duration::from_millis(50));
    let hits = api.hits.lock().unwrap();
    assert!(!hits.is_empty());
    let hit = &hits[0];
    assert_eq!(hit.path, "/api/users");
    assert_eq!(hit.host.as_deref(), Some("app.local"));
    assert!(hit.xff.as_ref().is_some_and(|v| !v.is_empty()), "{hit:?}");
    assert_eq!(hit.xfp.as_deref(), Some("http"));
    assert_eq!(hit.xfh.as_deref(), Some("app.local"));
}

#[test]
fn canonical_path_selects_route_before_upstream() {
    let api = spawn_mock("api");
    let web = spawn_mock("web");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/api"
    upstream: "api"
  - host: "*"
    path_prefix: "/public"
    upstream: "web"
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  api:
    endpoints: ["127.0.0.1:{}"]
  web:
    endpoints: ["127.0.0.1:{}"]
"#,
        api.port, web.port
    );
    let proxy = start_proxy(&config);

    // Dot segments resolve to `/api/users` before route selection.
    let (status, body) = http_get(proxy.addr, "app.local", "/public/../api/users");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=api"), "{body}");

    // Encoded dots resolve to `/admin`, which only matches the wildcard route.
    let (status, body) = http_get(proxy.addr, "app.local", "/api/%2e%2e/admin");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=web"), "{body}");

    // Encoded delimiters are ambiguous and rejected before any upstream call.
    let (status, body) = http_get(proxy.addr, "app.local", "/api%2Fusers");
    assert_eq!(status, 400, "{body}");

    thread::sleep(Duration::from_millis(50));
    let api_hits = api.hits.lock().unwrap();
    assert_eq!(api_hits.len(), 1, "{api_hits:?}");
    // Routing used the canonical path; the upstream still sees the raw target.
    assert_eq!(api_hits[0].path, "/public/../api/users");
    let web_hits = web.hits.lock().unwrap();
    assert_eq!(web_hits.len(), 1, "{web_hits:?}");
    assert_eq!(web_hits[0].path, "/api/%2e%2e/admin");
}

#[test]
fn spoofed_forwarding_headers_are_replaced() {
    let web = spawn_mock("web");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:{}"]
"#,
        web.port
    );
    let proxy = start_proxy(&config);

    let (status, body) = http_exchange(
        proxy.addr,
        "GET",
        "app.local",
        "/",
        &[],
        &[
            ("X-Forwarded-For", "203.0.113.9, 10.0.0.1"),
            ("X-Forwarded-Proto", "https"),
            ("X-Forwarded-Host", "evil.example"),
        ],
    );
    assert_eq!(status, 200, "{body}");

    thread::sleep(Duration::from_millis(50));
    let hits = web.hits.lock().unwrap();
    let hit = hits.first().expect("upstream should receive the request");
    // Client-supplied chains are discarded; only the observed peer remains.
    assert_eq!(hit.xff.as_deref(), Some("127.0.0.1"));
    assert_eq!(hit.xfp.as_deref(), Some("http"));
    assert_eq!(hit.xfh.as_deref(), Some("app.local"));
}

#[test]
fn trusted_proxy_preserves_forwarded_chain() {
    let web = spawn_mock("web");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:{}"]
trusted_proxies: ["127.0.0.1/32"]
"#,
        web.port
    );
    let proxy = start_proxy(&config);

    let (status, body) = http_exchange(
        proxy.addr,
        "GET",
        "app.local",
        "/",
        &[],
        &[("X-Forwarded-For", "203.0.113.9, 198.51.100.7")],
    );
    assert_eq!(status, 200, "{body}");

    thread::sleep(Duration::from_millis(50));
    let hits = web.hits.lock().unwrap();
    let hit = hits.first().expect("upstream should receive the request");
    assert_eq!(
        hit.xff.as_deref(),
        Some("203.0.113.9, 198.51.100.7, 127.0.0.1")
    );
}

#[test]
fn round_robin_distributes_requests_across_endpoints() {
    let first = spawn_mock("api-1");
    let second = spawn_mock("api-2");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{first}", "127.0.0.1:{second}"]
"#,
        first = first.port,
        second = second.port
    );
    let proxy = start_proxy(&config);

    for _ in 0..4 {
        let (status, body) = http_get(proxy.addr, "app.local", "/");
        assert_eq!(status, 200, "{body}");
    }

    thread::sleep(Duration::from_millis(50));
    let first_hits = first.hits.lock().unwrap().len();
    let second_hits = second.hits.lock().unwrap().len();
    assert_eq!(first_hits, 2, "first endpoint hits");
    assert_eq!(second_hits, 2, "second endpoint hits");
}

#[test]
fn weighted_schedule_follows_config() {
    let first = spawn_mock("api-1");
    let second = spawn_mock("api-2");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{first}", "127.0.0.1:{second}"]
    weights:
      "127.0.0.1:{first}": 3
      "127.0.0.1:{second}": 1
"#,
        first = first.port,
        second = second.port
    );
    let proxy = start_proxy(&config);

    for _ in 0..4 {
        let (status, body) = http_get(proxy.addr, "app.local", "/");
        assert_eq!(status, 200, "{body}");
    }

    thread::sleep(Duration::from_millis(50));
    assert_eq!(first.hits.lock().unwrap().len(), 3, "3:1 weights");
    assert_eq!(second.hits.lock().unwrap().len(), 1, "3:1 weights");
}

#[test]
fn connect_failure_parks_endpoint_for_cooldown() {
    let healthy = spawn_mock("healthy");
    let dead_port = free_port();
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{dead_port}", "127.0.0.1:{healthy}"]
    unhealthy_cooldown_ms: 60000
"#,
        healthy = healthy.port
    );
    let proxy = start_proxy(&config);

    // POST is not retried, so the first request fails on the dead endpoint.
    let (status, body) = http_exchange(proxy.addr, "POST", "app.local", "/", b"", &[]);
    assert_eq!(status, 502, "{body}");

    // Subsequent requests skip the parked endpoint instead of failing 1-in-2.
    for _ in 0..4 {
        let (status, body) = http_exchange(proxy.addr, "POST", "app.local", "/", b"", &[]);
        assert_eq!(status, 200, "{body}");
        assert!(body.contains("upstream=healthy"), "{body}");
    }

    thread::sleep(Duration::from_millis(50));
    assert_eq!(healthy.hits.lock().unwrap().len(), 4);
}

#[test]
fn idempotent_connect_failure_retries_alternate_endpoint() {
    let healthy = spawn_mock("healthy");
    let dead_port = free_port();
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{dead_port}", "127.0.0.1:{healthy}"]
"#,
        healthy = healthy.port
    );
    let proxy = start_proxy(&config);

    // GET is idempotent: the first attempt fails, one retry lands on the live peer.
    let (status, body) = http_get(proxy.addr, "app.local", "/");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=healthy"), "{body}");
}

#[test]
fn non_idempotent_connect_failure_is_not_retried() {
    let healthy = spawn_mock("healthy");
    let dead_port = free_port();
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{dead_port}", "127.0.0.1:{healthy}"]
"#,
        healthy = healthy.port
    );
    let proxy = start_proxy(&config);

    let (status, body) = http_exchange(proxy.addr, "POST", "app.local", "/", b"payload", &[]);
    assert_eq!(status, 502, "{body}");

    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        healthy.hits.lock().unwrap().len(),
        0,
        "non-idempotent request must not be retried"
    );
}

#[test]
fn management_endpoints_report_health() {
    let upstream = spawn_mock("web");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:{}"]
management:
  address: "127.0.0.1:__MGMT__"
"#,
        upstream.port
    );
    let (_proxy, management) = start_proxy_with_management(&config);

    for path in ["/healthz", "/readyz"] {
        let (status, body) = http_get(management, "localhost", path);
        assert_eq!(status, 200, "{path}: {body}");
    }

    let (status, _) = http_get(management, "localhost", "/nope");
    assert_eq!(status, 404);

    let (status, _) = http_exchange(management, "POST", "localhost", "/healthz", b"", &[]);
    assert_eq!(status, 405);
}

#[test]
fn reload_swaps_upstreams_and_keeps_last_known_good() {
    let old = spawn_mock("old");
    let new = spawn_mock("new");
    let make = |upstream: &str, port: u16, listener: &str| {
        format!(
            r#"
version: 1
listeners:
  - address: "{listener}"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "{upstream}"
upstreams:
  {upstream}:
    endpoints: ["127.0.0.1:{port}"]
"#
        )
    };

    let proxy = start_proxy(&make("old", old.port, "127.0.0.1:__LISTEN__"));
    let (status, body) = http_get(proxy.addr, "app.local", "/");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=old"), "{body}");

    // Invalid config is rejected; the old snapshot keeps serving.
    proxy.write_config("version: 1\n");
    thread::sleep(Duration::from_millis(1200));
    let (status, body) = http_get(proxy.addr, "app.local", "/");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=old"), "{body}");

    // A valid reload swaps upstreams without restarting the process.
    let listener = format!("127.0.0.1:{}", proxy.listen_port());
    proxy.write_config(&make("new", new.port, &listener));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let (status, body) = http_get(proxy.addr, "app.local", "/");
        if status == 200 && body.contains("upstream=new") {
            break;
        }
        assert!(Instant::now() < deadline, "reload was not applied: {body}");
        thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn reload_under_load_never_fails_requests() {
    let old = spawn_mock("old");
    let new = spawn_mock("new");
    let make = |upstream: &str, port: u16, listener: &str| {
        format!(
            r#"
version: 1
listeners:
  - address: "{listener}"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "{upstream}"
upstreams:
  {upstream}:
    endpoints: ["127.0.0.1:{port}"]
"#
        )
    };

    let proxy = start_proxy(&make("old", old.port, "127.0.0.1:__LISTEN__"));
    let listener = format!("127.0.0.1:{}", proxy.listen_port());
    let stop = Arc::new(AtomicBool::new(false));

    let mut clients = Vec::new();
    for _ in 0..4 {
        let addr = proxy.addr;
        let stop = Arc::clone(&stop);
        clients.push(thread::spawn(move || {
            // (old, new, failed, unexpected)
            let mut stats = (0usize, 0usize, 0usize, 0usize);
            while !stop.load(Ordering::Relaxed) {
                let (status, body) = http_get(addr, "app.local", "/");
                match status {
                    200 if body.contains("upstream=old") => stats.0 += 1,
                    200 if body.contains("upstream=new") => stats.1 += 1,
                    200 => stats.3 += 1,
                    _ => stats.2 += 1,
                }
            }
            stats
        }));
    }

    // Cycle valid and invalid configurations while requests are in flight.
    for _ in 0..2 {
        proxy.write_config("version: 1\n"); // rejected, last-known-good keeps serving
        thread::sleep(Duration::from_millis(700));
        proxy.write_config(&make("new", new.port, &listener));
        thread::sleep(Duration::from_millis(700));
        proxy.write_config(&make("old", old.port, &listener));
        thread::sleep(Duration::from_millis(700));
    }
    stop.store(true, Ordering::Relaxed);

    let totals = clients
        .into_iter()
        .map(|client| client.join().expect("client thread"))
        .fold((0, 0, 0, 0), |acc, stats| {
            (
                acc.0 + stats.0,
                acc.1 + stats.1,
                acc.2 + stats.2,
                acc.3 + stats.3,
            )
        });

    assert_eq!(totals.2, 0, "no request may fail while reloads are running");
    assert_eq!(
        totals.3, 0,
        "every response must come from a configured upstream"
    );
    assert!(totals.0 > 0, "the old snapshot must serve requests");
    assert!(totals.1 > 0, "the new snapshot must serve requests");
}

#[test]
fn websocket_upgrade_is_tunneled() {
    let upstream = spawn_websocket_echo_upstream();
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/socket"
    upstream: "ws"
upstreams:
  ws:
    endpoints: ["127.0.0.1:{upstream}"]
"#
    );
    let proxy = start_proxy(&config);

    // Masked text frame carrying "Hello" (RFC 6455 client frame).
    let frame: &[u8] = b"\x81\x85\x37\xfa\x21\x3d\x7f\x9f\x4d\x51\x58";
    let (status, headers, echoed) = websocket_tunnel(proxy.addr, "app.local", "/socket", frame);

    assert_eq!(status, 101, "{headers}");
    assert!(
        headers.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
        "{headers}"
    );
    assert!(
        headers.to_ascii_lowercase().contains("upgrade: websocket"),
        "{headers}"
    );
    assert_eq!(echoed, frame, "upgraded connection must tunnel bytes");
}

#[test]
fn metrics_reflect_proxied_requests() {
    let web = spawn_mock("web");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "app.local"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:{}"]
management:
  address: "127.0.0.1:__MGMT__"
"#,
        web.port
    );
    let (proxy, management) = start_proxy_with_management(&config);

    for _ in 0..2 {
        let (status, body) = http_get(proxy.addr, "app.local", "/");
        assert_eq!(status, 200, "{body}");
    }
    let (status, _) = http_get(proxy.addr, "unknown.local", "/");
    assert_eq!(status, 404);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (status, text) = http_get(management, "localhost", "/metrics");
        assert_eq!(status, 200, "{text}");
        let converged = text.contains("runnelon_requests_total 3")
            && text.contains("runnelon_responses_total{class=\"2\"} 2")
            && text.contains("runnelon_responses_total{class=\"4\"} 1")
            && text.contains("runnelon_inflight_requests 0")
            && text.contains("runnelon_response_duration_ms_count 3");
        if converged {
            assert!(
                text.contains("runnelon_response_duration_ms_bucket{le=\"+Inf\"} 3"),
                "{text}"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "metrics did not converge:\n{text}"
        );
        thread::sleep(Duration::from_millis(50));
    }

    let (status, _) = http_exchange(management, "POST", "localhost", "/metrics", b"", &[]);
    assert_eq!(status, 405);
}

#[test]
fn docker_discovery_reconciles_routes_and_keeps_last_known_good() {
    let first = spawn_mock("first");
    let second = spawn_mock("second");
    let docker = FakeDocker::spawn();
    docker.set_body(&docker.container(first.port));

    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes: []
upstreams: {{}}
docker:
  socket: "{}"
  poll_interval_ms: 200
"#,
        docker.socket.display()
    );
    let proxy = start_proxy(&config);

    let (status, body) = http_get(proxy.addr, "app.local", "/");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=first"), "{body}");

    // Endpoint changes converge through the next poll.
    docker.set_body(&docker.container(second.port));
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let (status, body) = http_get(proxy.addr, "app.local", "/");
        if status == 200 && body.contains("upstream=second") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "discovery did not converge: {body}"
        );
        thread::sleep(Duration::from_millis(100));
    }

    // Containers disappearing remove their routes.
    docker.set_body("[]");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let (status, _) = http_get(proxy.addr, "app.local", "/");
        if status == 404 {
            break;
        }
        assert!(Instant::now() < deadline, "routes did not disappear");
        thread::sleep(Duration::from_millis(100));
    }

    // A failing daemon response keeps the last-known-good (empty) state alive.
    docker.set_body("not json");
    thread::sleep(Duration::from_millis(600));
    let (status, _) = http_get(proxy.addr, "app.local", "/");
    assert_eq!(
        status, 404,
        "process must stay healthy after discovery failures"
    );

    assert!(
        docker.requests.load(Ordering::SeqCst) >= 4,
        "poller should have queried Docker repeatedly"
    );
}

#[test]
fn assigns_and_propagates_request_ids() {
    let web = spawn_mock("web");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:{}"]
"#,
        web.port
    );
    let proxy = start_proxy(&config);

    let (status, body) = http_get(proxy.addr, "app.local", "/");
    assert_eq!(status, 200, "{body}");
    let first = header_value(&body, "x-request-id").expect("response request id");
    assert_eq!(first.len(), 16, "{first}");

    let (status, body) = http_get(proxy.addr, "app.local", "/other");
    assert_eq!(status, 200, "{body}");
    let second = header_value(&body, "x-request-id").expect("second request id");
    assert_ne!(first, second, "request ids must be unique");

    thread::sleep(Duration::from_millis(50));
    let hits = web.hits.lock().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].request_id.as_deref(), Some(first.as_str()));
    assert_eq!(hits[1].request_id.as_deref(), Some(second.as_str()));
}

#[test]
fn tls_listener_terminates_https() {
    let web = spawn_mock("web");
    let dir = tempfile::tempdir().expect("tempdir");
    let (cert, key) = self_signed_cert(dir.path());
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
    tls:
      cert_path: "{}"
      key_path: "{}"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:{}"]
"#,
        cert.display(),
        key.display(),
        web.port
    );
    let proxy = start_proxy(&config);

    let (status, body) = https_get(proxy.addr, "app.local", "/");
    assert_eq!(status, 200, "{body}");
    assert!(body.contains("upstream=web"), "{body}");

    thread::sleep(Duration::from_millis(50));
    let hits = web.hits.lock().unwrap();
    let hit = hits.first().expect("upstream hit");
    assert_eq!(
        hit.xfp.as_deref(),
        Some("https"),
        "TLS listener must set https"
    );
}

#[test]
fn unknown_route_returns_404() {
    let web = spawn_mock("web");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "only.local"
    path_prefix: "/"
    upstream: "web"
upstreams:
  web:
    endpoints: ["127.0.0.1:{}"]
"#,
        web.port
    );
    let proxy = start_proxy(&config);
    let (status, body) = http_get(proxy.addr, "missing.local", "/");
    assert_eq!(status, 404, "{body}");
}

#[test]
fn upstream_connect_failure_returns_502() {
    // Nothing listens on this port.
    let dead_port = free_port();
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "dead"
upstreams:
  dead:
    endpoints: ["127.0.0.1:{dead_port}"]
    connect_timeout_ms: 500
    read_timeout_ms: 1000
    write_timeout_ms: 1000
"#
    );
    let proxy = start_proxy(&config);
    let (status, body) = http_get(proxy.addr, "any.local", "/");
    assert_eq!(status, 502, "{body}");
}

#[test]
fn connect_timeout_is_bounded() {
    // TEST-NET-1 address: connect cannot succeed, so the configured timeout must
    // bound how long the client waits.
    let config = r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "blackhole"
upstreams:
  blackhole:
    endpoints: ["192.0.2.1:9"]
    connect_timeout_ms: 300
    read_timeout_ms: 1000
    write_timeout_ms: 1000
"#;
    let proxy = start_proxy(config);

    let started = Instant::now();
    let (status, body) = http_get(proxy.addr, "any.local", "/");
    let elapsed = started.elapsed();
    assert_eq!(status, 502, "{body}");
    assert!(
        elapsed < Duration::from_secs(3),
        "connect attempt was not bounded: {elapsed:?}"
    );
}

#[test]
fn slow_upstream_hits_read_timeout_without_retry() {
    let (silent_port, accepts) = spawn_silent_upstream();
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "slow"
upstreams:
  slow:
    endpoints: ["127.0.0.1:{silent_port}"]
    connect_timeout_ms: 500
    read_timeout_ms: 300
    write_timeout_ms: 1000
"#
    );
    let proxy = start_proxy(&config);

    let started = Instant::now();
    let (status, body) = http_get(proxy.addr, "any.local", "/");
    let elapsed = started.elapsed();
    assert_eq!(status, 504, "{body}");
    assert!(
        elapsed >= Duration::from_millis(250),
        "returned before the read timeout: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "read timeout was not enforced: {elapsed:?}"
    );

    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        accepts.load(Ordering::SeqCst),
        1,
        "request must not be retried against the same endpoint"
    );
}

#[test]
fn streams_request_body_to_upstream() {
    let api = spawn_mock("api");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{}"]
"#,
        api.port
    );
    let proxy = start_proxy(&config);
    let payload = b"hello-proxy-body";
    let (status, body) = http_exchange(proxy.addr, "POST", "app.local", "/echo", payload, &[]);
    assert_eq!(status, 200, "{body}");
    thread::sleep(Duration::from_millis(50));
    let hits = api.hits.lock().unwrap();
    assert_eq!(hits.last().unwrap().body, payload);
}

#[test]
fn declared_body_over_limit_is_rejected_before_upstream() {
    let api = spawn_mock("api");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{}"]
limits:
  max_request_body_bytes: 1024
"#,
        api.port
    );
    let proxy = start_proxy(&config);

    let payload = vec![b'a'; 4096];
    let (status, body) = http_exchange(proxy.addr, "POST", "app.local", "/upload", &payload, &[]);
    assert_eq!(status, 413, "{body}");

    thread::sleep(Duration::from_millis(50));
    assert!(
        api.hits.lock().unwrap().is_empty(),
        "oversized request must not reach the upstream"
    );
}

#[test]
fn chunked_body_over_limit_returns_413() {
    let api = spawn_mock("api");
    let config = format!(
        r#"
version: 1
listeners:
  - address: "127.0.0.1:__LISTEN__"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "api"
upstreams:
  api:
    endpoints: ["127.0.0.1:{}"]
limits:
  max_request_body_bytes: 1024
"#,
        api.port
    );
    let proxy = start_proxy(&config);

    let chunk = vec![b'b'; 512];
    let chunks: Vec<&[u8]> = vec![chunk.as_slice(), chunk.as_slice(), chunk.as_slice()];
    let (status, body) = http_post_chunked(proxy.addr, "app.local", "/upload", &chunks);
    assert_eq!(status, 413, "{body}");
}
