use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use clap::Parser;
use litevpn_core::{
    auth::{AuthMode, BenchDirection, client_authenticate_with_mode},
    config::{ClientConfig, load_token, load_toml},
    crypto,
    quic::{
        DatagramBacklog, connection_stats_summary, create_udp_socket, ensure_datagram_capacity,
        pump_quic_to_tun, pump_tun_to_quic,
    },
    tun::{TunOptions, create_tun},
};
use quinn::{Endpoint, EndpointConfig};
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

    #[arg(long, default_value_t = 0)]
    bench_target_mbps: u64,

    #[arg(long, default_value_t = 1)]
    bench_runs: u64,

    #[arg(long, default_value_t = 250)]
    bench_run_gap_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct BenchMeasurement {
    mbps: f64,
    bytes: u64,
    packets: u64,
    elapsed: Duration,
    server: Option<ServerBenchMeasurement>,
}

#[derive(Debug, Clone, Copy)]
struct ServerBenchMeasurement {
    mbps: f64,
    bytes: u64,
    packets: u64,
    lost_packets: u64,
    congestion_events: u64,
    elapsed_ms: u64,
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
    let bench_direction = args
        .bench
        .as_deref()
        .map(parse_bench_direction)
        .transpose()?;
    let bench_target_mbps = bench_target_mbps(args.bench_target_mbps)?;
    let bench_runs = if bench_direction.is_some() {
        bench_runs(args.bench_runs)?
    } else {
        1
    };
    let client_config = crypto::client_config(
        &config.ca_cert_path,
        config.datagram_buffer_bytes,
        config.mtu,
        config.congestion_controller,
    )?;
    let (endpoint, connection) = connect_client(
        &config,
        server_addr,
        client_config.clone(),
        args.connect_timeout_secs,
    )
    .await?;

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
            target_mbps: bench_target_mbps,
        },
        None => AuthMode::Vpn,
    };
    client_authenticate_with_mode(&connection, &token, auth_mode).await?;
    info!(remote = %connection.remote_address(), "authenticated");

    if let Some(direction) = bench_direction {
        run_bench_iterations(
            endpoint,
            connection,
            &config,
            client_config,
            server_addr,
            &token,
            auth_mode,
            direction,
            args.bench_duration_secs,
            bench_payload_bytes,
            bench_target_mbps,
            bench_runs,
            args.bench_run_gap_ms,
            args.connect_timeout_secs,
        )
        .await?;
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
    info!(
        tun = %device_name,
        mtu = config.mtu,
        egress_target_mbps = config.egress_target_mbps,
        "client tun ready"
    );

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
    let up = pump_tun_to_quic(
        &device,
        connection.clone(),
        config.mtu as usize,
        "client",
        config.egress_target_mbps,
        config.datagram_backlog_packets,
    );
    let down = pump_quic_to_tun(&device, connection.clone(), "client");

    let run_result = tokio::select! {
        result = up => result,
        result = down => result,
        result = shutdown_signal() => {
            match result {
                Ok(()) => {
                    info!("shutdown requested");
                    Ok(())
                }
                Err(error) => Err(error),
            }
        }
    };

    #[cfg(target_os = "macos")]
    if let Some(routes) = routes.as_mut() {
        routes.cleanup();
    }

    connection.close(0_u32.into(), b"client shutdown");
    endpoint.wait_idle().await;
    run_result
}

async fn connect_client(
    config: &ClientConfig,
    server_addr: SocketAddr,
    client_config: quinn::ClientConfig,
    connect_timeout_secs: u64,
) -> Result<(Endpoint, quinn::Connection)> {
    let bind_addr: SocketAddr = if server_addr.is_ipv4() {
        "0.0.0.0:0".parse().unwrap()
    } else {
        "[::]:0".parse().unwrap()
    };
    let socket = create_udp_socket(
        bind_addr,
        config.udp_recv_buffer_bytes,
        config.udp_send_buffer_bytes,
    )?;
    let runtime = quinn::default_runtime().context("no async runtime found")?;
    let mut endpoint = Endpoint::new(EndpointConfig::default(), None, socket, runtime)?;
    endpoint.set_default_client_config(client_config);

    info!(server = %server_addr, server_name = %config.server_name, "connecting");
    let connecting = endpoint.connect(server_addr, &config.server_name)?;
    let connection = timeout(Duration::from_secs(connect_timeout_secs), connecting)
        .await
        .context("connect timed out")?
        .context("failed to connect to server")?;
    Ok((endpoint, connection))
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

fn bench_target_mbps(value: u64) -> Result<Option<u64>> {
    if value == 0 {
        return Ok(None);
    }
    if value > 10_000 {
        bail!("bench target Mbps must be <= 10000");
    }
    Ok(Some(value))
}

fn bench_runs(value: u64) -> Result<u64> {
    if value == 0 {
        bail!("bench runs must be greater than zero");
    }
    if value > 100 {
        bail!("bench runs must be <= 100");
    }
    Ok(value)
}

async fn run_bench_iterations(
    first_endpoint: Endpoint,
    first_connection: quinn::Connection,
    config: &ClientConfig,
    client_config: quinn::ClientConfig,
    server_addr: SocketAddr,
    token: &str,
    auth_mode: AuthMode,
    direction: BenchDirection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
    runs: u64,
    run_gap_ms: u64,
    connect_timeout_secs: u64,
) -> Result<()> {
    let mut measurements = Vec::with_capacity(runs as usize);
    let mut next_endpoint = Some(first_endpoint);
    let mut next_connection = Some(first_connection);

    for run in 1..=runs {
        let (endpoint, connection) = match (next_endpoint.take(), next_connection.take()) {
            (Some(endpoint), Some(connection)) => (endpoint, connection),
            _ => {
                let (endpoint, connection) = connect_client(
                    config,
                    server_addr,
                    client_config.clone(),
                    connect_timeout_secs,
                )
                .await?;
                client_authenticate_with_mode(&connection, token, auth_mode).await?;
                info!(remote = %connection.remote_address(), run, runs, "authenticated bench run");
                (endpoint, connection)
            }
        };

        if runs > 1 {
            println!("bench run {run}/{runs}");
        }
        let result = run_bench(
            &connection,
            direction,
            duration_secs,
            payload_bytes,
            target_mbps,
            config.datagram_backlog_packets,
        )
        .await;
        connection.close(0_u32.into(), b"bench complete");
        endpoint.wait_idle().await;
        measurements.push(result?);

        if run < runs && run_gap_ms > 0 {
            sleep(Duration::from_millis(run_gap_ms)).await;
        }
    }

    print_bench_aggregate(&measurements);
    Ok(())
}

async fn run_bench(
    connection: &quinn::Connection,
    direction: BenchDirection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
    datagram_backlog_packets: u64,
) -> Result<BenchMeasurement> {
    if duration_secs == 0 {
        bail!("bench duration must be greater than zero");
    }

    match direction {
        BenchDirection::Upload => {
            run_upload_bench(
                connection,
                duration_secs,
                payload_bytes,
                target_mbps,
                datagram_backlog_packets,
            )
            .await
        }
        BenchDirection::Download => {
            run_download_bench(connection, duration_secs, payload_bytes, target_mbps).await
        }
    }
}

async fn run_upload_bench(
    connection: &quinn::Connection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
    datagram_backlog_packets: u64,
) -> Result<BenchMeasurement> {
    let payload = Bytes::from(vec![0_u8; payload_bytes]);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(duration_secs);
    let mut packets = 0_u64;
    let mut bytes = 0_u64;
    let target_bytes_per_sec = target_bytes_per_sec(target_mbps);
    let burst_bytes = target_burst_bytes(target_bytes_per_sec, payload_bytes);
    let mut datagram_backlog = DatagramBacklog::new(connection, datagram_backlog_packets);
    let deadline_timer = sleep_until(deadline);
    tokio::pin!(deadline_timer);

    loop {
        tokio::select! {
            _ = &mut deadline_timer => {
                break;
            }
            result = connection.send_datagram_wait(payload.clone()) => {
                result?;
                if !datagram_backlog.queued_until(connection, deadline).await? {
                    break;
                }
                packets += 1;
                bytes += payload_bytes as u64;
                pace_to_target(started, bytes, target_bytes_per_sec, burst_bytes).await;
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
    print_local_bench(
        "upload sent",
        elapsed,
        bytes,
        packets,
        payload_bytes,
        target_mbps,
    );
    println!("client stats: {}", connection_stats_summary(connection));
    println!("server {summary}");
    sleep(Duration::from_millis(100)).await;
    Ok(bench_measurement(
        elapsed,
        bytes,
        packets,
        parse_server_bench_measurement(&summary),
    ))
}

async fn run_download_bench(
    connection: &quinn::Connection,
    duration_secs: u64,
    payload_bytes: usize,
    target_mbps: Option<u64>,
) -> Result<BenchMeasurement> {
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
                    target_mbps,
                );
                println!("client stats: {}", connection_stats_summary(connection));
                println!("server {summary}");
                return Ok(bench_measurement(
                    started.elapsed(),
                    bytes,
                    packets,
                    parse_server_bench_measurement(&summary),
                ));
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
    target_mbps: Option<u64>,
) {
    let seconds = elapsed.as_secs_f64().max(0.001);
    let mbps = bytes as f64 * 8.0 / seconds / 1_000_000.0;
    let target = target_mbps
        .map(|value| value.to_string())
        .unwrap_or_else(|| "unlimited".to_string());
    println!(
        "{label}: {mbps:.2} Mbps, target_mbps={target}, bytes={bytes}, packets={packets}, payload_bytes={payload_bytes}, elapsed_ms={}",
        elapsed.as_millis()
    );
}

fn bench_measurement(
    elapsed: Duration,
    bytes: u64,
    packets: u64,
    server: Option<ServerBenchMeasurement>,
) -> BenchMeasurement {
    let seconds = elapsed.as_secs_f64().max(0.001);
    BenchMeasurement {
        mbps: bytes as f64 * 8.0 / seconds / 1_000_000.0,
        bytes,
        packets,
        elapsed,
        server,
    }
}

fn print_bench_aggregate(measurements: &[BenchMeasurement]) {
    if measurements.len() <= 1 {
        return;
    }

    let runs = measurements.len();
    let total_mbps: f64 = measurements
        .iter()
        .map(|measurement| measurement.mbps)
        .sum();
    let total_bytes: u64 = measurements
        .iter()
        .map(|measurement| measurement.bytes)
        .sum();
    let total_packets: u64 = measurements
        .iter()
        .map(|measurement| measurement.packets)
        .sum();
    let total_elapsed_ms: u128 = measurements
        .iter()
        .map(|measurement| measurement.elapsed.as_millis())
        .sum();
    let min_mbps = measurements
        .iter()
        .map(|measurement| measurement.mbps)
        .fold(f64::INFINITY, f64::min);
    let max_mbps = measurements
        .iter()
        .map(|measurement| measurement.mbps)
        .fold(f64::NEG_INFINITY, f64::max);
    println!(
        "bench aggregate local: runs={runs}, avg_mbps={:.2}, min_mbps={min_mbps:.2}, max_mbps={max_mbps:.2}, total_bytes={total_bytes}, total_packets={total_packets}, total_elapsed_ms={total_elapsed_ms}",
        total_mbps / runs as f64
    );

    let server_measurements: Vec<_> = measurements
        .iter()
        .filter_map(|measurement| measurement.server)
        .collect();
    if server_measurements.len() != measurements.len() {
        return;
    }

    let server_total_mbps: f64 = server_measurements
        .iter()
        .map(|measurement| measurement.mbps)
        .sum();
    let server_total_bytes: u64 = server_measurements
        .iter()
        .map(|measurement| measurement.bytes)
        .sum();
    let server_total_packets: u64 = server_measurements
        .iter()
        .map(|measurement| measurement.packets)
        .sum();
    let server_total_lost_packets: u64 = server_measurements
        .iter()
        .map(|measurement| measurement.lost_packets)
        .sum();
    let server_total_congestion_events: u64 = server_measurements
        .iter()
        .map(|measurement| measurement.congestion_events)
        .sum();
    let server_total_elapsed_ms: u64 = server_measurements
        .iter()
        .map(|measurement| measurement.elapsed_ms)
        .sum();
    let server_min_mbps = server_measurements
        .iter()
        .map(|measurement| measurement.mbps)
        .fold(f64::INFINITY, f64::min);
    let server_max_mbps = server_measurements
        .iter()
        .map(|measurement| measurement.mbps)
        .fold(f64::NEG_INFINITY, f64::max);
    println!(
        "bench aggregate server: runs={runs}, avg_mbps={:.2}, min_mbps={server_min_mbps:.2}, max_mbps={server_max_mbps:.2}, total_bytes={server_total_bytes}, total_packets={server_total_packets}, lost_packets={server_total_lost_packets}, congestion_events={server_total_congestion_events}, total_elapsed_ms={server_total_elapsed_ms}",
        server_total_mbps / runs as f64
    );
}

fn parse_server_bench_measurement(summary: &str) -> Option<ServerBenchMeasurement> {
    let bytes = parse_summary_u64(summary, "bytes")?;
    let packets = parse_summary_u64(summary, "packets")?;
    let elapsed_ms = parse_summary_u64(summary, "measured_elapsed_ms")
        .or_else(|| parse_summary_u64(summary, "elapsed_ms"))?;
    let lost_packets = parse_summary_u64(summary, "lost_packets").unwrap_or(0);
    let congestion_events = parse_summary_u64(summary, "congestion_events").unwrap_or(0);
    let seconds = (elapsed_ms as f64 / 1000.0).max(0.001);
    Some(ServerBenchMeasurement {
        mbps: bytes as f64 * 8.0 / seconds / 1_000_000.0,
        bytes,
        packets,
        lost_packets,
        congestion_events,
        elapsed_ms,
    })
}

fn parse_summary_u64(summary: &str, key: &str) -> Option<u64> {
    let prefix = format!("{key}=");
    summary
        .split_whitespace()
        .find_map(|part| part.strip_prefix(&prefix))
        .and_then(|value| value.parse().ok())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_bench_measurement() {
        let summary = "direction=download bytes=28548000 packets=21960 payload_bytes=1300 target_mbps=38 elapsed_ms=6001 measured_elapsed_ms=6000 udp_tx_datagrams=21963 lost_packets=54 congestion_events=3";
        let measurement = parse_server_bench_measurement(summary).expect("parsed measurement");

        assert_eq!(measurement.bytes, 28_548_000);
        assert_eq!(measurement.packets, 21_960);
        assert_eq!(measurement.lost_packets, 54);
        assert_eq!(measurement.congestion_events, 3);
        assert_eq!(measurement.elapsed_ms, 6_000);
        assert!((measurement.mbps - 38.064).abs() < 0.001);
    }

    #[test]
    fn falls_back_to_server_elapsed_ms() {
        let summary = "direction=download bytes=1000 packets=1 elapsed_ms=1000 lost_packets=0";
        let measurement = parse_server_bench_measurement(summary).expect("parsed measurement");

        assert_eq!(measurement.elapsed_ms, 1_000);
        assert!((measurement.mbps - 0.008).abs() < 0.001);
    }

    #[test]
    fn rejects_incomplete_server_bench_measurement() {
        assert!(parse_server_bench_measurement("bytes=1 packets=1").is_none());
    }

    #[test]
    fn validates_bench_runs() {
        assert_eq!(bench_runs(1).expect("valid run count"), 1);
        assert!(bench_runs(0).is_err());
        assert!(bench_runs(101).is_err());
    }
}
