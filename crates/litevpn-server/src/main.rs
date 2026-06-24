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
    quic::{ensure_datagram_capacity, pump_quic_to_tun, pump_tun_to_quic},
    tun::{TunDevice, TunOptions, create_tun},
};
use quinn::{Connection, Endpoint};
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
    let endpoint = Endpoint::server(server_config, listen)?;
    let active = Arc::new(Mutex::new(None));
    let next_client_id = Arc::new(AtomicU64::new(1));

    info!(
        listen = %listen,
        tun = %device_name,
        mtu = config.mtu,
        external_interface = %config.external_interface,
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

        tokio::spawn(async move {
            let mut client_id = None;
            let result = handle_connection(
                incoming,
                device,
                token,
                mtu,
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
    } = mode
    {
        return run_bench(connection, direction, duration_secs, payload_bytes).await;
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

    let up = pump_tun_to_quic(&device, connection.clone(), mtu, "server");
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
) -> Result<()> {
    match direction {
        BenchDirection::Upload => run_upload_bench(connection, duration_secs, payload_bytes).await,
        BenchDirection::Download => {
            run_download_bench(connection, duration_secs, payload_bytes).await
        }
    }
}

async fn run_upload_bench(
    connection: Connection,
    duration_secs: u64,
    payload_bytes: usize,
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
        bytes,
        packets,
        payload_bytes,
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
    let deadline_timer = sleep_until(deadline);
    tokio::pin!(deadline_timer);

    loop {
        tokio::select! {
            _ = &mut deadline_timer => {
                break;
            }
            result = connection.send_datagram_wait(payload.clone()) => {
                result?;
                packets += 1;
                bytes += payload_bytes as u64;
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
        bytes,
        packets,
        payload_bytes,
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
    bytes: u64,
    packets: u64,
    payload_bytes: usize,
) -> Result<()> {
    let summary = format!(
        "{}direction={direction} bytes={bytes} packets={packets} payload_bytes={payload_bytes} elapsed_ms={}\n",
        std::str::from_utf8(BENCH_SUMMARY_MAGIC).expect("ascii magic"),
        elapsed.as_millis()
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
