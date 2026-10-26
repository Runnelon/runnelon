//! Structured access-log records.
//!
//! Only routing metadata is logged: method, host, path (no query string),
//! status, latency, selected upstream/endpoint, client address and request ID.
//! Headers, cookies, bodies and query strings are deliberately absent so
//! credentials cannot leak through access logs.

use serde::Serialize;

/// One access-log line.
#[derive(Debug, Serialize)]
pub struct AccessRecord<'a> {
    /// Per-process request identifier (hex).
    pub request_id: &'a str,
    /// HTTP method.
    pub method: &'a str,
    /// Request host header (as received).
    pub host: &'a str,
    /// Canonicalized request path without query string.
    pub path: &'a str,
    /// Response status, when one was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// Total proxy time in milliseconds.
    pub duration_ms: u64,
    /// Upstream pool name, when selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<&'a str>,
    /// Upstream endpoint, when selected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Client address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

impl AccessRecord<'_> {
    /// Render a single-line JSON object.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| "{\"error\":\"log serialization\"}".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_expected_fields_without_secrets() {
        let record = AccessRecord {
            request_id: "0000000000000007",
            method: "GET",
            host: "app.local",
            path: "/api/users",
            status: Some(200),
            duration_ms: 12,
            upstream: Some("api"),
            endpoint: Some("127.0.0.1:3001".to_string()),
            client: Some("127.0.0.1".to_string()),
        };
        let line = record.to_json();
        assert!(line.starts_with('{') && line.ends_with('}'), "{line}");
        for expected in [
            "\"request_id\":\"0000000000000007\"",
            "\"method\":\"GET\"",
            "\"path\":\"/api/users\"",
            "\"status\":200",
            "\"duration_ms\":12",
            "\"upstream\":\"api\"",
        ] {
            assert!(line.contains(expected), "missing {expected}: {line}");
        }
        assert!(!line.contains("authorization") && !line.contains("cookie"));
        assert!(
            !line.contains('?'),
            "query strings must not be logged: {line}"
        );
    }

    #[test]
    fn omits_unset_optional_fields() {
        let record = AccessRecord {
            request_id: "0000000000000001",
            method: "GET",
            host: "app.local",
            path: "/missing",
            status: None,
            duration_ms: 1,
            upstream: None,
            endpoint: None,
            client: None,
        };
        let line = record.to_json();
        assert!(!line.contains("status"), "{line}");
        assert!(!line.contains("endpoint"), "{line}");
    }
}
