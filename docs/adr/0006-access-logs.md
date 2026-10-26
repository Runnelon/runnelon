# ADR 0006: Structured access logs and request IDs

- **Status:** Accepted
- **Date:** 2026-10-26

## Context
Operators need per-request triage (latency, status, selected backend) without leaking credentials into log storage. Correlating a client report with proxy and upstream behaviour requires a request identifier.

## Decision
- Emit exactly one JSON access-log line per completed proxied request, on log target `runnelon::access` at `INFO`.
- Fields: `request_id`, `method`, `host`, `path` (query string stripped), `status`, `duration_ms`, `upstream`, `endpoint`, `client`.
- Never log headers, cookies, query strings, request/response bodies, or authorization material.
- Request IDs are per-process monotonic counters rendered as 16 hex characters. The ID is:
  - sent to the upstream as `X-Request-Id`, replacing any client value;
  - returned to the client as `X-Request-Id` on proxied responses.
- Client-supplied `X-Request-Id` values are ignored (no trust, no log injection).
- Locally generated error responses (`400`, `404`, `413`) are logged with an ID; adding the response header to those paths is not guaranteed by the Pingora error writer and is left as a known gap.
- No sampling, rotation, or log shipping configuration is provided; operators control collection via stdout/stderr and `RUST_LOG`.

## Consequences
- Logs are machine-parseable and safe to ship by default.
- IDs are unique per process, not globally; restarts reset the counter.
- One allocation per request for the JSON line and ID formatting.
- Adding fields later is backward compatible for JSON consumers.

## Alternatives
- Accept client-supplied IDs: better cross-system tracing, but allows log injection and collisions.
- UUIDs: global uniqueness without a dependency is not possible today; a counter is sufficient for proxy-local correlation.
- Log at debug: would hide access logs from default `INFO` deployments.
