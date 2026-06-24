use std::{net::Ipv4Addr, net::SocketAddr};

use anyhow::{Context, Result, bail};
use tokio::process::Command;
use tracing::{info, warn};

pub struct RouteGuard {
    server_ip: Ipv4Addr,
    gateway: Ipv4Addr,
    installed: bool,
}

impl RouteGuard {
    pub fn cleanup_stale(server_addr: SocketAddr) -> Result<()> {
        let server_ip = match server_addr.ip() {
            std::net::IpAddr::V4(ip) => ip,
            std::net::IpAddr::V6(_) => bail!("automatic macOS routes require an IPv4 server"),
        };
        if !stale_routes_present(server_ip)? {
            info!(%server_ip, "no stale macOS split default routes found");
            return Ok(());
        }
        if !cleanup_routes(server_ip) {
            bail!(
                "failed to remove one or more stale macOS routes; rerun with sudo or use --no-routes"
            );
        }
        info!(%server_ip, "removed stale macOS split default routes");
        Ok(())
    }

    pub async fn install(server_addr: SocketAddr, vpn_gateway: Ipv4Addr) -> Result<Self> {
        let server_ip = match server_addr.ip() {
            std::net::IpAddr::V4(ip) => ip,
            std::net::IpAddr::V6(_) => bail!("automatic macOS routes require an IPv4 server"),
        };
        let gateway = default_gateway().await?;
        let _ = cleanup_routes(server_ip);

        let install_result = async {
            run_route(&[
                "-n",
                "add",
                "-host",
                &server_ip.to_string(),
                &gateway.to_string(),
            ])
            .await?;
            run_route(&["-n", "add", "-net", "0.0.0.0/1", &vpn_gateway.to_string()]).await?;
            run_route(&["-n", "add", "-net", "128.0.0.0/1", &vpn_gateway.to_string()]).await
        }
        .await;

        if let Err(error) = install_result {
            let _ = cleanup_routes(server_ip);
            return Err(error);
        }

        info!(%server_ip, %gateway, %vpn_gateway, "installed macOS split default routes");
        Ok(Self {
            server_ip,
            gateway,
            installed: true,
        })
    }

    pub fn cleanup(&mut self) {
        if !self.installed {
            return;
        }

        let _ = cleanup_routes(self.server_ip);
        self.installed = false;
        info!(
            server_ip = %self.server_ip,
            gateway = %self.gateway,
            "removed macOS split default routes"
        );
    }
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn cleanup_routes(server_ip: Ipv4Addr) -> bool {
    let mut ok = true;
    ok &= delete_route(&["-n", "delete", "-net", "0.0.0.0/1"]);
    ok &= delete_route(&["-n", "delete", "-net", "128.0.0.0/1"]);
    ok &= delete_route(&["-n", "delete", "-host", &server_ip.to_string()]);
    ok
}

fn stale_routes_present(server_ip: Ipv4Addr) -> Result<bool> {
    let output = std::process::Command::new("netstat")
        .args(["-rn", "-f", "inet"])
        .output()
        .context("failed to run netstat -rn -f inet")?;
    if !output.status.success() {
        bail!("netstat -rn -f inet failed");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.lines().any(|line| {
        let Some(destination) = line.split_whitespace().next() else {
            return false;
        };
        is_litevpn_route_destination(destination, server_ip)
    }))
}

fn is_litevpn_route_destination(destination: &str, server_ip: Ipv4Addr) -> bool {
    let server_ip = server_ip.to_string();
    matches!(destination, "0/1" | "0.0.0.0/1" | "128.0/1" | "128.0.0.0/1")
        || destination == server_ip
        || destination == format!("{server_ip}/32")
}

async fn default_gateway() -> Result<Ipv4Addr> {
    let output = Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .await
        .context("failed to run route -n get default")?;
    if !output.status.success() {
        bail!("route -n get default failed");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("gateway:") {
            return value.trim().parse().context("failed to parse gateway");
        }
    }
    bail!("default gateway not found");
}

async fn run_route(args: &[&str]) -> Result<()> {
    let output = Command::new("route")
        .args(args)
        .output()
        .await
        .with_context(|| format!("failed to run route {}", args.join(" ")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("File exists") {
        warn!(command = %args.join(" "), "route already exists");
        return Ok(());
    }

    bail!("route {} failed: {}", args.join(" "), stderr.trim());
}

fn delete_route(args: &[&str]) -> bool {
    let output = std::process::Command::new("route").args(args).output();
    let Ok(output) = output else {
        warn!(command = %args.join(" "), "failed to run route cleanup command");
        return false;
    };
    if output.status.success() {
        return true;
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("not in table") {
        return true;
    }
    warn!(
        command = %args.join(" "),
        error = %stderr.trim(),
        "route cleanup command failed"
    );
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_litevpn_route_destinations() {
        let server_ip = Ipv4Addr::new(161, 33, 36, 181);

        assert!(is_litevpn_route_destination("0/1", server_ip));
        assert!(is_litevpn_route_destination("0.0.0.0/1", server_ip));
        assert!(is_litevpn_route_destination("128.0/1", server_ip));
        assert!(is_litevpn_route_destination("128.0.0.0/1", server_ip));
        assert!(is_litevpn_route_destination("161.33.36.181", server_ip));
        assert!(is_litevpn_route_destination("161.33.36.181/32", server_ip));
        assert!(!is_litevpn_route_destination("default", server_ip));
        assert!(!is_litevpn_route_destination("192.168.35.1", server_ip));
    }
}
