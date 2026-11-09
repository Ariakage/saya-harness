use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

use reqwest::Url;
use serde::{Deserialize, Serialize};

use super::{NetworkError, NetworkErrorKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginScheme {
    Http,
    Https,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NormalizedOrigin {
    pub scheme: OriginScheme,
    /// Lowercase IDNA ASCII domain or canonical IP without IPv6 brackets.
    pub host: String,
    pub port: u16,
}
impl NormalizedOrigin {
    pub fn parse(value: &str) -> Result<Self, NetworkError> {
        let target = normalize_target(value)?;
        if target.path_and_query != "/" {
            return Err(NetworkError::new(NetworkErrorKind::InvalidTarget));
        }
        Ok(target.origin)
    }

    pub fn authority(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        let default_port = if self.scheme == OriginScheme::Http {
            80
        } else {
            443
        };
        if self.port == default_port {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkTarget {
    pub origin: NormalizedOrigin,
    pub path_and_query: String,
}

/// Strict normalization shared by the proxy and host-owned HTTP tool clients.
/// It rejects user-info, fragments, backslashes, control characters and zero ports.
pub fn normalize_target(value: &str) -> Result<NetworkTarget, NetworkError> {
    let invalid = || NetworkError::new(NetworkErrorKind::InvalidTarget);
    if value.len() > 16 * 1024
        || value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return Err(invalid());
    }
    let authority = value
        .split_once("://")
        .ok_or_else(invalid)?
        .1
        .split(['/', '?', '#'])
        .next()
        .ok_or_else(invalid)?;
    if authority.is_empty() || authority.contains('@') || authority.contains('%') {
        return Err(invalid());
    }
    let url = Url::parse(value).map_err(|_| invalid())?;
    let scheme = match url.scheme() {
        "http" => OriginScheme::Http,
        "https" => OriginScheme::Https,
        _ => return Err(invalid()),
    };
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(invalid());
    }
    let raw_host = url
        .host_str()
        .ok_or_else(invalid)?
        .trim_start_matches('[')
        .trim_end_matches(']');
    let host = if let Ok(ip) = raw_host.parse::<IpAddr>() {
        ip.to_string()
    } else {
        let host = raw_host
            .strip_suffix('.')
            .unwrap_or(raw_host)
            .to_ascii_lowercase();
        if host.len() > 253
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        {
            return Err(invalid());
        }
        host
    };
    let port = url
        .port_or_known_default()
        .filter(|port| *port != 0)
        .ok_or_else(invalid)?;
    let mut path_and_query = url.path().to_owned();
    if let Some(query) = url.query() {
        path_and_query.push('?');
        path_and_query.push_str(query);
    }
    Ok(NetworkTarget {
        origin: NormalizedOrigin { scheme, host, port },
        path_and_query,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrokerLimits {
    pub max_header_bytes: usize,
    pub max_header_fields: usize,
    pub max_connections: usize,
    pub max_total_connections: usize,
    pub max_connection_bytes: u64,
    pub max_total_bytes: u64,
    pub header_timeout_ms: u64,
    pub connect_timeout_ms: u64,
    pub idle_timeout_ms: u64,
    pub connection_timeout_ms: u64,
    pub active_timeout_ms: u64,
}
impl Default for BrokerLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: 16 * 1024,
            max_header_fields: 128,
            max_connections: 32,
            max_total_connections: 1024,
            max_connection_bytes: 16 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
            header_timeout_ms: 5000,
            connect_timeout_ms: 5000,
            idle_timeout_ms: 30_000,
            connection_timeout_ms: 120_000,
            active_timeout_ms: 30 * 60 * 1000,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrokerPolicy {
    pub allowed_origins: Vec<String>,
    /// One exact HTTP(S) origin whose hostname is localhost or a loopback IP.
    /// It must also appear in allowed_origins and only loopback DNS answers are accepted.
    pub localhost_development_origin: Option<String>,
    pub unix_socket: Option<PathBuf>,
    pub limits: BrokerLimits,
}
impl BrokerPolicy {
    pub fn validate(&self) -> Result<(), NetworkError> {
        let invalid = || NetworkError::new(NetworkErrorKind::InvalidPolicy);
        let limits = &self.limits;
        if self.allowed_origins.len() > 256
            || limits.max_header_bytes == 0
            || limits.max_header_bytes > 64 * 1024
            || limits.max_header_fields == 0
            || limits.max_header_fields > 256
            || limits.max_connections == 0
            || limits.max_connections > 256
            || limits.max_total_connections == 0
            || limits.max_total_connections > 100_000
            || limits.max_connection_bytes == 0
            || limits.max_connection_bytes > 1024 * 1024 * 1024
            || limits.max_total_bytes == 0
            || limits.max_total_bytes > 16 * 1024 * 1024 * 1024
            || [
                limits.header_timeout_ms,
                limits.connect_timeout_ms,
                limits.idle_timeout_ms,
                limits.connection_timeout_ms,
                limits.active_timeout_ms,
            ]
            .iter()
            .any(|n| *n == 0 || *n > 24 * 60 * 60 * 1000)
        {
            return Err(invalid());
        }
        let origins = self
            .allowed_origins
            .iter()
            .map(|origin| NormalizedOrigin::parse(origin))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(origin) = &self.localhost_development_origin {
            let origin = NormalizedOrigin::parse(origin)?;
            if !is_loopback_host(&origin.host) || !origins.contains(&origin) {
                return Err(invalid());
            }
        }
        Ok(())
    }

    pub fn check_origin(&self, origin: &NormalizedOrigin) -> Result<(), NetworkError> {
        self.validate()?;
        if self
            .allowed_origins
            .iter()
            .filter_map(|origin| NormalizedOrigin::parse(origin).ok())
            .any(|allowed| allowed == *origin)
        {
            Ok(())
        } else {
            Err(NetworkError::new(NetworkErrorKind::OriginDenied))
        }
    }

    /// All answers must satisfy the policy. A mixed public/private answer set is rejected in full.
    pub fn check_addresses(
        &self,
        origin: &NormalizedOrigin,
        addresses: &[IpAddr],
    ) -> Result<(), NetworkError> {
        self.check_origin(origin)?;
        let local = self
            .localhost_development_origin
            .as_deref()
            .and_then(|value| NormalizedOrigin::parse(value).ok())
            .is_some_and(|allowed| allowed == *origin);
        if addresses.is_empty()
            || addresses.len() > 64
            || !addresses.iter().all(|ip| {
                if local {
                    ip.is_loopback()
                } else {
                    is_public_address(*ip)
                }
            })
        {
            Err(NetworkError::new(NetworkErrorKind::AddressDenied))
        } else {
            Ok(())
        }
    }
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Conservative public-unicast policy based on IANA special-purpose address registries.
/// Translation/tunnel ranges and reserved/documentation space are rejected as a whole.
/// Sources: https://www.iana.org/assignments/iana-ipv4-special-registry/ and
/// https://www.iana.org/assignments/iana-ipv6-special-registry/.
pub fn is_public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}
fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(a, 0 | 10 | 127 | 224..=255)
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && ((b == 0 && (c == 0 || c == 2)) || (b == 168) || (b == 88 && c == 99)))
        && !(a == 198 && ((b == 18 || b == 19) || (b == 51 && c == 100)))
        && !(a == 203 && b == 0 && c == 113)
}
fn public_v6(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    // Only currently allocated global-unicast space, excluding protocol, transition and docs.
    segments[0] & 0xe000 == 0x2000
        && !(segments[0] == 0x2001 && (segments[1] < 0x0200 || segments[1] == 0x0db8))
        && segments[0] != 0x2002
        && !(segments[0] == 0x3fff && segments[1] < 0x1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_normalize_idna_case_default_ports_and_ip_encodings() {
        assert_eq!(
            NormalizedOrigin::parse("http://EXAMPLE.com.:80/").unwrap(),
            NormalizedOrigin::parse("http://example.com").unwrap()
        );
        assert_eq!(
            NormalizedOrigin::parse("https://例え.テスト").unwrap().host,
            "xn--r8jz45g.xn--zckzah"
        );
        assert_eq!(
            NormalizedOrigin::parse("http://2130706433").unwrap().host,
            "127.0.0.1"
        );
        assert_eq!(
            NormalizedOrigin::parse("http://0x7f000001").unwrap().host,
            "127.0.0.1"
        );
        assert_eq!(
            NormalizedOrigin::parse("https://[::1]:443").unwrap().host,
            "::1"
        );
        for origin in [
            "http://u:p@example.com",
            "http://@example.com",
            "http://example.com/#",
            "http://example.com:0",
            "http://bad_.test",
            "file:///etc/passwd",
            "http://example.com/path",
            "http://example.com/\\x",
            "http://%65xample.com",
            "http://example.com\r\n",
        ] {
            assert!(NormalizedOrigin::parse(origin).is_err(), "{origin}");
        }
    }

    #[test]
    fn private_metadata_loopback_special_and_mapped_addresses_are_denied() {
        for address in [
            "0.0.0.0",
            "10.1.2.3",
            "127.0.0.1",
            "100.64.1.2",
            "169.254.169.254",
            "172.31.1.1",
            "192.168.0.1",
            "192.0.0.9",
            "192.0.2.1",
            "198.18.1.2",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:8.8.8.8",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "64:ff9b::a00:1",
            "2002:7f00:1::",
            "2001:db8::1",
            "3fff::1",
        ] {
            assert!(!is_public_address(address.parse().unwrap()), "{address}");
        }
        for address in [
            "8.8.8.8",
            "1.1.1.1",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
        ] {
            assert!(is_public_address(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn exact_origin_port_and_mixed_dns_answers_are_checked() {
        let policy = BrokerPolicy {
            allowed_origins: vec!["https://example.com".into()],
            ..Default::default()
        };
        let origin = NormalizedOrigin::parse("https://example.com").unwrap();
        assert!(
            policy
                .check_addresses(&origin, &["8.8.8.8".parse().unwrap()])
                .is_ok()
        );
        assert!(
            policy
                .check_addresses(
                    &origin,
                    &["8.8.8.8".parse().unwrap(), "10.0.0.1".parse().unwrap()]
                )
                .is_err()
        );
        for wrong in [
            "http://example.com",
            "https://sub.example.com",
            "https://example.com:444",
        ] {
            assert!(
                policy
                    .check_origin(&NormalizedOrigin::parse(wrong).unwrap())
                    .is_err()
            );
        }
    }

    #[test]
    fn localhost_development_exception_is_single_origin_and_loopback_only() {
        let policy = BrokerPolicy {
            allowed_origins: vec![
                "http://localhost:8080".into(),
                "http://127.0.0.1:8081".into(),
            ],
            localhost_development_origin: Some("http://localhost:8080".into()),
            ..Default::default()
        };
        let origin = NormalizedOrigin::parse("http://localhost:8080").unwrap();
        assert!(
            policy
                .check_addresses(
                    &origin,
                    &["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()]
                )
                .is_ok()
        );
        assert!(
            policy
                .check_addresses(&origin, &["192.168.0.1".parse().unwrap()])
                .is_err()
        );
        assert!(
            policy
                .check_addresses(
                    &NormalizedOrigin::parse("http://127.0.0.1:8081").unwrap(),
                    &["127.0.0.1".parse().unwrap()]
                )
                .is_err()
        );
        let invalid = BrokerPolicy {
            allowed_origins: vec!["https://example.com".into()],
            localhost_development_origin: Some("https://example.com".into()),
            ..Default::default()
        };
        assert!(
            invalid
                .check_addresses(
                    &NormalizedOrigin::parse("https://example.com").unwrap(),
                    &["127.0.0.1".parse().unwrap()]
                )
                .is_err()
        );
    }
}
