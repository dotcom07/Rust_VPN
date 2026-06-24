# Rust Lite VPN Plan

작성일: `2026-06-23`

참조 문서: `../AethirCheckerCLI/oracle-server-specs.md`

## 1. 목표

MacBook에서 사용할 수 있는 고성능 Rust 기반 VPN을 만든다.

초기 목표는 1인 사용 기준의 안정적인 MVP다. 서버는 Oracle Ubuntu 인스턴스에서 실행하고, 클라이언트는 macOS에서 CLI로 실행한다. 배포는 서버에서 빌드하지 않고, 로컬 Mac에서 Linux용 바이너리를 만든 뒤 서버의 기존 바이너리만 교체한다.

## 2. 확인된 서버 환경

| 항목 | 값 |
| --- | --- |
| SSH endpoint | `ubuntu@YOUR_SERVER_IP` |
| SSH 상태 | `22/tcp` 접속 가능 |
| SSH key | `~/.ssh/your_oci_key` |
| Hostname | `instance-20260409-1024` |
| OS | `Ubuntu 24.04.4 LTS` |
| Architecture | `x86_64` |
| OCI shape | `VM.Standard.E2.1.Micro` |
| CPU | `2 vCPU`, AMD EPYC 7551 |
| RAM | `954 MiB` |
| Root disk | `45G`, 약 `40G` 여유 |
| Interface | `ens3` |
| Private IP | `10.0.0.182/24` |
| `/dev/net/tun` | 사용 가능 |
| IP forwarding | `net.ipv4.ip_forward = 1` |

주의할 점:

- 기존에 `AethirCheckerCLI`와 keepalive cron이 돌고 있으므로 CPU와 메모리 사용량을 작게 유지한다.
- RAM이 1GB 미만이므로 서버 빌드는 하지 않는다.
- 비밀번호는 문서나 저장소에 남기지 않는다. 현재 운영 접속은 SSH key 기준으로 한다.
- 최초 메시지에는 `june` 계정도 언급됐지만, 현재 확인된 접속 계정은 `ubuntu`다. `june` 계정을 최종 운영 계정으로 쓸 경우 서버에서 별도 생성 및 authorized_keys 등록이 필요하다.

## 3. 권장 접근

커스텀 암호화 프로토콜을 직접 설계하지 않는다. VPN 구현에서 가장 위험한 부분은 암호화와 인증이므로, 검증된 Rust 생태계 라이브러리를 조합한다.

권장 MVP는 `TUN + QUIC` 구조다.

- macOS 클라이언트: `utun` 인터페이스 생성
- Linux 서버: `/dev/net/tun` 인터페이스 생성
- 전송 계층: UDP 기반 QUIC
- 암호화: QUIC의 TLS 1.3, `rustls`
- NAT: 서버의 `ens3`로 포워딩
- 인증: 초기에는 단일 클라이언트용 mTLS 또는 서버 인증서 + client token

왜 QUIC인가:

- TCP-over-TCP 문제를 피할 수 있다.
- UDP 기반이라 VPN 패킷 터널링에 맞다.
- TLS 1.3 암호화와 연결 재개를 활용할 수 있다.
- Rust에서는 `quinn` 생태계가 성숙한 편이다.

대안:

- `WireGuard` 호환 구현을 쓰면 성능과 안정성은 좋지만, Rust로 직접 만든 VPN이라는 성격은 약해진다.
- SOCKS5 프록시는 구현이 빠르지만 전체 시스템 트래픽 VPN이 아니다.
- Cloudflare Tunnel 같은 URL 터널링은 관리 페이지 배포에는 편하지만, 일반적인 L3 VPN 트래픽에는 맞지 않는다. 이 프로젝트에서는 직접 UDP 포트를 열고 붙는 방식을 우선한다.

## 4. 아키텍처

```text
MacBook
  litevpn-client
    - creates utun
    - captures IP packets
    - sends packets over QUIC
    - restores packets from server
        |
        | UDP 443 or UDP 51820
        v
Oracle Ubuntu server
  litevpn-server
    - creates tun0
    - receives encrypted QUIC packets
    - writes packets to tun0
    - reads response packets
    - sends packets back to client
        |
        v
  Linux IP forwarding + NAT via ens3
```

초기 네트워크 대역:

- VPN 클라이언트 IP: `10.66.0.2/24`
- VPN 서버 TUN IP: `10.66.0.1/24`
- TUN device: `tun0`
- 외부 NIC: `ens3`
- 권장 VPN port: `443/udp`

`443/udp`를 우선 추천한다. 네트워크에서 막힐 가능성이 낮고, QUIC와도 자연스럽다. 다만 Oracle Cloud Security List 또는 NSG에서 해당 UDP ingress를 열어야 한다.

## 5. Rust 구성

권장 workspace:

```text
lite_vpn/
  Cargo.toml
  crates/
    litevpn-core/
      src/
    litevpn-client/
      src/
    litevpn-server/
      src/
  config/
    client.example.toml
    server.example.toml
  deploy/
    litevpn-server.service
  rust-vpn-plan.md
```

주요 crate 후보:

| 용도 | 후보 |
| --- | --- |
| async runtime | `tokio` |
| QUIC | `quinn` |
| TLS | `rustls`, `rcgen` |
| TUN | `tun` 또는 `tun-rs` |
| CLI | `clap` |
| config | `serde`, `toml` |
| logging | `tracing`, `tracing-subscriber` |
| errors | `anyhow`, `thiserror` |
| metrics, debug | 초기에는 `tracing` 로그로 충분 |

서버는 작고 오래 떠 있어야 하므로 기본 로그 레벨은 `info`, 패킷 단위 로그는 `trace`로 둔다.

Linux TUN offload는 `recv_multiple/send_multiple`까지 함께 써야 안전하다. 초기 구현에서는 QUIC GSO, TUN tx queue, systemd FD limit을 우선 적용하고, TUN offload 기본값은 꺼둔다.

## 6. 서버 설정

초기 1회 설정은 서버에서 수행한다.

현재 서버는 `/dev/net/tun`과 `net.ipv4.ip_forward = 1`이 이미 확인됐다. 아래 명령은 재설치나 새 서버 복구 시에도 다시 적용할 수 있는 기준 절차다.

```bash
sudo apt-get update
sudo apt-get install -y ca-certificates

sudo modprobe tun
echo net.ipv4.ip_forward=1 | sudo tee /etc/sysctl.d/99-litevpn.conf
sudo sysctl --system

sudo mkdir -p /etc/litevpn
sudo mkdir -p /var/lib/litevpn
```

NAT 설정 예시:

```bash
sudo iptables -t nat -A POSTROUTING -s 10.66.0.0/24 -o ens3 -j MASQUERADE
sudo iptables -A FORWARD -i tun0 -o ens3 -j ACCEPT
sudo iptables -A FORWARD -i ens3 -o tun0 -m state --state RELATED,ESTABLISHED -j ACCEPT
```

운영에서는 `iptables-persistent` 또는 `nftables`로 영구화한다.

Oracle Cloud에서도 `443/udp` 또는 선택한 VPN 포트를 ingress로 열어야 한다.

## 7. systemd 운영

서버 바이너리 위치:

```text
/usr/local/bin/litevpn-server
```

서버 설정 위치:

```text
/etc/litevpn/server.toml
```

systemd service 초안:

```ini
[Unit]
Description=LiteVPN Server
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/litevpn-server --config /etc/litevpn/server.toml
Restart=always
RestartSec=3
User=root
Environment=RUST_LOG=info
LimitNOFILE=1048576

[Install]
WantedBy=multi-user.target
```

초기에는 root 실행이 단순하다. 이후 안정화되면 `CAP_NET_ADMIN`, `CAP_NET_BIND_SERVICE`만 부여하는 방식으로 줄인다.

## 8. 로컬 빌드 및 배포 전략

서버에서 컴파일하지 않는다. Mac에서 Linux x86_64 바이너리를 만들고 서버에 복사한 뒤 교체한다.

Apple Silicon Mac 기준 권장 빌드 방식:

```bash
cargo install cargo-zigbuild
rustup target add x86_64-unknown-linux-musl
cargo zigbuild --release --target x86_64-unknown-linux-musl -p litevpn-server
```

배포:

```bash
scp -i ~/.ssh/your_oci_key \
  target/x86_64-unknown-linux-musl/release/litevpn-server \
  ubuntu@YOUR_SERVER_IP:/tmp/litevpn-server.new

ssh -i ~/.ssh/your_oci_key ubuntu@YOUR_SERVER_IP '
  sudo install -m 0755 /tmp/litevpn-server.new /usr/local/bin/litevpn-server &&
  sudo systemctl restart litevpn-server &&
  sudo systemctl status litevpn-server --no-pager
'
```

Mac 클라이언트 빌드:

```bash
cargo build --release -p litevpn-client
```

Mac 실행은 TUN/route 조작 때문에 관리자 권한이 필요할 수 있다.

```bash
sudo ./target/release/litevpn-client --config config/client.toml
```

## 9. 설정 파일 초안

`server.toml`:

```toml
listen = "0.0.0.0:443"
tun_name = "tun0"
tun_ip = "10.66.0.1"
tun_cidr = 24
client_cidr = "10.66.0.0/24"
external_interface = "ens3"
cert_path = "/etc/litevpn/server.crt"
key_path = "/etc/litevpn/server.key"
auth_token_path = "/etc/litevpn/client.token"
```

`client.toml`:

```toml
server = "YOUR_SERVER_IP:443"
tun_name = "utun"
client_ip = "10.66.0.2"
server_ca_path = "./config/server.crt"
auth_token_path = "./config/client.token"
route_all = true
dns = ["1.1.1.1", "8.8.8.8"]
```

## 10. MVP 마일스톤

1. Workspace 생성
   - `litevpn-core`, `litevpn-client`, `litevpn-server` 패키지 생성
   - verify: `cargo test` 통과

2. QUIC 연결 MVP
   - client/server가 인증 후 연결
   - ping 메시지 왕복
   - verify: 로컬 loopback 통합 테스트

3. TUN 입출력
   - macOS `utun`, Linux `tun0` 생성
   - IP packet read/write
   - verify: 단일 ICMP packet 관찰

4. 서버 NAT
   - Linux IP forwarding과 NAT 적용
   - verify: 클라이언트에서 VPN 경유 `curl ifconfig.me`

5. 배포 자동화
   - 로컬 빌드 후 서버 바이너리 교체 스크립트
   - verify: `systemctl restart` 후 health check

6. 성능 튜닝
   - buffer size, batch read/write, logging 최소화
   - verify: `iperf3`로 throughput 측정

## 11. 검증 기준

서버 접속:

```bash
ssh -i ~/.ssh/your_oci_key ubuntu@YOUR_SERVER_IP \
  'hostname && uname -m && lsb_release -ds'
```

서비스 상태:

```bash
systemctl is-active litevpn-server
journalctl -u litevpn-server -n 100 --no-pager
```

VPN 연결 후 Mac에서 확인:

```bash
ifconfig | grep -A 8 utun
netstat -rn | head
curl https://ifconfig.me
ping -c 3 1.1.1.1
```

성능 확인:

```bash
iperf3 -s
iperf3 -c 10.66.0.1
```

## 12. 리스크와 결정 필요 사항

결정 필요:

- 최종 서버 계정을 `ubuntu`로 유지할지, `june` 계정을 만들지
- VPN 포트를 `443/udp`로 할지 `51820/udp`로 할지
- 전체 트래픽 라우팅만 할지, 특정 CIDR만 라우팅할지
- 인증을 mTLS로 갈지, 서버 인증서 + client token으로 단순화할지

리스크:

- Oracle Security List에서 UDP 포트를 열지 않으면 VPN 연결이 되지 않는다.
- 서버 RAM이 작아 무거운 dependency, 과도한 로그, 서버 빌드는 피해야 한다.
- macOS route/DNS 설정은 권한과 OS 버전에 민감하다.
- 직접 만든 VPN은 보안 리스크가 크므로 암호화와 handshake는 검증된 라이브러리에 맡겨야 한다.

## 13. 다음 작업

바로 다음 단계는 Rust workspace를 최소 구성으로 만들고, QUIC ping 왕복부터 검증하는 것이다.

권장 순서:

```text
1. Cargo workspace 생성
2. client/server CLI skeleton 작성
3. quinn 기반 ping 연결 구현
4. 로컬 통합 테스트
5. 서버 cross-compile 및 scp 배포 테스트
6. TUN 연결 추가
```
