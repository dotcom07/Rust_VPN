#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MODE="${MODE:-${1:-}}"
CONFIG="${CONFIG:-config/client.toml}"
HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-$HOME/.ssh/oracle_oci_ed25519}"
REMOTE_CONFIG="${REMOTE_CONFIG:-/etc/litevpn/server.toml}"
LOCAL_ONLY="${LOCAL_ONLY:-0}"
REMOTE_ONLY="${REMOTE_ONLY:-0}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" || -z "$MODE" ]]; then
  cat <<'HELP'
Usage:
  MODE=stream scripts/set-vpn-transport.sh
  MODE=datagram scripts/set-vpn-transport.sh

Environment:
  MODE=datagram|stream
  CONFIG=config/client.toml
  HOST=ubuntu@161.33.36.181
  KEY=~/.ssh/oracle_oci_ed25519
  REMOTE_CONFIG=/etc/litevpn/server.toml
  LOCAL_ONLY=0
  REMOTE_ONLY=0

Updates the local client config and the remote server config, then restarts
litevpn-server. Use LOCAL_ONLY=1 or REMOTE_ONLY=1 to limit the change.
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

if [[ "$CONFIG" = /* ]]; then
  CONFIG_PATH="$CONFIG"
else
  CONFIG_PATH="$ROOT/$CONFIG"
fi

set_local_mode() {
  local path="$1"
  local mode="$2"
  local tmp

  if [[ ! -f "$path" ]]; then
    echo "missing local config: $path" >&2
    exit 1
  fi

  tmp="$(mktemp)"
  awk -v mode="$mode" '
    BEGIN { changed = 0 }
    /^vpn_transport[[:space:]]*=/ {
      print "vpn_transport = \"" mode "\""
      changed = 1
      next
    }
    { print }
    END {
      if (!changed) {
        print "vpn_transport = \"" mode "\""
      }
    }
  ' "$path" > "$tmp"
  mv "$tmp" "$path"
}

if [[ "$REMOTE_ONLY" != "1" ]]; then
  set_local_mode "$CONFIG_PATH" "$MODE"
  echo "local $CONFIG_PATH: vpn_transport=$MODE"
fi

if [[ "$LOCAL_ONLY" != "1" ]]; then
  ssh -i "$KEY" "$HOST" "MODE='$MODE' REMOTE_CONFIG='$REMOTE_CONFIG' bash -s" <<'REMOTE'
set -euo pipefail
backup="${REMOTE_CONFIG}.bak.$(date +%Y%m%d-%H%M%S)"
sudo cp "$REMOTE_CONFIG" "$backup"
tmp="$(mktemp)"
sudo awk -v mode="$MODE" '
  BEGIN { changed = 0 }
  /^vpn_transport[[:space:]]*=/ {
    print "vpn_transport = \"" mode "\""
    changed = 1
    next
  }
  { print }
  END {
    if (!changed) {
      print "vpn_transport = \"" mode "\""
    }
  }
' "$REMOTE_CONFIG" > "$tmp"
sudo install -m 0644 "$tmp" "$REMOTE_CONFIG"
rm -f "$tmp"
sudo systemctl restart litevpn-server
sudo systemctl is-active litevpn-server
sudo grep -E '^(mtu|congestion_controller|egress_target_mbps|datagram_backlog_packets|adaptive_egress|vpn_transport)' "$REMOTE_CONFIG"
echo "backup=$backup"
REMOTE
fi
