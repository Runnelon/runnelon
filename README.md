# Runnelon

**Traffic flows better here.**

Runnelon is an early-stage open-source reverse proxy and API gateway project built in Rust, with Cloudflare Pingora as its proxy foundation.

> **Status:** Early development (M2 complete, M3 in progress). Not production ready. Feature set and configuration format may still change.

## What works today
- Pingora-backed static HTTP reverse proxy
- YAML config (`version: 1`): listeners, host + path-prefix routes, upstreams with timeouts
- Deterministic route precedence (longer path prefix, then declaration order)
- Weighted round-robin upstream selection with passive connect-failure cooldown (`weights`, `unhealthy_cooldown_ms`)
- One bounded retry for idempotent requests on connect failure
- Loopback-only management listener with `/healthz`, `/readyz`, and Prometheus `/metrics` (opt-in)
- Atomic config reload from the same file (polled every 500 ms); invalid updates keep last-known-good
- HTTP/1.1 upgrade/WebSocket tunneling (101 handshake + byte passthrough verified)
- Docker discovery from container labels (read-only socket, polling reconciliation, static config wins conflicts)
- Structured JSON access logs with request IDs (`X-Request-Id` to upstream and client)
- Per-listener TLS termination (PEM cert/key, `X-Forwarded-Proto: https`; restart required for rotation)
- Request-path canonicalization before routing (dot segments, percent-encoding) with `400` on ambiguous encodings
- Deterministic error responses: `400` malformed path, `404` no route, `502` connect failure, `504` upstream timeout
- Request-body limit: 1 MiB by default, configurable, `413` when exceeded (declared length rejected before upstream contact)
- Essential forwarding headers: `X-Forwarded-For`, `X-Forwarded-Proto`, `X-Forwarded-Host` (inbound values replaced unless the peer is listed in `trusted_proxies`)
- CLI `--help` / `--version` / `--config <file>`
- Unit + integration tests (including two-backend routing)
- Local checks and GitHub Actions CI

## What does not work yet
- Certificate rotation/SNI multi-cert, active health checks, HSTS/redirect policy
- Listener and management-address changes still require a restart
- Production packaging and published comparative benchmarks

See [ROADMAP.md](ROADMAP.md) for milestones.

## Prerequisites
- Stable Rust toolchain with `rustfmt` and `clippy` (see `rust-toolchain.toml`)
- Install via [rustup](https://rustup.rs/) if needed

## Quick start
```bash
git clone <repository-url> runnelon
cd runnelon

# Terminal A: a simple upstream (example)
python3 -m http.server 3000 --bind 127.0.0.1

# Terminal B: start Runnelon with the example config
cargo run -p runnelon -- --config examples/runnelon.yaml

# Terminal C: send a request (Host must match a route)
curl -s -H 'Host: app.local' http://127.0.0.1:8080/
```

Adjust upstream addresses in `examples/runnelon.yaml` to match your backends.

## Configuration (schema version 1)
```yaml
version: 1
listeners:
  - address: "127.0.0.1:8080"
  - address: "127.0.0.1:8443"       # optional TLS listener
    tls:
      cert_path: "/etc/runnelon/tls/cert.pem"
      key_path: "/etc/runnelon/tls/key.pem"
routes:
  - host: "app.local"    # or "*" for any host
    path_prefix: "/api"  # longer prefix wins; segment-boundary aware
    upstream: "api"
upstreams:
  api:
    endpoints:
      - "127.0.0.1:3001"
      - "127.0.0.1:3002"
    weights: {"127.0.0.1:3001": 3, "127.0.0.1:3002": 1}   # optional, default 1
    connect_timeout_ms: 5000   # optional, default 5000
    read_timeout_ms: 30000     # optional, default 30000
    write_timeout_ms: 30000    # optional, default 30000
    unhealthy_cooldown_ms: 5000   # optional; skip an endpoint after connect failure
limits:
  max_request_body_bytes: 1048576   # optional, default 1048576 (1 MiB)
trusted_proxies: []                 # optional; CIDRs allowed to supply X-Forwarded-For chains
management:
  address: "127.0.0.1:9090"         # optional; must be loopback
docker:
  socket: "/var/run/docker.sock"    # optional; read-only Engine API
  poll_interval_ms: 2000
```

With `docker` configured, `routes` and `upstreams` may be empty; routes come from container labels (`runnelon.upstream`, `runnelon.host`, optional `runnelon.path_prefix`, `runnelon.port`, `runnelon.network`). Static entries win conflicts.

Unknown YAML fields, duplicate listeners/routes/endpoints, and unmatchable `path_prefix` values are rejected. Invalid config fails startup and does not listen. Multiple endpoints are used round-robin per request; optional `weights` change their share, and connect-failed endpoints are skipped for `unhealthy_cooldown_ms` (default 5 s, `0` disables). The optional `management` listener must bind to loopback.

## Development checks
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

## Documentation
- [Roadmap](ROADMAP.md)
- [Agent and engineering instructions](AGENTS.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Threat model](docs/THREAT_MODEL.md)
- [Benchmark plan](docs/BENCHMARKS.md)
- [Benchmark harness](bench/README.md)
- [Architecture decisions](docs/adr/)
- [Contributing](CONTRIBUTING.md)
- [Security](SECURITY.md)

## License
Licensed under the [Apache License, Version 2.0](LICENSE).
