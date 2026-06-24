#!/usr/bin/env bash
set -euo pipefail

HOST="${HOST:-}"
KEY="${KEY:-$HOME/.ssh/your_oci_key}"
TARGET="${TARGET:-x86_64-unknown-linux-musl}"
BIN="target/$TARGET/release/litevpn-server"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  echo "Usage: HOST=ubuntu@YOUR_SERVER_IP KEY=/path/to/key scripts/install-server.sh"
  echo "Builds, copies config/secrets, prepares NAT, installs systemd, and starts LiteVPN."
  exit 0
fi

if [[ -z "$HOST" ]]; then
  echo "missing HOST; example: HOST=ubuntu@YOUR_SERVER_IP KEY=/path/to/key scripts/install-server.sh" >&2
  exit 1
fi

for file in config/server.toml config/server.crt config/server.key config/client.token; do
  test -f "$file" || {
    echo "missing $file; run litevpn-keygen and create config/server.toml first" >&2
    exit 1
  }
done

"$(dirname "$0")/build-server.sh"
test -x "$BIN"

scp -i "$KEY" \
  "$BIN" \
  config/server.toml \
  config/server.crt \
  config/server.key \
  config/client.token \
  deploy/litevpn-server.service \
  scripts/server-prepare.sh \
  "$HOST:/tmp/"

ssh -i "$KEY" "$HOST" '
  set -euo pipefail
  sudo install -m 0755 /tmp/litevpn-server /usr/local/bin/litevpn-server
  sudo install -d -m 0700 /etc/litevpn
  sudo install -m 0644 /tmp/server.toml /etc/litevpn/server.toml
  sudo install -m 0644 /tmp/server.crt /etc/litevpn/server.crt
  sudo install -m 0600 /tmp/server.key /etc/litevpn/server.key
  sudo install -m 0600 /tmp/client.token /etc/litevpn/client.token
  sudo install -d -m 0755 /usr/local/lib/litevpn
  sudo install -m 0755 /tmp/server-prepare.sh /usr/local/lib/litevpn/server-prepare.sh
  sudo /usr/local/lib/litevpn/server-prepare.sh
  sudo install -m 0644 /tmp/litevpn-server.service /etc/systemd/system/litevpn-server.service
  sudo systemctl daemon-reload
  sudo systemctl enable litevpn-server
  sudo systemctl restart litevpn-server
  sudo systemctl status litevpn-server --no-pager
'
