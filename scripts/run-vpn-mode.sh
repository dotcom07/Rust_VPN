#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODE="${MODE:-${1:-}}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
WG_NAME="${WG_NAME:-wg0}"
WG_CONF="${WG_CONF:-$ROOT/config/wireguard/$WG_NAME.conf}"
LITEVPN_CONFIG="${LITEVPN_CONFIG:-$ROOT/config/client.toml}"
RESTORE_LITEVPN="${RESTORE_LITEVPN:-1}"

usage() {
  cat <<'HELP'
Usage:
  MODE=wireguard HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/run-vpn-mode.sh
  MODE=litevpn   HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/run-vpn-mode.sh

Modes:
  wireguard  Stop the remote LiteVPN service, start remote wg0, then run local wg-quick.
  litevpn    Stop remote wg0, start remote LiteVPN service, then run the local LiteVPN client.

Environment:
  RESTORE_LITEVPN=1  Restore remote LiteVPN and stop remote wg0 when this script exits.
  WG_CONF=config/wireguard/wg0.conf
  LITEVPN_CONFIG=config/client.toml
HELP
}

if [[ -z "$MODE" || "$MODE" == "-h" || "$MODE" == "--help" ]]; then
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

case "$MODE" in
  wireguard)
    if [[ ! -f "$WG_CONF" ]]; then
      echo "missing $WG_CONF; run scripts/setup-wireguard-baseline.sh first" >&2
      exit 1
    fi

    trap restore_litevpn EXIT INT TERM
    remote "sudo systemctl stop litevpn-server; sudo wg-quick down '$WG_NAME' >/dev/null 2>&1 || true; sudo wg-quick up '$WG_NAME'; sudo wg show '$WG_NAME'"
    echo "Starting local WireGuard. macOS will ask for sudo password."
    sudo wg-quick up "$WG_CONF"
    echo "WireGuard is up. Press Ctrl-C to stop and restore LiteVPN on the server."
    trap 'sudo wg-quick down "$WG_CONF" >/dev/null 2>&1 || true; restore_litevpn; exit 0' INT TERM
    while true; do
      sleep 3600
    done
    ;;
  litevpn)
    restore_litevpn
    echo "Starting local LiteVPN. macOS will ask for sudo password."
    sudo "$ROOT/target/release/litevpn-client" --config "$LITEVPN_CONFIG"
    ;;
  *)
    echo "MODE must be wireguard or litevpn" >&2
    usage >&2
    exit 1
    ;;
esac
