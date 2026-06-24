#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

HOST="${HOST:-ubuntu@161.33.36.181}"
KEY="${KEY:-/Users/sungje/.ssh/oracle_oci_ed25519}"
SERVER_ENDPOINT="${SERVER_ENDPOINT:-161.33.36.181}"
WG_PORT="${WG_PORT:-443}"
WG_DIR="${WG_DIR:-$ROOT/config/wireguard}"
WG_NAME="${WG_NAME:-wg0}"
WG_CIDR="${WG_CIDR:-10.77.0.0/24}"
SERVER_WG_IP="${SERVER_WG_IP:-10.77.0.1}"
CLIENT_WG_IP="${CLIENT_WG_IP:-10.77.0.2}"
SERVER_IFACE="${SERVER_IFACE:-ens3}"
WG_MTU="${WG_MTU:-1420}"

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  cat <<'HELP'
Usage:
  HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/setup-wireguard-baseline.sh

Creates ignored local WireGuard files under config/wireguard/ and installs
/etc/wireguard/wg0.conf on the server. It does not start the tunnel.

Environment:
  HOST=ubuntu@161.33.36.181
  KEY=/Users/sungje/.ssh/oracle_oci_ed25519
  SERVER_ENDPOINT=161.33.36.181
  WG_PORT=443
  WG_NAME=wg0
  WG_CIDR=10.77.0.0/24
  SERVER_WG_IP=10.77.0.1
  CLIENT_WG_IP=10.77.0.2
  SERVER_IFACE=ens3
  WG_MTU=1420
HELP
  exit 0
fi

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

need ssh
need scp
need wg

mkdir -p "$WG_DIR"
chmod 700 "$WG_DIR"

SERVER_PRIVATE_KEY_FILE="$WG_DIR/server.private.key"
SERVER_PUBLIC_KEY_FILE="$WG_DIR/server.public.key"
CLIENT_PRIVATE_KEY_FILE="$WG_DIR/client.private.key"
CLIENT_PUBLIC_KEY_FILE="$WG_DIR/client.public.key"
CLIENT_CONF="$WG_DIR/$WG_NAME.conf"
SERVER_CONF="$WG_DIR/$WG_NAME.server.conf"

generate_keypair() {
  local private_file="$1"
  local public_file="$2"

  if [[ ! -s "$private_file" || ! -s "$public_file" ]]; then
    umask 077
    wg genkey > "$private_file"
    wg pubkey < "$private_file" > "$public_file"
  fi
}

generate_keypair "$SERVER_PRIVATE_KEY_FILE" "$SERVER_PUBLIC_KEY_FILE"
generate_keypair "$CLIENT_PRIVATE_KEY_FILE" "$CLIENT_PUBLIC_KEY_FILE"

SERVER_PRIVATE_KEY="$(<"$SERVER_PRIVATE_KEY_FILE")"
SERVER_PUBLIC_KEY="$(<"$SERVER_PUBLIC_KEY_FILE")"
CLIENT_PRIVATE_KEY="$(<"$CLIENT_PRIVATE_KEY_FILE")"
CLIENT_PUBLIC_KEY="$(<"$CLIENT_PUBLIC_KEY_FILE")"

cat > "$CLIENT_CONF" <<EOF
[Interface]
PrivateKey = $CLIENT_PRIVATE_KEY
Address = $CLIENT_WG_IP/32
DNS = 1.1.1.1, 8.8.8.8
MTU = $WG_MTU

[Peer]
PublicKey = $SERVER_PUBLIC_KEY
Endpoint = $SERVER_ENDPOINT:$WG_PORT
AllowedIPs = 0.0.0.0/0
PersistentKeepalive = 25
EOF
chmod 600 "$CLIENT_CONF"

cat > "$SERVER_CONF" <<EOF
[Interface]
PrivateKey = $SERVER_PRIVATE_KEY
Address = $SERVER_WG_IP/24
ListenPort = $WG_PORT
MTU = $WG_MTU
PostUp = sysctl -w net.ipv4.ip_forward=1; iptables -t nat -C POSTROUTING -s $WG_CIDR -o $SERVER_IFACE -j MASQUERADE 2>/dev/null || iptables -t nat -A POSTROUTING -s $WG_CIDR -o $SERVER_IFACE -j MASQUERADE; iptables -C FORWARD -i %i -o $SERVER_IFACE -j ACCEPT 2>/dev/null || iptables -I FORWARD 1 -i %i -o $SERVER_IFACE -j ACCEPT; iptables -C FORWARD -i $SERVER_IFACE -o %i -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || iptables -I FORWARD 1 -i $SERVER_IFACE -o %i -m state --state RELATED,ESTABLISHED -j ACCEPT
PostDown = iptables -t nat -D POSTROUTING -s $WG_CIDR -o $SERVER_IFACE -j MASQUERADE 2>/dev/null || true; iptables -D FORWARD -i %i -o $SERVER_IFACE -j ACCEPT 2>/dev/null || true; iptables -D FORWARD -i $SERVER_IFACE -o %i -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || true

[Peer]
PublicKey = $CLIENT_PUBLIC_KEY
AllowedIPs = $CLIENT_WG_IP/32
EOF
chmod 600 "$SERVER_CONF"

ssh -i "$KEY" "$HOST" "sudo mkdir -p /etc/wireguard && sudo chmod 700 /etc/wireguard"
scp -i "$KEY" "$SERVER_CONF" "$HOST:/tmp/$WG_NAME.conf"
ssh -i "$KEY" "$HOST" "sudo install -m 0600 /tmp/$WG_NAME.conf /etc/wireguard/$WG_NAME.conf && rm -f /tmp/$WG_NAME.conf && sudo sysctl -w net.ipv4.ip_forward=1"

echo "WireGuard baseline config installed."
echo "local_client_config=$CLIENT_CONF"
echo "server_config=/etc/wireguard/$WG_NAME.conf"
echo "client_tunnel_ip=$CLIENT_WG_IP"
echo "server_tunnel_ip=$SERVER_WG_IP"
echo "endpoint=$SERVER_ENDPOINT:$WG_PORT"
