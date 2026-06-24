#!/usr/bin/env bash
set -euo pipefail

MODE="${MODE:-${1:-wireguard}}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
DURATION="${DURATION:-10}"
PARALLEL="${PARALLEL:-1}"
LOG_DIR="${LOG_DIR:-}"
IPERF_SERVER_PID=""

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'HELP'
Usage:
  MODE=wireguard HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/bench-vpn-throughput.sh
  MODE=litevpn   HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/bench-vpn-throughput.sh

Run this after the selected VPN mode is already up. It measures ping,
client-to-server iperf3 upload, and server-to-client iperf3 download over the
tunnel IP.

Environment:
  DURATION=10
  PARALLEL=1
  LOG_DIR=bench-results/vpn-compare-.../wireguard
  SERVER_TUN_IP=10.77.0.1  # wireguard default
  SERVER_TUN_IP=10.66.0.1  # litevpn default
HELP
  exit 0
fi

case "$MODE" in
  wireguard)
    SERVER_TUN_IP="${SERVER_TUN_IP:-10.77.0.1}"
    ;;
  litevpn)
    SERVER_TUN_IP="${SERVER_TUN_IP:-10.66.0.1}"
    ;;
  *)
    echo "MODE must be wireguard or litevpn" >&2
    exit 1
    ;;
esac

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

need ssh
need iperf3
need jq
need ping

if [[ -n "$LOG_DIR" ]]; then
  mkdir -p "$LOG_DIR"
else
  LOG_DIR="$(mktemp -d "${TMPDIR:-/tmp}/litevpn-bench.XXXXXX")"
fi

PING_LOG="$LOG_DIR/ping.txt"
UPLOAD_JSON="$LOG_DIR/upload.json"
DOWNLOAD_JSON="$LOG_DIR/download.json"
SUMMARY="$LOG_DIR/summary.csv"

remote() {
  ssh -i "$KEY" "$HOST" "$@"
}

cleanup() {
  remote "pkill iperf3 >/dev/null 2>&1 || true" >/dev/null 2>&1 || true
  if [[ -n "$IPERF_SERVER_PID" ]]; then
    wait "$IPERF_SERVER_PID" >/dev/null 2>&1 || true
    IPERF_SERVER_PID=""
  fi
}

start_iperf_server() {
  remote "pkill iperf3 >/dev/null 2>&1 || true; iperf3 -s -B '$SERVER_TUN_IP' -1" &
  IPERF_SERVER_PID=$!
  sleep 1
}

finish_iperf_server() {
  wait "$IPERF_SERVER_PID" || true
  IPERF_SERVER_PID=""
}

trap cleanup EXIT INT TERM

echo "== VPN throughput benchmark =="
date '+%Y-%m-%dT%H:%M:%S%z'
echo "mode=$MODE"
echo "server_tunnel_ip=$SERVER_TUN_IP"
echo "duration_secs=$DURATION parallel=$PARALLEL"
echo "log_dir=$LOG_DIR"

echo
echo "== ping =="
ping -c 20 "$SERVER_TUN_IP" | tee "$PING_LOG"

ping_stats="$(awk -F' = ' '/round-trip|rtt/ { print $2 }' "$PING_LOG" | tail -1)"
ping_min_ms=""
ping_avg_ms=""
ping_max_ms=""
ping_stddev_ms=""
if [[ -n "$ping_stats" ]]; then
  ping_min_ms="$(printf '%s\n' "$ping_stats" | awk -F'/' '{ print $1 }')"
  ping_avg_ms="$(printf '%s\n' "$ping_stats" | awk -F'/' '{ print $2 }')"
  ping_max_ms="$(printf '%s\n' "$ping_stats" | awk -F'/' '{ print $3 }')"
  ping_stddev_ms="$(printf '%s\n' "$ping_stats" | awk -F'/' '{ print $4 }' | awk '{ print $1 }')"
fi

json_field_mbps() {
  local file="$1"
  local filter="$2"

  jq -r "$filter // empty" "$file" |
    awk '{ if ($1 != "") printf "%.2f", $1 / 1000000 }'
}

echo
echo "== upload: client -> server =="
start_iperf_server
iperf3 -c "$SERVER_TUN_IP" -t "$DURATION" -P "$PARALLEL" --json > "$UPLOAD_JSON"
finish_iperf_server
upload_sender_mbps="$(json_field_mbps "$UPLOAD_JSON" '.end.sum_sent.bits_per_second')"
upload_receiver_mbps="$(json_field_mbps "$UPLOAD_JSON" '.end.sum_received.bits_per_second')"
echo "upload sender_mbps=$upload_sender_mbps receiver_mbps=$upload_receiver_mbps"

echo
echo "== download: server -> client =="
start_iperf_server
iperf3 -c "$SERVER_TUN_IP" -t "$DURATION" -P "$PARALLEL" -R --json > "$DOWNLOAD_JSON"
finish_iperf_server
download_sender_mbps="$(json_field_mbps "$DOWNLOAD_JSON" '.end.sum_sent.bits_per_second')"
download_receiver_mbps="$(json_field_mbps "$DOWNLOAD_JSON" '.end.sum_received.bits_per_second')"
echo "download sender_mbps=$download_sender_mbps receiver_mbps=$download_receiver_mbps"

echo "mode,ping_min_ms,ping_avg_ms,ping_max_ms,ping_stddev_ms,upload_sender_mbps,upload_receiver_mbps,download_sender_mbps,download_receiver_mbps,log_dir" > "$SUMMARY"
echo "$MODE,$ping_min_ms,$ping_avg_ms,$ping_max_ms,$ping_stddev_ms,$upload_sender_mbps,$upload_receiver_mbps,$download_sender_mbps,$download_receiver_mbps,$LOG_DIR" >> "$SUMMARY"

echo
echo "== summary =="
cat "$SUMMARY"
