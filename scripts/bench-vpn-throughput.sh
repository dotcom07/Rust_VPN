#!/usr/bin/env bash
set -euo pipefail

MODE="${MODE:-${1:-wireguard}}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
DURATION="${DURATION:-10}"
PARALLEL="${PARALLEL:-1}"

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

start_iperf_server() {
  remote "pkill iperf3 >/dev/null 2>&1 || true; iperf3 -s -B '$SERVER_TUN_IP' -1" &
  IPERF_SERVER_PID=$!
  sleep 1
}

finish_iperf_server() {
  wait "$IPERF_SERVER_PID" || true
}

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
