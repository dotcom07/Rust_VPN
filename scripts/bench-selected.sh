#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

CONFIG="${CONFIG:-config/client.toml}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-$HOME/.ssh/oracle_oci_ed25519}"
DURATION="${DURATION:-10}"
RUNS="${RUNS:-3}"
RUN_GAP_MS="${RUN_GAP_MS:-150}"
PAYLOAD_BYTES="${PAYLOAD_BYTES:-1300}"
DOWNLOAD_MBPS="${DOWNLOAD_MBPS:-34}"
UPLOAD_MBPS="${UPLOAD_MBPS:-13}"
CONNECT_TIMEOUT_SECS="${CONNECT_TIMEOUT_SECS:-10}"
OUT_DIR="${OUT_DIR:-bench-results}"
SNAPSHOT="${SNAPSHOT:-1}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'HELP'
Usage:
  HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/bench-selected.sh

Environment:
  CONFIG=config/client.toml
  DURATION=10
  RUNS=3
  RUN_GAP_MS=150
  PAYLOAD_BYTES=1300
  DOWNLOAD_MBPS=34
  UPLOAD_MBPS=13
  CONNECT_TIMEOUT_SECS=10
  OUT_DIR=bench-results
  SNAPSHOT=1
HELP
  exit 0
fi

CLIENT="$ROOT/target/release/litevpn-client"
if [[ ! -x "$CLIENT" ]]; then
  echo "missing $CLIENT; run cargo build --release --workspace first" >&2
  exit 1
fi

mkdir -p "$ROOT/$OUT_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"
LOG="$ROOT/$OUT_DIR/selected-$STAMP.log"

exec > >(tee "$LOG") 2>&1

echo "== LiteVPN selected benchmark =="
date '+%Y-%m-%dT%H:%M:%S%z'
echo "config=$CONFIG"
echo "host=$HOST"
echo "duration_secs=$DURATION runs=$RUNS run_gap_ms=$RUN_GAP_MS payload_bytes=$PAYLOAD_BYTES"
echo "download_mbps=$DOWNLOAD_MBPS upload_mbps=$UPLOAD_MBPS"
echo "log=$LOG"

FAILURES=0

run_snapshot() {
  local label="$1"
  if [[ "$SNAPSHOT" != "1" ]]; then
    return
  fi

  echo
  echo "== server snapshot: $label =="
  HOST="$HOST" KEY="$KEY" "$ROOT/scripts/server-snapshot.sh"
}

run_bench() {
  local direction="$1"
  local mbps="$2"

  echo
  echo "== bench $direction target ${mbps} Mbps =="
  if ! "$CLIENT" \
    --config "$ROOT/$CONFIG" \
    --bench "$direction" \
    --bench-duration-secs "$DURATION" \
    --bench-target-mbps "$mbps" \
    --bench-payload-bytes "$PAYLOAD_BYTES" \
    --bench-runs "$RUNS" \
    --bench-run-gap-ms "$RUN_GAP_MS" \
    --connect-timeout-secs "$CONNECT_TIMEOUT_SECS"; then
    echo "bench $direction failed"
    FAILURES=$((FAILURES + 1))
  fi
}

run_snapshot before
run_bench download "$DOWNLOAD_MBPS"
run_bench upload "$UPLOAD_MBPS"
run_snapshot after

echo
echo "== complete =="
echo "log=$LOG"

if [[ "$FAILURES" -gt 0 ]]; then
  echo "failures=$FAILURES"
  exit 1
fi
