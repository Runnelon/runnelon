//! Host and path-prefix route matching against an immutable snapshot.

use crate::config::{CompiledRoute, HostMatch, Snapshot};
use std::borrow::Cow;
use thiserror::Error;

/// Result of a successful route match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchResult<'a> {
    /// Upstream name to use (borrowed from the snapshot).
    pub upstream: &'a str,
    /// Matched route declaration order (for debugging/tests).
    pub route_order: usize,
    /// Index into [`Snapshot::routes`] (for per-request state).
    pub route_index: usize,
}

/// Errors returned when a request-target cannot be canonicalized safely.
///
/// Every variant maps to a client error (400) and must be raised before any
/// upstream is contacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PathError {
    /// Request-target is not origin-form (`/...`).
    #[error("request path must start with '/'")]
    NotOriginForm,
    /// Raw control byte in the path.
    #[error("request path contains a control character")]
    ControlCharacter,
    /// Raw backslash in the path.
    #[error("request path contains a backslash")]
    Backslash,
    /// Malformed `%XX` escape.
    #[error("request path contains an invalid percent-encoding")]
    InvalidEscape,
    /// Percent-decoding produced bytes that are not valid UTF-8.
    #[error("request path is not valid UTF-8 after percent-decoding")]
    NonUtf8,
    /// Encoded delimiter that would change path structure (`%2F`, `%5C`).
    #[error("request path contains an ambiguous percent-encoded delimiter")]
    AmbiguousDelimiter,
    /// Control byte produced by percent-decoding.
    #[error("request path contains a percent-encoded control character")]
    EncodedControlCharacter,
    /// Double-encoded path (`%25XX` survives one decode).
    #[error("request path contains nested percent-encoding")]
    NestedEncoding,
}

/// Canonicalize a request path for route matching.
///
/// Rules:
/// 1. The path must be origin-form (`/...`); raw backslashes and control bytes
///    are rejected.
/// 2. Percent-encoding is decoded exactly once. Encoded `/`, `\`, control bytes,
///    malformed escapes, non-UTF-8 results and nested (`%25XX`) encodings are
///    rejected because different servers would interpret them differently.
/// 3. Dot segments (`.`/`..`) are removed per RFC 3986 §5.2.4; leading `..`
///    cannot escape the root.
///
/// Canonicalization is for route selection only: the upstream still receives
/// the original request-target, so proxy HTTP semantics are unchanged.
pub fn normalize_path(raw: &str) -> Result<Cow<'_, str>, PathError> {
    if !raw.starts_with('/') {
        return Err(PathError::NotOriginForm);
    }

    let mut needs_work = false;
    for &byte in raw.as_bytes() {
        match byte {
            b'\\' => return Err(PathError::Backslash),
            0x00..=0x1f | 0x7f => return Err(PathError::ControlCharacter),
            b'%' | b'.' => needs_work = true,
            _ => {}
        }
    }
    if !needs_work {
        return Ok(Cow::Borrowed(raw));
    }

    let decoded = percent_decode(raw)?;
    let normalized = remove_dot_segments(&decoded);
    Ok(Cow::Owned(normalized))
}

/// Decode `%XX` escapes once, rejecting ambiguous or invalid encodings.
fn percent_decode(raw: &str) -> Result<String, PathError> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte != b'%' {
            decoded.push(byte);
            idx += 1;
            continue;
        }
        let (high, low) = match (bytes.get(idx + 1), bytes.get(idx + 2)) {
            (Some(high), Some(low)) => (hex_value(*high)?, hex_value(*low)?),
            _ => return Err(PathError::InvalidEscape),
        };
        let value = (high << 4) | low;
        match value {
            b'/' | b'\\' => return Err(PathError::AmbiguousDelimiter),
            0x00..=0x1f | 0x7f => return Err(PathError::EncodedControlCharacter),
            _ => decoded.push(value),
        }
        idx += 3;
    }

    let decoded = String::from_utf8(decoded).map_err(|_| PathError::NonUtf8)?;
    // A remaining `%XX` after one decode would be decoded again by some
    // upstreams, which makes the effective path ambiguous.
    let decoded_bytes = decoded.as_bytes();
    for idx in 0..decoded_bytes.len() {
        if decoded_bytes[idx] == b'%'
            && decoded_bytes
                .get(idx + 1)
                .is_some_and(|byte| hex_value(*byte).is_ok())
            && decoded_bytes
                .get(idx + 2)
                .is_some_and(|byte| hex_value(*byte).is_ok())
        {
            return Err(PathError::NestedEncoding);
        }
    }
    Ok(decoded)
}

fn hex_value(byte: u8) -> Result<u8, PathError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(PathError::InvalidEscape),
    }
}

/// Remove `.` and `..` segments per RFC 3986 §5.2.4.
fn remove_dot_segments(path: &str) -> String {
    let mut output = String::with_capacity(path.len());
    let mut rest = path;

    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix("../") {
            rest = stripped;
        } else if let Some(stripped) = rest.strip_prefix("./") {
            rest = stripped;
        } else if rest.starts_with("/./") {
            rest = &rest[2..];
        } else if rest == "/." {
            rest = "/";
        } else if rest.starts_with("/../") {
            rest = &rest[3..];
            remove_last_segment(&mut output);
        } else if rest == "/.." {
            rest = "/";
            remove_last_segment(&mut output);
        } else if rest == "." || rest == ".." {
            rest = "";
        } else {
            let start = usize::from(rest.starts_with('/'));
            let end = rest[start..]
                .find('/')
                .map_or(rest.len(), |offset| offset + start);
            output.push_str(&rest[..end]);
            rest = &rest[end..];
        }
    }

    output
}

fn remove_last_segment(output: &mut String) {
    match output.rfind('/') {
        Some(idx) => output.truncate(idx),
        None => output.clear(),
    }
}

/// Match `host` and `path` against the snapshot's compiled route table.
///
/// `path` must already be canonicalized with [`normalize_path`]; callers that
/// skip it can be confused by dot segments or encoded delimiters.
///
/// Precedence (applied at compile time, then scanned in order):
/// 1. Longer `path_prefix` wins.
/// 2. Equal path length → earlier declaration order wins.
/// 3. Host must match (`*` or exact normalized host).
///
/// Host normalization: lowercase; strip a trailing `:port` from the request Host.
pub fn match_route<'a>(snapshot: &'a Snapshot, host: &str, path: &str) -> Option<MatchResult<'a>> {
    let normalized_host = normalize_request_host(host);
    for (route_index, route) in snapshot.routes.iter().enumerate() {
        if !host_matches(&route.host, &normalized_host) {
            continue;
        }
        if !path_matches(&route.path_prefix, path) {
            continue;
        }
        return Some(MatchResult {
            upstream: route.upstream.as_str(),
            route_order: route.order,
            route_index,
        });
    }
    None
}

fn host_matches(expected: &HostMatch, request_host: &str) -> bool {
    match expected {
        HostMatch::Any => true,
        HostMatch::Exact(h) => h == request_host,
    }
}

fn path_matches(prefix: &str, path: &str) -> bool {
    if prefix == "/" {
        return path.starts_with('/');
    }
    // Boundary-aware prefix: `/api` matches `/api` and `/api/x`, not `/apiv2`.
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('/'),
        None => false,
    }
}

/// Normalize a request `Host` header value for matching.
///
/// Borrows the input when it is already lowercase and port-free, which is the
/// common case; only allocates for uppercase hosts.
pub fn normalize_request_host(host: &str) -> Cow<'_, str> {
    let host = host.trim();
    // Strip port: `example.com:8080` → `example.com`. Bracketed IPv6 not supported in M1.
    let without_port = match host.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    if without_port.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(without_port.to_ascii_lowercase())
    } else {
        Cow::Borrowed(without_port)
    }
}

/// Expose compiled routes for tests.
pub fn routes(snapshot: &Snapshot) -> &[CompiledRoute] {
    &snapshot.routes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_snapshot_from_str;

    fn snap() -> Snapshot {
        load_snapshot_from_str(
            r#"
version: 1
listeners:
  - address: "127.0.0.1:8080"
routes:
  - host: "app.local"
    path_prefix: "/api/v1"
    upstream: "api"
  - host: "app.local"
    path_prefix: "/api"
    upstream: "api-legacy"
  - host: "other.local"
    path_prefix: "/"
    upstream: "other"
  - host: "*"
    path_prefix: "/"
    upstream: "default"
upstreams:
  api:
    endpoints: ["127.0.0.1:1"]
  api-legacy:
    endpoints: ["127.0.0.1:2"]
  other:
    endpoints: ["127.0.0.1:3"]
  default:
    endpoints: ["127.0.0.1:4"]
"#,
        )
        .unwrap()
    }

    #[test]
    fn longest_prefix_wins() {
        let s = snap();
        let m = match_route(&s, "app.local", "/api/v1/users").unwrap();
        assert_eq!(m.upstream, "api");
        let m = match_route(&s, "app.local", "/api/users").unwrap();
        assert_eq!(m.upstream, "api-legacy");
    }

    #[test]
    fn host_is_case_insensitive_and_port_stripped() {
        let s = snap();
        let m = match_route(&s, "APP.LOCAL:8080", "/api/v1").unwrap();
        assert_eq!(m.upstream, "api");
    }

    #[test]
    fn wildcard_host_is_fallback() {
        let s = snap();
        let m = match_route(&s, "unknown.example", "/anything").unwrap();
        assert_eq!(m.upstream, "default");
        let m = match_route(&s, "other.local", "/x").unwrap();
        assert_eq!(m.upstream, "other");
    }

    #[test]
    fn normalize_request_host_examples() {
        assert_eq!(
            normalize_request_host("ExAmPle.COM").as_ref(),
            "example.com"
        );
        assert_eq!(
            normalize_request_host("example.com:443").as_ref(),
            "example.com"
        );
        assert_eq!(
            normalize_request_host("  example.com  ").as_ref(),
            "example.com"
        );
        assert!(matches!(
            normalize_request_host("example.com:8080"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            normalize_request_host("Example.com"),
            Cow::Owned(_)
        ));
    }

    #[test]
    fn path_prefix_respects_segment_boundary() {
        let s = snap();
        assert_eq!(
            match_route(&s, "app.local", "/apiv2").unwrap().upstream,
            "default"
        );
        assert_eq!(
            match_route(&s, "app.local", "/api").unwrap().upstream,
            "api-legacy"
        );
        assert_eq!(
            match_route(&s, "app.local", "/api/").unwrap().upstream,
            "api-legacy"
        );
    }

    fn norm(path: &str) -> String {
        normalize_path(path)
            .unwrap_or_else(|err| panic!("{path} should be valid: {err}"))
            .into_owned()
    }

    #[test]
    fn normalize_path_removes_dot_segments() {
        assert_eq!(norm("/"), "/");
        assert_eq!(norm("/api"), "/api");
        assert_eq!(norm("/api/./users"), "/api/users");
        assert_eq!(norm("/api/../admin"), "/admin");
        assert_eq!(norm("/api/.."), "/");
        assert_eq!(norm("/api/."), "/api/");
        assert_eq!(norm("/a//b"), "/a//b");
        assert_eq!(norm("/../secret"), "/secret");
        assert_eq!(norm("/a/../../b"), "/b");
    }

    #[test]
    fn normalize_path_decodes_escapes_for_matching() {
        assert_eq!(norm("/%61pi"), "/api");
        assert_eq!(norm("/api/%2e%2e/admin"), "/admin");
        assert_eq!(norm("/api/%2E/users"), "/api/users");
        assert_eq!(norm("/caf%C3%A9"), "/caf\u{e9}");
    }

    #[test]
    fn normalize_path_rejects_ambiguous_input() {
        for (path, expected) in [
            ("api", PathError::NotOriginForm),
            ("/a\\b", PathError::Backslash),
            ("/a\u{1}b", PathError::ControlCharacter),
            ("/a%2Fb", PathError::AmbiguousDelimiter),
            ("/a%5cb", PathError::AmbiguousDelimiter),
            ("/a%zz", PathError::InvalidEscape),
            ("/a%2", PathError::InvalidEscape),
            ("/a%00b", PathError::EncodedControlCharacter),
            ("/a%FF", PathError::NonUtf8),
            ("/a%252fb", PathError::NestedEncoding),
        ] {
            assert_eq!(normalize_path(path), Err(expected), "{path}");
        }
    }
}
