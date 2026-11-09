use saya_core::boundary::Cancellation;
use saya_core::network::{
    BrokerLimits, BrokerPolicy, NetworkBroker, NetworkError, NetworkErrorKind, NetworkResolver,
};
use std::future::{Future, pending};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct Cancel(watch::Sender<bool>);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let mut receiver = self.0.subscribe();
        Box::pin(async move {
            while !*receiver.borrow_and_update() {
                if receiver.changed().await.is_err() {
                    break;
                }
            }
        })
    }
}

fn policy(origin: &str) -> BrokerPolicy {
    BrokerPolicy {
        allowed_origins: vec![origin.into()],
        localhost_development_origin: Some(origin.into()),
        ..Default::default()
    }
}
async fn send(endpoint: SocketAddr, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(endpoint).await.unwrap();
    stream.write_all(request).await.unwrap();
    stream.shutdown().await.unwrap();
    let mut response = Vec::new();
    let read = timeout(Duration::from_secs(3), stream.read_to_end(&mut response))
        .await
        .unwrap();
    if let Err(error) = read {
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
    }
    response
}
async fn origin_server(
    response: &'static [u8],
) -> (
    SocketAddr,
    oneshot::Receiver<Vec<u8>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let (sender, received) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        // Broker half-closes the HTTP upstream after the exact declared body.
        stream.read_to_end(&mut request).await.unwrap();
        let _ = sender.send(request);
        stream.write_all(response).await.unwrap();
    });
    (endpoint, received, task)
}

#[tokio::test]
async fn http_is_rewritten_once_without_proxy_credentials_or_pipeline_forwarding() {
    let (origin, captured, server) =
        origin_server(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok").await;
    let origin = format!("http://{origin}");
    let broker = NetworkBroker::start(policy(&origin), Arc::new(Never))
        .await
        .unwrap();
    let request = format!(
        "POST {origin}/path?q=1 HTTP/1.1\r\nHost: {}\r\nProxy-Authorization: Bearer synthetic-secret\r\nAuthorization: Bearer origin-only\r\nContent-Length: 3\r\nConnection: keep-alive\r\n\r\nabcGET http://denied.test/ HTTP/1.1\r\nHost: denied.test\r\n\r\n",
        origin.trim_start_matches("http://")
    );
    let response = send(broker.tcp_endpoint(), request.as_bytes()).await;
    assert!(response.starts_with(b"HTTP/1.1 200"));
    let captured = String::from_utf8(captured.await.unwrap()).unwrap();
    assert!(captured.starts_with("POST /path?q=1 HTTP/1.1\r\n"));
    assert!(captured.contains("Connection: close\r\n"));
    assert!(captured.contains("authorization: Bearer origin-only\r\n"));
    assert!(!captured.contains("synthetic-secret"));
    assert!(!captured.contains("denied.test"));
    assert!(captured.ends_with("\r\n\r\nabc"));
    server.await.unwrap();
    broker.shutdown().await.unwrap();
}

#[tokio::test]
async fn default_private_ip_policy_denies_even_allowlisted_loopback() {
    let origin = "http://127.0.0.1:9";
    let broker = NetworkBroker::start(
        BrokerPolicy {
            allowed_origins: vec![origin.into()],
            ..Default::default()
        },
        Arc::new(Never),
    )
    .await
    .unwrap();
    let response = send(
        broker.tcp_endpoint(),
        b"GET http://127.0.0.1:9/ HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n",
    )
    .await;
    assert!(response.starts_with(b"HTTP/1.1 403"));
    let denied = send(
        broker.ipv6_endpoint(),
        b"GET http://example.test/ HTTP/1.1\r\nHost: example.test\r\n\r\n",
    )
    .await;
    assert!(denied.starts_with(b"HTTP/1.1 403"));
    broker.shutdown().await.unwrap();
}

#[tokio::test]
async fn redirects_are_returned_without_fetching_the_location() {
    let (origin, _, server) = origin_server(b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/metadata\r\nContent-Length: 0\r\n\r\n").await;
    let url = format!("http://{origin}");
    let broker = NetworkBroker::start(policy(&url), Arc::new(Never))
        .await
        .unwrap();
    let response = send(
        broker.tcp_endpoint(),
        format!("GET {url}/ HTTP/1.1\r\nHost: {origin}\r\n\r\n").as_bytes(),
    )
    .await;
    assert!(response.starts_with(b"HTTP/1.1 302"));
    assert!(String::from_utf8_lossy(&response).contains("169.254.169.254"));
    server.await.unwrap();
    broker.shutdown().await.unwrap();
}

#[tokio::test]
async fn connect_tunnels_transfer_bytes_and_explicit_shutdown_closes_both_ends() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = origin.local_addr().unwrap();
    let broker = NetworkBroker::start(policy(&format!("https://{endpoint}")), Arc::new(Never))
        .await
        .unwrap();
    let mut client = TcpStream::connect(broker.ipv6_endpoint()).await.unwrap();
    client
        .write_all(
            format!("CONNECT {endpoint} HTTP/1.1\r\nHost: {endpoint}\r\n\r\nhello").as_bytes(),
        )
        .await
        .unwrap();
    let (mut upstream, _) = origin.accept().await.unwrap();
    let mut hello = [0u8; 5];
    upstream.read_exact(&mut hello).await.unwrap();
    assert_eq!(&hello, b"hello");
    upstream.write_all(b"world").await.unwrap();
    let mut response = vec![0u8; b"HTTP/1.1 200 Connection Established\r\n\r\nworld".len()];
    client.read_exact(&mut response).await.unwrap();
    assert!(response.ends_with(b"world"));
    let broker_endpoint = broker.tcp_endpoint();
    broker.shutdown().await.unwrap();
    let mut byte = [0u8; 1];
    assert_eq!(
        timeout(Duration::from_secs(1), client.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert_eq!(
        timeout(Duration::from_secs(1), upstream.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(TcpStream::connect(broker_endpoint).await.is_err());
}

struct RebindingResolver(AtomicUsize);
impl NetworkResolver for RebindingResolver {
    fn resolve<'a>(
        &'a self,
        _: &'a str,
        _: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, NetworkError>> + Send + 'a>> {
        let count = self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(vec![
                if count == 0 {
                    "127.0.0.1"
                } else {
                    "169.254.169.254"
                }
                .parse()
                .unwrap(),
            ])
        })
    }
}
#[tokio::test]
async fn each_connection_resolves_again_and_rebinding_cannot_reuse_local_exception() {
    let (origin, _, server) = origin_server(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let url = format!("http://localhost:{}", origin.port());
    let resolver = Arc::new(RebindingResolver(AtomicUsize::new(0)));
    let broker =
        NetworkBroker::start_with_resolver(policy(&url), Arc::new(Never), resolver.clone())
            .await
            .unwrap();
    let request = format!(
        "GET {url}/ HTTP/1.1\r\nHost: localhost:{}\r\n\r\n",
        origin.port()
    );
    let first = send(broker.tcp_endpoint(), request.as_bytes()).await;
    assert!(first.starts_with(b"HTTP/1.1 200"));
    let second = send(broker.tcp_endpoint(), request.as_bytes()).await;
    assert!(second.starts_with(b"HTTP/1.1 403"));
    assert_eq!(resolver.0.load(Ordering::SeqCst), 2);
    server.await.unwrap();
    broker.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancellation_closes_pending_requests_and_both_listeners() {
    let cancel = Arc::new(Cancel(watch::channel(false).0));
    let broker = NetworkBroker::start(Default::default(), cancel.clone())
        .await
        .unwrap();
    let mut client = TcpStream::connect(broker.tcp_endpoint()).await.unwrap();
    client.write_all(b"GET ").await.unwrap();
    cancel.0.send_replace(true);
    let endpoint = broker.tcp_endpoint();
    broker.shutdown().await.unwrap();
    let mut response = Vec::new();
    let result = timeout(Duration::from_secs(1), client.read_to_end(&mut response))
        .await
        .unwrap();
    assert!(result.is_ok() || result.unwrap_err().kind() == std::io::ErrorKind::ConnectionReset);
    assert!(TcpStream::connect(endpoint).await.is_err());
}

#[tokio::test]
async fn header_timeout_size_body_and_byte_limits_are_enforced() {
    let broker = NetworkBroker::start(
        BrokerPolicy {
            limits: BrokerLimits {
                max_header_bytes: 64,
                header_timeout_ms: 20,
                ..Default::default()
            },
            ..Default::default()
        },
        Arc::new(Never),
    )
    .await
    .unwrap();
    let mut slow = TcpStream::connect(broker.tcp_endpoint()).await.unwrap();
    slow.write_all(b"GET ").await.unwrap();
    let mut response = Vec::new();
    timeout(Duration::from_secs(1), slow.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 408"));
    let large = send(broker.tcp_endpoint(), &[b'X'; 128]).await;
    assert!(large.starts_with(b"HTTP/1.1 413"));
    broker.shutdown().await.unwrap();

    let (origin, _, server) = origin_server(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let url = format!("http://{origin}");
    let mut limited = policy(&url);
    limited.limits.max_connection_bytes = 3;
    let broker = NetworkBroker::start(limited, Arc::new(Never))
        .await
        .unwrap();
    let response = send(
        broker.tcp_endpoint(),
        format!("POST {url}/ HTTP/1.1\r\nHost: {origin}\r\nContent-Length: 4\r\n\r\nbody")
            .as_bytes(),
    )
    .await;
    assert!(response.starts_with(b"HTTP/1.1 413"));
    broker.shutdown().await.unwrap();
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn unix_bridge_is_private_and_cleaned_without_overwriting_existing_files() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.path().join("broker.sock");
    let broker = NetworkBroker::start(
        BrokerPolicy {
            unix_socket: Some(path.clone()),
            ..Default::default()
        },
        Arc::new(Never),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
    stream
        .write_all(b"GET http://denied.test/ HTTP/1.1\r\nHost: denied.test\r\n\r\n")
        .await
        .unwrap();
    let mut result = Vec::new();
    stream.read_to_end(&mut result).await.unwrap();
    assert!(result.starts_with(b"HTTP/1.1 403"));
    broker.shutdown().await.unwrap();
    assert!(!path.exists());
    std::fs::write(&path, "preserve").unwrap();
    let result = NetworkBroker::start(
        BrokerPolicy {
            unix_socket: Some(path.clone()),
            ..Default::default()
        },
        Arc::new(Never),
    )
    .await;
    assert!(matches!(
        result,
        Err(NetworkError {
            kind: NetworkErrorKind::InvalidPolicy
        })
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "preserve");
}

#[tokio::test]
async fn active_timeout_closes_clients_but_port_leases_remain_until_handle_cleanup() {
    let broker = NetworkBroker::start(
        BrokerPolicy {
            limits: BrokerLimits {
                active_timeout_ms: 20,
                ..Default::default()
            },
            ..Default::default()
        },
        Arc::new(Never),
    )
    .await
    .unwrap();
    let v4 = broker.tcp_endpoint();
    let v6 = broker.ipv6_endpoint();
    let mut stream = TcpStream::connect(v4).await.unwrap();
    stream.write_all(b"GET ").await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!broker.is_running());
    assert!(std::net::TcpListener::bind(v4).is_err());
    assert!(std::net::TcpListener::bind(v6).is_err());
    let mut result = Vec::new();
    let read = timeout(Duration::from_secs(1), stream.read_to_end(&mut result))
        .await
        .unwrap();
    assert!(read.is_ok() || read.unwrap_err().kind() == std::io::ErrorKind::ConnectionReset);
    broker.shutdown().await.unwrap();
    assert!(TcpStream::connect(v4).await.is_err());
    assert!(TcpStream::connect(v6).await.is_err());
}

#[tokio::test]
async fn concurrent_tunnels_and_transfer_budgets_are_bounded() {
    let origin = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = origin.local_addr().unwrap();
    let mut config = policy(&format!("https://{endpoint}"));
    config.limits.max_connections = 1;
    config.limits.max_total_bytes = 3;
    let broker = NetworkBroker::start(config, Arc::new(Never)).await.unwrap();
    let mut client = TcpStream::connect(broker.tcp_endpoint()).await.unwrap();
    client
        .write_all(format!("CONNECT {endpoint} HTTP/1.1\r\nHost: {endpoint}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let (mut upstream, _) = origin.accept().await.unwrap();
    let mut handshake = vec![0u8; b"HTTP/1.1 200 Connection Established\r\n\r\n".len()];
    client.read_exact(&mut handshake).await.unwrap();
    let mut other = TcpStream::connect(broker.tcp_endpoint()).await.unwrap();
    let mut byte = [0u8; 1];
    let read = timeout(Duration::from_secs(1), other.read(&mut byte))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0)) || read.is_err());
    client.write_all(b"four").await.unwrap();
    let read = timeout(Duration::from_secs(1), upstream.read(&mut byte))
        .await
        .unwrap();
    assert_eq!(read.unwrap(), 0);
    broker.shutdown().await.unwrap();
}

#[tokio::test]
async fn declared_body_is_fully_forwarded_when_arriving_after_headers() {
    let (origin, captured, server) =
        origin_server(b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\n\r\n").await;
    let url = format!("http://{origin}");
    let broker = NetworkBroker::start(policy(&url), Arc::new(Never))
        .await
        .unwrap();
    let mut client = TcpStream::connect(broker.tcp_endpoint()).await.unwrap();
    client
        .write_all(
            format!("POST {url}/upload HTTP/1.1\r\nHost: {origin}\r\nContent-Length: 5\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(10)).await;
    client.write_all(b"hello").await.unwrap();
    let mut response = Vec::new();
    timeout(Duration::from_secs(1), client.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(response.starts_with(b"HTTP/1.1 201"));
    assert!(captured.await.unwrap().ends_with(b"\r\n\r\nhello"));
    server.await.unwrap();
    broker.shutdown().await.unwrap();
}
