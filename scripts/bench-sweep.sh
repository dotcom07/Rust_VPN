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
  DIRECTION=stream-upload TARGETS="20 40 80" scripts/bench-sweep.sh

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
  download|upload|stream-download|stream-upload) ;;
  *)
    echo "DIRECTION must be download, upload, stream-download, or stream-upload" >&2
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

echo "direction,target_mbps,status,local_avg_mbps,server_avg_mbps,local_total_bytes,server_total_bytes,byte_gap,client_lost_packets,client_congestion_events,server_lost_packets,server_congestion_events,delivery_ok,log" > "$SUMMARY"

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
  local_aggregate="$(grep 'bench aggregate local:' "$log" | tail -1 || true)"
  local_avg_mbps=""
  local_total_bytes=""
  local_total_packets=""
  avg_mbps=""
  min_mbps=""
  max_mbps=""
  lost_packets=""
  congestion_events=""
  total_bytes=""
  total_packets=""
  total_elapsed_ms=""
  client_lost_packets=0
  client_congestion_events=0
  byte_gap=""
  packet_gap=""
  delivery_ok=0

  if [[ -n "$local_aggregate" ]]; then
    local_avg_mbps="$(extract_field "$local_aggregate" "avg_mbps")"
    local_total_bytes="$(extract_field "$local_aggregate" "total_bytes")"
    local_total_packets="$(extract_field "$local_aggregate" "total_packets")"
  else
    case "$DIRECTION" in
      upload) local_summary="$(grep '^upload sent:' "$log" | tail -1 || true)" ;;
      download) local_summary="$(grep '^download received:' "$log" | tail -1 || true)" ;;
      stream-upload) local_summary="$(grep '^stream upload sent:' "$log" | tail -1 || true)" ;;
      stream-download) local_summary="$(grep '^stream download received:' "$log" | tail -1 || true)" ;;
    esac
    if [[ -n "${local_summary:-}" ]]; then
      local_total_bytes="$(extract_field "$local_summary" "bytes")"
      local_total_packets="$(extract_field "$local_summary" "packets")"
      local_elapsed_ms="$(extract_field "$local_summary" "elapsed_ms")"
      local_avg_mbps="$(awk -v bytes="$local_total_bytes" -v elapsed_ms="$local_elapsed_ms" 'BEGIN { if (elapsed_ms > 0) printf "%.2f", bytes * 8 / elapsed_ms / 1000 }')"
    fi
  fi

  while IFS= read -r client_stats; do
    lost="$(extract_field "$client_stats" "lost_packets")"
    congestion="$(extract_field "$client_stats" "congestion_events")"
    client_lost_packets=$((client_lost_packets + ${lost:-0}))
    client_congestion_events=$((client_congestion_events + ${congestion:-0}))
  done < <(grep '^client stats:' "$log" || true)

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

  if [[ -n "$local_total_bytes" && -n "$total_bytes" ]]; then
    byte_gap=$((local_total_bytes - total_bytes))
    if [[ "$byte_gap" -lt 0 ]]; then
      byte_gap=$((-byte_gap))
    fi
  fi
  if [[ -n "$local_total_packets" && -n "$total_packets" ]]; then
    packet_gap=$((local_total_packets - total_packets))
    if [[ "$packet_gap" -lt 0 ]]; then
      packet_gap=$((-packet_gap))
    fi
  fi

  if [[ "$status" == "ok" ]]; then
    if [[ "$DIRECTION" == stream-* ]]; then
      if [[ "$local_total_bytes" == "$total_bytes" ]]; then
        delivery_ok=1
      fi
    elif [[ "${lost_packets:-}" == "0" \
      && "${congestion_events:-}" == "0" \
      && "$client_lost_packets" == "0" \
      && "$client_congestion_events" == "0" ]]; then
      if [[ "$DIRECTION" == "upload" ]]; then
        if [[ "$local_total_bytes" == "$total_bytes" && "$local_total_packets" == "$total_packets" ]]; then
          delivery_ok=1
        fi
      elif [[ -z "$packet_gap" || "$packet_gap" -le 4 ]]; then
        delivery_ok=1
      fi
    fi
  fi

  echo "$DIRECTION,$target,$status,$local_avg_mbps,$avg_mbps,$local_total_bytes,$total_bytes,$byte_gap,$client_lost_packets,$client_congestion_events,$lost_packets,$congestion_events,$delivery_ok,$log" >> "$SUMMARY"

  if [[ "$delivery_ok" == "1" ]]; then
    if [[ -n "$avg_mbps" ]] && {
      [[ -z "$best_mbps" ]] ||
        awk -v candidate="$avg_mbps" -v best="$best_mbps" 'BEGIN { exit !(candidate > best) }'
    }; then
      best_target="$target"
      best_mbps="$avg_mbps"
    fi
  fi
done

echo
echo "== sweep summary =="
cat "$SUMMARY"

if [[ -n "$best_target" ]]; then
  echo
  echo "selected_delivery_ok_target_mbps=$best_target"
  echo "selected_delivery_ok_server_avg_mbps=$best_mbps"
else
  echo
  echo "selected_delivery_ok_target_mbps="
  echo "no delivery-ok target found" >&2
fi

if [[ "$failures" -gt 0 ]]; then
  echo "failures=$failures"
  exit 1
fi
