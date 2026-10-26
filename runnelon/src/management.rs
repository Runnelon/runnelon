//! Loopback-only management service: liveness, readiness, and metrics.

use crate::metrics::Metrics;
use async_trait::async_trait;
use bytes::Bytes;
use pingora::http::ResponseHeader;
use pingora::proxy::{ProxyHttp, Session};
use pingora::upstreams::peer::HttpPeer;
use pingora::Result;
use std::sync::Arc;

/// Serves `/healthz`, `/readyz` and `/metrics`.
///
/// Probes return `200` once the process is accepting connections: configuration
/// is validated and published before any listener binds. Unknown paths return
/// `404`; non-GET/HEAD methods return `405`. `/metrics` is GET-only and renders
/// Prometheus text.
pub struct ManagementService {
    metrics: Arc<Metrics>,
}

impl ManagementService {
    /// Create a management service exposing `metrics`.
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self { metrics }
    }
}

enum Action {
    Metrics,
    Status(u16),
}

#[async_trait]
impl ProxyHttp for ManagementService {
    type CTX = ();

    fn new_ctx(&self) {}

    async fn request_filter(&self, session: &mut Session, _ctx: &mut Self::CTX) -> Result<bool>
    where
        Self::CTX: Send + Sync,
    {
        let action = {
            let request = session.req_header();
            match (request.method.as_str(), request.uri.path()) {
                ("GET", "/metrics") => Action::Metrics,
                ("GET" | "HEAD", "/healthz" | "/readyz") => Action::Status(200),
                ("GET" | "HEAD", _) => Action::Status(404),
                _ => Action::Status(405),
            }
        };

        match action {
            Action::Metrics => {
                let body = self.metrics.render();
                let mut header = ResponseHeader::build(200, Some(3))?;
                header.insert_header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")?;
                header.insert_header("Content-Length", body.len().to_string())?;
                header.insert_header("Cache-Control", "no-store")?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some(Bytes::from(body)), true)
                    .await?;
            }
            Action::Status(status) => session.respond_error(status).await?,
        }

        Ok(true)
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        Err(pingora::Error::new_str(
            "management service does not proxy to an upstream",
        ))
    }
}
