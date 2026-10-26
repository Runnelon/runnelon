//! Pingora `ProxyHttp` implementation backed by an immutable config snapshot.

use crate::access::AccessRecord;
use crate::config::Snapshot;
use crate::metrics::Metrics;
use crate::net::IpNet;
use crate::routing::{match_route, normalize_path, normalize_request_host};
use arc_swap::ArcSwap;
use async_trait::async_trait;
use bytes::Bytes;
use log::{debug, error, info, warn};
use pingora::http::{RequestHeader, ResponseHeader};
use pingora::protocols::l4::socket::SocketAddr as PingoraSockAddr;
use pingora::proxy::{FailToProxy, ProxyHttp, Session};
use pingora::upstreams::peer::HttpPeer;
use pingora::Result;
use pingora::{Error as PingoraError, ErrorSource, ErrorType};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Source of per-process request identifiers.
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Maximum total upstream connection attempts per request (initial + one retry).
const MAX_ENDPOINT_ATTEMPTS: usize = 2;

/// Methods that may safely be retried after a connect failure (RFC 9110
/// idempotent methods). The request never reached an upstream, but retrying
/// non-idempotent methods would still change observable semantics.
fn is_idempotent_method(method: &str) -> bool {
    matches!(
        method,
        "GET" | "HEAD" | "OPTIONS" | "TRACE" | "PUT" | "DELETE"
    )
}

/// Per-request context shared across proxy phases.
#[derive(Debug, Default)]
pub struct RequestCtx {
    /// Snapshot used for this request (kept consistent across reloads).
    pub snapshot: Option<Arc<Snapshot>>,
    /// Index of the matched route in the snapshot.
    pub route: Option<usize>,
    /// Selected endpoint index, set in `upstream_peer`.
    pub endpoint: Option<usize>,
    /// Endpoints already attempted for this request.
    pub tried: Vec<usize>,
    /// Request body bytes observed so far (streaming limit enforcement).
    pub body_bytes: u64,
    /// When proxying started, for latency measurement.
    pub started: Option<Instant>,
    /// Per-process request identifier (hex).
    pub request_id: String,
    /// Selected endpoint address, for access logging.
    pub endpoint_addr: Option<SocketAddr>,
}

/// Reverse-proxy application reading the live [`Snapshot`] published by reloads.
pub struct RunnelonProxy {
    snapshot: Arc<ArcSwap<Snapshot>>,
    metrics: Arc<Metrics>,
    scheme: &'static str,
}

impl RunnelonProxy {
    /// Create a proxy that reads the current snapshot on every request.
    ///
    /// `scheme` is the downstream protocol for this listener (`http`/`https`)
    /// and feeds `X-Forwarded-Proto`.
    pub fn new(
        snapshot: Arc<ArcSwap<Snapshot>>,
        metrics: Arc<Metrics>,
        scheme: &'static str,
    ) -> Self {
        Self {
            snapshot,
            metrics,
            scheme,
        }
    }
}

#[async_trait]
impl ProxyHttp for RunnelonProxy {
    type CTX = RequestCtx;

    fn new_ctx(&self) -> Self::CTX {
        RequestCtx::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        self.metrics.request_started();
        ctx.started = Some(Instant::now());
        ctx.request_id = format!("{:016x}", NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed));

        let snapshot = self.snapshot.load_full();
        let req = session.req_header();
        let host = req
            .headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let raw_path = req.uri.path();

        // Canonicalize before routing so `..`, `%2e` and similar cannot select a
        // different upstream than the one the path actually resolves to.
        let path = match normalize_path(raw_path) {
            Ok(path) => path,
            Err(err) => {
                warn!(
                    "rejected request host={} path={}: {err}",
                    normalize_request_host(host),
                    raw_path
                );
                session.respond_error(400).await?;
                return Ok(true);
            }
        };

        match match_route(&snapshot, host, &path) {
            Some(m) => {
                if let Some(declared) = declared_content_length(req) {
                    if declared > snapshot.max_request_body_bytes {
                        warn!(
                            "rejected request host={} path={}: content-length {declared} exceeds limit {}",
                            normalize_request_host(host),
                            path,
                            snapshot.max_request_body_bytes
                        );
                        session.respond_error(413).await?;
                        return Ok(true);
                    }
                }
                debug!(
                    "matched route order={} upstream={} host={} path={}",
                    m.route_order, m.upstream, host, path
                );
                ctx.route = Some(m.route_index);
                ctx.snapshot = Some(snapshot);
                Ok(false)
            }
            None => {
                warn!(
                    "no route for host={} path={}",
                    normalize_request_host(host),
                    path
                );
                session.respond_error(404).await?;
                Ok(true)
            }
        }
    }

    async fn request_body_filter(
        &self,
        _session: &mut Session,
        body: &mut Option<Bytes>,
        _end_of_stream: bool,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        let Some(chunk) = body.as_ref() else {
            return Ok(());
        };
        let Some(limit) = ctx.snapshot.as_ref().map(|s| s.max_request_body_bytes) else {
            return Ok(());
        };
        ctx.body_bytes = ctx.body_bytes.saturating_add(chunk.len() as u64);
        if ctx.body_bytes > limit {
            warn!("rejected request body: exceeded limit of {limit} bytes");
            return Err(PingoraError::new(ErrorType::HTTPStatus(413)));
        }
        Ok(())
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let snapshot = ctx.snapshot.clone().ok_or_else(|| {
            PingoraError::new_str("internal error: snapshot missing before upstream_peer")
        })?;
        let route = ctx
            .route
            .and_then(|index| snapshot.routes.get(index))
            .ok_or_else(|| {
                PingoraError::new_str("internal error: route not selected before upstream_peer")
            })?;
        let upstream = snapshot.upstreams.get(&route.upstream).ok_or_else(|| {
            PingoraError::new_str("internal error: selected upstream missing from snapshot")
        })?;
        // Weighted selection that skips cooling endpoints and endpoints already
        // tried for this request.
        let endpoint = if ctx.tried.is_empty() {
            upstream.select_endpoint()
        } else {
            match upstream.select_endpoint_avoiding(&ctx.tried) {
                Some(endpoint) => endpoint,
                None => {
                    return Err(PingoraError::new_up(ErrorType::ConnectError));
                }
            }
        };
        let addr = upstream.endpoints[endpoint];
        ctx.endpoint = Some(endpoint);
        ctx.endpoint_addr = Some(addr);
        ctx.tried.push(endpoint);
        let mut peer = Box::new(HttpPeer::new(addr, false, String::new()));
        peer.options.connection_timeout = Some(upstream.connect_timeout);
        peer.options.read_timeout = Some(upstream.read_timeout);
        peer.options.write_timeout = Some(upstream.write_timeout);
        Ok(peer)
    }

    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut RequestHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        let trusted_proxies: &[IpNet] = ctx
            .snapshot
            .as_ref()
            .map_or(&[], |snapshot| snapshot.trusted_proxies.as_slice());
        apply_forwarding_headers(session, upstream_request, trusted_proxies, self.scheme);
        let _ = upstream_request.insert_header("X-Request-Id", ctx.request_id.as_str());
        Ok(())
    }

    async fn response_filter(
        &self,
        _session: &mut Session,
        response: &mut ResponseHeader,
        ctx: &mut Self::CTX,
    ) -> Result<()>
    where
        Self::CTX: Send + Sync,
    {
        let _ = response.insert_header("X-Request-Id", ctx.request_id.as_str());
        Ok(())
    }

    async fn logging(
        &self,
        session: &mut Session,
        _error: Option<&PingoraError>,
        ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        let Some(started) = ctx.started.take() else {
            return;
        };
        let elapsed = started.elapsed();
        let status = session
            .response_written()
            .map(|response| response.status.as_u16());

        let record = {
            let request = session.req_header();
            let host = request
                .headers
                .get("host")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("");
            let upstream_name = match (ctx.route, ctx.snapshot.as_ref()) {
                (Some(index), Some(snapshot)) => snapshot
                    .routes
                    .get(index)
                    .map(|route| route.upstream.as_str()),
                _ => None,
            };
            AccessRecord {
                request_id: ctx.request_id.as_str(),
                method: request.method.as_str(),
                host,
                path: request.uri.path(),
                status,
                duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                upstream: upstream_name,
                endpoint: ctx.endpoint_addr.map(|addr| addr.to_string()),
                client: session.client_addr().map(|addr| addr.to_string()),
            }
        };
        info!(target: "runnelon::access", "{}", record.to_json());

        self.metrics.request_finished(status, elapsed);
    }

    fn fail_to_connect(
        &self,
        session: &mut Session,
        _peer: &HttpPeer,
        ctx: &mut Self::CTX,
        mut e: Box<PingoraError>,
    ) -> Box<PingoraError> {
        let Some(snapshot) = ctx.snapshot.as_ref() else {
            e.set_retry(false);
            return e;
        };
        let Some(route) = ctx.route.and_then(|index| snapshot.routes.get(index)) else {
            e.set_retry(false);
            return e;
        };
        let Some(upstream) = snapshot.upstreams.get(&route.upstream) else {
            e.set_retry(false);
            return e;
        };

        if let Some(endpoint) = ctx.endpoint {
            upstream.mark_unhealthy(endpoint);
            if let Some(addr) = upstream.endpoints.get(endpoint) {
                warn!(
                    "upstream '{}' endpoint {addr} failed to connect; pausing it for the configured cooldown",
                    route.upstream
                );
            }
        }

        let idempotent = is_idempotent_method(session.req_header().method.as_str());
        let retry = idempotent
            && ctx.tried.len() < MAX_ENDPOINT_ATTEMPTS
            && upstream.has_endpoint_avoiding(&ctx.tried);
        if retry {
            warn!(
                "retrying idempotent request on another endpoint (attempt {} of {MAX_ENDPOINT_ATTEMPTS})",
                ctx.tried.len() + 1
            );
        }
        e.set_retry(retry);
        e
    }

    async fn fail_to_proxy(
        &self,
        session: &mut Session,
        error: &PingoraError,
        _ctx: &mut Self::CTX,
    ) -> FailToProxy
    where
        Self::CTX: Send + Sync,
    {
        // Mirrors Pingora's default error mapping, except that upstream read and
        // write timeouts are reported as 504 Gateway Timeout instead of 502.
        let code = match error.etype() {
            ErrorType::HTTPStatus(code) => *code,
            _ => match error.esource() {
                ErrorSource::Upstream => match error.etype() {
                    ErrorType::ReadTimedout | ErrorType::WriteTimedout => 504,
                    _ => 502,
                },
                ErrorSource::Downstream => match error.etype() {
                    ErrorType::WriteError | ErrorType::ReadError | ErrorType::ConnectionClosed => 0,
                    _ => 400,
                },
                ErrorSource::Internal | ErrorSource::Unset => 500,
            },
        };

        if code > 0 {
            if let Err(err) = session.respond_error(code).await {
                error!("failed to send error response to downstream: {err}");
            }
        }

        FailToProxy {
            error_code: code,
            can_reuse_downstream: false,
        }
    }
}

/// Read a valid `Content-Length` declaration, if present.
fn declared_content_length(request: &RequestHeader) -> Option<u64> {
    request
        .headers
        .get("content-length")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Set essential reverse-proxy forwarding headers.
///
/// `X-Forwarded-Proto` and `X-Forwarded-Host` always reflect what Runnelon
/// observed. `X-Forwarded-For` behaviour depends on the peer:
/// - untrusted peer: inbound chain is discarded, only the peer address is kept;
/// - trusted peer (`trusted_proxies`): inbound chain is preserved and the peer
///   is appended, matching conventional multi-hop proxy semantics.
fn apply_forwarding_headers(
    session: &Session,
    upstream_request: &mut RequestHeader,
    trusted_proxies: &[IpNet],
    scheme: &'static str,
) {
    let client_addr = session.client_addr();
    let client_ip = match client_addr {
        Some(PingoraSockAddr::Inet(sa)) => Some(sa.ip()),
        _ => None,
    };
    let client_label = client_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| match client_addr {
            Some(other) => other.to_string(),
            None => "unknown".to_string(),
        });

    let trusted = client_ip.is_some_and(|ip| trusted_proxies.iter().any(|net| net.contains(ip)));
    let inbound_chain = if trusted {
        upstream_request
            .headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
    } else {
        None
    };

    match inbound_chain {
        Some(existing) => {
            let combined = format!("{existing}, {client_label}");
            let _ = upstream_request.insert_header("X-Forwarded-For", combined);
        }
        None => {
            let _ = upstream_request.insert_header("X-Forwarded-For", client_label.as_str());
        }
    }

    let _ = upstream_request.insert_header("X-Forwarded-Proto", scheme);

    if let Some(host) = session.req_header().headers.get("host") {
        if let Ok(host) = host.to_str() {
            let _ = upstream_request.insert_header("X-Forwarded-Host", host);
        }
    }
}
