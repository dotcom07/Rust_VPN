use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use clap::Parser;
use litevpn_core::{
    auth::{AuthMode, BenchDirection, client_authenticate_with_mode},
    config::{ClientConfig, load_token, load_toml},
    crypto,
    quic::{ensure_datagram_capacity, pump_quic_to_tun, pump_tun_to_quic},
    tun::{TunOptions, create_tun},
};
use quinn::Endpoint;
use tokio::time::{Duration, Instant, sleep, sleep_until, timeout};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "macos")]
mod macos_routes;

const BENCH_SUMMARY_MAGIC: &[u8] = b"LVPNBENCH ";

#[derive(Debug, Parser)]
struct Args {
    #[arg(short, long, default_value = "config/client.toml")]
    config: PathBuf,

    #[arg(long)]
    no_routes: bool,

    #[arg(long)]
    probe: bool,

    #[arg(long, default_value_t = 0)]
    probe_hold_secs: u64,

    #[arg(long, default_value_t = 10)]
    connect_timeout_secs: u64,

    #[arg(long, value_parser = ["upload", "download"])]
    bench: Option<String>,

    #[arg(long, default_value_t = 10)]
    bench_duration_secs: u64,

    #[arg(long, default_value_t = 0)]
    bench_payload_bytes: usize,
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let config: ClientConfig = load_toml(&args.config)?;
    config.validate()?;
    let token = load_token(&config.auth_token_path)?;

    let server_addr: SocketAddr = config.server.parse().context("invalid server address")?;
    let client_config = crypto::client_config(
        &config.ca_cert_path,
        config.datagram_buffer_bytes,
        config.mtu,
    )?;
    let bind_addr: SocketAddr = if server_addr.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let mut endpoint = Endpoint::client(bind_addr)?;
    endpoint.set_default_client_config(client_config);

    info!(server = %server_addr, server_name = %config.server_name, "connecting");
    let connecting = endpoint.connect(server_addr, &config.server_name)?;
    let connection = timeout(Duration::from_secs(args.connect_timeout_secs), connecting)
        .await
        .context("connect timed out")?
        .context("failed to connect to server")?;

    let bench_direction = args
        .bench
        .as_deref()
        .map(parse_bench_direction)
        .transpose()?;
    let bench_payload_bytes = match bench_direction {
        Some(_) => bench_payload_bytes(
            args.bench_payload_bytes,
            connection.max_datagram_size(),
            config.mtu as usize,
        )?,
        None => 0,
    };
    let auth_mode = match bench_direction {
        Some(direction) => AuthMode::Bench {
            direction,
            duration_secs: args.bench_duration_secs,
            payload_bytes: bench_payload_bytes,
        },
        None => AuthMode::Vpn,
    };
    client_authenticate_with_mode(&connection, &token, auth_mode).await?;
    info!(remote = %connection.remote_address(), "authenticated");

    if let Some(direction) = bench_direction {
        run_bench(
            &connection,
            direction,
            args.bench_duration_secs,
            bench_payload_bytes,
        )
        .await?;
        connection.close(0_u32.into(), b"bench complete");
        endpoint.wait_idle().await;
        return Ok(());
    }

    if args.probe {
        println!("LiteVPN probe OK: connected and authenticated to {server_addr}");
        if args.probe_hold_secs > 0 {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(args.probe_hold_secs)) => {}
                reason = connection.closed() => {
                    println!("LiteVPN probe connection closed while holding: {reason}");
                }
            }
        }
        connection.close(0_u32.into(), b"probe complete");
        endpoint.wait_idle().await;
        return Ok(());
    }

    ensure_datagram_capacity(&connection, config.mtu as usize, "client")?;

    let device = create_tun(TunOptions {
        name: config.tun_name.clone(),
        address: config.client_ip,
        prefix: config.tun_prefix,
        destination: Some(config.server_tun_ip),
        mtu: config.mtu,
        enable_linux_offload: false,
        tx_queue_len: None,
    })
    .context("failed to create client tun device")?;
    let device_name = device.name().unwrap_or_else(|_| config.tun_name.clone());
    info!(tun = %device_name, mtu = config.mtu, "client tun ready");

    #[cfg(target_os = "macos")]
    let mut routes = if config.route_all && !args.no_routes {
        Some(macos_routes::RouteGuard::install(server_addr, config.server_tun_ip).await?)
    } else {
        None
    };

    #[cfg(not(target_os = "macos"))]
    if config.route_all && !args.no_routes {
        tracing::warn!("automatic route installation is currently implemented only on macOS");
    }

    let device = Arc::new(device);
    let up = pump_tun_to_quic(&device, connection.clone(), config.mtu as usize, "client");
    let down = pump_quic_to_tun(&device, connection.clone(), "client");

    tokio::select! {
        result = up => result?,
        result = down => result?,
        result = shutdown_signal() => {
            result?;
            info!("shutdown requested");
        }
    }

    #[cfg(target_os = "macos")]
    if let Some(routes) = routes.as_mut() {
        routes.cleanup();
    }

    connection.close(0_u32.into(), b"client shutdown");
    endpoint.wait_idle().await;
    Ok(())
}

fn parse_bench_direction(value: &str) -> Result<BenchDirection> {
    match value {
        "upload" => Ok(BenchDirection::Upload),
        "download" => Ok(BenchDirection::Download),
        _ => bail!("bench must be upload or download"),
    }
}

fn bench_payload_bytes(
    requested: usize,
    max_datagram_size: Option<usize>,
    config_mtu: usize,
) -> Result<usize> {
    let max = max_datagram_size.unwrap_or(config_mtu).min(config_mtu);
    if max < 64 {
        bail!("QUIC datagram payload limit is too small: {max} bytes");
    }

    let payload_bytes = if requested == 0 { max } else { requested };
    if payload_bytes < 64 {
        bail!("bench payload must be at least 64 bytes");
    }
    if payload_bytes > max {
        bail!("bench payload {payload_bytes} exceeds current QUIC/datagram limit {max}");
    }
    Ok(payload_bytes)
}

async fn run_bench(
    connection: &quinn::Connection,
    direction: BenchDirection,
    duration_secs: u64,
    payload_bytes: usize,
) -> Result<()> {
    if duration_secs == 0 {
        bail!("bench duration must be greater than zero");
    }

    match direction {
        BenchDirection::Upload => run_upload_bench(connection, duration_secs, payload_bytes).await,
        BenchDirection::Download => {
            run_download_bench(connection, duration_secs, payload_bytes).await
        }
    }
}

async fn run_upload_bench(
    connection: &quinn::Connection,
    duration_secs: u64,
    payload_bytes: usize,
) -> Result<()> {
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
                bail!("connection closed during upload bench: {reason}");
            }
        }
    }

    let elapsed = started.elapsed();
    let summary = timeout(
        Duration::from_secs(5),
        read_bench_summary_stream(connection.clone()),
    )
    .await
    .context("timed out waiting for bench summary")??;
    print_local_bench("upload sent", elapsed, bytes, packets, payload_bytes);
    println!("server {summary}");
    sleep(Duration::from_millis(100)).await;
    Ok(())
}

async fn run_download_bench(
    connection: &quinn::Connection,
    duration_secs: u64,
    payload_bytes: usize,
) -> Result<()> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(duration_secs + 5);
    let mut packets = 0_u64;
    let mut bytes = 0_u64;
    let mut summary_task = tokio::spawn(read_bench_summary_stream(connection.clone()));
    let deadline_timer = sleep_until(deadline);
    tokio::pin!(deadline_timer);

    loop {
        tokio::select! {
            summary = &mut summary_task => {
                let summary = summary.context("bench summary task failed")??;
                print_local_bench(
                    "download received",
                    started.elapsed(),
                    bytes,
                    packets,
                    payload_bytes,
                );
                println!("server {summary}");
                return Ok(());
            }
            packet = connection.read_datagram() => {
                let packet = packet?;
                packets += 1;
                bytes += packet.len() as u64;
            }
            _ = &mut deadline_timer => {
                summary_task.abort();
                bail!("timed out waiting for download bench summary");
            }
        }
    }
}

async fn read_bench_summary_stream(connection: quinn::Connection) -> Result<String> {
    let (_send, mut recv) = connection
        .accept_bi()
        .await
        .context("failed to accept bench summary stream")?;
    let packet = recv
        .read_to_end(1024)
        .await
        .context("failed to read bench summary")?;
    let Some(summary) = parse_bench_summary(&packet)? else {
        bail!("invalid bench summary stream");
    };
    Ok(summary)
}

fn parse_bench_summary(packet: &[u8]) -> Result<Option<String>> {
    let Some(summary) = packet.strip_prefix(BENCH_SUMMARY_MAGIC) else {
        return Ok(None);
    };
    let summary = std::str::from_utf8(summary).context("bench summary is not utf-8")?;
    Ok(Some(summary.trim().to_string()))
}

fn print_local_bench(
    label: &str,
    elapsed: Duration,
    bytes: u64,
    packets: u64,
    payload_bytes: usize,
) {
    let seconds = elapsed.as_secs_f64().max(0.001);
    let mbps = bytes as f64 * 8.0 / seconds / 1_000_000.0;
    println!(
        "{label}: {mbps:.2} Mbps, bytes={bytes}, packets={packets}, payload_bytes={payload_bytes}, elapsed_ms={}",
        elapsed.as_millis()
    );
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let mut terminate =
            signal(SignalKind::terminate()).context("failed to install SIGTERM handler")?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => {
                result.context("failed to listen for Ctrl+C")?;
            }
            _ = terminate.recv() => {}
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c()
            .await
            .context("failed to listen for Ctrl+C")?;
        Ok(())
    }
}
