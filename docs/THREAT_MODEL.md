# Threat Model (initial)

## Assets
TLS private keys, routing configuration, upstream credentials, Docker access, admin credentials, traffic confidentiality/integrity, service availability.

## Threats and mitigations
| Threat | Proposed mitigation | Validation |
|---|---|---|
| Request smuggling / ambiguous HTTP parsing | Rely on maintained Pingora parser; enforce safe header semantics | malformed request tests |
| SSRF via config/upstreams | Restrict config writers; validate endpoints; optional network policy | malicious endpoint tests |
| Docker socket compromise | Read-only Engine API (`GET /containers/json`), control-plane only; socket optional and never on the request path; docker-socket-proxy recommended in production | unit + fake-daemon e2e tests; deployment review |
| Admin endpoint exposure | Management listener is opt-in, must bind loopback, and exposes only `/healthz`, `/readyz`, `/metrics`; metrics carry status classes only (no paths/hosts/clients) | config validation tests + e2e probe/metrics tests |
| Slow clients/resource exhaustion | Upstream timeouts and a 1 MiB request-body limit (configurable); concurrency/socket limits still missing | e2e oversized `Content-Length` and chunked-body tests |
| Header spoofing | Inbound `X-Forwarded-*` replaced with observed peer/proto/host; chains preserved only from `trusted_proxies` peers (ADR 0003) | e2e `spoofed_forwarding_headers_are_replaced` and `trusted_proxy_preserves_forwarded_chain` |
| Path confusion / route bypass (`..`, `%2e`, `%2f`) | Canonicalize request path before matching; reject ambiguous encodings with `400` | unit tests (`routing.rs`) and e2e `canonical_path_selects_route_before_upstream` |
| Plaintext interception | Optional per-listener TLS termination (Pingora/OpenSSL intermediate defaults); no HSTS/redirect policy yet | e2e TLS test asserting `200` and `X-Forwarded-Proto: https` |
| Secret leakage | JSON access logs carry method/host/path/status/timing/upstream/client only; headers, cookies, bodies and query strings excluded; client-supplied request IDs replaced | unit tests assert absent secrets; log format review |
| Retry amplification | Bounded retries; idempotency-aware policy | fault injection |
| Malicious config/reload | Validation + atomic snapshot + last-known-good | invalid reload tests |
| Dependency compromise | Lockfile, dependency review, CI checks | supply-chain review |

This document is a starting checklist, not a security certification. Revisit before M3 and each public release.
