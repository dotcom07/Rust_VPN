#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
WG_NAME="${WG_NAME:-wg0}"
WG_CONF="${WG_CONF:-$ROOT/config/wireguard/$WG_NAME.conf}"
LITEVPN_CONFIG="${LITEVPN_CONFIG:-$ROOT/config/client.toml}"
DURATION="${DURATION:-10}"
PARALLEL="${PARALLEL:-1}"
OUT_DIR="${OUT_DIR:-bench-results}"
RUN_MODES="${RUN_MODES:-wireguard litevpn}"

LITEVPN_PID=""
LOCAL_WG_UP=0
SUDO_KEEPALIVE_PID=""
STAMP="$(date +%Y%m%d-%H%M%S)"
COMPARE_DIR="$ROOT/$OUT_DIR/vpn-compare-$STAMP"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'HELP'
Usage:
  HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/compare-vpn-modes.sh

Runs WireGuard and LiteVPN sequentially, benchmarks each tunnel with iperf3,
and writes logs under bench-results/vpn-compare-*/.

Environment:
  RUN_MODES="wireguard litevpn"
  DURATION=10
  PARALLEL=1
  WG_CONF=config/wireguard/wg0.conf
  LITEVPN_CONFIG=config/client.toml
HELP
  exit 0
fi

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

remote() {
  ssh -i "$KEY" "$HOST" "$@"
}

start_sudo_keepalive() {
  sudo -v
  while true; do
    sudo -n true >/dev/null 2>&1 || exit 0
    sleep 30
  done &
  SUDO_KEEPALIVE_PID=$!
}

stop_sudo_keepalive() {
  if [[ -n "$SUDO_KEEPALIVE_PID" ]]; then
    kill "$SUDO_KEEPALIVE_PID" >/dev/null 2>&1 || true
    wait "$SUDO_KEEPALIVE_PID" >/dev/null 2>&1 || true
    SUDO_KEEPALIVE_PID=""
  fi
}

restore_remote_litevpn() {
  remote "sudo wg-quick down '$WG_NAME' >/dev/null 2>&1 || true; sudo pkill iperf3 >/dev/null 2>&1 || true; sudo systemctl start litevpn-server" || true
}

stop_local_wireguard() {
  if [[ "$LOCAL_WG_UP" == "1" ]]; then
    sudo wg-quick down "$WG_CONF" >/dev/null 2>&1 || true
    LOCAL_WG_UP=0
  fi
}

stop_local_litevpn() {
  if [[ -n "$LITEVPN_PID" ]]; then
    kill "$LITEVPN_PID" >/dev/null 2>&1 || true
    wait "$LITEVPN_PID" >/dev/null 2>&1 || true
    LITEVPN_PID=""
  fi
  sudo pkill -f litevpn-client >/dev/null 2>&1 || true
  sudo "$ROOT/target/release/litevpn-client" --config "$LITEVPN_CONFIG" --cleanup-routes >/dev/null 2>&1 || true
}

cleanup() {
  stop_local_wireguard
  stop_local_litevpn
  restore_remote_litevpn
  stop_sudo_keepalive
}

run_bench() {
  local mode="$1"
  local log="$COMPARE_DIR/$mode.log"

  echo
  echo "== benchmark $mode =="
  MODE="$mode" \
    HOST="$HOST" \
    KEY="$KEY" \
    DURATION="$DURATION" \
    PARALLEL="$PARALLEL" \
    "$ROOT/scripts/bench-vpn-throughput.sh" 2>&1 | tee "$log"
}

run_wireguard() {
  if [[ ! -f "$WG_CONF" ]]; then
    echo "missing $WG_CONF; run scripts/setup-wireguard-baseline.sh first" >&2
    exit 1
  fi

  echo
  echo "== start wireguard =="
  remote "sudo systemctl stop litevpn-server; sudo wg-quick down '$WG_NAME' >/dev/null 2>&1 || true; sudo wg-quick up '$WG_NAME'; sudo wg show '$WG_NAME'"
  sudo wg-quick up "$WG_CONF"
  LOCAL_WG_UP=1
  sleep 2
  run_bench wireguard
  stop_local_wireguard
  restore_remote_litevpn
}

run_litevpn() {
  echo
  echo "== start litevpn =="
  restore_remote_litevpn
  sudo "$ROOT/target/release/litevpn-client" --config "$LITEVPN_CONFIG" > "$COMPARE_DIR/litevpn-client.log" 2>&1 &
  LITEVPN_PID=$!
  sleep 4
  run_bench litevpn
  stop_local_litevpn
}

need ssh
need sudo
need wg-quick
need iperf3
need ping

mkdir -p "$COMPARE_DIR"

echo "== VPN mode comparison =="
date '+%Y-%m-%dT%H:%M:%S%z'
echo "host=$HOST"
echo "duration_secs=$DURATION parallel=$PARALLEL"
echo "run_modes=$RUN_MODES"
echo "logs=$COMPARE_DIR"

start_sudo_keepalive
trap cleanup EXIT INT TERM

for mode in $RUN_MODES; do
  case "$mode" in
    wireguard) run_wireguard ;;
    litevpn) run_litevpn ;;
    *)
      echo "unknown mode in RUN_MODES: $mode" >&2
      exit 1
      ;;
  esac
done

echo
echo "comparison logs: $COMPARE_DIR"
