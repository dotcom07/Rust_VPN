use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use clap::Parser;
use litevpn_core::{
    auth::client_authenticate,
    config::{ClientConfig, load_token, load_toml},
    crypto,
    quic::{pump_quic_to_tun, pump_tun_to_quic},
    tun::{TunOptions, create_tun},
};
use quinn::Endpoint;
use tokio::time::{Duration, timeout};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "macos")]
mod macos_routes;

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
    let client_config = crypto::client_config(&config.ca_cert_path, config.datagram_buffer_bytes)?;
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
    client_authenticate(&connection, &token).await?;
    info!(remote = %connection.remote_address(), "authenticated");

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
