#!/usr/bin/env bash
set -euo pipefail

MODE="${MODE:-${1:-wireguard}}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
DURATION="${DURATION:-10}"
PARALLEL="${PARALLEL:-1}"
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
need ping

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

echo
echo "== ping =="
ping -c 20 "$SERVER_TUN_IP"

echo
echo "== upload: client -> server =="
start_iperf_server
iperf3 -c "$SERVER_TUN_IP" -t "$DURATION" -P "$PARALLEL"
finish_iperf_server

echo
echo "== download: server -> client =="
start_iperf_server
iperf3 -c "$SERVER_TUN_IP" -t "$DURATION" -P "$PARALLEL" -R
finish_iperf_server
