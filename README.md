# LiteVPN

Rust QUIC/TUN VPN for one Mac client and one small Oracle Ubuntu server.

## Local setup

```bash
cargo run -p litevpn-keygen -- --out-dir config --server-name litevpn.local
cp config/server.example.toml config/server.toml
cp config/client.example.toml config/client.toml
```

Copy these to the server:

```bash
scp -i ~/.ssh/your_oci_key \
  config/server.toml config/server.crt config/server.key config/client.token \
  ubuntu@YOUR_SERVER_IP:/tmp/
```

Install them on the server:

```bash
ssh -i ~/.ssh/your_oci_key ubuntu@YOUR_SERVER_IP
sudo mkdir -p /etc/litevpn
sudo install -m 0644 /tmp/server.toml /etc/litevpn/server.toml
sudo install -m 0644 /tmp/server.crt /etc/litevpn/server.crt
sudo install -m 0600 /tmp/server.key /etc/litevpn/server.key
sudo install -m 0600 /tmp/client.token /etc/litevpn/client.token
```

## Build

```bash
cargo build --release
scripts/build-server.sh
```

## Server

```bash
HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/install-server.sh
```

Oracle Cloud Security List or NSG must allow the selected UDP port:

```text
source: 0.0.0.0/0
protocol: UDP
destination port: 443
```

After the first install, deploy only a rebuilt server binary:

```bash
HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/deploy-server.sh
```

## Client

```bash
./target/release/litevpn-client --config config/client.toml --probe --connect-timeout-secs 10
sudo ./target/release/litevpn-client --config config/client.toml
```

Use `--no-routes` to test the tunnel without changing macOS routes.

On macOS the client removes stale LiteVPN split-default routes before connecting. If a previous run
was killed and the next probe/client run times out, clean them manually:

```bash
sudo ./target/release/litevpn-client --config config/client.toml --cleanup-routes
```

If the probe times out while the server service is active, open `443/udp` in the Oracle Cloud Security List or NSG for the instance subnet.

## Benchmarks

```bash
./target/release/litevpn-client --config config/client.toml --bench download --bench-duration-secs 10 --bench-target-mbps 36 --bench-payload-bytes 1300 --bench-runs 3
./target/release/litevpn-client --config config/client.toml --bench upload --bench-duration-secs 10 --bench-target-mbps 13 --bench-payload-bytes 1300 --bench-runs 3
```

Repeated benchmark output includes local send/receive aggregate stats and parsed server-side aggregate stats. Upload server Mbps uses `measured_elapsed_ms`, excluding the extra drain window.

`scripts/bench-sweep.sh` reports both the clean candidate and the delivery-ok candidate with the highest server-observed average Mbps. For DATAGRAM benchmarks, clean and delivery-ok currently mean server loss/congestion, client-side QUIC loss/congestion, and delivery gap checks all pass. For stream diagnostics, delivery-ok means local and server bytes match; clean additionally requires zero client/server QUIC loss and congestion events, so retransmission-heavy stream runs are not mistaken for the safest target.

`datagram_backlog_packets` caps queued QUIC DATAGRAMs that have not reached Quinn's transmit stats yet. `64` is the selected default for this path; `0` disables the cap.

`vpn_transport = "datagram"` is the selected VPN data plane. `vpn_transport = "stream"` enables the experimental reliable QUIC stream packet mode; it is useful for diagnostics and may improve delivery under loss, but can introduce head-of-line blocking.

Switch the client and server transport mode together:

```bash
MODE=stream HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/set-vpn-transport.sh
MODE=datagram HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/set-vpn-transport.sh
```

By default this also applies the tested pacing presets: DATAGRAM uses client
`13 Mbps` and server `36 Mbps`; stream uses client `40 Mbps` and server
`36 Mbps`. Set `APPLY_PRESETS=0` to change only `vpn_transport`.

For an interactive macOS TUN smoke test, use the wrapper below. It switches the
client and server, starts the local client with `sudo`, and restores DATAGRAM
when the client exits:

```bash
MODE=stream HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/run-tun-smoke.sh
```

Server runtime/network snapshot:

```bash
HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/server-snapshot.sh
```

Selected stability benchmark with before/after server snapshots:

```bash
HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/bench-selected.sh
```

Logs are written under `bench-results/`, which is intentionally ignored by git.

The performance rationale and next experiment ranking are in
[`PERFORMANCE_THEORY.md`](PERFORMANCE_THEORY.md).

Target sweep for comparing candidate pacing limits:

```bash
DIRECTION=download TARGETS="30 34 38 40" scripts/bench-sweep.sh
DIRECTION=upload TARGETS="10 12 13" scripts/bench-sweep.sh
DIRECTION=stream-upload TARGETS="13 20 40" scripts/bench-sweep.sh
DIRECTION=stream-download TARGETS="36 50" scripts/bench-sweep.sh
DIRECTION=stream-packet-upload TARGETS="20 40 60" scripts/bench-sweep.sh
DIRECTION=stream-packet-download TARGETS="36 40 50" scripts/bench-sweep.sh
```
