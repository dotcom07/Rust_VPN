use std::net::{SocketAddr, UdpSocket};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use quinn::Connection;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::time::{Duration, Instant, sleep, sleep_until};
use tracing::{debug, trace};
use tun_rs::AsyncDevice;

pub struct DatagramBacklog {
    baseline_tx_datagrams: u64,
    queued_datagrams: u64,
    max_backlog_packets: u64,
}

pub struct EgressPacer {
    max_bytes_per_sec: Option<f64>,
    current_bytes_per_sec: Option<f64>,
    min_bytes_per_sec: Option<f64>,
    burst_bytes: u64,
    epoch_started: Instant,
    epoch_bytes: u64,
    adaptive: bool,
    last_adjust: Instant,
    last_lost_packets: Option<u64>,
    last_congestion_events: Option<u64>,
}

impl EgressPacer {
    pub fn new(target_mbps: u64, packet_bytes: usize, adaptive: bool) -> Self {
        let max_bytes_per_sec = target_bytes_per_sec(target_mbps);
        let min_bytes_per_sec = max_bytes_per_sec.map(|value| value * 0.50);
        let now = Instant::now();
        Self {
            max_bytes_per_sec,
            current_bytes_per_sec: max_bytes_per_sec,
            min_bytes_per_sec,
            burst_bytes: target_burst_bytes(max_bytes_per_sec, packet_bytes),
            epoch_started: now,
            epoch_bytes: 0,
            adaptive,
            last_adjust: now,
            last_lost_packets: None,
            last_congestion_events: None,
        }
    }

    pub fn from_optional(target_mbps: Option<u64>, packet_bytes: usize, adaptive: bool) -> Self {
        Self::new(target_mbps.unwrap_or(0), packet_bytes, adaptive)
    }

    pub async fn record_and_wait(&mut self, packet_bytes: usize, connection: Option<&Connection>) {
        self.epoch_bytes = self.epoch_bytes.saturating_add(packet_bytes as u64);

        if self.adaptive {
            if let Some(connection) = connection {
                self.adjust(connection);
            }
        }

        self.wait().await;
    }

    fn adjust(&mut self, connection: &Connection) {
        let Some(current) = self.current_bytes_per_sec else {
            return;
        };
        if self.last_adjust.elapsed() < Duration::from_millis(250) {
            return;
        }

        let stats = connection.stats();
        let lost_packets = stats.path.lost_packets;
        let congestion_events = stats.path.congestion_events;
        let Some(previous_lost_packets) = self.last_lost_packets.replace(lost_packets) else {
            self.last_congestion_events = Some(congestion_events);
            self.last_adjust = Instant::now();
            return;
        };
        let previous_congestion_events = self
            .last_congestion_events
            .replace(congestion_events)
            .unwrap_or(congestion_events);
        self.last_adjust = Instant::now();

        let next = if lost_packets > previous_lost_packets
            || congestion_events > previous_congestion_events
        {
            let min = self.min_bytes_per_sec.unwrap_or(current);
            (current * 0.85).max(min)
        } else {
            let max = self.max_bytes_per_sec.unwrap_or(current);
            (current * 1.02).min(max)
        };

        if (next - current).abs() > f64::EPSILON {
            self.current_bytes_per_sec = Some(next);
            self.reset_epoch();
            debug!(
                adaptive_egress_mbps = next * 8.0 / 1_000_000.0,
                lost_packets, congestion_events, "adjusted egress pacer"
            );
        }
    }

    fn reset_epoch(&mut self) {
        self.epoch_started = Instant::now();
        self.epoch_bytes = 0;
    }

    async fn wait(&self) {
        let Some(current_bytes_per_sec) = self.current_bytes_per_sec else {
            return;
        };
        if self.epoch_bytes <= self.burst_bytes {
            return;
        }

        let elapsed = self.epoch_started.elapsed().as_secs_f64();
        let allowed_bytes = elapsed * current_bytes_per_sec + self.burst_bytes as f64;
        if self.epoch_bytes as f64 <= allowed_bytes {
            return;
        }
        let target_elapsed = Duration::from_secs_f64(
            (self.epoch_bytes - self.burst_bytes) as f64 / current_bytes_per_sec,
        );
        let target_time = self.epoch_started + target_elapsed;
        if target_time > Instant::now() {
            sleep_until(target_time).await;
        }
    }
}

impl DatagramBacklog {
    pub fn new(connection: &Connection, max_backlog_packets: u64) -> Self {
        Self {
            baseline_tx_datagrams: connection.stats().frame_tx.datagram,
            queued_datagrams: 0,
            max_backlog_packets,
        }
    }

    pub async fn queued(&mut self, connection: &Connection) -> Result<()> {
        self.queued_datagrams = self.queued_datagrams.saturating_add(1);
        self.wait_until(connection, None).await.map(|_| ())
    }

    pub async fn queued_until(
        &mut self,
        connection: &Connection,
        deadline: Instant,
    ) -> Result<bool> {
        self.queued_datagrams = self.queued_datagrams.saturating_add(1);
        self.wait_until(connection, Some(deadline)).await
    }

    async fn wait_until(&self, connection: &Connection, deadline: Option<Instant>) -> Result<bool> {
        if self.max_backlog_packets == 0 {
            return Ok(true);
        }

        loop {
            let transmitted = connection
                .stats()
                .frame_tx
                .datagram
                .saturating_sub(self.baseline_tx_datagrams);
            let backlog = self.queued_datagrams.saturating_sub(transmitted);
            if backlog <= self.max_backlog_packets {
                return Ok(true);
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(false);
            }

            if let Some(deadline) = deadline {
                tokio::select! {
                    _ = sleep(Duration::from_millis(2)) => {}
                    _ = sleep_until(deadline) => {
                        return Ok(false);
                    }
                    reason = connection.closed() => {
                        bail!("connection closed while waiting for datagram backlog: {reason}");
                    }
                }
            } else {
                tokio::select! {
                    _ = sleep(Duration::from_millis(2)) => {}
                    reason = connection.closed() => {
                        bail!("connection closed while waiting for datagram backlog: {reason}");
                    }
                }
            }
        }
    }
}

pub fn create_udp_socket(
    addr: SocketAddr,
    recv_buffer_bytes: usize,
    send_buffer_bytes: usize,
) -> Result<UdpSocket> {
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))
        .context("failed to create UDP socket")?;
    if addr.is_ipv6() {
        if let Err(error) = socket.set_only_v6(false) {
            debug!(%error, "unable to make UDP socket dual-stack");
        }
    }
    if recv_buffer_bytes > 0 {
        socket
            .set_recv_buffer_size(recv_buffer_bytes)
            .context("failed to set UDP receive buffer size")?;
    }
    if send_buffer_bytes > 0 {
        socket
            .set_send_buffer_size(send_buffer_bytes)
            .context("failed to set UDP send buffer size")?;
    }
    socket
        .bind(&addr.into())
        .with_context(|| format!("failed to bind UDP socket to {addr}"))?;
    Ok(socket.into())
}

pub fn ensure_datagram_capacity(
    connection: &Connection,
    mtu: usize,
    label: &'static str,
) -> Result<()> {
    if let Some(max) = connection.max_datagram_size() {
        if max < mtu {
            bail!("{label}: QUIC datagram capacity {max} is below configured tunnel MTU {mtu}");
        }
    }
    Ok(())
}

pub fn connection_stats_summary(connection: &Connection) -> String {
    let stats = connection.stats();
    format!(
        "udp_tx_datagrams={} udp_tx_bytes={} udp_rx_datagrams={} udp_rx_bytes={} lost_packets={} lost_bytes={} congestion_events={} cwnd={} rtt_ms={} current_mtu={} frame_tx_datagram={} frame_rx_datagram={}",
        stats.udp_tx.datagrams,
        stats.udp_tx.bytes,
        stats.udp_rx.datagrams,
        stats.udp_rx.bytes,
        stats.path.lost_packets,
        stats.path.lost_bytes,
        stats.path.congestion_events,
        stats.path.cwnd,
        stats.path.rtt.as_millis(),
        stats.path.current_mtu,
        stats.frame_tx.datagram,
        stats.frame_rx.datagram,
    )
}

pub async fn pump_tun_to_quic(
    device: &AsyncDevice,
    connection: Connection,
    mtu: usize,
    label: &'static str,
    egress_target_mbps: u64,
    datagram_backlog_packets: u64,
    adaptive_egress: bool,
) -> Result<()> {
    let mut buf = vec![0_u8; mtu + 64];
    let mut pacer = EgressPacer::new(egress_target_mbps, mtu, adaptive_egress);
    let mut datagram_backlog = DatagramBacklog::new(&connection, datagram_backlog_packets);
    loop {
        let n = tokio::select! {
            result = device.recv(&mut buf) => result?,
            reason = connection.closed() => {
                bail!("{label}: connection closed while waiting for tun packet: {reason}");
            }
        };
        if n == 0 {
            continue;
        }
        if let Some(max) = connection.max_datagram_size() {
            if n > max {
                debug!(
                    label,
                    packet_bytes = n,
                    max_datagram_bytes = max,
                    "dropping oversized packet"
                );
                continue;
            }
        }
        trace!(label, packet_bytes = n, "tun -> quic");
        connection
            .send_datagram_wait(Bytes::copy_from_slice(&buf[..n]))
            .await?;
        datagram_backlog.queued(&connection).await?;
        pacer.record_and_wait(n, Some(&connection)).await;
    }
}

pub async fn pump_quic_to_tun(
    device: &AsyncDevice,
    connection: Connection,
    label: &'static str,
) -> Result<()> {
    loop {
        let packet = connection.read_datagram().await?;
        let written = device.send(&packet).await?;
        if written != packet.len() {
            bail!(
                "{label}: partial tun write: wrote {written} of {} bytes",
                packet.len()
            );
        }
        trace!(label, packet_bytes = written, "quic -> tun");
    }
}

fn target_bytes_per_sec(target_mbps: u64) -> Option<f64> {
    if target_mbps == 0 {
        return None;
    }
    Some(target_mbps as f64 * 1_000_000.0 / 8.0)
}

fn target_burst_bytes(target_bytes_per_sec: Option<f64>, mtu: usize) -> u64 {
    target_bytes_per_sec
        .map(|bytes_per_sec| (bytes_per_sec * 0.010).max(mtu as f64).ceil() as u64)
        .unwrap_or(0)
}
