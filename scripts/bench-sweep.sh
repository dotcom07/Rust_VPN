#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

CONFIG="${CONFIG:-config/client.toml}"
DIRECTION="${DIRECTION:-download}"
TARGETS="${TARGETS:-30 34 38 40}"
DURATION="${DURATION:-10}"
RUNS="${RUNS:-3}"
RUN_GAP_MS="${RUN_GAP_MS:-150}"
PAYLOAD_BYTES="${PAYLOAD_BYTES:-1300}"
CONNECT_TIMEOUT_SECS="${CONNECT_TIMEOUT_SECS:-10}"
OUT_DIR="${OUT_DIR:-bench-results}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'HELP'
Usage:
  DIRECTION=download TARGETS="30 34 38 40" scripts/bench-sweep.sh
  DIRECTION=upload TARGETS="10 12 13" scripts/bench-sweep.sh

Environment:
  CONFIG=config/client.toml
  DIRECTION=download
  TARGETS="30 34 38 40"
  DURATION=10
  RUNS=3
  RUN_GAP_MS=150
  PAYLOAD_BYTES=1300
  CONNECT_TIMEOUT_SECS=10
  OUT_DIR=bench-results
HELP
  exit 0
fi

case "$DIRECTION" in
  download|upload) ;;
  *)
    echo "DIRECTION must be download or upload" >&2
    exit 1
    ;;
esac

CLIENT="$ROOT/target/release/litevpn-client"
if [[ "$CONFIG" = /* ]]; then
  CONFIG_PATH="$CONFIG"
else
  CONFIG_PATH="$ROOT/$CONFIG"
fi
if [[ ! -x "$CLIENT" ]]; then
  echo "missing $CLIENT; run cargo build --release --workspace first" >&2
  exit 1
fi

mkdir -p "$ROOT/$OUT_DIR"
STAMP="$(date +%Y%m%d-%H%M%S)"
SWEEP_DIR="$ROOT/$OUT_DIR/sweep-$DIRECTION-$STAMP"
SUMMARY="$SWEEP_DIR/summary.csv"
mkdir -p "$SWEEP_DIR"

echo "direction,target_mbps,status,server_avg_mbps,server_min_mbps,server_max_mbps,lost_packets,congestion_events,total_bytes,total_packets,total_elapsed_ms,log" > "$SUMMARY"

extract_field() {
  local line="$1"
  local key="$2"
  printf '%s\n' "$line" | tr ' ,' '\n' | awk -F= -v key="$key" '$1 == key { print $2; exit }'
}

best_target=""
best_mbps=""
failures=0

echo "== LiteVPN bench sweep =="
date '+%Y-%m-%dT%H:%M:%S%z'
echo "direction=$DIRECTION"
echo "config=$CONFIG_PATH"
echo "targets=$TARGETS"
echo "duration_secs=$DURATION runs=$RUNS run_gap_ms=$RUN_GAP_MS payload_bytes=$PAYLOAD_BYTES"
echo "summary=$SUMMARY"

for target in $TARGETS; do
  log="$SWEEP_DIR/${DIRECTION}-${target}mbps.log"
  echo
  echo "== bench $DIRECTION target ${target} Mbps =="

  if "$CLIENT" \
    --config "$CONFIG_PATH" \
    --bench "$DIRECTION" \
    --bench-duration-secs "$DURATION" \
    --bench-target-mbps "$target" \
    --bench-payload-bytes "$PAYLOAD_BYTES" \
    --bench-runs "$RUNS" \
    --bench-run-gap-ms "$RUN_GAP_MS" \
    --connect-timeout-secs "$CONNECT_TIMEOUT_SECS" 2>&1 | tee "$log"; then
    status="ok"
  else
    status="failed"
    failures=$((failures + 1))
  fi

  aggregate="$(grep 'bench aggregate server:' "$log" | tail -1 || true)"
  avg_mbps=""
  min_mbps=""
  max_mbps=""
  lost_packets=""
  congestion_events=""
  total_bytes=""
  total_packets=""
  total_elapsed_ms=""

  if [[ -n "$aggregate" ]]; then
    avg_mbps="$(extract_field "$aggregate" "avg_mbps")"
    min_mbps="$(extract_field "$aggregate" "min_mbps")"
    max_mbps="$(extract_field "$aggregate" "max_mbps")"
    lost_packets="$(extract_field "$aggregate" "lost_packets")"
    congestion_events="$(extract_field "$aggregate" "congestion_events")"
    total_bytes="$(extract_field "$aggregate" "total_bytes")"
    total_packets="$(extract_field "$aggregate" "total_packets")"
    total_elapsed_ms="$(extract_field "$aggregate" "total_elapsed_ms")"
  else
    server_summary="$(grep "^server direction=$DIRECTION " "$log" | tail -1 || true)"
    if [[ -n "$server_summary" ]]; then
      total_bytes="$(extract_field "$server_summary" "bytes")"
      total_packets="$(extract_field "$server_summary" "packets")"
      total_elapsed_ms="$(extract_field "$server_summary" "measured_elapsed_ms")"
      if [[ -z "$total_elapsed_ms" ]]; then
        total_elapsed_ms="$(extract_field "$server_summary" "elapsed_ms")"
      fi
      lost_packets="$(extract_field "$server_summary" "lost_packets")"
      congestion_events="$(extract_field "$server_summary" "congestion_events")"
      avg_mbps="$(awk -v bytes="$total_bytes" -v elapsed_ms="$total_elapsed_ms" 'BEGIN { if (elapsed_ms > 0) printf "%.2f", bytes * 8 / elapsed_ms / 1000 }')"
      min_mbps="$avg_mbps"
      max_mbps="$avg_mbps"
    fi
  fi

  echo "$DIRECTION,$target,$status,$avg_mbps,$min_mbps,$max_mbps,$lost_packets,$congestion_events,$total_bytes,$total_packets,$total_elapsed_ms,$log" >> "$SUMMARY"

  if [[ "$status" == "ok" && "${lost_packets:-}" == "0" && "${congestion_events:-}" == "0" ]]; then
    best_target="$target"
    best_mbps="$avg_mbps"
  fi
done

echo
echo "== sweep summary =="
cat "$SUMMARY"

if [[ -n "$best_target" ]]; then
  echo
  echo "selected_zero_loss_target_mbps=$best_target"
  echo "selected_zero_loss_server_avg_mbps=$best_mbps"
else
  echo
  echo "selected_zero_loss_target_mbps="
  echo "no zero-loss target found" >&2
fi

if [[ "$failures" -gt 0 ]]; then
  echo "failures=$failures"
  exit 1
fi
