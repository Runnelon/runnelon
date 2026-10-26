# Benchmark Plan

## Purpose
Compare Runnelon fairly with NGINX and Traefik, not to manufacture a ranking.

## Reproducibility checklist
- Record CPU model, cores, RAM, kernel, NIC, container limits, TLS library, Rust/tool versions, proxy versions, exact configs and commit SHAs.
- Same backend, client load generator, network topology, concurrency, keepalive, TLS settings, and route complexity.
- Separate warm-up from measurements; repeat runs and publish variance.
- Record throughput (req/s), p50/p95/p99 latency, error rate, CPU, RSS, open FDs, and connection behavior.
- Test HTTP keepalive, HTTPS, small/large responses, multiple upstreams, WebSocket if supported, and config reload under load.
- Include overload behavior and a longer soak test before release.

## Suggested tools
`wrk2` or `vegeta` for controlled load, `h2load` for HTTP/2, and system monitoring tools. Choose and pin actual tool versions when scripts are implemented.

A runnable smoke harness lives in `bench/` (`run.sh`, `upstream.py`). It prints raw output only and is intentionally not a published comparison.

## Release build
`[profile.release]` uses `lto = "thin"`, `codegen-units = 1`, and strips debuginfo. Record the resulting binary size and build command with any size claim; do not trade diagnostics for size without documenting it.

## Publication rule
Do not publish 'faster than NGINX/Traefik' claims without raw data, scripts, and methodology. Label targets as aspirations, not results.
