# ADR 0001: Use Pingora as proxy foundation

- **Status:** Accepted
- **Date:** 2026-10-26

## Context
Runnelon needs a mature HTTP proxy foundation while retaining Rust-level control over routing and operations.

## Decision
Build the request data plane on Cloudflare Pingora rather than implementing an HTTP server/proxy stack from scratch. Pin **Pingora 0.9.0** (`pingora` crate with the `proxy` feature) in the Cargo lockfile; Apache-2.0 license is compatible with this project.

## Consequences
- Faster path to a functional proxy; reuse mature connection handling.
- Upstream Pingora API and behavior constrain design.
- Project must maintain integration tests across dependency upgrades.

## Alternatives
Hyper-based custom proxy, wrapping NGINX, writing a proxy stack from scratch.
