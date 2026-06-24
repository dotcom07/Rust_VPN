use anyhow::{Result, bail};
use bytes::Bytes;
use quinn::Connection;
use tracing::{debug, trace};
use tun_rs::AsyncDevice;

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
