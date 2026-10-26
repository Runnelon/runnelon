# Runnelon Roadmap

> Planning document, not a promise of release dates. Status legend: `planned`, `in progress`, `done`, `blocked`. A milestone is **done only after its exit criteria are demonstrated**.

## North star
A trustworthy, high-performance Rust reverse proxy with an excellent container-native experience. Performance is measured against NGINX and Traefik under equivalent conditions; no performance ranking is assumed.

## M0 — Repository & developer foundation (`done`)
**Deliverables**
- [x] Public repository with Apache-2.0 license and contributor/security policies
- [x] Working Rust binary (`cargo run`) and reproducible build
- [x] CI: format, Clippy, unit tests, build
- [x] Basic CLI help/version and documented development setup
- [x] Architecture and initial ADRs

**Exit criteria:** clean clone builds/tests on supported platform; CI passes; docs describe what works today.

**Evidence:** `runnelon` crate CLI (`--help` / `--version`), workspace `Cargo.lock`, `.github/workflows/ci.yml`, and local `cargo fmt` / `clippy` / `test` / `build` checks.

## M1 — Static HTTP reverse proxy MVP (`done`)
**Deliverables**
- [x] Pingora-backed HTTP listener and upstream forwarding
- [x] Static YAML config parsing, schema validation, actionable errors
- [x] Host and path-prefix route matching with deterministic precedence
- [x] Request-path canonicalization (dot segments, percent-encoding) with `400` on ambiguous encodings
- [x] One or more upstream endpoints, defined timeout behavior
- [x] HTTP request/response streaming and essential proxy header handling
- [x] Unit + integration tests using local mock upstreams

**Exit criteria:** two backends routed correctly; malformed config fails safely; request failures/timeouts tested; no unbounded hot-path work.

**Evidence:** `runnelon --config` with schema v1; unit tests for config/routing (including duplicate and unmatchable-prefix rejection); `tests/proxy_e2e.rs` (two-backend routing, 404, 502 on connect failure, 504 read timeout with no retry, body streaming, path canonicalization, forwarding-header sanitization); Pingora `0.9.0` pinned in `Cargo.lock`.

## M2 — Resilience & dynamic routing (`done`)
**Deliverables**
- [x] Weighted upstream selection (documented semantics)
- [x] Round-robin selection across configured endpoints
- [x] Passive connect-failure health checks with cooldown recovery (no active probes)
- [x] One bounded retry for idempotent connect failures (ADR 0004)
- [x] Atomic reload and last-known-good snapshot retention (polling; listeners require restart)
- [x] Docker discovery with least-privilege guidance (read-only API, polling reconciliation; events deferred, ADR 0005)
- [x] HTTP/1.1 upgrade (WebSocket) tunneling validated (`101` + byte passthrough)
- [x] Load/reload concurrency tests (no failed requests while configs cycle)

**Exit criteria:** routing continues through invalid reload; backend changes converge predictably; discovery failure cannot crash data plane.

**Evidence:** `tests/proxy_e2e.rs` covers weighted/round-robin fan-out, passive health parking, idempotent retry vs non-idempotent no-retry, invalid/valid reload under concurrent load, Docker label discovery + endpoint churn + disappearing routes + failing daemon keeping last-known-good, and upgrade tunneling. `config`/`docker`/`publisher`/`reload` unit tests cover merge precedence and label parsing. Pingora `0.9.0`; no unsupported API used.

## M3 — Security & operations (`in progress`)
**Deliverables**
- [x] TLS termination per listener with Pingora/OpenSSL defaults (ADR 0007)
- [ ] Certificate reload/rotation policy, SNI multi-cert, HSTS/redirect policy
- [x] Trusted-proxy `X-Forwarded-For` handling with explicit CIDR allowlist (ADR 0003)
- [x] Request metrics: counts, status classes, latency histogram, in-flight gauge (`GET /metrics`)
- [ ] Connection-level metrics (accepted/open connections, pool reuse) and metrics hardening review
- [x] Structured JSON access logs with request IDs and secret-free fields (ADR 0006)
- [x] Liveness/readiness endpoints (opt-in, loopback-only; metrics pending)
- [ ] Resource limits, abuse tests, and documented threat mitigations
- [ ] Container image, example Compose deployment, operational runbook

**Exit criteria:** security checklist passes; incident scenarios and safe startup/shutdown documented; basic soak test succeeds.

## M4 — Public beta & benchmark (`planned`)
**Deliverables**
- [ ] Versioned release artifacts, checksums, SBOM where practical
- [ ] End-to-end quickstart and configuration reference
- [ ] Repeatable comparison with NGINX and Traefik
- [ ] Throughput, p50/p95/p99, errors, CPU, memory, and reload behavior
- [ ] Known limitations and upgrade policy

**Exit criteria:** independent developer can reproduce quickstart and benchmark methodology; all claims trace to results.

## Later / not committed
Kubernetes provider, ACME automation, rate limiting, authentication plugins, gRPC-specific routing, HTTP/3, WASM extensions, distributed control plane. Prioritize based on users and measured operational needs.

## Issue labels (suggested)
`area/proxy`, `area/config`, `area/discovery`, `area/security`, `area/observability`, `area/docs`, `kind/bug`, `kind/feature`, `kind/chore`, `good first issue`, `help wanted`.

## Roadmap maintenance
Update status only with linked PRs/tests. Add ADR for any significant architectural reversal. Avoid calendar commitments until delivery capacity and scope are validated.
