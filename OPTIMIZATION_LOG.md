# LiteVPN Optimization Log

Date: 2026-06-24
Server: `ubuntu@161.33.36.181`, OCI Osaka, `VM.Standard.E2.1.Micro`

## Current Selected Settings

- Tunnel MTU: `1300`
- QUIC initial MTU: `min(tun_mtu + 160, 1452)`
- Benchmark payload: auto, capped by `connection.max_datagram_size()` and config MTU
- Server kernel buffers: `rmem/wmem_max=16777216`, `rmem/wmem_default=1048576`, `netdev_max_backlog=4096`
- Server congestion controller: `cubic`
- Explicit UDP socket buffers: disabled (`0`, OS default)
- Stable benchmark targets on current path: download `34 Mbps`, upload `13 Mbps`
- VPN egress pacing: server `34 Mbps`, client `13 Mbps` on this deployment
- Congestion controller: server `cubic`, client `bbr`
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
| Cleanup-path confirmation | download | 1300 | 38.02 Mbps | 47,548,800 bytes / 10s | Server loss 0, congestion 0 |
| Cleanup-path confirmation | upload | 1300 | 14.01 Mbps | 17,509,700 bytes / 11s | Server loss 0 |
| Repeated bench aggregate | download | 1300 | 37.97 Mbps local / 38.06 Mbps server | 57,023,200 local bytes / 57,093,400 server bytes | 2 runs, server lost 54 packets, congestion 3 |
| Repeated bench aggregate | upload | 1300 | 14.01 Mbps local / 12.74 Mbps server | 35,036,300 local bytes / 35,029,800 server bytes | 2 runs, server lost 4 packets, congestion 0 |
| Repeated target sweep | download | 1300 | 37.02 Mbps local / 37.04 Mbps server | 111,137,000 local bytes / 111,140,900 server bytes | 3 runs, server loss 0, congestion 0 |
| Repeated target sweep | download | 1300 | 38.02 Mbps local / 38.05 Mbps server | 114,140,000 local bytes / 114,140,000 server bytes | 3 runs, server loss 0, congestion 0; selected |
| Repeated target sweep | download | 1300 | 38.86 Mbps local / 39.03 Mbps server | 116,698,400 local bytes / 117,120,900 server bytes | 39 Mbps target caused 320 lost packets, congestion 25 |
| Repeated target sweep | upload | 1300 | 10.31 Mbps local / 8.02 Mbps server | 30,936,100 local bytes / 27,056,900 server bytes | 12 Mbps target had one cold-start collapse |
| Repeated target sweep | upload | 1300 | 13.01 Mbps local / 10.76 Mbps server | 39,050,700 local bytes / 36,501,400 server bytes | 3 runs, server loss 0, congestion 0; selected for stability |
| Repeated target sweep | upload | 1300 | 12.51 Mbps local / 10.04 Mbps server | 37,533,600 local bytes / 33,897,500 server bytes | 14 Mbps target had one low run |
| Repeated target sweep | upload | 1300 | 13.80 Mbps local / 11.06 Mbps server | 41,398,500 local bytes / 37,332,100 server bytes | 15 Mbps target had worse min run and client loss spikes |
| Datagram backlog cap | upload | 1300 | 13.01 Mbps local / 11.57 Mbps server | 39,050,700 local bytes / 39,048,100 server bytes | 3 runs, server loss 0, congestion 0; delivery gap fixed |
| Datagram backlog cap | upload | 1300 | 12.22 Mbps local / 10.86 Mbps server | 36,678,200 local bytes / 36,641,800 server bytes | 14 Mbps target still had low runs |
| Datagram backlog cap | download | 1300 | 37.89 Mbps local / 37.97 Mbps server | 113,991,800 local bytes / 114,146,500 server bytes | 38 Mbps target still hit loss under RTT spike |
| Datagram backlog cap | download | 1300 | 35.02 Mbps local / 35.04 Mbps server | 105,131,000 local bytes / 105,131,000 server bytes | 3 runs, server loss 0, congestion 0; selected for stability |
| Backlog value smoke | upload | 1300 | 13.02 Mbps local / 10.86 Mbps server | 16,285,100 local bytes / 16,285,100 server bytes | backlog 32, 2 runs, loss 0 |
| Backlog value smoke | upload | 1300 | 13.02 Mbps local / 10.86 Mbps server | 16,285,100 local bytes / 16,282,500 server bytes | backlog 128, 2 runs, loss 0 |
| Backlog value smoke | download | 1300 | 35.13 Mbps local / 35.07 Mbps server | 43,834,700 local bytes / 43,838,600 server bytes | backlog 32, 2 runs, loss 0 |
| Backlog value smoke | download | 1300 | 35.02 Mbps local / 35.06 Mbps server | 43,834,700 local bytes / 43,841,200 server bytes | backlog 128, 2 runs, loss 0 |
| Server counter check | mixed | 1300 | download 35.08 Mbps local / 35.07 Mbps server; upload 12.86 Mbps local / 10.66 Mbps server | UDP error counters unchanged | Concurrent selected-target stress; NIC drops/errors stayed 0 |
| Measured elapsed summary | upload | 1300 | 13.02 Mbps local / 13.03 Mbps server | 16,283,800 local bytes / 16,283,800 server bytes | Server summary now separates `elapsed_ms` from `measured_elapsed_ms` |
| Selected harness check | mixed | 1300 | download 35.03 Mbps local / 35.06 Mbps server; upload 12.35 Mbps local / 12.35 Mbps server | UDP error counters unchanged | `scripts/bench-selected.sh`, 5s x2; no summary timeout after deadline-aware backlog fix |
| Upload reselection | upload | 1300 | 12.02 Mbps local / 12.03 Mbps server | 22,548,500 bytes / 15s | 3 runs, server/client loss 0, congestion 0; CUBIC-only comparison |
| Client BBR reselection | mixed | 1300 | download 34.03 Mbps local / 34.06 Mbps server; upload 13.02 Mbps local / 13.02 Mbps server | 42,588,000 download server bytes, 16,269,500 upload server bytes | 5s x2 after redeploy; server loss/congestion 0 in both directions; selected |
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
- Added configurable Quinn congestion control. Early BBR overran before pacing/backlog, but after those fixes the selected path is server Cubic and client BBR.
- Added explicit UDP socket buffer controls and tested 4MiB; kept OS default because throughput regressed.
- Added Quinn connection stats to benchmark output. The latest low-throughput runs showed path RTT and loss spikes, not just local CPU pressure.
- Added `--bench-target-mbps` pacing. Per-packet sleep was too coarse on macOS, so pacing uses a 10ms burst budget. Current stable benchmark targets are about 34 Mbps down and 13 Mbps up.
- Added optional VPN-mode TUN-to-QUIC egress pacing. The selected defaults are server `34 Mbps` and client `13 Mbps`; set `egress_target_mbps = 0` to disable.
- Retested larger MTUs under pacing. Selected `1300`; `1400` is too close to the edge.
- Made macOS route installation idempotent by deleting stale LiteVPN split-default routes before install and rolling back partial installs on failure.
- Ensured client VPN mode still runs macOS route cleanup, QUIC close, and endpoint drain when either packet pump exits with an error.
- Added `--bench-runs` and parsed server-side aggregate stats so repeated tests compare local queued throughput against server-observed delivery/loss.
- Re-swept paced targets with repeated runs. Client-side BBR with pacing restored upload stability at `13 Mbps`; server remains on Cubic because download is stable there.
- Added a QUIC DATAGRAM backlog cap using `frame_tx_datagram` stats. This fixed the upload local/server delivery gap at 13 Mbps. After retesting under RTT spikes, the selected download target was lowered to `34 Mbps` for zero-loss stability.
- Made DATAGRAM backlog cap configurable as `datagram_backlog_packets`; selected default remains `64` because 32/64/128 worked around the selected targets.
- OCI networking was left unchanged because UDP `443` is reachable; the observed drops correlate with pacing/RTT rather than Security List or NSG blocking.
- Added `scripts/server-snapshot.sh` for service, CPU, UDP, NIC, and sysctl snapshots. Current selected-target stress did not increase `UdpRcvbufErrors`, `UdpSndbufErrors`, or NIC drops/errors.
- Added `measured_elapsed_ms` to server benchmark summaries so upload server Mbps excludes the extra drain window.
- Added `scripts/bench-selected.sh` to run the selected download/upload benchmarks with before/after server snapshots and local log capture.
- Made benchmark DATAGRAM backlog waits deadline-aware so a congested download run still exits and reports a summary instead of hanging until the client times out.

## Next Candidates

- Run a sudo TUN-mode browser/fast.com smoke test from macOS when an interactive password is available.
- Inspect QUIC ACK/MTU discovery settings that directly affect DATAGRAM behavior under loss.
- Compare against kernel WireGuard on the same OCI instance as the theoretical performance target.
