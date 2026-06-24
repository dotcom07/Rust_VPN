pub mod auth;
pub mod config;
pub mod crypto;
pub mod quic;
pub mod tun;

pub const ALPN: &[u8] = b"litevpn/1";
pub const AUTH_MAGIC: &[u8] = b"LVPN1 ";
pub const AUTH_OK: &[u8] = b"OK\n";
pub const AUTH_ERR: &[u8] = b"ERR\n";
pub const DEFAULT_MTU: u16 = 1300;
pub const MAX_MTU: u16 = 1400;
pub const DEFAULT_DATAGRAM_BUFFER_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_DATAGRAM_BACKLOG_PACKETS: u64 = 64;
pub const DEFAULT_UDP_SOCKET_BUFFER_BYTES: usize = 0;
