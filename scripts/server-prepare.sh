#!/usr/bin/env bash
set -euo pipefail

IFACE="${IFACE:-ens3}"
VPN_CIDR="${VPN_CIDR:-10.66.0.0/24}"
PORT="${PORT:-443}"
SUDO="sudo"

if [[ "$(id -u)" -eq 0 ]]; then
  SUDO=""
fi

$SUDO mkdir -p /etc/litevpn
$SUDO chmod 700 /etc/litevpn
$SUDO modprobe tun

$SUDO tee /etc/sysctl.d/99-litevpn.conf >/dev/null <<'SYSCTL'
net.ipv4.ip_forward=1
net.core.rmem_max=16777216
net.core.wmem_max=16777216
net.core.rmem_default=1048576
net.core.wmem_default=1048576
net.core.netdev_max_backlog=4096
net.ipv4.udp_rmem_min=16384
net.ipv4.udp_wmem_min=16384
SYSCTL

$SUDO sysctl -w \
  net.ipv4.ip_forward=1 \
  net.core.rmem_max=16777216 \
  net.core.wmem_max=16777216 \
  net.core.rmem_default=1048576 \
  net.core.wmem_default=1048576 \
  net.core.netdev_max_backlog=4096 \
  net.ipv4.udp_rmem_min=16384 \
  net.ipv4.udp_wmem_min=16384 >/dev/null

if command -v nft >/dev/null 2>&1; then
  $SUDO nft list table inet litevpn >/dev/null 2>&1 || $SUDO nft add table inet litevpn
  $SUDO nft list chain inet litevpn postrouting >/dev/null 2>&1 || \
    $SUDO nft add chain inet litevpn postrouting '{ type nat hook postrouting priority srcnat; policy accept; }'
  $SUDO nft list chain inet litevpn forward >/dev/null 2>&1 || \
    $SUDO nft add chain inet litevpn forward '{ type filter hook forward priority filter; policy accept; }'
  $SUDO nft add rule inet litevpn postrouting ip saddr "$VPN_CIDR" oifname "$IFACE" masquerade 2>/dev/null || true
  $SUDO nft add rule inet litevpn forward iifname "tun0" oifname "$IFACE" accept 2>/dev/null || true
  $SUDO nft add rule inet litevpn forward iifname "$IFACE" oifname "tun0" ct state related,established accept 2>/dev/null || true
else
  $SUDO iptables -t nat -C POSTROUTING -s "$VPN_CIDR" -o "$IFACE" -j MASQUERADE 2>/dev/null || \
    $SUDO iptables -t nat -A POSTROUTING -s "$VPN_CIDR" -o "$IFACE" -j MASQUERADE
  $SUDO iptables -C FORWARD -i tun0 -o "$IFACE" -j ACCEPT 2>/dev/null || \
    $SUDO iptables -A FORWARD -i tun0 -o "$IFACE" -j ACCEPT
  $SUDO iptables -C FORWARD -i "$IFACE" -o tun0 -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || \
    $SUDO iptables -A FORWARD -i "$IFACE" -o tun0 -m state --state RELATED,ESTABLISHED -j ACCEPT
fi

if command -v iptables >/dev/null 2>&1; then
  insert_iptables_rule() {
    local chain="$1"
    shift
    local reject_line
    reject_line="$($SUDO iptables -L "$chain" --line-numbers 2>/dev/null | awk '$2 == "REJECT" { print $1; exit }')"
    if [[ -n "$reject_line" ]]; then
      $SUDO iptables -I "$chain" "$reject_line" "$@"
    else
      $SUDO iptables -A "$chain" "$@"
    fi
  }

  $SUDO iptables -C INPUT -p udp --dport "$PORT" -j ACCEPT 2>/dev/null || \
    insert_iptables_rule INPUT -p udp --dport "$PORT" -j ACCEPT
  $SUDO iptables -C FORWARD -i tun0 -o "$IFACE" -j ACCEPT 2>/dev/null || \
    insert_iptables_rule FORWARD -i tun0 -o "$IFACE" -j ACCEPT
  $SUDO iptables -C FORWARD -i "$IFACE" -o tun0 -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || \
    insert_iptables_rule FORWARD -i "$IFACE" -o tun0 -m state --state RELATED,ESTABLISHED -j ACCEPT

  if [[ -d /etc/iptables ]]; then
    $SUDO iptables-save | $SUDO tee /etc/iptables/rules.v4 >/dev/null
  fi
fi

if command -v ufw >/dev/null 2>&1; then
  $SUDO ufw allow "$PORT/udp" || true
fi

echo "server prepared for LiteVPN on $IFACE, UDP $PORT, CIDR $VPN_CIDR"
