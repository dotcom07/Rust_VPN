use anyhow::{Context, Result, bail};
use quinn::Connection;

use crate::{AUTH_ERR, AUTH_MAGIC, AUTH_OK};

const MAX_AUTH_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchDirection {
    Upload,
    Download,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    Vpn,
    Bench {
        direction: BenchDirection,
        duration_secs: u64,
        payload_bytes: usize,
    },
}

pub async fn client_authenticate(connection: &Connection, token: &str) -> Result<()> {
    client_authenticate_with_mode(connection, token, AuthMode::Vpn).await
}

pub async fn client_authenticate_with_mode(
    connection: &Connection,
    token: &str,
    mode: AuthMode,
) -> Result<()> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .context("failed to open auth stream")?;

    let mut request = Vec::with_capacity(AUTH_MAGIC.len() + token.len() + 1);
    request.extend_from_slice(AUTH_MAGIC);
    request.extend_from_slice(token.as_bytes());
    request.push(b'\n');
    match mode {
        AuthMode::Vpn => {}
        AuthMode::Bench {
            direction,
            duration_secs,
            payload_bytes,
        } => {
            let direction = match direction {
                BenchDirection::Upload => "upload",
                BenchDirection::Download => "download",
            };
            request.extend_from_slice(
                format!("bench {direction} {duration_secs} {payload_bytes}\n").as_bytes(),
            );
        }
    }

    send.write_all(&request)
        .await
        .context("failed to send auth token")?;
    send.finish().context("failed to finish auth stream")?;

    let response = recv
        .read_to_end(MAX_AUTH_BYTES)
        .await
        .context("failed to read auth response")?;
    if response.as_slice() != AUTH_OK {
        bail!("server rejected client token");
    }
    Ok(())
}

pub async fn server_authenticate(
    connection: &Connection,
    expected_token: &str,
) -> Result<AuthMode> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .context("failed to accept auth stream")?;

    let request = recv
        .read_to_end(MAX_AUTH_BYTES)
        .await
        .context("failed to read auth request")?;

    let (token, mode) = parse_request(&request)?;
    let ok = token == expected_token;
    send.write_all(if ok { AUTH_OK } else { AUTH_ERR })
        .await
        .context("failed to send auth response")?;
    send.finish().context("failed to finish auth response")?;

    if !ok {
        bail!("invalid client token");
    }
    Ok(mode)
}

fn parse_request(request: &[u8]) -> Result<(&str, AuthMode)> {
    let payload = request
        .strip_prefix(AUTH_MAGIC)
        .context("invalid auth magic")?;
    let payload = std::str::from_utf8(payload).context("auth request is not utf-8")?;
    let payload = payload
        .strip_suffix('\n')
        .context("auth request must end with newline")?;

    let mut lines = payload.lines();
    let token = lines.next().context("missing auth token")?;
    let mode = match lines.next() {
        None => AuthMode::Vpn,
        Some(line) => parse_mode(line)?,
    };

    if lines.next().is_some() {
        bail!("too many auth request lines");
    }

    Ok((token, mode))
}

fn parse_mode(line: &str) -> Result<AuthMode> {
    let mut parts = line.split_whitespace();
    if parts.next() != Some("bench") {
        bail!("unknown auth mode");
    }
    let direction = match parts.next() {
        Some("upload") => BenchDirection::Upload,
        Some("download") => BenchDirection::Download,
        _ => bail!("bench direction must be upload or download"),
    };
    let duration_secs = parts
        .next()
        .context("missing bench duration")?
        .parse()
        .context("invalid bench duration")?;
    let payload_bytes = parts
        .next()
        .context("missing bench payload size")?
        .parse()
        .context("invalid bench payload size")?;
    if parts.next().is_some() {
        bail!("too many bench mode fields");
    }
    if duration_secs == 0 {
        bail!("bench duration must be greater than zero");
    }
    if !(64..=1452).contains(&payload_bytes) {
        bail!("bench payload size must be between 64 and 1452 bytes");
    }
    Ok(AuthMode::Bench {
        direction,
        duration_secs,
        payload_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::{AuthMode, BenchDirection, parse_request};
    use crate::AUTH_MAGIC;

    #[test]
    fn parses_token() {
        let mut req = AUTH_MAGIC.to_vec();
        req.extend_from_slice(b"abcdef\n");
        assert_eq!(parse_request(&req).unwrap(), ("abcdef", AuthMode::Vpn));
    }

    #[test]
    fn parses_bench_mode() {
        let mut req = AUTH_MAGIC.to_vec();
        req.extend_from_slice(b"abcdef\nbench download 3 1200\n");
        assert_eq!(
            parse_request(&req).unwrap(),
            (
                "abcdef",
                AuthMode::Bench {
                    direction: BenchDirection::Download,
                    duration_secs: 3,
                    payload_bytes: 1200
                }
            )
        );
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(parse_request(b"bad token\n").is_err());
    }
}
