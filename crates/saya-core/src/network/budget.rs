use super::{BrokerLimits, BrokerPolicy, NetworkError, NetworkErrorKind};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

/// One host-owned Run ceiling shared by independent origin-scoped listeners.
/// Connection attempts and bytes reserved before forwarding are cumulative and
/// never refunded. Only live connection slots are released on drop or abort.
#[derive(Debug)]
pub struct RunNetworkBudget {
    max_connections: u64,
    max_total_connections: u64,
    max_total_bytes: u64,
    deadline: tokio::time::Instant,
    active: AtomicU64,
    accepted: AtomicU64,
    bytes: AtomicU64,
    unavailable: AtomicBool,
}
impl RunNetworkBudget {
    /// The deadline starts here and remains fixed when listeners are added or
    /// restarted. Construct this once when the host's Run begins networking.
    pub fn new(limits: &BrokerLimits) -> Result<Arc<Self>, NetworkError> {
        BrokerPolicy {
            limits: limits.clone(),
            ..Default::default()
        }
        .validate()?;
        Ok(Arc::new(Self {
            max_connections: limits.max_connections as u64,
            max_total_connections: limits.max_total_connections as u64,
            max_total_bytes: limits.max_total_bytes,
            deadline: tokio::time::Instant::now() + Duration::from_millis(limits.active_timeout_ms),
            active: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            unavailable: AtomicBool::new(false),
        }))
    }
    pub(super) fn check(&self) -> Result<(), NetworkError> {
        if self.unavailable.load(Ordering::Acquire) || tokio::time::Instant::now() >= self.deadline
        {
            return Err(NetworkError::new(NetworkErrorKind::LimitExceeded));
        }
        Ok(())
    }
    pub(super) fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
    pub(super) fn charge(&self, bytes: u64) -> Result<(), NetworkError> {
        self.check()?;
        self.bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes)
                    .filter(|total| *total <= self.max_total_bytes)
            })
            .map(|_| ())
            .map_err(|_| NetworkError::new(NetworkErrorKind::LimitExceeded))
    }
    pub(super) fn acquire(self: &Arc<Self>) -> Result<ConnectionLease, NetworkError> {
        self.check()?;
        self.active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active
                    .checked_add(1)
                    .filter(|next| *next <= self.max_connections)
            })
            .map_err(|_| NetworkError::new(NetworkErrorKind::LimitExceeded))?;
        let lease = ConnectionLease(self.clone());
        self.accepted
            .try_update(Ordering::AcqRel, Ordering::Acquire, |accepted| {
                accepted
                    .checked_add(1)
                    .filter(|next| *next <= self.max_total_connections)
            })
            .map_err(|_| NetworkError::new(NetworkErrorKind::LimitExceeded))?;
        self.check()?;
        Ok(lease)
    }
}
pub(super) struct ConnectionLease(Arc<RunNetworkBudget>);
impl Drop for ConnectionLease {
    fn drop(&mut self) {
        if self
            .0
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                active.checked_sub(1)
            })
            .is_err()
        {
            // A broken ownership invariant must never wrap or reopen capacity.
            self.0.unavailable.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slots_release_on_failure_but_total_attempts_and_bytes_never_refund() {
        let limits = BrokerLimits {
            max_connections: 1,
            max_total_connections: 2,
            max_total_bytes: 10,
            ..Default::default()
        };
        let budget = RunNetworkBudget::new(&limits).unwrap();
        let first = budget.acquire().unwrap();
        assert!(budget.acquire().is_err());
        assert_eq!(budget.accepted.load(Ordering::Acquire), 1);
        drop(first);
        let second = budget.acquire().unwrap();
        drop(second);
        assert!(budget.acquire().is_err());
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
        assert_eq!(budget.accepted.load(Ordering::Acquire), 2);
        budget.charge(7).unwrap();
        assert!(budget.charge(4).is_err());
        assert_eq!(budget.bytes.load(Ordering::Acquire), 7);
        budget.charge(3).unwrap();
        assert!(budget.charge(1).is_err());
    }
    #[test]
    fn concurrent_reservations_never_exceed_the_shared_total() {
        let limits = BrokerLimits {
            max_connections: 256,
            max_total_connections: 101,
            max_total_bytes: 1000,
            ..Default::default()
        };
        let budget = RunNetworkBudget::new(&limits).unwrap();
        let threads = (0..8)
            .map(|_| {
                let budget = budget.clone();
                std::thread::spawn(move || {
                    let mut count = 0;
                    for _ in 0..100 {
                        if let Ok(lease) = budget.acquire() {
                            count += 1;
                            drop(lease);
                        }
                        let _ = budget.charge(3);
                    }
                    count
                })
            })
            .collect::<Vec<_>>();
        let accepted = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum::<usize>();
        assert_eq!(accepted, 101);
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
        assert_eq!(budget.accepted.load(Ordering::Acquire), 101);
        assert_eq!(budget.bytes.load(Ordering::Acquire), 999);
    }
    #[tokio::test(start_paused = true)]
    async fn shared_run_deadline_is_not_restarted_by_later_use() {
        let budget = RunNetworkBudget::new(&BrokerLimits {
            active_timeout_ms: 10,
            ..Default::default()
        })
        .unwrap();
        tokio::time::advance(Duration::from_millis(11)).await;
        assert!(budget.acquire().is_err());
        assert!(budget.charge(1).is_err());
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
    }
    #[tokio::test]
    async fn aborting_an_unfinished_task_releases_only_its_live_slot() {
        let budget = RunNetworkBudget::new(&BrokerLimits {
            max_connections: 1,
            ..Default::default()
        })
        .unwrap();
        let slot = budget.acquire().unwrap();
        let task = tokio::spawn(async move {
            let _slot = slot;
            std::future::pending::<()>().await
        });
        task.abort();
        let _ = task.await;
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
        assert_eq!(budget.accepted.load(Ordering::Acquire), 1);
        drop(budget.acquire().unwrap());
        assert_eq!(budget.accepted.load(Ordering::Acquire), 2);
    }
    struct Never;
    impl crate::boundary::Cancellation for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }
    async fn wait_active(budget: &RunNetworkBudget, expected: u64) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while budget.active.load(Ordering::Acquire) != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }
    async fn expect_closed(mut stream: tokio::net::TcpStream) {
        use tokio::io::AsyncReadExt;
        let mut byte = [0u8; 1];
        let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(result, Ok(0) | Err(_)));
    }
    #[tokio::test]
    async fn independent_listeners_share_live_slots_total_attempts_and_shutdown_release() {
        use super::super::NetworkBroker;
        let budget = RunNetworkBudget::new(&BrokerLimits {
            max_connections: 1,
            max_total_connections: 2,
            ..Default::default()
        })
        .unwrap();
        let first =
            NetworkBroker::start_scoped(BrokerPolicy::default(), Arc::new(Never), budget.clone())
                .await
                .unwrap();
        let second =
            NetworkBroker::start_scoped(BrokerPolicy::default(), Arc::new(Never), budget.clone())
                .await
                .unwrap();
        let held = tokio::net::TcpStream::connect(first.tcp_endpoint())
            .await
            .unwrap();
        wait_active(&budget, 1).await;
        expect_closed(
            tokio::net::TcpStream::connect(second.tcp_endpoint())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(budget.accepted.load(Ordering::Acquire), 1);
        first.shutdown().await.unwrap();
        wait_active(&budget, 0).await;
        drop(held);
        let held = tokio::net::TcpStream::connect(second.tcp_endpoint())
            .await
            .unwrap();
        wait_active(&budget, 1).await;
        assert_eq!(budget.accepted.load(Ordering::Acquire), 2);
        drop(held);
        wait_active(&budget, 0).await;
        expect_closed(
            tokio::net::TcpStream::connect(second.tcp_endpoint())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(budget.active.load(Ordering::Acquire), 0);
        assert_eq!(budget.accepted.load(Ordering::Acquire), 2);
        second.shutdown().await.unwrap();
    }

    async fn server() -> (
        String,
        tokio::sync::mpsc::Receiver<Vec<u8>>,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (sender, received) = tokio::sync::mpsc::channel(8);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let sender = sender.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let _ = stream.read_to_end(&mut bytes).await;
                    let _ = sender.send(bytes).await;
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await;
                });
            }
        });
        (origin, received, task)
    }
    fn policy(origin: &str) -> BrokerPolicy {
        BrokerPolicy {
            allowed_origins: vec![origin.into()],
            localhost_development_origin: Some(origin.into()),
            ..Default::default()
        }
    }
    async fn send(endpoint: std::net::SocketAddr, origin: &str) -> Vec<u8> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        let request = format!(
            "GET {origin}/ HTTP/1.1\r\nHost: {}\r\n\r\n",
            origin.trim_start_matches("http://")
        );
        let _ = stream.write_all(request.as_bytes()).await;
        let _ = stream.shutdown().await;
        let mut bytes = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut bytes))
            .await
            .unwrap();
        bytes
    }
    #[tokio::test]
    async fn separate_origin_listeners_share_bytes_and_restarting_cannot_reset_spending() {
        use super::super::NetworkBroker;
        let (first_origin, mut first_received, first_server) = server().await;
        let (second_origin, mut second_received, second_server) = server().await;
        let rewritten = format!(
            "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            first_origin.trim_start_matches("http://")
        );
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
        let capacity = rewritten.len() as u64 + response.len() as u64 + 1;
        let budget = RunNetworkBudget::new(&BrokerLimits {
            max_total_bytes: capacity,
            ..Default::default()
        })
        .unwrap();
        let first =
            NetworkBroker::start_scoped(policy(&first_origin), Arc::new(Never), budget.clone())
                .await
                .unwrap();
        let second =
            NetworkBroker::start_scoped(policy(&second_origin), Arc::new(Never), budget.clone())
                .await
                .unwrap();
        assert!(
            send(second.tcp_endpoint(), &first_origin)
                .await
                .starts_with(b"HTTP/1.1 403")
        );
        assert_eq!(budget.bytes.load(Ordering::Acquire), 0);
        assert!(
            send(first.tcp_endpoint(), &first_origin)
                .await
                .starts_with(b"HTTP/1.1 200")
        );
        assert!(!first_received.recv().await.unwrap().is_empty());
        assert_eq!(budget.bytes.load(Ordering::Acquire), capacity - 1);
        assert!(
            !send(second.tcp_endpoint(), &second_origin)
                .await
                .starts_with(b"HTTP/1.1 200")
        );
        assert!(second_received.recv().await.unwrap().is_empty());
        assert_eq!(budget.bytes.load(Ordering::Acquire), capacity - 1);
        first.shutdown().await.unwrap();
        let restarted =
            NetworkBroker::start_scoped(policy(&first_origin), Arc::new(Never), budget.clone())
                .await
                .unwrap();
        assert!(
            !send(restarted.tcp_endpoint(), &first_origin)
                .await
                .starts_with(b"HTTP/1.1 200")
        );
        assert!(first_received.recv().await.unwrap().is_empty());
        second.shutdown().await.unwrap();
        restarted.shutdown().await.unwrap();
        first_server.abort();
        second_server.abort();
        let _ = first_server.await;
        let _ = second_server.await;
    }
}
