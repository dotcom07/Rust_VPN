#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODE="${MODE:-}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
WG_NAME="${WG_NAME:-wg0}"
WG_CONF="${WG_CONF:-$ROOT/config/wireguard/$WG_NAME.conf}"
LITEVPN_CONFIG="${LITEVPN_CONFIG:-$ROOT/config/client.toml}"
RESTORE_LITEVPN="${RESTORE_LITEVPN:-1}"
WG_QUICK_BIN="${WG_QUICK_BIN:-}"
LOCAL_WG_UP=0
CLEANED_UP=0

usage() {
  cat <<'HELP'
Usage:
  scripts/run-vpn-mode.sh --mode wireguard
  scripts/run-vpn-mode.sh --mode litevpn
  MODE=wireguard HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/run-vpn-mode.sh
  MODE=litevpn   HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/run-vpn-mode.sh

Modes:
  wireguard  Stop the remote LiteVPN service, start remote wg0, then run local wg-quick.
  litevpn    Stop remote wg0, start remote LiteVPN service, then run the local LiteVPN client.

Environment:
  RESTORE_LITEVPN=1  Restore remote LiteVPN and stop remote wg0 when this script exits.
  WG_QUICK_BIN=/opt/homebrew/bin/wg-quick
  WG_CONF=config/wireguard/wg0.conf
  LITEVPN_CONFIG=config/client.toml
HELP
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    --mode)
      if [[ -z "${2:-}" ]]; then
        echo "--mode requires wireguard or litevpn" >&2
        exit 1
      fi
      MODE="$2"
      shift 2
      ;;
    --mode=*)
      MODE="${1#*=}"
      shift
      ;;
    wireguard|litevpn)
      MODE="$1"
      shift
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "$MODE" ]]; then
  usage
  exit 0
fi

remote() {
  ssh -i "$KEY" "$HOST" "$@"
}

restore_litevpn() {
  if [[ "$RESTORE_LITEVPN" != "1" ]]; then
    return
  fi

  remote "sudo wg-quick down '$WG_NAME' >/dev/null 2>&1 || true; sudo systemctl start litevpn-server" || true
}

cleanup_wireguard() {
  if [[ "$CLEANED_UP" == "1" ]]; then
    return
  fi
  CLEANED_UP=1

  if [[ "$LOCAL_WG_UP" == "1" ]]; then
    sudo "$WG_QUICK_BIN" down "$WG_CONF" >/dev/null 2>&1 || true
  fi
  restore_litevpn
}

case "$MODE" in
  wireguard)
    if [[ ! -f "$WG_CONF" ]]; then
      echo "missing $WG_CONF; run scripts/setup-wireguard-baseline.sh first" >&2
      exit 1
    fi
    if [[ -z "$WG_QUICK_BIN" ]]; then
      WG_QUICK_BIN="$(command -v wg-quick)"
    fi

    echo "Checking local sudo before switching the remote server to WireGuard."
    sudo -v
    trap cleanup_wireguard EXIT INT TERM
    remote "sudo systemctl stop litevpn-server; sudo wg-quick down '$WG_NAME' >/dev/null 2>&1 || true; sudo wg-quick up '$WG_NAME'; sudo wg show '$WG_NAME'"
    echo "Starting local WireGuard with sudo."
    sudo "$WG_QUICK_BIN" up "$WG_CONF"
    LOCAL_WG_UP=1
    echo "WireGuard is up. Press Ctrl-C to stop and restore LiteVPN on the server."
    while true; do
      sleep 3600
    done
    ;;
  litevpn)
    echo "Checking local sudo before switching the remote server to LiteVPN."
    sudo -v
    restore_litevpn
    echo "Starting local LiteVPN with sudo."
    sudo "$ROOT/target/release/litevpn-client" --config "$LITEVPN_CONFIG"
    ;;
  *)
    echo "MODE must be wireguard or litevpn" >&2
    usage >&2
    exit 1
    ;;
esac
