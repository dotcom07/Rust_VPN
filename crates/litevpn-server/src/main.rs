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
    config::{ServerConfig, load_token, load_toml},
    crypto,
    quic::{
        DatagramBacklog, connection_stats_summary, create_udp_socket, ensure_datagram_capacity,
        pump_quic_to_tun, pump_tun_to_quic,
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

        tokio::spawn(async move {
            let mut client_id = None;
            let result = handle_connection(
                incoming,
                device,
                token,
                mtu,
                egress_target_mbps,
                datagram_backlog_packets,
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
        )
        .await;
    }

    ensure_datagram_capacity(&connection, mtu, "server")?;

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

    let up = pump_tun_to_quic(
        &device,
        connection.clone(),
        mtu,
        "server",
        egress_target_mbps,
        datagram_backlog_packets,
    );
    let down = pump_quic_to_tun(&device, connection.clone(), "server");

    tokio::select! {
        result = up => result?,
        result = down => result?,
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
    let target_bytes_per_sec = target_bytes_per_sec(target_mbps);
    let burst_bytes = target_burst_bytes(target_bytes_per_sec, payload_bytes);
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
                pace_to_target(started, bytes, target_bytes_per_sec, burst_bytes).await;
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

fn target_bytes_per_sec(target_mbps: Option<u64>) -> Option<f64> {
    target_mbps.map(|mbps| mbps as f64 * 1_000_000.0 / 8.0)
}

fn target_burst_bytes(target_bytes_per_sec: Option<f64>, payload_bytes: usize) -> u64 {
    target_bytes_per_sec
        .map(|bytes_per_sec| (bytes_per_sec * 0.010).max(payload_bytes as f64).ceil() as u64)
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
