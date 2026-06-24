#!/usr/bin/env bash
set -euo pipefail

HOST="${HOST:-}"
KEY="${KEY:-$HOME/.ssh/your_oci_key}"
TARGET="${TARGET:-x86_64-unknown-linux-musl}"
BIN="target/$TARGET/release/litevpn-server"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  echo "Usage: HOST=ubuntu@YOUR_SERVER_IP KEY=/path/to/key scripts/deploy-server.sh"
  echo "Rebuilds and replaces only /usr/local/bin/litevpn-server on an already installed server."
  exit 0
fi

if [[ -z "$HOST" ]]; then
  echo "missing HOST; example: HOST=ubuntu@YOUR_SERVER_IP KEY=/path/to/key scripts/deploy-server.sh" >&2
  exit 1
fi

"$(dirname "$0")/build-server.sh"
test -x "$BIN"

scp -i "$KEY" "$BIN" "$HOST:/tmp/litevpn-server.new"
ssh -i "$KEY" "$HOST" '
  sudo install -m 0755 /tmp/litevpn-server.new /usr/local/bin/litevpn-server
  sudo systemctl daemon-reload
  sudo systemctl restart litevpn-server
  sudo systemctl status litevpn-server --no-pager
'
