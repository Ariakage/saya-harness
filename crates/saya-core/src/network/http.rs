use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;

use super::{
    BoxStream, BrokerLimits, BrokerState, NetworkError, NetworkErrorKind, NormalizedOrigin,
    OriginScheme, normalize_target,
};

struct Request {
    target: super::NetworkTarget,
    connect: bool,
    headers: Vec<(String, String)>,
    method: String,
    content_length: u64,
}

pub(super) async fn serve(
    mut client: BoxStream,
    state: Arc<BrokerState>,
) -> Result<(), NetworkError> {
    let parsed = timeout(
        Duration::from_millis(state.policy.limits.header_timeout_ms),
        read_head(&mut client, &state.policy.limits),
    )
    .await;
    let (head, buffered) = match parsed {
        Ok(Ok(head)) => head,
        Ok(Err(error)) => {
            reject(&mut client, error.kind).await;
            return Err(error);
        }
        Err(_) => {
            reject(&mut client, NetworkErrorKind::Timeout).await;
            return Err(NetworkError::new(NetworkErrorKind::Timeout));
        }
    };
    let request = match parse_request(&head, &state.policy.limits) {
        Ok(request) => request,
        Err(error) => {
            reject(&mut client, error.kind).await;
            return Err(error);
        }
    };
    if request.content_length > state.policy.limits.max_connection_bytes {
        reject(&mut client, NetworkErrorKind::LimitExceeded).await;
        return Err(NetworkError::new(NetworkErrorKind::LimitExceeded));
    }
    let mut upstream = match state.connect(&request.target.origin).await {
        Ok(stream) => stream,
        Err(error) => {
            reject(&mut client, error.kind).await;
            return Err(error);
        }
    };
    if request.connect {
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
        let initial = buffered.len() as u64;
        if initial > state.policy.limits.max_connection_bytes {
            return Err(NetworkError::new(NetworkErrorKind::LimitExceeded));
        }
        state.charge(buffered.len())?;
        upstream.write_all(&buffered).await?;
        let (client_read, client_write) = tokio::io::split(client);
        let (server_read, server_write) = upstream.into_split();
        tokio::try_join!(
            copy_limited(client_read, server_write, state.clone(), initial),
            copy_limited(server_read, client_write, state, 0),
        )?;
    } else {
        let mut rewritten = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
            request.method,
            request.target.path_and_query,
            request.target.origin.authority()
        );
        for (name, value) in request.headers {
            rewritten.push_str(&name);
            rewritten.push_str(": ");
            rewritten.push_str(&value);
            rewritten.push_str("\r\n");
        }
        rewritten.push_str("\r\n");
        state.charge(rewritten.len())?;
        upstream.write_all(rewritten.as_bytes()).await?;
        let count = buffered.len().min(request.content_length as usize);
        state.charge(count)?;
        upstream.write_all(&buffered[..count]).await?;
        // Only this request body is forwarded. Extra pipelined bytes are never sent upstream.
        let remaining = request.content_length - count as u64;
        let (mut client_read, client_write) = tokio::io::split(client);
        let (server_read, mut server_write) = upstream.into_split();
        let send_body = async {
            let mut left = remaining;
            let mut buffer = [0u8; 8192];
            while left > 0 {
                let capacity = left.min(buffer.len() as u64) as usize;
                let read = timeout(
                    Duration::from_millis(state.policy.limits.idle_timeout_ms),
                    client_read.read(&mut buffer[..capacity]),
                )
                .await
                .map_err(|_| NetworkError::new(NetworkErrorKind::Timeout))??;
                if read == 0 {
                    return Err(NetworkError::new(NetworkErrorKind::InvalidRequest));
                }
                state.charge(read)?;
                server_write.write_all(&buffer[..read]).await?;
                left -= read as u64;
            }
            server_write.shutdown().await?;
            Ok(())
        };
        tokio::try_join!(
            send_body,
            copy_limited(server_read, client_write, state.clone(), 0)
        )?;
    }
    Ok(())
}

async fn reject(stream: &mut BoxStream, kind: NetworkErrorKind) {
    let status = match kind {
        NetworkErrorKind::OriginDenied | NetworkErrorKind::AddressDenied => "403 Forbidden",
        NetworkErrorKind::LimitExceeded => "413 Content Too Large",
        NetworkErrorKind::Timeout => "408 Request Timeout",
        NetworkErrorKind::Io | NetworkErrorKind::DnsFailed => "502 Bad Gateway",
        _ => "400 Bad Request",
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(response.as_bytes()).await;
}

async fn read_head(
    stream: &mut BoxStream,
    limits: &BrokerLimits,
) -> Result<(Vec<u8>, Vec<u8>), NetworkError> {
    let mut head = Vec::new();
    let mut buffer = [0u8; 2048];
    loop {
        if let Some(end) = head.windows(4).position(|window| window == b"\r\n\r\n") {
            if end + 4 > limits.max_header_bytes {
                return Err(NetworkError::new(NetworkErrorKind::LimitExceeded));
            }
            let body = head.split_off(end + 4);
            return Ok((head, body));
        }
        if head.len() >= limits.max_header_bytes {
            return Err(NetworkError::new(NetworkErrorKind::LimitExceeded));
        }
        let size = buffer.len().min(limits.max_header_bytes - head.len());
        let read = stream.read(&mut buffer[..size]).await?;
        if read == 0 {
            return Err(NetworkError::new(NetworkErrorKind::InvalidRequest));
        }
        head.extend_from_slice(&buffer[..read]);
    }
}

fn parse_request(head: &[u8], limits: &BrokerLimits) -> Result<Request, NetworkError> {
    let invalid = || NetworkError::new(NetworkErrorKind::InvalidRequest);
    if !head.is_ascii() {
        return Err(invalid());
    }
    let head = std::str::from_utf8(head).map_err(|_| invalid())?;
    let mut lines = head
        .strip_suffix("\r\n\r\n")
        .ok_or_else(invalid)?
        .split("\r\n");
    let line = lines.next().ok_or_else(invalid)?;
    let parts = line.split(' ').collect::<Vec<_>>();
    if parts.len() != 3
        || parts.iter().any(|part| part.is_empty())
        || !token(parts[0])
        || !matches!(parts[2], "HTTP/1.1" | "HTTP/1.0")
    {
        return Err(invalid());
    }
    let connect = parts[0] == "CONNECT";
    let target = if connect {
        if parts[1].contains(['/', '?', '#', '@', '\\']) || !parts[1].contains(':') {
            return Err(invalid());
        }
        let url = format!("https://{}/", parts[1]);
        let target = normalize_target(&url)?;
        // CONNECT authority always supplies a port, including bracketed IPv6.
        let suffix = parts[1].rsplit_once(':').ok_or_else(invalid)?.1;
        if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
            return Err(invalid());
        }
        target
    } else {
        let target = normalize_target(parts[1])?;
        if target.origin.scheme != OriginScheme::Http {
            return Err(invalid());
        }
        target
    };
    let mut headers = Vec::new();
    let mut host = None;
    let mut content_length = None;
    for (index, line) in lines.enumerate() {
        if index >= limits.max_header_fields {
            return Err(NetworkError::new(NetworkErrorKind::LimitExceeded));
        }
        let (name, value) = line.split_once(':').ok_or_else(invalid)?;
        if !token(name) || value.bytes().any(|b| (b < 32 && b != b'\t') || b == 127) {
            return Err(invalid());
        }
        let name = name.to_ascii_lowercase();
        let value = value.trim_matches([' ', '\t']);
        match name.as_str() {
            "host" => {
                if host.replace(value).is_some() {
                    return Err(invalid());
                }
            }
            "content-length" => {
                if content_length.is_some()
                    || value.is_empty()
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(invalid());
                }
                content_length = Some(value.parse::<u64>().map_err(|_| invalid())?);
                headers.push((name, value.to_owned()));
            }
            // Connection-nominated headers create ambiguous hop semantics. Accept only closure
            // preferences, rebuild the header and reject protocol upgrades or chunked framing.
            "connection" => {
                if value.split(',').any(|part| {
                    !matches!(
                        part.trim().to_ascii_lowercase().as_str(),
                        "close" | "keep-alive"
                    )
                }) {
                    return Err(invalid());
                }
            }
            "transfer-encoding" | "upgrade" | "expect" | "trailer" => return Err(invalid()),
            "keep-alive" => {}
            _ if name.starts_with("proxy-") => {}
            _ => headers.push((name, value.to_owned())),
        }
    }
    let host = host.ok_or_else(invalid)?;
    if host.contains(['/', '?', '#', '@', '\\']) {
        return Err(invalid());
    }
    let scheme = if target.origin.scheme == OriginScheme::Http {
        "http"
    } else {
        "https"
    };
    let host_origin = NormalizedOrigin::parse(&format!("{scheme}://{host}/"))?;
    if host_origin != target.origin || (connect && content_length.is_some_and(|length| length != 0))
    {
        return Err(invalid());
    }
    Ok(Request {
        target,
        connect,
        headers,
        method: parts[0].into(),
        content_length: content_length.unwrap_or(0),
    })
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

async fn copy_limited<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut reader: R,
    mut writer: W,
    state: Arc<BrokerState>,
    mut copied: u64,
) -> Result<(), NetworkError> {
    let mut buffer = [0u8; 8192];
    loop {
        let read = timeout(
            Duration::from_millis(state.policy.limits.idle_timeout_ms),
            reader.read(&mut buffer),
        )
        .await
        .map_err(|_| NetworkError::new(NetworkErrorKind::Timeout))??;
        if read == 0 {
            writer.shutdown().await?;
            return Ok(());
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or_else(|| NetworkError::new(NetworkErrorKind::LimitExceeded))?;
        if copied > state.policy.limits.max_connection_bytes {
            return Err(NetworkError::new(NetworkErrorKind::LimitExceeded));
        }
        state.charge(read)?;
        timeout(
            Duration::from_millis(state.policy.limits.idle_timeout_ms),
            writer.write_all(&buffer[..read]),
        )
        .await
        .map_err(|_| NetworkError::new(NetworkErrorKind::Timeout))??;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_smuggling_and_authority_ambiguities_are_rejected() {
        let limits = BrokerLimits::default();
        let cases = [
            "GET http://example.com/ HTTP/1.1\r\nHost: other.com\r\n\r\n",
            "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nhOSt: example.com\r\n\r\n",
            "POST http://example.com/ HTTP/1.1\r\nHost: example.com\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\n",
            "POST http://example.com/ HTTP/1.1\r\nHost: example.com\r\nTransfer-Encoding: chunked\r\n\r\n",
            "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n folded: data\r\n\r\n",
            "GET /relative HTTP/1.1\r\nHost: example.com\r\n\r\n",
            "GET http://a@example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n",
            "GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\nConnection: x-auth\r\nx-auth: secret\r\n\r\n",
            "CONNECT example.com HTTP/1.1\r\nHost: example.com\r\n\r\n",
        ];
        for case in cases {
            assert!(parse_request(case.as_bytes(), &limits).is_err(), "{case}");
        }
    }

    #[test]
    fn headers_are_validated_and_proxy_credentials_are_omitted() {
        let parsed = parse_request(b"GET http://EXAMPLE.com:80/x?q=1 HTTP/1.1\r\nHost: example.com\r\nProxy-Authorization: Bearer private\r\nAuthorization: Bearer origin-token\r\n\r\n", &Default::default()).unwrap();
        assert_eq!(parsed.target.path_and_query, "/x?q=1");
        assert_eq!(
            parsed.headers,
            [("authorization".to_owned(), "Bearer origin-token".to_owned())]
        );
        assert!(
            parse_request(
                b"CONNECT [::1]:443 HTTP/1.1\r\nHost: [::1]:443\r\n\r\n",
                &Default::default()
            )
            .is_ok()
        );
    }
}
