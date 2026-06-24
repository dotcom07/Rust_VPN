#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODE="${MODE:-stream}"
RESTORE_MODE="${RESTORE_MODE:-datagram}"
RESTORE_ON_EXIT="${RESTORE_ON_EXIT:-1}"
CONFIG="${CONFIG:-config/client.toml}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-$HOME/.ssh/oracle_oci_ed25519}"
CONNECT_TIMEOUT_SECS="${CONNECT_TIMEOUT_SECS:-10}"
NO_ROUTES="${NO_ROUTES:-0}"
SUDO="${SUDO:-sudo}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'HELP'
Usage:
  MODE=stream HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/run-tun-smoke.sh
  MODE=datagram HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/run-tun-smoke.sh

Environment:
  MODE=stream|datagram
  RESTORE_MODE=datagram
  RESTORE_ON_EXIT=1
  CONFIG=config/client.toml
  HOST=ubuntu@161.33.36.181
  KEY=~/.ssh/oracle_oci_ed25519
  CONNECT_TIMEOUT_SECS=10
  NO_ROUTES=0
  SUDO=sudo

Switches local and remote configs to MODE with tested pacing presets, starts the
macOS TUN client, and restores RESTORE_MODE on exit. While the client is running,
open Fast.com or another browser speed test, then press Ctrl+C here to restore.
HELP
  exit 0
fi

case "$MODE" in
  datagram|stream) ;;
  *)
    echo "MODE must be datagram or stream" >&2
    exit 1
    ;;
esac

if [[ -n "$RESTORE_MODE" ]]; then
  case "$RESTORE_MODE" in
    datagram|stream) ;;
    *)
      echo "RESTORE_MODE must be datagram, stream, or empty" >&2
      exit 1
      ;;
  esac
fi

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

restore() {
  local status="$1"
  if [[ "$RESTORE_ON_EXIT" == "1" && -n "$RESTORE_MODE" ]]; then
    echo
    echo "== restoring $RESTORE_MODE transport =="
    MODE="$RESTORE_MODE" \
      CONFIG="$CONFIG" \
      HOST="$HOST" \
      KEY="$KEY" \
      "$ROOT/scripts/set-vpn-transport.sh" || true
  fi
  exit "$status"
}

trap 'restore $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

echo "== switching to $MODE transport =="
MODE="$MODE" \
  CONFIG="$CONFIG" \
  HOST="$HOST" \
  KEY="$KEY" \
  "$ROOT/scripts/set-vpn-transport.sh"

echo
echo "== starting LiteVPN client =="
echo "mode=$MODE config=$CONFIG_PATH"
echo "Run a browser speed test now; press Ctrl+C here when finished."

client_args=(
  "$CLIENT"
  --config "$CONFIG_PATH"
  --connect-timeout-secs "$CONNECT_TIMEOUT_SECS"
)

if [[ "$NO_ROUTES" == "1" ]]; then
  client_args+=(--no-routes)
fi

if [[ -n "$SUDO" ]]; then
  "$SUDO" "${client_args[@]}"
else
  "${client_args[@]}"
fi
