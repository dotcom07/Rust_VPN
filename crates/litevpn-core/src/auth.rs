use anyhow::{Context, Result, bail};
use quinn::Connection;

use crate::{AUTH_ERR, AUTH_MAGIC, AUTH_OK};

const MAX_AUTH_BYTES: usize = 1024;

pub async fn client_authenticate(connection: &Connection, token: &str) -> Result<()> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .context("failed to open auth stream")?;

    let mut request = Vec::with_capacity(AUTH_MAGIC.len() + token.len() + 1);
    request.extend_from_slice(AUTH_MAGIC);
    request.extend_from_slice(token.as_bytes());
    request.push(b'\n');

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

pub async fn server_authenticate(connection: &Connection, expected_token: &str) -> Result<()> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .context("failed to accept auth stream")?;

    let request = recv
        .read_to_end(MAX_AUTH_BYTES)
        .await
        .context("failed to read auth request")?;

    let token = parse_token(&request)?;
    let ok = token == expected_token;
    send.write_all(if ok { AUTH_OK } else { AUTH_ERR })
        .await
        .context("failed to send auth response")?;
    send.finish().context("failed to finish auth response")?;

    if !ok {
        bail!("invalid client token");
    }
    Ok(())
}

fn parse_token(request: &[u8]) -> Result<&str> {
    let payload = request
        .strip_prefix(AUTH_MAGIC)
        .context("invalid auth magic")?;
    let payload = payload
        .strip_suffix(b"\n")
        .context("auth request must end with newline")?;
    std::str::from_utf8(payload).context("auth token is not utf-8")
}

#[cfg(test)]
mod tests {
    use super::parse_token;
    use crate::AUTH_MAGIC;

    #[test]
    fn parses_token() {
        let mut req = AUTH_MAGIC.to_vec();
        req.extend_from_slice(b"abcdef\n");
        assert_eq!(parse_token(&req).unwrap(), "abcdef");
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(parse_token(b"bad token\n").is_err());
    }
}
