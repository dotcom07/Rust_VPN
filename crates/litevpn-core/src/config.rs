use std::{fs, net::Ipv4Addr, path::Path, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{DEFAULT_DATAGRAM_BUFFER_BYTES, DEFAULT_MTU, DEFAULT_UDP_SOCKET_BUFFER_BYTES, MAX_MTU};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CongestionController {
    #[default]
    Cubic,
    Bbr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub listen: String,
    pub tun_name: String,
    pub tun_ip: Ipv4Addr,
    pub tun_prefix: u8,
    pub client_ip: Ipv4Addr,
    pub mtu: u16,
    pub cert_path: PathBuf,
    pub key_path: PathBuf,
    pub auth_token_path: PathBuf,
    pub external_interface: String,
    pub datagram_buffer_bytes: usize,
    pub enable_linux_offload: bool,
    pub tx_queue_len: Option<u32>,
    pub congestion_controller: CongestionController,
    pub udp_recv_buffer_bytes: usize,
    pub udp_send_buffer_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:443".to_string(),
            tun_name: "tun0".to_string(),
            tun_ip: Ipv4Addr::new(10, 66, 0, 1),
            tun_prefix: 24,
            client_ip: Ipv4Addr::new(10, 66, 0, 2),
            mtu: DEFAULT_MTU,
            cert_path: "/etc/litevpn/server.crt".into(),
            key_path: "/etc/litevpn/server.key".into(),
            auth_token_path: "/etc/litevpn/client.token".into(),
            external_interface: "ens3".to_string(),
            datagram_buffer_bytes: DEFAULT_DATAGRAM_BUFFER_BYTES,
            enable_linux_offload: false,
            tx_queue_len: Some(10_000),
            congestion_controller: CongestionController::Cubic,
            udp_recv_buffer_bytes: DEFAULT_UDP_SOCKET_BUFFER_BYTES,
            udp_send_buffer_bytes: DEFAULT_UDP_SOCKET_BUFFER_BYTES,
        }
    }
}

impl ServerConfig {
    pub fn validate(&self) -> Result<()> {
        validate_common(self.mtu, self.tun_prefix, self.datagram_buffer_bytes)?;
        validate_udp_socket_buffers(self.udp_recv_buffer_bytes, self.udp_send_buffer_bytes)?;
        if self.tun_name.trim().is_empty() {
            bail!("server tun_name must not be empty");
        }
        if self.external_interface.trim().is_empty() {
            bail!("external_interface must not be empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    pub server: String,
    pub server_name: String,
    pub tun_name: String,
    pub client_ip: Ipv4Addr,
    pub server_tun_ip: Ipv4Addr,
    pub tun_prefix: u8,
    pub mtu: u16,
    pub ca_cert_path: PathBuf,
    pub auth_token_path: PathBuf,
    pub route_all: bool,
    pub dns: Vec<String>,
    pub datagram_buffer_bytes: usize,
    pub congestion_controller: CongestionController,
    pub udp_recv_buffer_bytes: usize,
    pub udp_send_buffer_bytes: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            server: "127.0.0.1:443".to_string(),
            server_name: "litevpn.local".to_string(),
            tun_name: String::new(),
            client_ip: Ipv4Addr::new(10, 66, 0, 2),
            server_tun_ip: Ipv4Addr::new(10, 66, 0, 1),
            tun_prefix: 24,
            mtu: DEFAULT_MTU,
            ca_cert_path: "./config/server.crt".into(),
            auth_token_path: "./config/client.token".into(),
            route_all: true,
            dns: vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()],
            datagram_buffer_bytes: DEFAULT_DATAGRAM_BUFFER_BYTES,
            congestion_controller: CongestionController::Cubic,
            udp_recv_buffer_bytes: DEFAULT_UDP_SOCKET_BUFFER_BYTES,
            udp_send_buffer_bytes: DEFAULT_UDP_SOCKET_BUFFER_BYTES,
        }
    }
}

impl ClientConfig {
    pub fn validate(&self) -> Result<()> {
        validate_common(self.mtu, self.tun_prefix, self.datagram_buffer_bytes)?;
        validate_udp_socket_buffers(self.udp_recv_buffer_bytes, self.udp_send_buffer_bytes)?;
        if self.server.trim().is_empty() {
            bail!("server must not be empty");
        }
        if self.server_name.trim().is_empty() {
            bail!("server_name must not be empty");
        }
        Ok(())
    }
}

fn validate_common(mtu: u16, prefix: u8, datagram_buffer_bytes: usize) -> Result<()> {
    if mtu < 576 {
        bail!("mtu must be >= 576");
    }
    if mtu > MAX_MTU {
        bail!("mtu must be <= {MAX_MTU} for QUIC datagram safety");
    }
    if prefix > 32 {
        bail!("tun_prefix must be <= 32");
    }
    if datagram_buffer_bytes < 64 * 1024 {
        bail!("datagram_buffer_bytes is too small");
    }
    Ok(())
}

pub fn validate_udp_socket_buffers(
    recv_buffer_bytes: usize,
    send_buffer_bytes: usize,
) -> Result<()> {
    if recv_buffer_bytes != 0 && recv_buffer_bytes < 64 * 1024 {
        bail!("udp_recv_buffer_bytes must be 0 or >= 65536");
    }
    if send_buffer_bytes != 0 && send_buffer_bytes < 64 * 1024 {
        bail!("udp_send_buffer_bytes must be 0 or >= 65536");
    }
    Ok(())
}

pub fn load_toml<T: DeserializeOwned>(path: impl AsRef<Path>) -> Result<T> {
    let path = path.as_ref();
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("failed to parse config {}", path.display()))
}

pub fn load_token(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let token = fs::read_to_string(path)
        .with_context(|| format!("failed to read token {}", path.display()))?;
    let token = token.trim().to_string();
    if token.len() < 32 {
        return Err(anyhow!(
            "token in {} is too short; run litevpn-keygen",
            path.display()
        ));
    }
    Ok(token)
}
