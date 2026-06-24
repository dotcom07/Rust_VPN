# LiteVPN Optimization Log

Date: 2026-06-24
Server: `ubuntu@161.33.36.181`, OCI Osaka, `VM.Standard.E2.1.Micro`

## Current Selected Settings

- Tunnel MTU: `1300`
- QUIC initial MTU: `min(tun_mtu + 160, 1452)`
- Benchmark payload: auto, capped by `connection.max_datagram_size()` and config MTU
- Server kernel buffers: `rmem/wmem_max=16777216`, `rmem/wmem_default=1048576`, `netdev_max_backlog=4096`
- Congestion controller: `cubic`
- Explicit UDP socket buffers: disabled (`0`, OS default)
- Stable benchmark targets on current path: download `38 Mbps`, upload `14 Mbps`
- VPN egress pacing: server `38 Mbps`, client `14 Mbps` on this deployment
- Server deployment: local Rust build, replace only `/usr/local/bin/litevpn-server`

## Why 1300 Is Selected

The first benchmark exposed a mismatch: config MTU was `1200`, but QUIC's effective app datagram capacity was `1162`.
That meant full-size TUN packets could be dropped before this fix. Larger payloads were tested, but this server/client path was
very sensitive above `1162` in the download direction without pacing. After adding egress pacing, payloads up to `1400`
were retested. `1300` is selected because it reduces packet count materially while keeping margin below the path/QUIC edge;
`1400` worked at 38 Mbps once, but failed at a higher target with `datagram too large`.

## Benchmark Results

Commands:

```sh
./target/release/litevpn-client --config config/client.toml --bench upload --bench-duration-secs 10
./target/release/litevpn-client --config config/client.toml --bench download --bench-duration-secs 10
```

| Variant | Direction | Payload | Client throughput | Server throughput / bytes | Result |
| --- | --- | ---: | ---: | ---: | --- |
| Baseline benchmark, MTU 1200 | upload | 1162 | 15.08 Mbps | 16,053,030 bytes / 11s | Works, low upload |
| Baseline benchmark, MTU 1200 | download | 1162 | 46.97 Mbps | 71,469,972 bytes / 10s | Works |
| MTU 1300 | upload | 1300 | 21.49 Mbps | 23,977,200 bytes / 11s | Upload improves |
| MTU 1300 | download | 1300 | 8.56 Mbps | 22,284,600 bytes / 10s | Not selected |
| Payload sweep | download | 1162 | 48.80 Mbps | 59,303,832 bytes / 8s | Best sweep result |
| Payload sweep | download | 1200 | 5.68 Mbps | 11,288,400 bytes / 8s | Not selected |
| Selected MTU 1162 | upload | 1162 | 38.51 Mbps | 47,525,800 bytes / 11s | Selected |
| Selected MTU 1162 | download | 1162 | 40.35 Mbps | 60,173,008 bytes / 10s | Selected |
| MTU 1162 + kernel buffers | upload | 1162 | 39.91 Mbps | 47,316,640 bytes / 11s | Keep |
| MTU 1162 + kernel buffers | download | 1162 | 45.68 Mbps | 70,992,390 bytes / 10s | Keep |
| BBR experiment | upload | 1162 | 120.56 Mbps sent | 54,346,740 bytes / 11s | Not selected; sender overran buffers |
| BBR experiment | download | 1162 | 40.08 Mbps | 72,476,264 bytes / 10s | Not selected |
| Cubic revert spot check | download | 1162 | 48.58 Mbps | 48,293,882 bytes / 6s | Keep Cubic |
| Explicit socket buffers 4MiB | upload | 1162 | 39.76 Mbps | 41,840,134 bytes / 11s | Not selected |
| Explicit socket buffers 4MiB | download | 1162 | 26.29 Mbps | 43,611,022 bytes / 10s | Not selected |
| Socket buffers reverted to OS default | download | 1162 | 40.90 Mbps | 40,717,642 bytes / 6s | Keep |
| Socket buffers reverted to OS default | upload | 1162 | 46.64 Mbps | 56,516,194 bytes / 11s | Keep |
| Stats run, high RTT path | download | 1162 | 12.84 Mbps | 20,264,118 bytes / 10s | RTT 153ms, server lost 10,721 bytes |
| Stats run, high loss path | download | 1162 | 27.37 Mbps | 47,331,746 bytes / 10s | Server lost 7,691 packets / 9,170,434 bytes |
| Stats run, high RTT path | upload | 1162 | 8.07 Mbps | 6,883,688 bytes / 11s | RTT 60ms, client congestion events 14 |
| Paced download | download | 1162 | 20.00 Mbps | 25,001,592 bytes / 10s | 0 server loss, 0 congestion |
| Paced download | download | 1162 | 30.00 Mbps | 37,504,712 bytes / 10s | 0 server loss, 0 congestion |
| Paced download | download | 1162 | 35.00 Mbps | 43,748,138 bytes / 10s | 0 server loss, 0 congestion |
| Paced download | download | 1162 | 37.98 Mbps | 47,499,074 bytes / 10s | 0 server loss, 0 congestion |
| Paced download | download | 1162 | 39.36 Mbps | 50,002,022 bytes / 10s | Server lost 678 packets, congestion 42 |
| Paced download | download | 1162 | 48.20 Mbps | 62,500,494 bytes / 10s | Server congestion 87 |
| Paced upload | upload | 1162 | 10.01 Mbps | 12,505,444 bytes / 11s | Improved by burst pacing |
| Paced upload | upload | 1162 | 15.01 Mbps | 18,755,842 bytes / 11s | Stable target |
| Paced upload | upload | 1162 | 18.03 Mbps | 19,719,140 bytes / 11s | Client congestion 14 |
| Unlimited comparison | download | 1162 | 35.14 Mbps | 53,629,786 bytes / 10s | Server lost 4,742 packets / 5,653,484 bytes |
| VPN egress pacing deploy check | download | 1162 | 38.03 Mbps | 47,546,716 bytes / 10s | Server loss 0, congestion 0 |
| VPN egress pacing deploy check | upload | 1162 | 15.01 Mbps | 18,702,390 bytes / 11s | Target reached |
| Paced MTU retest | download | 1200 | 38.01 Mbps | 47,546,400 bytes / 10s | 0 server loss, 0 congestion |
| Paced MTU retest | download | 1250 | 38.01 Mbps | 47,547,500 bytes / 10s | 0 server loss, 0 congestion |
| Paced MTU retest | download | 1300 | 38.02 Mbps | 47,547,500 bytes / 10s | 0 server loss, 0 congestion |
| Paced MTU retest | upload | 1300 | 15.01 Mbps | 18,762,900 bytes / 11s | 0 server loss |
| Post-deploy confirmation | download | 1300 | 38.01 Mbps | 47,547,500 bytes / 10s | Server loss 0, congestion 0 |
| Post-deploy confirmation | upload | 1300 | 15.01 Mbps | 18,768,100 bytes / 11s | Client/server loss 0, congestion 0 |
| Upload stability sweep | upload | 1300 | 15.01 Mbps | 18,634,200 bytes / 11s | Client loss 104 packets, congestion 5 |
| Upload stability sweep | upload | 1300 | 12.01 Mbps | 15,012,400 bytes / 11s | Server loss 0, client loss 2 packets |
| Upload stability sweep | upload | 1300 | 13.01 Mbps | 16,261,700 bytes / 11s | Server loss 0, client loss 4 packets |
| Upload stability sweep | upload | 1300 | 14.01 Mbps | 17,514,900 bytes / 11s | Server loss 0, client loss 3 packets |
| Selected upload confirmation | upload | 1300 | 14.01 Mbps | 17,514,900 bytes / 11s | Server loss 0, client loss 2 packets |
| Paced MTU retest | download | 1350 | 37.82 Mbps | 47,548,350 bytes / 10s | 0 server loss, higher RTT |
| Paced MTU retest | download | 1400 | 39.99 Mbps | 47,353,600 bytes / 10s | 0 server loss at 38 target, but edge-risk |
| Paced MTU edge check | download | 1400 | failed | n/a | `datagram too large` at 45 Mbps target |
| Paced MTU edge check | download | 1300 | 34.89 Mbps | 50,044,800 bytes / 10s | 40 Mbps target caused loss; keep 38 Mbps target |

## Code Changes In This Iteration

- Added `--bench upload|download` to isolate QUIC datagram throughput without macOS TUN/sudo routing.
- Added reliable benchmark summary reporting over QUIC bidirectional stream.
- Added deadline handling so benchmark send loops exit even under QUIC backpressure.
- Raised QUIC transport initial MTU headroom while keeping the selected TUN MTU conservative.
- Added datagram capacity checks before entering VPN mode to avoid silent oversized TUN packet drops.
- Raised Linux UDP/socket buffer ceilings and defaults in `server-prepare.sh`.
- Added configurable Quinn congestion control and tested BBR; kept Cubic for this path.
- Added explicit UDP socket buffer controls and tested 4MiB; kept OS default because throughput regressed.
- Added Quinn connection stats to benchmark output. The latest low-throughput runs showed path RTT and loss spikes, not just local CPU pressure.
- Added `--bench-target-mbps` pacing. Per-packet sleep was too coarse on macOS, so pacing uses a 10ms burst budget. Current stable benchmark targets are about 38 Mbps down and 14 Mbps up.
- Added optional VPN-mode TUN-to-QUIC egress pacing. The selected defaults are server `38 Mbps` and client `14 Mbps`; set `egress_target_mbps = 0` to disable.
- Retested larger MTUs under pacing. Selected `1300`; `1400` is too close to the edge.
- Made macOS route installation idempotent by deleting stale LiteVPN split-default routes before install and rolling back partial installs on failure.

## Next Candidates

- Add CPU/network counters around benchmarks: server `pidstat`, `sar`, and `ss -u`.
- Run a sudo TUN-mode browser/fast.com smoke test from macOS when an interactive password is available.
- Inspect QUIC ACK/MTU discovery settings that directly affect DATAGRAM behavior under loss.
- Compare against kernel WireGuard on the same OCI instance as the theoretical performance target.
