#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
MTUS="${MTUS:-1280 1380 1420}"
DURATION="${DURATION:-10}"
PARALLEL="${PARALLEL:-1}"
FASTCOM_PAUSE="${FASTCOM_PAUSE:-0}"
RESTORE_MTU="${RESTORE_MTU:-1420}"
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT_DIR="${OUT_DIR:-bench-results/wg-mtu-sweep-$STAMP}"
SUMMARY="$ROOT/$OUT_DIR/summary.csv"

usage() {
  cat <<'HELP'
Usage:
  MTUS="1280 1380 1420" HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/sweep-wireguard-mtu.sh

Installs each WireGuard MTU candidate on the same server, runs the WireGuard
VPN throughput benchmark for that MTU, summarizes each run, and writes a sweep
summary under bench-results/wg-mtu-sweep-*.

Environment:
  MTUS="1280 1380 1420"
  DURATION=10
  PARALLEL=1
  FASTCOM_PAUSE=0
  RESTORE_MTU=1420  # set empty to leave the last tested MTU installed
  OUT_DIR=bench-results/wg-mtu-sweep-YYYYMMDD-HHMMSS
HELP
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

validate_mtu() {
  case "$1" in
    ''|*[!0-9]*)
      echo "invalid MTU: $1" >&2
      exit 1
      ;;
  esac
}

restore_mtu() {
  if [[ -z "$RESTORE_MTU" ]]; then
    return
  fi

  echo
  echo "== restore wireguard mtu $RESTORE_MTU =="
  WG_MTU="$RESTORE_MTU" \
    HOST="$HOST" \
    KEY="$KEY" \
    "$ROOT/scripts/setup-wireguard-baseline.sh" >/dev/null || true
}

cleanup() {
  local code=$?

  trap - EXIT INT TERM
  restore_mtu
  exit "$code"
}

need awk
need find
need sort
need sudo

for mtu in $MTUS; do
  validate_mtu "$mtu"
done
if [[ -n "$RESTORE_MTU" ]]; then
  validate_mtu "$RESTORE_MTU"
fi

echo "Checking local sudo before changing remote WireGuard MTU configs."
sudo -v

mkdir -p "$ROOT/$OUT_DIR"
echo "mtu,compare_dir,ping_avg_ms,upload_receiver_mbps,download_receiver_mbps,score,status" > "$SUMMARY"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for mtu in $MTUS; do
  run_out_dir="$OUT_DIR/mtu-$mtu"
  compare_dir=""
  status="ok"

  echo
  echo "== wireguard mtu $mtu =="
  WG_MTU="$mtu" \
    HOST="$HOST" \
    KEY="$KEY" \
    "$ROOT/scripts/setup-wireguard-baseline.sh"

  if FASTCOM_PAUSE="$FASTCOM_PAUSE" \
    HOST="$HOST" \
    KEY="$KEY" \
    DURATION="$DURATION" \
    PARALLEL="$PARALLEL" \
    OUT_DIR="$run_out_dir" \
    "$ROOT/scripts/compare-vpn-modes.sh" --mode wireguard; then
    compare_dir="$(find "$ROOT/$run_out_dir" -maxdepth 1 -type d -name 'vpn-compare-*' | sort | tail -1)"
  else
    status="failed"
  fi

  if [[ -n "$compare_dir" && -f "$compare_dir/summary.csv" ]]; then
    "$ROOT/scripts/summarize-vpn-comparison.sh" "$compare_dir" >/dev/null || true
    awk -F',' -v mtu="$mtu" -v dir="$compare_dir" -v status="$status" '
      NR == 2 {
        ping=$3 + 0
        upload=$7 + 0
        download=$9 + 0
        score=download + upload - (ping / 10)
        printf "%s,%s,%s,%s,%s,%.2f,%s\n", mtu, dir, $3, $7, $9, score, status
        found=1
      }
      END {
        if (!found) {
          printf "%s,%s,,,,,%s\n", mtu, dir, status
        }
      }
    ' "$compare_dir/summary.csv" >> "$SUMMARY"
  else
    echo "$mtu,$compare_dir,,,,,$status" >> "$SUMMARY"
  fi
done

echo
echo "== WireGuard MTU sweep summary =="
cat "$SUMMARY"

echo
awk -F',' '
  NR == 1 { next }
  $7 == "ok" && $6 != "" {
    score=$6 + 0
    if (!seen || score > best_score) {
      seen=1
      best_score=score
      best_mtu=$1
      best_ping=$3
      best_upload=$4
      best_download=$5
      best_dir=$2
    }
  }
  END {
    if (seen) {
      printf "recommended_mtu=%s score=%.2f ping_avg_ms=%s upload_receiver_mbps=%s download_receiver_mbps=%s compare_dir=%s\n", best_mtu, best_score, best_ping, best_upload, best_download, best_dir
    } else {
      print "recommended_mtu=pending"
    }
  }
' "$SUMMARY"
