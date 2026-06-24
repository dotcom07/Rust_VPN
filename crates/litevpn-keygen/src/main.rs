use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
use rcgen::{CertifiedKey, generate_simple_self_signed};

#[derive(Debug, Parser)]
struct Args {
    #[arg(short, long, default_value = "config")]
    out_dir: PathBuf,

    #[arg(long, default_value = "litevpn.local")]
    server_name: String,

    #[arg(long, default_value_t = 32)]
    token_bytes: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("failed to create {}", args.out_dir.display()))?;

    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec![args.server_name.clone()])?;

    let mut token = vec![0_u8; args.token_bytes];
    getrandom::fill(&mut token)?;
    let token = hex::encode(token);

    let cert_path = args.out_dir.join("server.crt");
    let key_path = args.out_dir.join("server.key");
    let token_path = args.out_dir.join("client.token");

    fs::write(&cert_path, cert.pem())?;
    fs::write(&key_path, signing_key.serialize_pem())?;
    fs::write(&token_path, format!("{token}\n"))?;
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;

    println!("wrote {}", cert_path.display());
    println!("wrote {}", key_path.display());
    println!("wrote {}", token_path.display());
    Ok(())
}
