use std::net::Ipv4Addr;

use anyhow::Result;
use tun_rs::{AsyncDevice, DeviceBuilder, Layer};

pub type TunDevice = AsyncDevice;

#[derive(Debug, Clone)]
pub struct TunOptions {
    pub name: String,
    pub address: Ipv4Addr,
    pub prefix: u8,
    pub destination: Option<Ipv4Addr>,
    pub mtu: u16,
    pub enable_linux_offload: bool,
    pub tx_queue_len: Option<u32>,
}

pub fn create_tun(options: TunOptions) -> Result<AsyncDevice> {
    let mut builder = DeviceBuilder::new().layer(Layer::L3).mtu(options.mtu).ipv4(
        options.address,
        options.prefix,
        options.destination,
    );

    if !options.name.trim().is_empty() {
        builder = builder.name(options.name);
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    {
        builder = builder.packet_information(false);
    }

    #[cfg(target_os = "linux")]
    {
        builder = builder.offload(options.enable_linux_offload);
        builder = builder.multi_queue(false);
        if let Some(tx_queue_len) = options.tx_queue_len {
            builder = builder.tx_queue_len(tx_queue_len);
        }
    }

    #[cfg(target_os = "macos")]
    {
        builder = builder.associate_route(true).persist(false);
    }

    Ok(builder.build_async()?)
}
