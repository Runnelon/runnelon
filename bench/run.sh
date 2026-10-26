#!/usr/bin/env bash
# Smoke benchmark runner. Produces raw output only; no comparative claims.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
LISTEN=${LISTEN:-127.0.0.1:18080}
UPSTREAM_PORT=${UPSTREAM_PORT:-18000}
CONCURRENCY=${CONCURRENCY:-50}
REQUESTS=${REQUESTS:-20000}
WORKDIR=$(mktemp -d)
cleanup() {
  [[ -n "${PROXY_PID:-}" ]] && kill "$PROXY_PID" 2>/dev/null || true
  [[ -n "${UPSTREAM_PID:-}" ]] && kill "$UPSTREAM_PID" 2>/dev/null || true
  rm -rf "$WORKDIR"
}
trap cleanup EXIT

echo "== environment =="
uname -sm
sysctl -n machdep.cpu.brand_string 2>/dev/null || grep -m1 'model name' /proc/cpuinfo || true
python3 --version
"$ROOT/target/release/runnelon" --version

echo "== build =="
cargo build --release --locked --manifest-path "$ROOT/Cargo.toml"

cat >"$WORKDIR/runnelon.yaml" <<EOF
version: 1
listeners:
  - address: "$LISTEN"
routes:
  - host: "*"
    path_prefix: "/"
    upstream: "bench"
upstreams:
  bench:
    endpoints: ["127.0.0.1:$UPSTREAM_PORT"]
EOF

python3 "$ROOT/bench/upstream.py" "$UPSTREAM_PORT" &
UPSTREAM_PID=$!
# Keep access logs out of the measurement path unless explicitly requested.
RUST_LOG="${RUST_LOG:-warn}" "$ROOT/target/release/runnelon" --config "$WORKDIR/runnelon.yaml" &
PROXY_PID=$!

for _ in $(seq 1 100); do
  if curl -sf -o /dev/null "http://$LISTEN/"; then break; fi
  sleep 0.1
done

echo "== warmup =="
for _ in $(seq 1 50); do curl -sf -o /dev/null "http://$LISTEN/"; done

echo "== load =="
if command -v wrk2 >/dev/null; then
  wrk2 -t4 -c"$CONCURRENCY" -d30s -R200000 --latency "http://$LISTEN/"
elif command -v wrk >/dev/null; then
  wrk -t4 -c"$CONCURRENCY" -d30s --latency "http://$LISTEN/"
elif command -v ab >/dev/null; then
  ab -k -n"$REQUESTS" -c"$CONCURRENCY" "http://$LISTEN/"
else
  echo "No load generator found (install wrk2, wrk, or ab)." >&2
  exit 1
fi

echo "== note =="
echo "Raw output above. Publish only with hardware, versions, config, repetition count and variance."
