use super::*;
use crate::{
    boundary::Cancellation,
    network::{
        BrokerLimits, BrokerPolicy, NetworkBroker, NetworkError, NetworkResolver, RunNetworkBudget,
    },
};
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
struct PrivateResolver(Arc<AtomicUsize>);
impl NetworkResolver for PrivateResolver {
    fn resolve<'a>(
        &'a self,
        _: &'a str,
        _: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, NetworkError>> + Send + 'a>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
        })
    }
}
fn limits() -> McpLimits {
    McpLimits {
        connect_timeout: Duration::from_secs(2),
        ..McpLimits::default()
    }
}
fn policy(origin: &str, limits: BrokerLimits) -> BrokerPolicy {
    BrokerPolicy {
        allowed_origins: vec![origin.into()],
        localhost_development_origin: Some(origin.into()),
        limits,
        ..BrokerPolicy::default()
    }
}
struct Origin {
    endpoint: SocketAddr,
    requests: Arc<Mutex<Vec<Vec<u8>>>>,
    task: JoinHandle<()>,
}
impl Origin {
    async fn start(body: Vec<u8>, redirect: Option<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0u8; 1024];
                    let count = stream.read(&mut buffer).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                        break;
                    }
                }
                captured.lock().unwrap().push(request);
                let mut response = if let Some(location) = &redirect {
                    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").into_bytes()
                } else {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes()
                };
                if redirect.is_none() {
                    response.extend_from_slice(&body);
                }
                let _ = stream.write_all(&response).await;
            }
        });
        Self {
            endpoint,
            requests,
            task,
        }
    }
    fn url(&self) -> String {
        format!("http://{}/mcp", self.endpoint)
    }
    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

#[test]
fn scoped_proxy_builder_rejects_non_loopback_or_zero_port() {
    assert!(McpHttpClient::new_scoped_proxy(&limits(), "192.0.2.1:80".parse().unwrap()).is_err());
    assert!(McpHttpClient::new_scoped_proxy(&limits(), "127.0.0.1:0".parse().unwrap()).is_err());
}

#[tokio::test]
async fn scoped_https_resolves_through_broker_and_rejects_private_answers_before_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let origin = format!("https://controlled.example.test:{port}");
    let rules = BrokerPolicy {
        allowed_origins: vec![origin.clone()],
        ..BrokerPolicy::default()
    };
    let shared = RunNetworkBudget::new(&rules.limits).unwrap();
    let resolved = Arc::new(AtomicUsize::new(0));
    let broker = NetworkBroker::start_scoped_with_resolver(
        rules,
        Arc::new(Never),
        shared,
        Arc::new(PrivateResolver(resolved.clone())),
    )
    .await
    .unwrap();
    let client = McpHttpClient::new_scoped_proxy(&limits(), broker.tcp_endpoint()).unwrap();
    assert!(
        client
            .client
            .get(format!("{origin}/mcp"))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_err()
    );
    assert_eq!(resolved.load(Ordering::SeqCst), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
    broker.shutdown().await.unwrap();
}

#[tokio::test]
async fn loopback_destination_cannot_bypass_the_explicit_origin_proxy() {
    let origin = Origin::start(b"origin response".to_vec(), None).await;
    let rules = BrokerPolicy {
        allowed_origins: vec!["http://elsewhere.example.test".into()],
        ..BrokerPolicy::default()
    };
    let broker = NetworkBroker::start(rules, Arc::new(Never)).await.unwrap();
    let client = McpHttpClient::new_scoped_proxy(&limits(), broker.tcp_endpoint()).unwrap();
    let response = client
        .client
        .get(origin.url())
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    assert!(origin.requests.lock().unwrap().is_empty());
    broker.shutdown().await.unwrap();
    origin.stop().await;
}

#[tokio::test]
async fn independently_scoped_http_clients_share_the_run_byte_ceiling() {
    let body = vec![b'x'; 300];
    let origin = Origin::start(body.clone(), None).await;
    let host = format!("http://{}", origin.endpoint);
    let rules = policy(
        &host,
        BrokerLimits {
            max_total_bytes: 600,
            ..BrokerLimits::default()
        },
    );
    let shared = RunNetworkBudget::new(&rules.limits).unwrap();
    let first = NetworkBroker::start_scoped(rules.clone(), Arc::new(Never), shared.clone())
        .await
        .unwrap();
    let second = NetworkBroker::start_scoped(rules, Arc::new(Never), shared)
        .await
        .unwrap();
    let one = McpHttpClient::new_scoped_proxy(&limits(), first.tcp_endpoint()).unwrap();
    let two = McpHttpClient::new_scoped_proxy(&limits(), second.tcp_endpoint()).unwrap();
    let response = one
        .client
        .get(origin.url())
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.bytes().await.unwrap().as_ref(), body);
    let second_response = two
        .client
        .get(origin.url())
        .timeout(Duration::from_secs(2))
        .send()
        .await;
    if let Ok(response) = second_response
        && let Ok(received) = response.bytes().await
    {
        assert_ne!(received.as_ref(), body);
    }
    assert!(!origin.requests.lock().unwrap().is_empty());
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
    origin.stop().await;
}

#[tokio::test]
async fn scoped_client_does_not_follow_redirects_or_fall_back_after_proxy_shutdown() {
    let destination = Origin::start(b"must remain private".to_vec(), None).await;
    let origin = Origin::start(Vec::new(), Some(destination.url())).await;
    let host = format!("http://{}", origin.endpoint);
    let broker = NetworkBroker::start(policy(&host, BrokerLimits::default()), Arc::new(Never))
        .await
        .unwrap();
    let client = McpHttpClient::new_scoped_proxy(&limits(), broker.tcp_endpoint()).unwrap();
    let response = client
        .client
        .get(origin.url())
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 302);
    assert_eq!(origin.requests.lock().unwrap().len(), 1);
    assert!(destination.requests.lock().unwrap().is_empty());
    broker.shutdown().await.unwrap();
    assert!(
        client
            .client
            .get(origin.url())
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_err()
    );
    assert_eq!(origin.requests.lock().unwrap().len(), 1);
    origin.stop().await;
    destination.stop().await;
}
