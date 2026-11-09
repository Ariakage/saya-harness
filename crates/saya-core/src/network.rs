//! Per-run HTTP proxy with exact origin authorization and DNS-to-socket pinning.
//!
//! CONNECT authorizes a host/port tunnel, so encrypted methods and paths remain opaque. HTTP
//! requests are individually framed and use a fresh upstream connection. No redirects are followed.
//! The sandbox must independently prevent direct egress and access to other local sockets.

mod http;
mod policy;

pub use policy::{
    BrokerLimits, BrokerPolicy, NetworkTarget, NormalizedOrigin, OriginScheme, is_public_address,
    normalize_target,
};

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use crate::boundary::Cancellation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkErrorKind {
    InvalidPolicy,
    InvalidTarget,
    OriginDenied,
    AddressDenied,
    DnsFailed,
    InvalidRequest,
    LimitExceeded,
    Timeout,
    Io,
    Shutdown,
}

/// Error messages intentionally exclude URLs, header contents, credentials and resolver errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkError {
    pub kind: NetworkErrorKind,
}
impl NetworkError {
    fn new(kind: NetworkErrorKind) -> Self {
        Self { kind }
    }
}
impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "network broker {:?}", self.kind)
    }
}
impl std::error::Error for NetworkError {}
impl From<std::io::Error> for NetworkError {
    fn from(_: std::io::Error) -> Self {
        Self::new(NetworkErrorKind::Io)
    }
}

type ResolveFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<IpAddr>, NetworkError>> + Send + 'a>>;

/// Trusted host resolver boundary. Every new upstream connection invokes it anew.
pub trait NetworkResolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a>;
}

pub struct SystemResolver;
impl NetworkResolver for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> ResolveFuture<'a> {
        Box::pin(async move {
            tokio::net::lookup_host((host, port))
                .await
                .map(|addresses| addresses.take(65).map(|address| address.ip()).collect())
                .map_err(|_| NetworkError::new(NetworkErrorKind::DnsFailed))
        })
    }
}

pub struct NetworkBroker;

/// Owns the listener lifecycle. Explicit shutdown waits until all tunnels have been dropped.
/// Dropping the handle also signals shutdown, but does not wait for task completion.
/// Stop sandbox payloads and bridge processes before releasing this handle, because their
/// installed network rules may otherwise outlive the listener's port ownership.
pub struct BrokerHandle {
    endpoint: SocketAddr,
    ipv6_endpoint: SocketAddr,
    unix_socket: Option<PathBuf>,
    shutdown: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    // Keep Seatbelt's permitted ports owned even after the active-time supervisor stops.
    _ipv4_lease: std::net::TcpListener,
    _ipv6_lease: std::net::TcpListener,
}
impl BrokerHandle {
    pub fn tcp_endpoint(&self) -> SocketAddr {
        self.endpoint
    }
    pub fn ipv6_endpoint(&self) -> SocketAddr {
        self.ipv6_endpoint
    }
    pub fn unix_socket(&self) -> Option<&Path> {
        self.unix_socket.as_deref()
    }
    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }
    pub async fn shutdown(mut self) -> Result<(), NetworkError> {
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.take() {
            task.await
                .map_err(|_| NetworkError::new(NetworkErrorKind::Shutdown))?;
        }
        Ok(())
    }
}
impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.shutdown.send_replace(true);
    }
}

impl NetworkBroker {
    /// Bind both loopback families at one port and an optional private Unix socket, then serve
    /// bounded connections until cancellation or active-time expiry. Either bind failing aborts
    /// startup. No outbound connection is made until a parsed request passes target checks.
    pub async fn start(
        policy: BrokerPolicy,
        cancel: Arc<dyn Cancellation>,
    ) -> Result<BrokerHandle, NetworkError> {
        Self::start_with_resolver(policy, cancel, Arc::new(SystemResolver)).await
    }

    /// A custom resolver is host-owned infrastructure, never a model-selected bypass.
    pub async fn start_with_resolver(
        policy: BrokerPolicy,
        cancel: Arc<dyn Cancellation>,
        resolver: Arc<dyn NetworkResolver>,
    ) -> Result<BrokerHandle, NetworkError> {
        policy.validate()?;
        if cancel.is_cancelled() {
            return Err(NetworkError::new(NetworkErrorKind::Shutdown));
        }
        let (listener, ipv6) = bind_loopback_pair(0)?;
        let (listener, ipv4_lease) = retain_listener(listener)?;
        let (ipv6, ipv6_lease) = retain_listener(ipv6)?;
        let endpoint = listener.local_addr()?;
        let ipv6_endpoint = ipv6.local_addr()?;
        #[cfg(unix)]
        let unix = UnixEndpoint::bind(policy.unix_socket.as_deref())?;
        #[cfg(not(unix))]
        if policy.unix_socket.is_some() {
            return Err(NetworkError::new(NetworkErrorKind::InvalidPolicy));
        }
        let unix_socket = policy.unix_socket.clone();
        let (shutdown, receiver) = watch::channel(false);
        let state = Arc::new(BrokerState {
            policy,
            resolver,
            total_bytes: AtomicU64::new(0),
        });
        let task = tokio::spawn(async move {
            supervise(
                listener,
                ipv6,
                #[cfg(unix)]
                unix,
                state,
                cancel,
                receiver,
            )
            .await;
        });
        Ok(BrokerHandle {
            endpoint,
            ipv6_endpoint,
            unix_socket,
            shutdown,
            task: Some(task),
            _ipv4_lease: ipv4_lease,
            _ipv6_lease: ipv6_lease,
        })
    }
}

fn retain_listener(
    listener: TcpListener,
) -> Result<(TcpListener, std::net::TcpListener), NetworkError> {
    let listener = listener.into_std()?;
    let lease = listener.try_clone()?;
    Ok((TcpListener::from_std(listener)?, lease))
}

// Seatbelt's localhost filter covers both families. Own the same port on both, or fail closed.
fn bind_loopback_pair(port: u16) -> Result<(TcpListener, TcpListener), NetworkError> {
    use std::os::fd::AsRawFd;
    let v4 = tokio::net::TcpSocket::new_v4()?;
    v4.bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port))?;
    let port = v4.local_addr()?.port();
    let v6 = tokio::net::TcpSocket::new_v6()?;
    let enabled: libc::c_int = 1;
    // This option is applied before bind and ensures IPv4-mapped traffic cannot use the v6 socket.
    let result = unsafe {
        libc::setsockopt(
            v6.as_raw_fd(),
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            (&enabled as *const libc::c_int).cast(),
            std::mem::size_of_val(&enabled) as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(NetworkError::new(NetworkErrorKind::Io));
    }
    v6.bind(SocketAddr::new(std::net::Ipv6Addr::LOCALHOST.into(), port))?;
    Ok((v4.listen(128)?, v6.listen(128)?))
}

struct BrokerState {
    policy: BrokerPolicy,
    resolver: Arc<dyn NetworkResolver>,
    total_bytes: AtomicU64,
}
impl BrokerState {
    fn charge(&self, bytes: usize) -> Result<(), NetworkError> {
        let count = bytes as u64;
        self.total_bytes
            .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(count)
                    .filter(|total| *total <= self.policy.limits.max_total_bytes)
            })
            .map(|_| ())
            .map_err(|_| NetworkError::new(NetworkErrorKind::LimitExceeded))
    }

    async fn connect(&self, origin: &NormalizedOrigin) -> Result<TcpStream, NetworkError> {
        self.policy.check_origin(origin)?;
        let operation = async {
            let addresses = if let Ok(ip) = origin.host.parse::<IpAddr>() {
                vec![ip]
            } else {
                self.resolver.resolve(&origin.host, origin.port).await?
            };
            self.policy.check_addresses(origin, &addresses)?;
            // Connect directly to the checked IP, never resolve the hostname again in connect().
            for address in addresses {
                if let Ok(stream) = TcpStream::connect(SocketAddr::new(address, origin.port)).await
                {
                    return Ok(stream);
                }
            }
            Err(NetworkError::new(NetworkErrorKind::Io))
        };
        timeout(
            Duration::from_millis(self.policy.limits.connect_timeout_ms),
            operation,
        )
        .await
        .map_err(|_| NetworkError::new(NetworkErrorKind::Timeout))?
    }
}

trait ClientStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> ClientStream for T {}
type BoxStream = Box<dyn ClientStream>;

async fn supervise(
    listener: TcpListener,
    ipv6: TcpListener,
    #[cfg(unix)] unix: Option<UnixEndpoint>,
    state: Arc<BrokerState>,
    cancel: Arc<dyn Cancellation>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut jobs = JoinSet::new();
    let lifetime = tokio::time::sleep(Duration::from_millis(state.policy.limits.active_timeout_ms));
    tokio::pin!(lifetime);
    let mut accepted = 0usize;
    loop {
        tokio::select! { biased;
            _ = cancel.cancelled() => break,
            _ = shutdown.changed() => break,
            _ = &mut lifetime => break,
            Some(_) = jobs.join_next(), if !jobs.is_empty() => {},
            result = listener.accept() => {
                let Ok((stream, _)) = result else { break; };
                spawn_client(Box::new(stream), &state, &mut jobs, &mut accepted);
            },
            result = ipv6.accept() => {
                let Ok((stream, _)) = result else { break; };
                spawn_client(Box::new(stream), &state, &mut jobs, &mut accepted);
            },
            result = accept_unix(#[cfg(unix)] unix.as_ref()) => {
                if let Ok(stream) = result { spawn_client(stream, &state, &mut jobs, &mut accepted); }
                else { break; }
            },
        }
    }
    // Abort drops both halves of every owned connection. Join before reporting shutdown complete.
    jobs.abort_all();
    while jobs.join_next().await.is_some() {}
    drop(listener);
    drop(ipv6);
    #[cfg(unix)]
    drop(unix);
}

fn spawn_client(
    stream: BoxStream,
    state: &Arc<BrokerState>,
    jobs: &mut JoinSet<()>,
    accepted: &mut usize,
) {
    if jobs.len() >= state.policy.limits.max_connections
        || *accepted >= state.policy.limits.max_total_connections
    {
        return;
    }
    *accepted += 1;
    let state = state.clone();
    jobs.spawn(async move {
        let _ = timeout(
            Duration::from_millis(state.policy.limits.connection_timeout_ms),
            http::serve(stream, state),
        )
        .await;
    });
}

async fn accept_unix(
    #[cfg(unix)] endpoint: Option<&UnixEndpoint>,
) -> Result<BoxStream, NetworkError> {
    #[cfg(unix)]
    if let Some(endpoint) = endpoint {
        return endpoint
            .listener
            .accept()
            .await
            .map(|(stream, _)| Box::new(stream) as BoxStream)
            .map_err(Into::into);
    }
    std::future::pending().await
}

#[cfg(unix)]
struct UnixEndpoint {
    listener: tokio::net::UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
}
#[cfg(unix)]
impl UnixEndpoint {
    fn bind(path: Option<&Path>) -> Result<Option<Self>, NetworkError> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let Some(path) = path else {
            return Ok(None);
        };
        let parent = path
            .parent()
            .ok_or_else(|| NetworkError::new(NetworkErrorKind::InvalidPolicy))?;
        let metadata = std::fs::symlink_metadata(parent)?;
        if !path.is_absolute()
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || std::fs::symlink_metadata(path).is_ok()
        {
            return Err(NetworkError::new(NetworkErrorKind::InvalidPolicy));
        }
        let listener = tokio::net::UnixListener::bind(path)?;
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            let _ = std::fs::remove_file(path);
            return Err(error.into());
        }
        let metadata = std::fs::symlink_metadata(path)?;
        Ok(Some(Self {
            listener,
            path: path.into(),
            device: metadata.dev(),
            inode: metadata.ino(),
        }))
    }
}
#[cfg(unix)]
impl Drop for UnixEndpoint {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|metadata| metadata.dev() == self.device && metadata.ino() == self.inode)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dual_family_binding_owns_same_port_and_rejects_port_collisions() {
        let (v4, v6) = bind_loopback_pair(0).unwrap();
        let port = v4.local_addr().unwrap().port();
        assert_eq!(v6.local_addr().unwrap().port(), port);
        assert!(bind_loopback_pair(port).is_err());
        let v4_connect = TcpStream::connect(v4.local_addr().unwrap()).await.unwrap();
        let v6_connect = TcpStream::connect(v6.local_addr().unwrap()).await.unwrap();
        drop((v4_connect, v6_connect, v4));
        // The occupied v6 half prevents a v4-only broker from ever being returned.
        assert!(bind_loopback_pair(port).is_err());
        drop(v6);
        assert!(bind_loopback_pair(port).is_ok());
    }
}
