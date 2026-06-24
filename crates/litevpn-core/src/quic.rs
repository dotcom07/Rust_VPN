use std::net::{SocketAddr, UdpSocket};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use quinn::Connection;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::time::{Duration, Instant, sleep, sleep_until};
use tracing::{debug, trace};
use tun_rs::AsyncDevice;

pub const DEFAULT_DATAGRAM_BACKLOG_PACKETS: u64 = 64;

pub struct DatagramBacklog {
    baseline_tx_datagrams: u64,
    queued_datagrams: u64,
    max_backlog_packets: u64,
}

impl DatagramBacklog {
    pub fn new(connection: &Connection) -> Self {
        Self {
            baseline_tx_datagrams: connection.stats().frame_tx.datagram,
            queued_datagrams: 0,
            max_backlog_packets: DEFAULT_DATAGRAM_BACKLOG_PACKETS,
        }
    }

    pub async fn queued(&mut self, connection: &Connection) -> Result<()> {
        self.queued_datagrams = self.queued_datagrams.saturating_add(1);
        self.wait(connection).await
    }

    async fn wait(&self, connection: &Connection) -> Result<()> {
        loop {
            let transmitted = connection
                .stats()
                .frame_tx
                .datagram
                .saturating_sub(self.baseline_tx_datagrams);
            let backlog = self.queued_datagrams.saturating_sub(transmitted);
            if backlog <= self.max_backlog_packets {
                return Ok(());
            }

            tokio::select! {
                _ = sleep(Duration::from_millis(2)) => {}
                reason = connection.closed() => {
                    bail!("connection closed while waiting for datagram backlog: {reason}");
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
) -> Result<()> {
    let mut buf = vec![0_u8; mtu + 64];
    let started = Instant::now();
    let mut bytes = 0_u64;
    let target_bytes_per_sec = target_bytes_per_sec(egress_target_mbps);
    let burst_bytes = target_burst_bytes(target_bytes_per_sec, mtu);
    let mut datagram_backlog = DatagramBacklog::new(&connection);
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
        bytes += n as u64;
        pace_to_target(started, bytes, target_bytes_per_sec, burst_bytes).await;
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

async fn pace_to_target(
    started: Instant,
    bytes: u64,
    target_bytes_per_sec: Option<f64>,
    burst_bytes: u64,
) {
    let Some(target_bytes_per_sec) = target_bytes_per_sec else {
        return;
    };
    if bytes <= burst_bytes {
        return;
    }
    let elapsed = started.elapsed().as_secs_f64();
    let allowed_bytes = elapsed * target_bytes_per_sec + burst_bytes as f64;
    if bytes as f64 <= allowed_bytes {
        return;
    }
    let target_elapsed =
        Duration::from_secs_f64((bytes - burst_bytes) as f64 / target_bytes_per_sec);
    let target_time = started + target_elapsed;
    if target_time > Instant::now() {
        sleep_until(target_time).await;
    }
}
