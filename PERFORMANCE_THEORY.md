# LiteVPN Performance Theory

Date: 2026-06-24

This note maps the main VPN performance literature and protocol specs to the
current LiteVPN implementation. The constraint is fixed hardware: one macOS
client, one small OCI Ubuntu server, and no server shape change.

## Current Position

- Selected safe default: QUIC DATAGRAM VPN data plane, MTU `1300`, server
  pacing `36 Mbps` adaptive, client pacing `13 Mbps` static.
- Best stream-packet upload signal so far: `40 Mbps` can be clean in short
  runs; higher stream targets can deliver all bytes but with heavy QUIC
  retransmission/congestion.
- OCI firewall is not the current bottleneck: UDP `443` is reachable, and
  drops correlate with path RTT/loss and sender pacing.

## What The Fastest Shape Looks Like

WireGuard is the closest practical reference design for a fast general-purpose
VPN. Its Linux data plane runs as a kernel virtual network interface, keeps the
protocol small, uses UDP, and avoids a long userspace TLS/TUN packet path. The
WireGuard paper explicitly attributes the OpenVPN gap to userspace scheduling
and repeated kernel/userspace packet copying. It also calls out queues,
parallelism, and software-fast ChaCha20-Poly1305 as key design choices.

For LiteVPN, this means the theoretical fastest version on the same server
would be close to:

1. Kernel data plane or a kernel-grade baseline such as WireGuard.
2. UDP packet tunnel, not reliable byte-stream tunneling for every packet.
3. Minimal per-packet work: no avoidable allocations, copies, syscalls, logging,
   or task wakeups.
4. Explicit pacing tied to RTT/congestion signals.
5. Bounded queues, with application-level accounting for what was delivered.

## QUIC DATAGRAM vs QUIC Stream

RFC 9221 explicitly lists VPN/IP tunneling as a use case for unreliable QUIC
datagrams. That matches DATAGRAM mode: IP packets are already packetized and
some loss is normally preferable to head-of-line blocking.

The important catch is that QUIC DATAGRAM has no explicit flow control and may
be delayed or dropped by the sender when the congestion controller does not
allow transmission. That is exactly why LiteVPN now has:

- `datagram_backlog_packets`
- egress pacing
- benchmark delivery-gap checks
- client/server QUIC loss and congestion counters

QUIC streams solve delivery gaps by retransmitting, and our `stream-packet-*`
benchmarks prove that the path can deliver more upload bytes than DATAGRAM.
But stream mode can trade packet loss for retransmission delay and
head-of-line blocking. Therefore stream mode is a serious candidate only if
full TUN browser tests show better user-facing speed without bad latency.

## Why Pacing Matters

RFC 9002 recommends pacing in-flight QUIC packets and warns that bursts can
create short-term congestion and loss. Our measurements agree with that:
unpaced or over-targeted runs often show high loss/congestion, while selected
targets become usable when paced.

The current sweep rule should therefore prefer:

1. Highest `clean_ok` target for default settings.
2. Highest `delivery_ok` target only as an experimental candidate.
3. Rejection of high-throughput runs that have heavy loss/congestion, unless
   the test is explicitly measuring reliable stream behavior.

## Kernel Bypass And Kernel Modules

netmap and DPDK show the deeper systems lesson: high packet I/O comes from
preallocated buffers, batching, shared rings, avoiding per-packet syscalls, and
avoiding unnecessary copies. netmap reports line-rate 10 Gbit/s packet I/O on a
single low-frequency core by removing allocation, syscall, and copy overheads.
DPDK exposes the same broad design family through hugepages, mempools, rings,
poll-mode drivers, and kernel-bypass NIC access.

For this project, DPDK/netmap are not the next move:

- OCI virtual NIC access and cloud networking make true kernel-bypass awkward.
- macOS client TUN traffic still has to cross the OS network extension/TUN path.
- A custom Linux kernel module adds crash, upgrade, and signing/maintenance
  cost.
- WireGuard kernel code is open source but GPL-2.0-oriented; copying it into
  this project would impose licensing constraints.

The useful near-term takeaway is not "write a kernel module now"; it is
"imitate the data-plane discipline": batch, preallocate, reduce copies, cap
queues, and verify with delivery/loss counters.

## Next Experiments

1. Full TUN mode comparison:

   ```bash
   MODE=datagram HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/run-tun-smoke.sh
   MODE=stream HOST=ubuntu@161.33.36.181 KEY=/Users/sungje/.ssh/oracle_oci_ed25519 scripts/run-tun-smoke.sh
   ```

   Measure Fast.com download/upload and interactive latency. Keep DATAGRAM if
   stream improves upload but makes loaded latency much worse.

2. Stream clean envelope:

   ```bash
   DIRECTION=stream-packet-upload TARGETS="20 30 40 50 60" DURATION=5 RUNS=2 scripts/bench-sweep.sh
   DIRECTION=stream-packet-download TARGETS="36 40 45 50" DURATION=5 RUNS=2 scripts/bench-sweep.sh
   ```

   Pick clean targets only; keep delivery-only targets as diagnostics.

3. DATAGRAM repair candidate:

   Add a tiny optional repair/FEC layer for selected packet classes instead of
   moving the whole VPN to reliable streams. Success criterion: fewer upload
   delivery gaps than DATAGRAM with lower loaded latency than stream mode.

4. Data-plane overhead reduction:

   Add buffer reuse and small batch loops around TUN read/write and QUIC send
   paths. Success criterion: same clean target with lower CPU or a higher clean
   target without increasing loss/congestion.

5. WireGuard baseline:

   Install WireGuard alongside LiteVPN only for a controlled benchmark, then
   compare Fast.com and `iperf3`-style throughput. This gives the practical
   upper bound for the fixed OCI server.

## Sources

- WireGuard paper: https://www.wireguard.com/papers/wireguard.pdf
- WireGuard official repositories: https://www.wireguard.com/repositories/
- WireGuard Linux source tree: https://git.zx2c4.com/wireguard-linux/tree/drivers/net/wireguard
- QUIC transport, RFC 9000: https://www.rfc-editor.org/rfc/rfc9000.html
- QUIC loss detection and congestion control, RFC 9002: https://www.rfc-editor.org/rfc/rfc9002.html
- QUIC DATAGRAM, RFC 9221: https://www.rfc-editor.org/rfc/rfc9221.html
- netmap paper: https://www.usenix.org/system/files/conference/atc12/atc12-final186.pdf
- DPDK overview: https://www.dpdk.org/about/
