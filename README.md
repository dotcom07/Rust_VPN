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

If the probe times out while the server service is active, open `443/udp` in the Oracle Cloud Security List or NSG for the instance subnet.

## Benchmarks

```bash
./target/release/litevpn-client --config config/client.toml --bench download --bench-duration-secs 10 --bench-target-mbps 35 --bench-payload-bytes 1300 --bench-runs 3
./target/release/litevpn-client --config config/client.toml --bench upload --bench-duration-secs 10 --bench-target-mbps 13 --bench-payload-bytes 1300 --bench-runs 3
```

Repeated benchmark output includes local send/receive aggregate stats and parsed server-side aggregate stats.

`datagram_backlog_packets` caps queued QUIC DATAGRAMs that have not reached Quinn's transmit stats yet. `64` is the selected default for this path; `0` disables the cap.

Server runtime/network snapshot:

```bash
HOST=ubuntu@YOUR_SERVER_IP KEY=~/.ssh/your_oci_key scripts/server-snapshot.sh
```
