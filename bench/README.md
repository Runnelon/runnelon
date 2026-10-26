# Benchmark harness

Purpose: make performance claims **testable**, not to advertise a ranking.
No "faster than X" statement is permitted without raw data, methodology, and
reproduction steps (see `AGENTS.md` and `docs/BENCHMARKS.md`).

## What this provides
- `bench/upstream.py` — minimal threaded static backend (not a production server;
  its overhead is part of the measurement and must be recorded).
- `bench/run.sh` — builds the release binary, starts backend + proxy, warms up,
  then runs `wrk2` (preferred), `wrk`, or `ab`, whichever is installed. Raw
  output goes to stdout only.

## Run
```bash
CONCURRENCY=50 REQUESTS=20000 ./bench/run.sh
# optional:
LISTEN=127.0.0.1:18080 UPSTREAM_PORT=18000 ./bench/run.sh
```

## Required record for any published number
- Commit SHA (or working-tree diff), Runnelon version, `cargo --version`, `rustc --version`.
- CPU model, core count, RAM, kernel, container/cgroup limits.
- Proxy config, backend implementation and language, payload size, keepalive on/off.
- Load generator and version, concurrency, duration, request rate (with `wrk2`).
- Warmup policy, repetitions, and variance across runs.
- p50/p95/p99 latency, throughput, error count, CPU, RSS, open FDs.
- Comparison runs must use identical workloads, topology, and tuning effort for
  NGINX/Traefik/Runnelon, and must state each proxy version.

## Guardrails
- Do not extrapolate development-laptop numbers to production.
- Do not compare against a default-configured competitor while tuning only Runnelon.
- Label exploratory runs as smoke, not evidence.
