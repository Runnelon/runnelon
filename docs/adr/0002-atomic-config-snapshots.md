# ADR 0002: Immutable compiled configuration snapshots

- **Status:** Accepted (static publish in M1; reload/LKG in M2)
- **Date:** 2026-10-26

## Context
Dynamic providers can change while requests are in flight. Partial updates can cause inconsistent routing.

## Decision
Parse and validate candidate configuration off the request path, compile immutable routing/upstream snapshots, and publish atomically (e.g., via an appropriate Arc-based snapshot mechanism). Keep last-known-good on failure.

### M1 note
Startup loads one validated `Snapshot` into an `Arc` shared by the Pingora service; invalid files fail before listen. Reload was added in M2 (below).

### M2 note
Reload is implemented by polling the config file and publishing a new immutable
snapshot through an `ArcSwap`; requests hold one snapshot for their lifetime.
Listener/management address changes are rejected and invalid candidates keep the
last-known-good snapshot. Per-upstream counters and passive-health state reset
when a new snapshot is published.

## Consequences
- Each request sees a consistent routing view.
- Requires explicit snapshot lifetime and memory management.
- Reload failures remain observable without replacing valid state.
