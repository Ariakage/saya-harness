use super::worker::NamespaceBridge;
use std::{
    fs, io,
    net::SocketAddr,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::UnixListener as StdUnixListener,
    },
    path::{Path, PathBuf},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpStream, UnixListener},
    sync::watch,
    task::JoinSet,
};

const MAX_CONNECTIONS: usize = 32;
const IO_BUFFER_BYTES: usize = 16 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) struct LoopbackRelay {
    bridges: Vec<NamespaceBridge>,
    owned_sockets: Vec<(PathBuf, (u64, u64))>,
    stop: watch::Sender<bool>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl LoopbackRelay {
    pub(super) fn start(directory: &Path, endpoints: &[SocketAddr]) -> io::Result<Self> {
        if !(1..=2).contains(&endpoints.len())
            || endpoints
                .iter()
                .any(|endpoint| !endpoint.ip().is_loopback() || endpoint.port() == 0)
            || (endpoints.len() == 2 && endpoints[0] == endpoints[1])
        {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let (stop, cancel) = watch::channel(false);
        let mut relay = Self {
            bridges: Vec::new(),
            owned_sockets: Vec::new(),
            stop,
            worker: None,
        };
        let mut listeners = Vec::new();
        for (index, endpoint) in endpoints.iter().enumerate() {
            let socket = directory.join(format!("r{index}"));
            let listener = StdUnixListener::bind(&socket)?;
            let metadata = fs::symlink_metadata(&socket)?;
            if !metadata.file_type().is_socket() {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            relay
                .owned_sockets
                .push((socket.clone(), (metadata.dev(), metadata.ino())));
            relay.bridges.push(NamespaceBridge {
                socket: socket.clone(),
                endpoint: *endpoint,
            });
            fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
            listener.set_nonblocking(true)?;
            listeners.push(listener);
        }
        let endpoints = endpoints.to_vec();
        let (ready, started) = mpsc::sync_channel(1);
        relay.worker = Some(
            thread::Builder::new()
                .name("saya-loopback-relay".into())
                .spawn(move || serve(listeners, endpoints, cancel, ready))?,
        );
        started
            .recv_timeout(CONNECT_TIMEOUT)
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        Ok(relay)
    }

    pub(super) fn bridges(&self) -> &[NamespaceBridge] {
        &self.bridges
    }

    pub(super) fn shutdown(&mut self) -> io::Result<()> {
        self.stop.send_replace(true);
        let mut result = match self.worker.take() {
            Some(worker) => worker.join().unwrap_or_else(|_| {
                Err(io::Error::other(
                    "loopback relay thread could not finish normally",
                ))
            }),
            None => Ok(()),
        };
        self.owned_sockets.retain(|(socket, identity)| {
            let metadata = match fs::symlink_metadata(socket) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
                Err(error) => {
                    if result.is_ok() {
                        result = Err(error);
                    }
                    return true;
                }
            };
            if !metadata.file_type().is_socket() || (metadata.dev(), metadata.ino()) != *identity {
                return false;
            }
            match fs::remove_file(socket) {
                Ok(()) => false,
                Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                Err(error) => {
                    if result.is_ok() {
                        result = Err(error);
                    }
                    true
                }
            }
        });
        self.bridges.retain(|bridge| {
            self.owned_sockets
                .iter()
                .any(|(socket, _)| *socket == bridge.socket)
        });
        result
    }
}

impl Drop for LoopbackRelay {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn serve(
    listeners: Vec<StdUnixListener>,
    endpoints: Vec<SocketAddr>,
    mut cancel: watch::Receiver<bool>,
    ready: mpsc::SyncSender<io::Result<()>>,
) -> io::Result<()> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = ready.send(Err(io::Error::other(
                "loopback relay runtime is unavailable",
            )));
            return Err(error);
        }
    };
    runtime.block_on(async move {
        let listeners = match listeners
            .into_iter()
            .map(UnixListener::from_std)
            .collect::<io::Result<Vec<_>>>()
        {
            Ok(listeners) => listeners,
            Err(error) => {
                let _ = ready.send(Err(io::Error::other(
                    "loopback relay listeners are unavailable",
                )));
                return Err(error);
            }
        };
        ready
            .send(Ok(()))
            .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let mut connections = JoinSet::new();
        let mut failure = None;
        loop {
            if *cancel.borrow() {
                break;
            }
            let (index, incoming) = tokio::select! {
                biased;
                _ = cancel.changed() => break,
                _ = connections.join_next(), if !connections.is_empty() => continue,
                incoming = listeners[0].accept() => (0, incoming),
                incoming = async {
                    match listeners.get(1) {
                        Some(listener) => listener.accept().await,
                        None => std::future::pending().await,
                    }
                } => (1, incoming),
            };
            let mut peer = match incoming {
                Ok((peer, _)) => peer,
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            if connections.len() >= MAX_CONNECTIONS {
                continue;
            }
            let endpoint = endpoints[index];
            connections.spawn(async move {
                // HTTP is client-first; empty readiness probes must not reach the broker budget.
                let first_byte = if index == 1 {
                    let mut first = [0];
                    if peer.read(&mut first).await? == 0 {
                        return Ok(());
                    }
                    Some(first)
                } else {
                    None
                };
                let mut upstream =
                    tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(endpoint))
                        .await
                        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
                if let Some(first) = first_byte {
                    upstream.write_all(&first).await?;
                }
                tokio::io::copy_bidirectional_with_sizes(
                    &mut peer,
                    &mut upstream,
                    IO_BUFFER_BYTES,
                    IO_BUFFER_BYTES,
                )
                .await
                .map(|_| ())
            });
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, UnixStream},
    };

    #[tokio::test]
    async fn relay_forwards_with_a_long_private_directory() {
        let directory = tempfile::tempdir().expect("private socket fixture");
        let padding = 94usize
            .checked_sub(directory.path().as_os_str().len() + 1)
            .expect("fixture TMPDIR leaves room for the Unix socket boundary");
        assert!(padding > 0);
        let private = directory.path().join("s".repeat(padding));
        fs::create_dir(&private).unwrap();
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut relay = LoopbackRelay::start(&private, &[upstream.local_addr().unwrap()])
            .expect("private directory must leave room for the socket name");
        let mut peer = UnixStream::connect(&relay.bridges()[0].socket)
            .await
            .unwrap();
        let (mut source, _) = tokio::time::timeout(Duration::from_secs(2), upstream.accept())
            .await
            .unwrap()
            .unwrap();
        source.write_all(b"LONG_PATH_OK").await.unwrap();
        let mut received = [0; 12];
        tokio::time::timeout(Duration::from_secs(2), peer.read_exact(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&received, b"LONG_PATH_OK");
        relay.shutdown().unwrap();
    }

    #[tokio::test]
    async fn fixed_loopback_relays_forward_both_private_channel_destinations() {
        let directory = tempfile::tempdir().expect("short private socket directory");
        let ssh = TcpListener::bind("127.0.0.1:0").await.expect("SSH fixture");
        let broker = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("broker fixture");
        let endpoints = [ssh.local_addr().unwrap(), broker.local_addr().unwrap()];
        let mut relay = LoopbackRelay::start(directory.path(), &endpoints).expect("fixed relays");
        for (index, (listener, reply)) in [(&ssh, "SSH_OK"), (&broker, "BROKER_OK")]
            .into_iter()
            .enumerate()
        {
            let bridge = &relay.bridges()[index];
            assert_eq!(bridge.endpoint, endpoints[index]);
            let mut peer = UnixStream::connect(&bridge.socket).await.unwrap();
            peer.write_all(b"N5_PROBE").await.unwrap();
            let (mut upstream, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .expect("upstream accept deadline")
                .unwrap();
            let mut request = [0; 8];
            upstream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"N5_PROBE");
            upstream.write_all(reply.as_bytes()).await.unwrap();
            let mut received = vec![0; reply.len()];
            tokio::time::timeout(Duration::from_secs(2), peer.read_exact(&mut received))
                .await
                .expect("relay response deadline")
                .unwrap();
            assert_eq!(received, reply.as_bytes());
        }
        let sockets = relay
            .bridges()
            .iter()
            .map(|bridge| bridge.socket.clone())
            .collect::<Vec<_>>();
        relay.shutdown().expect("relay shutdown");
        relay.shutdown().expect("idempotent relay shutdown");
        assert!(sockets.iter().all(|path| !path.exists()));
    }

    #[tokio::test]
    async fn relay_startup_failure_preserves_existing_files_and_removes_owned_sockets() {
        let directory = tempfile::tempdir().expect("short private socket directory");
        let ssh = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let broker = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoints = [ssh.local_addr().unwrap(), broker.local_addr().unwrap()];
        LoopbackRelay::start(directory.path(), &endpoints)
            .expect("working startup positive control")
            .shutdown()
            .unwrap();
        let reserved = directory.path().join("r1");
        std::fs::write(&reserved, b"RESERVED").unwrap();
        assert!(LoopbackRelay::start(directory.path(), &endpoints).is_err());
        assert!(!directory.path().join("r0").exists());
        assert_eq!(std::fs::read(&reserved).unwrap(), b"RESERVED");
    }

    #[tokio::test]
    async fn repeated_cleanup_preserves_paths_reused_after_shutdown() {
        let directory = tempfile::tempdir().unwrap();
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut relay =
            LoopbackRelay::start(directory.path(), &[upstream.local_addr().unwrap()]).unwrap();
        let socket = relay.bridges()[0].socket.clone();
        relay.shutdown().unwrap();
        std::fs::write(&socket, b"REUSED").unwrap();
        relay.shutdown().unwrap();
        drop(relay);
        assert_eq!(std::fs::read(&socket).unwrap(), b"REUSED");
    }

    #[tokio::test]
    async fn cleanup_preserves_socket_paths_replaced_while_the_relay_was_active() {
        let directory = tempfile::tempdir().unwrap();
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut relay =
            LoopbackRelay::start(directory.path(), &[upstream.local_addr().unwrap()]).unwrap();
        let socket = relay.bridges()[0].socket.clone();
        std::fs::rename(&socket, directory.path().join("previous.sock")).unwrap();
        std::fs::write(&socket, b"REPLACEMENT").unwrap();
        relay.shutdown().unwrap();
        drop(relay);
        assert_eq!(std::fs::read(&socket).unwrap(), b"REPLACEMENT");
    }

    #[tokio::test]
    async fn empty_broker_readiness_does_not_spend_the_runs_only_connection() {
        use crate::{
            boundary::Cancellation,
            browser::network::{BrowserNetwork, BrowserNetworkSettings},
            network::BrokerLimits,
        };
        use std::sync::Arc;

        struct NeverCancel;
        impl Cancellation for NeverCancel {
            fn is_cancelled(&self) -> bool {
                false
            }

            fn cancelled(
                &self,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
                Box::pin(std::future::pending())
            }
        }

        let settings = BrowserNetworkSettings {
            enabled: true,
            public_web: true,
            limits: BrokerLimits {
                max_total_connections: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let network = BrowserNetwork::start(
            &settings,
            "relay-readiness",
            "vm-starting",
            Arc::new(NeverCancel),
        )
        .await
        .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let ssh = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoints = [
            ssh.local_addr().unwrap(),
            format!("127.0.0.1:{}", network.endpoint_port())
                .parse()
                .unwrap(),
        ];
        let mut relay = LoopbackRelay::start(directory.path(), &endpoints).unwrap();
        let mut ssh_peer = UnixStream::connect(&relay.bridges()[0].socket)
            .await
            .unwrap();
        let (mut server, _) = tokio::time::timeout(Duration::from_secs(2), ssh.accept())
            .await
            .unwrap()
            .unwrap();
        server.write_all(b"SSH_BANNER").await.unwrap();
        let mut banner = [0; 10];
        tokio::time::timeout(Duration::from_secs(2), ssh_peer.read_exact(&mut banner))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&banner, b"SSH_BANNER");

        let mut readiness = UnixStream::connect(&relay.bridges()[1].socket)
            .await
            .unwrap();
        readiness.shutdown().await.unwrap();
        let mut discarded = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(2),
            readiness.read_to_end(&mut discarded),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(network.observations().is_empty());

        let mut request = UnixStream::connect(&relay.bridges()[1].socket)
            .await
            .unwrap();
        request
            .write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), request.read_to_end(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&response).contains("403"));
        let observations = network.observations();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].outcome, "denied");
        assert_eq!(
            observations[0].origin.as_deref(),
            Some("https://127.0.0.1:9")
        );
        relay.shutdown().unwrap();
        network.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn relay_shutdown_closes_active_streams_before_returning() {
        let directory = tempfile::tempdir().expect("short private socket directory");
        let upstream = TcpListener::bind("[::1]:0").await.expect("IPv6 fixture");
        let endpoint = upstream.local_addr().unwrap();
        let mut relay = LoopbackRelay::start(directory.path(), &[endpoint]).expect("IPv6 relay");
        let socket = relay.bridges()[0].socket.clone();
        let mut peer = UnixStream::connect(&socket).await.unwrap();
        peer.write_all(b"ACTIVE").await.unwrap();
        let (mut connection, _) = tokio::time::timeout(Duration::from_secs(2), upstream.accept())
            .await
            .expect("upstream accept deadline")
            .unwrap();
        let mut probe = [0; 6];
        connection.read_exact(&mut probe).await.unwrap();
        assert_eq!(&probe, b"ACTIVE");
        relay.shutdown().expect("shutdown with live connection");
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), peer.read(&mut byte))
                .await
                .expect("private peer closes")
                .unwrap(),
            0
        );
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), connection.read(&mut byte))
                .await
                .expect("upstream closes")
                .unwrap(),
            0
        );
        assert!(!socket.exists());
    }
}
