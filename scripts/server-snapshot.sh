#!/usr/bin/env bash
set -euo pipefail

HOST="${HOST:-ubuntu@YOUR_SERVER_IP}"
KEY="${KEY:-}"
IFACE="${IFACE:-ens3}"
PORT="${PORT:-443}"

SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=10)
if [[ -n "$KEY" ]]; then
  SSH_OPTS=(-i "$KEY" "${SSH_OPTS[@]}")
fi

ssh "${SSH_OPTS[@]}" "$HOST" "IFACE='$IFACE' PORT='$PORT' bash -s" <<'REMOTE'
set -euo pipefail

echo "== litevpn server snapshot =="
date -Is
uname -srmo
echo

echo "== service =="
systemctl is-active litevpn-server || true
systemctl show litevpn-server --property=MainPID,ActiveState,SubState,NRestarts,MemoryCurrent,CPUUsageNSec --no-pager || true
ps -C litevpn-server -o pid,pcpu,pmem,rss,etime,cmd --no-headers || true
echo

echo "== cpu =="
mpstat 1 1 || true
echo

echo "== process io/cpu =="
pidstat -C litevpn-server -rud 1 1 || true
echo

echo "== udp sockets =="
ss -u -a -n -i | grep -E "(:${PORT}|State|Recv-Q|Send-Q)" || true
echo

echo "== nstat udp/ip =="
nstat -az | awk '/^(Udp|UdpLite|Ip|IpExt)/ { print }' || true
echo

echo "== link stats ${IFACE} =="
ip -s link show "$IFACE" || true
echo

echo "== link stats tun0 =="
ip -s link show tun0 || true
echo

echo "== ethtool drops/errors ${IFACE} =="
ethtool -S "$IFACE" 2>/dev/null | awk '/(^NIC|rx|tx|drop|err|timeout|miss|coll|fifo|queue)/ { print }' || true
echo

echo "== selected sysctl =="
sysctl net.core.rmem_max net.core.wmem_max net.core.netdev_max_backlog net.ipv4.udp_rmem_min net.ipv4.udp_wmem_min net.ipv4.ip_forward 2>/dev/null || true
REMOTE
