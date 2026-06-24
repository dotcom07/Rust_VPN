# LiteVPN Optimization Log

Date: 2026-06-24
Server: `ubuntu@161.33.36.181`, OCI Osaka, `VM.Standard.E2.1.Micro`

## Current Selected Settings

- Tunnel MTU: `1162`
- QUIC initial MTU: `min(tun_mtu + 160, 1452)`
- Benchmark payload: auto, capped by `connection.max_datagram_size()` and config MTU
- Server kernel buffers: `rmem/wmem_max=16777216`, `rmem/wmem_default=1048576`, `netdev_max_backlog=4096`
- Congestion controller: `cubic`
- Server deployment: local Rust build, replace only `/usr/local/bin/litevpn-server`

## Why 1162 Is Selected

The first benchmark exposed a mismatch: config MTU was `1200`, but QUIC's effective app datagram capacity was `1162`.
That meant full-size TUN packets could be dropped before this fix. Larger payloads were tested, but this server/client path was
very sensitive above `1162` in the download direction.

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

## Code Changes In This Iteration

- Added `--bench upload|download` to isolate QUIC datagram throughput without macOS TUN/sudo routing.
- Added reliable benchmark summary reporting over QUIC bidirectional stream.
- Added deadline handling so benchmark send loops exit even under QUIC backpressure.
- Raised QUIC transport initial MTU headroom while keeping the selected TUN MTU conservative.
- Added datagram capacity checks before entering VPN mode to avoid silent oversized TUN packet drops.
- Raised Linux UDP/socket buffer ceilings and defaults in `server-prepare.sh`.
- Added configurable Quinn congestion control and tested BBR; kept Cubic for this path.

## Next Candidates

- Add CPU/network counters around benchmarks: server `pidstat`, `sar`, `ss -u`, and Quinn connection stats if available.
- Tune Linux UDP socket buffers after checking whether Quinn's endpoint construction can use a preconfigured socket.
- Compare against kernel WireGuard on the same OCI instance as the theoretical performance target.
