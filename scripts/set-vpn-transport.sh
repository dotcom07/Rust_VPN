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
APPLY_PRESETS="${APPLY_PRESETS:-1}"
DATAGRAM_CLIENT_EGRESS_MBPS="${DATAGRAM_CLIENT_EGRESS_MBPS:-13}"
DATAGRAM_SERVER_EGRESS_MBPS="${DATAGRAM_SERVER_EGRESS_MBPS:-36}"
STREAM_CLIENT_EGRESS_MBPS="${STREAM_CLIENT_EGRESS_MBPS:-40}"
STREAM_SERVER_EGRESS_MBPS="${STREAM_SERVER_EGRESS_MBPS:-36}"
DATAGRAM_CLIENT_ADAPTIVE="${DATAGRAM_CLIENT_ADAPTIVE:-false}"
DATAGRAM_SERVER_ADAPTIVE="${DATAGRAM_SERVER_ADAPTIVE:-true}"
STREAM_CLIENT_ADAPTIVE="${STREAM_CLIENT_ADAPTIVE:-false}"
STREAM_SERVER_ADAPTIVE="${STREAM_SERVER_ADAPTIVE:-true}"

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
  APPLY_PRESETS=1
  DATAGRAM_CLIENT_EGRESS_MBPS=13
  DATAGRAM_SERVER_EGRESS_MBPS=36
  STREAM_CLIENT_EGRESS_MBPS=40
  STREAM_SERVER_EGRESS_MBPS=36

Updates the local client config and the remote server config, then restarts
litevpn-server. Use LOCAL_ONLY=1 or REMOTE_ONLY=1 to limit the change.
When APPLY_PRESETS=1, the script also applies the selected transport's tested
egress/adaptive pacing preset.
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

case "$MODE" in
  datagram)
    CLIENT_EGRESS_MBPS="$DATAGRAM_CLIENT_EGRESS_MBPS"
    SERVER_EGRESS_MBPS="$DATAGRAM_SERVER_EGRESS_MBPS"
    CLIENT_ADAPTIVE="$DATAGRAM_CLIENT_ADAPTIVE"
    SERVER_ADAPTIVE="$DATAGRAM_SERVER_ADAPTIVE"
    ;;
  stream)
    CLIENT_EGRESS_MBPS="$STREAM_CLIENT_EGRESS_MBPS"
    SERVER_EGRESS_MBPS="$STREAM_SERVER_EGRESS_MBPS"
    CLIENT_ADAPTIVE="$STREAM_CLIENT_ADAPTIVE"
    SERVER_ADAPTIVE="$STREAM_SERVER_ADAPTIVE"
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
  local egress_mbps="$3"
  local adaptive="$4"
  local tmp

  if [[ ! -f "$path" ]]; then
    echo "missing local config: $path" >&2
    exit 1
  fi

  tmp="$(mktemp)"
  awk \
    -v mode="$mode" \
    -v apply_presets="$APPLY_PRESETS" \
    -v egress_mbps="$egress_mbps" \
    -v adaptive="$adaptive" '
    BEGIN {
      changed_transport = 0
      changed_egress = 0
      changed_adaptive = 0
    }
    /^vpn_transport[[:space:]]*=/ {
      print "vpn_transport = \"" mode "\""
      changed_transport = 1
      next
    }
    /^egress_target_mbps[[:space:]]*=/ && apply_presets == "1" {
      print "egress_target_mbps = " egress_mbps
      changed_egress = 1
      next
    }
    /^adaptive_egress[[:space:]]*=/ && apply_presets == "1" {
      print "adaptive_egress = " adaptive
      changed_adaptive = 1
      next
    }
    { print }
    END {
      if (!changed_transport) {
        print "vpn_transport = \"" mode "\""
      }
      if (apply_presets == "1" && !changed_egress) {
        print "egress_target_mbps = " egress_mbps
      }
      if (apply_presets == "1" && !changed_adaptive) {
        print "adaptive_egress = " adaptive
      }
    }
  ' "$path" > "$tmp"
  mv "$tmp" "$path"
}

if [[ "$REMOTE_ONLY" != "1" ]]; then
  set_local_mode "$CONFIG_PATH" "$MODE" "$CLIENT_EGRESS_MBPS" "$CLIENT_ADAPTIVE"
  if [[ "$APPLY_PRESETS" == "1" ]]; then
    echo "local $CONFIG_PATH: vpn_transport=$MODE egress_target_mbps=$CLIENT_EGRESS_MBPS adaptive_egress=$CLIENT_ADAPTIVE"
  else
    echo "local $CONFIG_PATH: vpn_transport=$MODE"
  fi
fi

if [[ "$LOCAL_ONLY" != "1" ]]; then
  ssh -i "$KEY" "$HOST" "MODE='$MODE' REMOTE_CONFIG='$REMOTE_CONFIG' APPLY_PRESETS='$APPLY_PRESETS' SERVER_EGRESS_MBPS='$SERVER_EGRESS_MBPS' SERVER_ADAPTIVE='$SERVER_ADAPTIVE' bash -s" <<'REMOTE'
set -euo pipefail
backup="${REMOTE_CONFIG}.bak.$(date +%Y%m%d-%H%M%S)"
sudo cp "$REMOTE_CONFIG" "$backup"
tmp="$(mktemp)"
sudo awk \
  -v mode="$MODE" \
  -v apply_presets="$APPLY_PRESETS" \
  -v egress_mbps="$SERVER_EGRESS_MBPS" \
  -v adaptive="$SERVER_ADAPTIVE" '
  BEGIN {
    changed_transport = 0
    changed_egress = 0
    changed_adaptive = 0
  }
  /^vpn_transport[[:space:]]*=/ {
    print "vpn_transport = \"" mode "\""
    changed_transport = 1
    next
  }
  /^egress_target_mbps[[:space:]]*=/ && apply_presets == "1" {
    print "egress_target_mbps = " egress_mbps
    changed_egress = 1
    next
  }
  /^adaptive_egress[[:space:]]*=/ && apply_presets == "1" {
    print "adaptive_egress = " adaptive
    changed_adaptive = 1
    next
  }
  { print }
  END {
    if (!changed_transport) {
      print "vpn_transport = \"" mode "\""
    }
    if (apply_presets == "1" && !changed_egress) {
      print "egress_target_mbps = " egress_mbps
    }
    if (apply_presets == "1" && !changed_adaptive) {
      print "adaptive_egress = " adaptive
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
