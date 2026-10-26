# ADR 0005: Docker discovery via read-only Engine API polling

- **Status:** Accepted
- **Date:** 2026-10-26

## Context
Runnelon targets container-native deployments. Operators expect routes to follow containers instead of editing YAML for each service. The roadmap requires Docker discovery with least-privilege guidance and event reconciliation, and the request path must never touch the Docker socket.

## Decision
- Discovery uses the Docker Engine HTTP API over a Unix domain socket, read-only
  (`GET /containers/json`). No `POST`, no exec, no container mutation.
- Discovery runs in a dedicated thread on a polling interval. Events (`/events`)
  and inotify-style streaming are deferred; polling is the reconciliation
  mechanism, so missed events cannot desynchronize state.
- Containers opt in with labels:
  - `runnelon.upstream` (required) — pool name shared by replicas.
  - `runnelon.host` (required) — route host or `*`.
  - `runnelon.path_prefix` (optional, default `/`).
  - `runnelon.port` (optional) — container port; otherwise the lowest exposed TCP port is used.
  - `runnelon.network` (optional) — Docker network to take the IP from; otherwise a deterministically chosen network with an address.
- Multiple containers with the same upstream label form one endpoint pool.
- Static configuration wins conflicts: a discovered route with the same
  `host + path_prefix` as a static route is ignored, and a discovered upstream
  whose name already exists statically is ignored. Conflicts are logged at debug
  level, not treated as fatal.
- The published snapshot is always `merge(static, docker)`. Both the config
  reloader and the Docker poller publish through one `Publisher` guarded by a
  mutex; requests read the result lock-free via `ArcSwap`.
- Initial discovery runs before listeners bind: an unreachable socket fails
  startup. Later failures keep the last-known-good merged snapshot and never
  affect forwarding.

## Consequences
- The socket is only read, only from the control plane; mounting it into the
  proxy container is optional and documented as read-only.
- Convergence latency equals the poll interval (default 2 s).
- Docker-derived upstreams use default timeouts, weights and health cooldown;
  label-level tuning is not implemented.
- A Docker outage freezes routes at the last successful discovery.
- Changing `docker` settings requires a restart, like listener changes.
- Config files may omit `routes`/`upstreams` when `docker` is configured,
  allowing pure-discovery deployments.

## Alternatives
- Docker events stream: lower latency, but requires reconnect/reconciliation logic.
- Docker socket proxy sidecar: recommended for production hardening, documented
  as a deployment option; Runnelon still speaks the same read-only API.
- Kubernetes provider: out of scope until explicitly approved. Docker Compose labels
  are the supported discovery surface.
