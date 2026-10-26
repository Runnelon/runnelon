# Runnelon Architecture (proposed)

## Goals
- Predictable low-latency HTTP reverse proxy built on Pingora.
- Simple static configuration first; safe dynamic updates later.
- Docker-native operation without requiring Docker access on the request path.
- Observable and secure by default.

## Logical components
```text
               HTTP clients
                    |
          [listener / TLS]
                    |
        [Pingora request pipeline]
                    |
       [immutable routing snapshot] ------> [upstream pools] ---> services
                    ^
                    | atomic publication
             [config compiler]
                    ^
             [validation layer]
                    ^
          [file / Docker providers]

     [metrics, readiness, admin]  (isolated management surface)
```

## Data plane
Pingora owns HTTP connection handling. Runnelon supplies routing policy, upstream selection, and request lifecycle hooks. A request reads one consistent compiled configuration snapshot; no provider I/O occurs in the hot path. Define forwarding-header trust boundaries and request normalization explicitly.

Hot-path properties (verified by code review and tests, not benchmark claims): snapshot access is a lock-free `ArcSwap` load; request host and path normalization borrow the request unless case-folding or escape decoding is required; route matching returns a borrowed upstream name and a route index instead of cloning strings; access logging allocates one JSON line only when `INFO` is enabled; upstream selection uses atomics with no mutex; config, Docker, and log/reload work never runs on request threads.

## Control plane
Providers generate candidate configuration. Parse, validate, compile route tables and upstreams, then atomically publish a new immutable snapshot. On any error, reject the candidate and retain last-known-good state. Docker watch must recover from missed events via periodic reconciliation.

M2 implementation: a dedicated thread polls the config file every 500 ms. Valid candidates are published through an `ArcSwap`, so requests load a snapshot lock-free and keep one consistent view for their entire lifetime. Listener and management-address changes are rejected (a restart is required); routes, upstreams, limits and trusted proxies reload. Replacing the snapshot resets per-upstream selection counters and passive-health state. Event-based watching (inotify/Docker) is not implemented.

### Docker discovery (M2)
A second thread polls `GET /containers/json` on the Docker Unix socket (read-only) every `docker.poll_interval_ms`. Containers opt in with labels:

| Label | Required | Meaning |
|---|---|---|
| `runnelon.upstream` | yes | Upstream pool name shared by replicas |
| `runnelon.host` | yes | Route host, or `*` |
| `runnelon.path_prefix` | no | Path prefix, default `/` |
| `runnelon.port` | no | Container TCP port; otherwise lowest exposed TCP port |
| `runnelon.network` | no | Docker network to take the IP from |

The statically compiled snapshot and the latest Docker state are merged by a single `Publisher` (mutex on writers, lock-free readers). Static configuration wins: conflicting Docker routes or upstream names are skipped. Initial discovery must succeed before listeners bind; later failures keep the last-known-good merged snapshot. Changing `docker` settings requires a restart (ADR 0005).

## Management plane
Readiness/liveness, metrics, and (later) administrative APIs. Default to loopback or separate listener; authentication and authorization required before remote exposure. Management failure must not interrupt existing request forwarding.

M2 implementation: optional `management.address` served by a separate Pingora service with `/healthz` and `/readyz` (`200`), `404` for unknown paths and `405` for non-GET/HEAD. The address must be loopback and must not collide with a proxy listener. Remote admin is unimplemented; metrics are described below.

Metrics implementation: `GET /metrics` renders Prometheus text with `runnelon_requests_total`, `runnelon_responses_total{class="1xx".."5xx"}`, `runnelon_inflight_requests`, and the `runnelon_response_duration_ms` histogram (1 ms … 10 s buckets plus `+Inf`). Only status classes are labelled; request paths, hosts and client addresses are never exported. Metrics are process-local and reset on restart.

### Access logs and request IDs
Every completed proxied request emits one JSON line on log target `runnelon::access` at `INFO` (ADR 0006): `request_id`, `method`, `host`, `path` (no query), `status`, `duration_ms`, `upstream`, `endpoint`, `client`. Headers, cookies, bodies and query strings are never logged. IDs are per-process counters (16 hex chars), forwarded upstream as `X-Request-Id` and returned to clients on proxied responses; client-supplied values are replaced. Locally generated `400`/`404`/`413` responses are logged with an ID but may not carry the response header.

## Routing contract (M1)
- Match by `Host` (exact, case-insensitive; `*` = any) and path-prefix.
- Precedence: longer `path_prefix` wins; equal length → earlier declaration order.
- Path prefixes are segment-boundary aware (`/api` matches `/api` and `/api/x`, not `/apiv2`).
- Request paths are canonicalized before matching: percent-decoded once, then RFC 3986 dot-segment removal. Encoded `/`, `\`, control bytes, malformed escapes, non-UTF-8 results and nested `%25XX` encodings are rejected with `400`. Leading `..` cannot escape `/`. Upstreams receive the original request-target unchanged; canonicalization only selects the route.
- Request `Host` is normalized by lowercasing and stripping a numeric `:port` suffix.
- Config `host` must not include a port. Unknown fields are rejected.
- Forwarding headers (M1): `X-Forwarded-For` contains the observed peer address. It is replaced for untrusted peers; when the peer is inside `trusted_proxies`, the inbound chain is preserved and the peer appended. `X-Forwarded-Proto` is `https` on TLS listeners and `http` otherwise; `X-Forwarded-Host` is the request `Host`. Trusted status does not change proto/host. See ADR 0003 and ADR 0007.
- HTTP/1.1 `Upgrade` (WebSocket) requests are tunneled after `101 Switching Protocols`; validated end-to-end with a 101 handshake and byte passthrough. There is no upgrade-specific configuration or frame awareness.
- Upstream selection: weighted round-robin over a schedule compiled at load time. Each endpoint defaults to weight 1; `weights` may raise it, but the total schedule is capped at 1024 slots. Connect failures park an endpoint for `unhealthy_cooldown_ms` (default 5000 ms; `0` disables). If every endpoint is cooling down, selection fails open to the next schedule slot. Idempotent requests retry once on connect failure against a different endpoint (ADR 0004); no active probes.

## Configuration contract (schema version 1)
Working format (see `examples/runnelon.yaml`):
```yaml
version: 1
listeners:
  - address: '127.0.0.1:8080'
routes:
  - host: 'example.local'
    path_prefix: '/'
    upstream: 'app'
upstreams:
  app:
    endpoints:
      - '127.0.0.1:3000'
    connect_timeout_ms: 5000
    read_timeout_ms: 30000
    write_timeout_ms: 30000
```
- `version` must be `1`.
- Timeouts default to connect 5000 / read 30000 / write 30000 ms when omitted.
- `serde` `deny_unknown_fields` applies to all objects.
- Duplicate listeners, duplicate endpoints, and duplicate `host` + `path_prefix` routes are rejected (hosts compared after normalization).
- `path_prefix` must be a decoded path: whitespace, control characters, `%`, `?` and `#` are rejected because they cannot match a request path.
- `limits.max_request_body_bytes` defaults to 1 MiB and must be greater than zero. A declared `Content-Length` above the limit is rejected with `413` before upstream selection; streamed bodies are cut off with `413` once the limit is exceeded.
- `trusted_proxies` is an optional list of IPv4/IPv6 CIDRs (bare address = host route). Empty by default. Only peers inside these networks may supply an `X-Forwarded-For` chain; invalid or duplicate networks are rejected.
- `weights` maps an exact endpoint string to a positive integer (default 1). Unknown endpoint keys, zero weights, and totals above 1024 are rejected.
- `unhealthy_cooldown_ms` defaults to 5000 and may be set to 0 to disable passive health marking.
- `management.address` is optional, must parse as a loopback socket address, and must not duplicate a proxy listener. It serves only `/healthz` and `/readyz`.
- Listener `tls` is optional; `cert_path` and `key_path` must be readable PEM files. TLS listeners use Pingora's intermediate defaults and set `X-Forwarded-Proto: https`.
- `docker.socket` defaults to `/var/run/docker.sock`; `docker.poll_interval_ms` defaults to 2000 and must be greater than zero. With `docker` set, `routes`/`upstreams` may be empty. Static routes/upstreams take precedence over discovered ones.
- Invalid config fails process startup; nothing is published to the data plane.

## Failure model
- Invalid config: reject update; retain last-known-good.
- Unhealthy upstream: one bounded retry for idempotent methods (`GET`, `HEAD`, `OPTIONS`, `TRACE`, `PUT`, `DELETE`) on connect failure, using a different endpoint; `POST`/`PATCH` are never retried. A connect failure parks the endpoint for `unhealthy_cooldown_ms`, after which it is eligible again (first request acts as the probe). Deterministic error response.
- Error responses (M1): `400` for an ambiguous or malformed request path, `404` when no route matches, `502` for upstream connect/response failures, `504` for upstream read/write timeouts.
- Early upstream connect failures can reset the downstream connection while a request body is still in flight; clients may observe a dropped connection instead of `502` on that race. Empty-body requests and GETs are unaffected.
- Provider unavailable: existing data-plane snapshot continues to serve.
- Graceful shutdown: stop accepting new requests; drain with deadline.
- OOM/FD exhaustion: protect with resource limits and monitoring; never promise immunity.

## Security boundaries
Untrusted clients, upstream applications, Docker socket, config files, admin endpoints, and CI artifacts are distinct trust domains. See `docs/THREAT_MODEL.md`.

## Performance methodology
Measure throughput, latency distribution, error rate, memory, CPU, connection reuse, and configuration reload cost with equivalent workloads. See `docs/BENCHMARKS.md`.
