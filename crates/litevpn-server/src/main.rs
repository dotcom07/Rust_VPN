use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result};
use bytes::Bytes;
use clap::Parser;
use litevpn_core::{
    auth::{AuthMode, BenchDirection, server_authenticate},
    config::{ServerConfig, VpnTransportMode, load_token, load_toml},
    crypto,
    quic::{
        DatagramBacklog, EgressPacer, connection_stats_summary, create_udp_socket,
        ensure_datagram_capacity, finish_stream_with_ack, pump_quic_to_tun, pump_stream_to_tun,
        pump_tun_to_quic, pump_tun_to_stream, read_stream_packet, write_stream_packet,
    },
    tun::{TunDevice, TunOptions, create_tun},
};
use quinn::{Connection, Endpoint, EndpointConfig};
use tokio::sync::Mutex;
use tokio::time::{Duration, Instant, sleep, sleep_until, timeout, timeout_at};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const BENCH_SUMMARY_MAGIC: &[u8] = b"LVPNBENCH ";

#[derive(Debug, Parser)]
struct Args {
    #[arg(short, long, default_value = "/etc/litevpn/server.toml")]
    config: PathBuf,
}

struct ActiveClient {
    id: u64,
    connection: Connection,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let config: ServerConfig = load_toml(&args.config)?;
    config.validate()?;
    let token = load_token(&config.auth_token_path)?;

    let device = create_tun(TunOptions {
        name: config.tun_name.clone(),
        address: config.tun_ip,
        prefix: config.tun_prefix,
        destination: Some(config.client_ip),
        mtu: config.mtu,
        enable_linux_offload: config.enable_linux_offload,
        tx_queue_len: config.tx_queue_len,
    })
    .context("failed to create server tun device")?;
    let device_name = device.name().unwrap_or_else(|_| config.tun_name.clone());
    let device = Arc::new(device);

    let listen: SocketAddr = config.listen.parse().context("invalid listen address")?;
    let server_config = crypto::server_config(
        &config.cert_path,
        &config.key_path,
        config.datagram_buffer_bytes,
        config.mtu,
        config.congestion_controller,
    )?;
    let socket = create_udp_socket(
        listen,
        config.udp_recv_buffer_bytes,
        config.udp_send_buffer_bytes,
    )?;
    let runtime = quinn::default_runtime().context("no async runtime found")?;
    let endpoint = Endpoint::new(
        EndpointConfig::default(),
        Some(server_config),
        socket,
        runtime,
    )?;
    let active = Arc::new(Mutex::new(None));
    let next_client_id = Arc::new(AtomicU64::new(1));

    info!(
        listen = %listen,
        tun = %device_name,
        mtu = config.mtu,
        external_interface = %config.external_interface,
        egress_target_mbps = config.egress_target_mbps,
        adaptive_egress = config.adaptive_egress,
        vpn_transport = ?config.vpn_transport,
        "litevpn server ready"
    );

    loop {
        let Some(incoming) = endpoint.accept().await else {
            continue;
        };

        let token = token.clone();
        let device = Arc::clone(&device);
        let active = Arc::clone(&active);
        let next_client_id = Arc::clone(&next_client_id);
        let mtu = config.mtu as usize;
        let egress_target_mbps = config.egress_target_mbps;
        let datagram_backlog_packets = config.datagram_backlog_packets;
        let adaptive_egress = config.adaptive_egress;
        let vpn_transport = config.vpn_transport;

        tokio::spawn(async move {
            let mut client_id = None;
            let result = handle_connection(
                incoming,
                device,
                token,
                mtu,
                egress_target_mbps,
                datagram_backlog_packets,
                adaptive_egress,
                vpn_transport,
                active.clone(),
                next_client_id,
                &mut client_id,
            )
            .await;

            if let Some(id) = client_id {
                let mut active = active.lock().await;
                let should_clear = active
                    .as_ref()
                    .map(|client| client.id == id)
                    .unwrap_or(false);
                if should_clear {
                    *active = None;
                }
            }

            if let Err(error) = result {
                warn!(%error, "client connection ended");
            }
        });
    }
}

async fn handle_connection(
    incoming: quinn::Incoming,
    device: Arc<TunDevice>,
    token: String,
    mtu: usize,
    egress_target_mbps: u64,
    datagram_backlog_packets: u64,
    adaptive_egress: bool,
    vpn_transport: VpnTransportMode,
    active: Arc<Mutex<Option<ActiveClient>>>,
    next_client_id: Arc<AtomicU64>,
    client_id: &mut Option<u64>,
) -> Result<()> {
    let connection = incoming.accept()?.await?;
    info!(remote = %connection.remote_address(), "client connected");

    let mode = timeout(
        Duration::from_secs(10),
        server_authenticate(&connection, &token),
    )
    .await
    .context("auth timed out")??;
    info!(remote = %connection.remote_address(), "client authenticated");

    if let AuthMode::Bench {
        direction,
        duration_secs,
        payload_bytes,
        target_mbps,
    } = mode
    {
        return run_bench(
            connection,
            direction,
            duration_secs,
            payload_bytes,
            target_mbps,
            datagram_backlog_packets,
            adaptive_egress,
        )
        .await;
    }

    if vpn_transport == VpnTransportMode::Datagram {
        ensure_datagram_capacity(&connection, mtu, "server")?;
    }

    let id = next_client_id.fetch_add(1, Ordering::Relaxed);
    *client_id = Some(id);
    let previous = {
        let mut active = active.lock().await;
        active.replace(ActiveClient {
            id,
            connection: connection.clone(),
        })
    };
    if let Some(previous) = previous {
        warn!(
            previous_client_id = previous.id,
            new_client_id = id,
            "replacing previous authenticated client"
        );
        previous
            .connection
            .close(0_u32.into(), b"replaced by new authenticated client");
    }

    match vpn_transport {
        VpnTransportMode::Datagram => {
            let up = pump_tun_to_quic(
                &device,
                connection.clone(),
                mtu,
                "server",
                egress_target_mbps,
                datagram_backlog_packets,
                adaptive_egress,
            );
            let down = pump_quic_to_tun(&device, connection.clone(), "server");

            tokio::select! {
                result = up => result?,
                result = down => result?,
            }
        }
        VpnTransportMode::Stream => {
            let send_stream = connection
                .open_uni()
                .await
                .context("failed to open server packet stream")?;
            let recv_stream = connection
                .accept_uni()
                .await
                .context("failed to accept client packet stream")?;
            let up = pump_tun_to_stream(
                &device,
                send_stream,
                connection.clone(),
                mtu,
                "server",
                egress_target_mbps,
                adaptive_egress,
            );
            let down = pump_stream_to_tun(&device, recv_stream, mtu + 64, "server");

            tokio::select! {
                result = up => result?,
                result = down => result?,
            }
        }
    }

    error!("unreachable pump exit");
    Ok(())
}

async fn run_bench(
    connection: Connection,
    direction: BenchDirection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
    datagram_backlog_packets: u64,
    adaptive_egress: bool,
) -> Result<()> {
    match direction {
        BenchDirection::Upload => {
            run_upload_bench(connection, duration_secs, payload_bytes, target_mbps).await
        }
        BenchDirection::Download => {
            run_download_bench(
                connection,
                duration_secs,
                payload_bytes,
                target_mbps,
                datagram_backlog_packets,
                adaptive_egress,
            )
            .await
        }
        BenchDirection::StreamUpload => {
            run_stream_upload_bench(connection, duration_secs, payload_bytes, target_mbps, false)
                .await
        }
        BenchDirection::StreamDownload => {
            run_stream_download_bench(
                connection,
                duration_secs,
                payload_bytes,
                target_mbps,
                adaptive_egress,
                false,
            )
            .await
        }
        BenchDirection::StreamPacketUpload => {
            run_stream_upload_bench(connection, duration_secs, payload_bytes, target_mbps, true)
                .await
        }
        BenchDirection::StreamPacketDownload => {
            run_stream_download_bench(
                connection,
                duration_secs,
                payload_bytes,
                target_mbps,
                adaptive_egress,
                true,
            )
            .await
        }
    }
}

async fn run_upload_bench(
    connection: Connection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
) -> Result<()> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(duration_secs + 1);
    let mut packets = 0_u64;
    let mut bytes = 0_u64;

    loop {
        let packet = match timeout_at(deadline, connection.read_datagram()).await {
            Ok(Ok(packet)) => packet,
            Ok(Err(error)) => return Err(error.into()),
            Err(_) => break,
        };
        packets += 1;
        bytes += packet.len() as u64;
    }

    send_bench_summary(
        &connection,
        "upload",
        started.elapsed(),
        Duration::from_secs(duration_secs),
        bytes,
        packets,
        payload_bytes,
        target_mbps,
    )
    .await?;
    info!(
        bytes,
        packets,
        payload_bytes,
        elapsed_ms = started.elapsed().as_millis(),
        "upload bench complete"
    );
    Ok(())
}

async fn run_download_bench(
    connection: Connection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
    datagram_backlog_packets: u64,
    adaptive_egress: bool,
) -> Result<()> {
    let requested_payload_bytes = payload_bytes;
    let payload_bytes = connection
        .max_datagram_size()
        .unwrap_or(payload_bytes)
        .min(payload_bytes);
    if payload_bytes < requested_payload_bytes {
        warn!(
            requested_payload_bytes,
            payload_bytes, "capping download bench payload to QUIC datagram capacity"
        );
    }

    let payload = Bytes::from(vec![0_u8; payload_bytes]);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(duration_secs);
    let mut packets = 0_u64;
    let mut bytes = 0_u64;
    let mut pacer = EgressPacer::from_optional(target_mbps, payload_bytes, adaptive_egress);
    let mut datagram_backlog = DatagramBacklog::new(&connection, datagram_backlog_packets);
    let deadline_timer = sleep_until(deadline);
    tokio::pin!(deadline_timer);

    loop {
        tokio::select! {
            _ = &mut deadline_timer => {
                break;
            }
            result = connection.send_datagram_wait(payload.clone()) => {
                result?;
                if !datagram_backlog.queued_until(&connection, deadline).await? {
                    break;
                }
                packets += 1;
                bytes += payload_bytes as u64;
                pacer.record_and_wait(payload_bytes, Some(&connection)).await;
            }
            reason = connection.closed() => {
                warn!(%reason, "download bench connection closed");
                break;
            }
        }
    }

    send_bench_summary(
        &connection,
        "download",
        started.elapsed(),
        started.elapsed(),
        bytes,
        packets,
        payload_bytes,
        target_mbps,
    )
    .await?;
    info!(
        bytes,
        packets,
        payload_bytes,
        elapsed_ms = started.elapsed().as_millis(),
        "download bench complete"
    );
    Ok(())
}

async fn run_stream_upload_bench(
    connection: Connection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
    framed: bool,
) -> Result<()> {
    let mut stream = connection
        .accept_uni()
        .await
        .context("failed to accept stream upload")?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(duration_secs + 1);
    let mut buf = vec![0_u8; payload_bytes];
    let mut packets = 0_u64;
    let mut bytes = 0_u64;

    if framed {
        loop {
            match timeout_at(
                deadline,
                read_stream_packet(&mut stream, &mut buf, "stream bench"),
            )
            .await
            {
                Ok(Ok(Some(0))) => {}
                Ok(Ok(Some(n))) => {
                    packets += 1;
                    bytes += n as u64;
                }
                Ok(Ok(None)) => break,
                Ok(Err(error)) => return Err(error),
                Err(_) => break,
            }
        }
    } else {
        loop {
            match timeout_at(deadline, stream.read(&mut buf)).await {
                Ok(Ok(Some(n))) => {
                    packets += 1;
                    bytes += n as u64;
                }
                Ok(Ok(None)) => break,
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => break,
            }
        }
    }

    let direction = if framed {
        "stream-packet-upload"
    } else {
        "stream-upload"
    };
    send_bench_summary(
        &connection,
        direction,
        started.elapsed(),
        Duration::from_secs(duration_secs),
        bytes,
        packets,
        payload_bytes,
        target_mbps,
    )
    .await?;
    info!(
        bytes,
        packets,
        payload_bytes,
        elapsed_ms = started.elapsed().as_millis(),
        framed,
        "stream upload bench complete"
    );
    Ok(())
}

async fn run_stream_download_bench(
    connection: Connection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
    adaptive_egress: bool,
    framed: bool,
) -> Result<()> {
    let payload = vec![0_u8; payload_bytes];
    let mut frame_header = [0_u8; 2];
    let mut stream = connection
        .open_uni()
        .await
        .context("failed to open stream download")?;
    let started = Instant::now();
    let deadline = started + Duration::from_secs(duration_secs);
    let mut packets = 0_u64;
    let mut bytes = 0_u64;
    let mut pacer = EgressPacer::from_optional(target_mbps, payload_bytes, adaptive_egress);

    loop {
        if Instant::now() >= deadline {
            break;
        }
        if framed {
            tokio::select! {
                result = write_stream_packet(&mut stream, &mut frame_header, &payload, "stream bench") => {
                    result?;
                    packets += 1;
                    bytes += payload_bytes as u64;
                    pacer
                        .record_and_wait(payload_bytes, Some(&connection))
                        .await;
                }
                reason = connection.closed() => {
                    warn!(%reason, "stream packet download bench connection closed");
                    break;
                }
            }
        } else {
            match timeout_at(deadline, stream.write(&payload)).await {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    packets += 1;
                    bytes += n as u64;
                    pacer.record_and_wait(n, Some(&connection)).await;
                }
                Ok(Err(error)) => return Err(error.into()),
                Err(_) => break,
            }
        }
    }
    finish_stream_with_ack(&mut stream, "stream download").await?;

    let direction = if framed {
        "stream-packet-download"
    } else {
        "stream-download"
    };
    send_bench_summary(
        &connection,
        direction,
        started.elapsed(),
        started.elapsed(),
        bytes,
        packets,
        payload_bytes,
        target_mbps,
    )
    .await?;
    info!(
        bytes,
        packets,
        payload_bytes,
        elapsed_ms = started.elapsed().as_millis(),
        framed,
        "stream download bench complete"
    );
    Ok(())
}

async fn send_bench_summary(
    connection: &Connection,
    direction: &str,
    elapsed: Duration,
    measured_elapsed: Duration,
    bytes: u64,
    packets: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
) -> Result<()> {
    let target = target_mbps
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unlimited".to_string());
    let summary = format!(
        "{}direction={direction} bytes={bytes} packets={packets} payload_bytes={payload_bytes} target_mbps={target} elapsed_ms={} measured_elapsed_ms={} {}\n",
        std::str::from_utf8(BENCH_SUMMARY_MAGIC).expect("ascii magic"),
        elapsed.as_millis(),
        measured_elapsed.as_millis(),
        connection_stats_summary(connection)
    );

    let (mut send, _recv) = connection
        .open_bi()
        .await
        .context("failed to open bench summary stream")?;
    send.write_all(summary.as_bytes())
        .await
        .context("failed to write bench summary")?;
    send.finish().context("failed to finish bench summary")?;

    tokio::select! {
        _ = connection.closed() => {}
        _ = sleep(Duration::from_millis(300)) => {}
    }
    Ok(())
}
