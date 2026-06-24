use std::{
    fs::File,
    io::BufReader,
    path::Path,
    sync::{Arc, Once},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use quinn::{
    ClientConfig, ServerConfig, TransportConfig, VarInt,
    crypto::rustls::{QuicClientConfig, QuicServerConfig},
};
use rustls::RootCertStore;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::ALPN;

static RUSTLS_PROVIDER: Once = Once::new();

pub fn server_config(
    cert_path: impl AsRef<Path>,
    key_path: impl AsRef<Path>,
    datagram_buffer_bytes: usize,
) -> Result<ServerConfig> {
    install_crypto_provider();

    let certs = read_certs(cert_path)?;
    let key = read_private_key(key_path)?;

    let mut server_crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    server_crypto.alpn_protocols = vec![ALPN.to_vec()];

    let mut config =
        ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server_crypto)?));
    config.transport_config(Arc::new(transport_config(datagram_buffer_bytes)));
    Ok(config)
}

pub fn client_config(
    ca_cert_path: impl AsRef<Path>,
    datagram_buffer_bytes: usize,
) -> Result<ClientConfig> {
    install_crypto_provider();

    let mut roots = RootCertStore::empty();
    for cert in read_certs(ca_cert_path)? {
        roots.add(cert)?;
    }

    let mut client_crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    client_crypto.alpn_protocols = vec![ALPN.to_vec()];

    let mut config = ClientConfig::new(Arc::new(QuicClientConfig::try_from(client_crypto)?));
    config.transport_config(Arc::new(transport_config(datagram_buffer_bytes)));
    Ok(config)
}

fn install_crypto_provider() {
    RUSTLS_PROVIDER.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn read_certs(path: impl AsRef<Path>) -> Result<Vec<CertificateDer<'static>>> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let certs = rustls_pemfile::certs(&mut reader)
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("failed to parse certificates in {}", path.display()))?;
    if certs.is_empty() {
        bail!("no certificates found in {}", path.display());
    }
    Ok(certs)
}

fn read_private_key(path: impl AsRef<Path>) -> Result<PrivateKeyDer<'static>> {
    let path = path.as_ref();
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::private_key(&mut reader)
        .with_context(|| format!("failed to parse private key in {}", path.display()))?
        .with_context(|| format!("no private key found in {}", path.display()))
}

fn transport_config(datagram_buffer_bytes: usize) -> TransportConfig {
    let mut transport = TransportConfig::default();
    tune_transport(&mut transport);
    transport.datagram_receive_buffer_size(Some(datagram_buffer_bytes));
    transport.datagram_send_buffer_size(datagram_buffer_bytes);
    transport
}

fn tune_transport(transport: &mut TransportConfig) {
    transport
        .max_concurrent_bidi_streams(4_u8.into())
        .max_concurrent_uni_streams(0_u8.into())
        .keep_alive_interval(Some(Duration::from_secs(5)))
        .max_idle_timeout(Some(
            Duration::from_secs(30)
                .try_into()
                .expect("valid idle timeout"),
        ))
        .initial_mtu(1200)
        .receive_window(VarInt::from_u32(8 * 1024 * 1024))
        .stream_receive_window(VarInt::from_u32(512 * 1024))
        .send_window(8 * 1024 * 1024)
        .enable_segmentation_offload(true)
        .send_fairness(false);
}
