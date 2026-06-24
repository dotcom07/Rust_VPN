use std::net::{SocketAddr, UdpSocket};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use quinn::Connection;
use socket2::{Domain, Protocol, Socket, Type};
use tracing::{debug, trace};
use tun_rs::AsyncDevice;

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
) -> Result<()> {
    let mut buf = vec![0_u8; mtu + 64];
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
