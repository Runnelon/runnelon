# AGENTS.md — Runnelon Engineering Instructions

> Applies to the entire repository. More specific `AGENTS.md` files in subdirectories may refine these instructions. Read `ROADMAP.md`, `docs/ARCHITECTURE.md`, and relevant ADRs before making architectural changes.

## Mission
Build **Runnelon**, an open-source, cloud-native reverse proxy and API gateway in Rust, using Cloudflare Pingora as its proxy foundation. Prioritize correctness, predictable latency, security, and operational simplicity. Do **not** claim to be faster than NGINX or Traefik without reproducible measurements.

## Current status and source of truth
- Project is in early development; do not assume planned capabilities exist.
- `ROADMAP.md` defines milestones and acceptance criteria.
- `docs/ARCHITECTURE.md` describes intended boundaries; ADRs record accepted decisions.
- `Cargo.toml` and actual source code are authoritative for implemented features and dependency versions.
- If documentation conflicts with running code, identify the discrepancy; do not silently invent behavior.

## Non-negotiable rules
1. **Small vertical slices:** implement one observable, testable feature at a time. Avoid speculative abstraction.
2. **Pingora first:** use supported Pingora extension points rather than rewriting its networking core. Confirm API compatibility with the version pinned in Cargo.lock.
3. **Safe configuration:** parse -> validate -> compile -> atomically publish immutable snapshots. Reject invalid updates; preserve last-known-good configuration.
4. **Never block the request path** on Docker API calls, disk I/O, DNS management, or configuration parsing.
5. **Secure defaults:** bind management endpoints to loopback by default; never expose Docker socket, credentials, or internal debug data in logs.
6. **Bound resources:** timeouts, body/header limits, connection limits, retry budgets, and bounded queues must be explicit where applicable.
7. **No silent retries** for non-idempotent requests. Preserve HTTP semantics, including forwarding headers and streaming behavior.
8. **No invented metrics:** benchmarks must document hardware, versions, workload, config, repetitions, and raw results.
9. **No unrelated changes:** avoid large formatting churn, unnecessary dependencies, or broad refactors without a tracked reason.
10. **No premature scope expansion:** Kubernetes, WASM plugins, HTTP/3, and distributed control plane are out of scope until explicitly approved in a roadmap update.

## Expected architecture
- Data plane: Pingora HTTP request processing, route matching, upstream selection, forwarding.
- Control plane: file/Docker providers, validation, compiled snapshots, reload orchestration.
- Management plane: health/readiness, metrics, optional administrative interfaces with strict isolation.
- Prefer a single deployable binary and an internal Cargo workspace when complexity warrants it.
- Keep request-path code independent of discovery/provider implementations.

## Development workflow for coding agents
1. Inspect relevant code, tests, roadmap milestone, and ADRs.
2. Write a short implementation plan listing files to change, risks, and validation steps.
3. Implement the smallest complete slice, including failure paths.
4. Add/update tests for success, malformed input, boundary cases, and regressions.
5. Run the checks below. If unavailable, report precisely what was not run and why.
6. Update user-facing docs and roadmap status **only when evidence supports it**.
7. Summarize changed files, behavior, tests, security implications, and remaining limitations.

## Required local checks
```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```
When the project adds integration/soak/benchmark suites, run the relevant suite for affected behavior; never present a microbenchmark as production evidence.

## Rust standards
- Stable Rust toolchain, edition declared in Cargo manifests; commit Cargo.lock for the binary application.
- Prefer explicit types and errors (`thiserror`/`anyhow` where appropriate), structured tracing, and documented invariants.
- Avoid `unwrap()`/`expect()` in request-handling and configuration paths; test code may use them.
- Avoid `unsafe` unless justified in an ADR and accompanied by safety comments and tests.
- Use `Arc` for shared immutable state; minimize locks in the hot path.
- Never log secrets, authorization headers, session cookies, private keys, or raw sensitive payloads.
- Do not add crates without explaining necessity, maintenance/security implications, and alternatives.

## Definition of done
- Acceptance criteria for the issue are satisfied.
- Automated tests cover normal and failure behavior.
- Formatting, Clippy, tests, and build pass (or limitations explicitly disclosed).
- Docs/examples match actual behavior; no aspirational CLI examples presented as working.
- Security and performance implications reviewed.
- PR is focused, reviewable, and linked to an issue/milestone.

## Agent task prompt template
**Goal:** [specific user-visible outcome]
**Milestone/issue:** [ROADMAP section / issue link]
**Constraints:** [security, compatibility, performance]
**Acceptance criteria:** [observable tests]
**Non-goals:** [explicit exclusions]
**Validation:** [commands + integration scenario]

## Naming and communication
- Product, executable, and primary crate: `runnelon`.
- Configuration example: `runnelon.yaml` (format remains provisional until implemented).
- Write code, comments, and public docs in English; communicate with maintainers in their requested language.
- Do not add generated branding, badges, version numbers, release dates, or feature claims without verification.
