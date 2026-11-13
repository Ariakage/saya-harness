//! Strict browser network configuration and the host-side broker used by the
//! dedicated-VM browser backend.
//!
//! The VM's default egress is never used for page traffic. The host starts a
//! per-Run [`crate::network::NetworkBroker`], the long-lived VM SSH channel
//! carries one reverse streamlocal forward to it, and a `socat` listener inside
//! a private network namespace presents that socket as a loopback HTTP proxy
//! to Chromium. Every connection is therefore normalized, DNS/address checked
//! and byte/time budgeted by the same broker code used elsewhere.
//!
//! Observations retain only the normalized origin, outcome, byte count and
//! elapsed time. Request paths, queries, headers and credentials are never
//! recorded, and CONNECT tunnels are explicitly marked as opaque.

use crate::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo},
    network::{
        BrokerLimits, BrokerPolicy, NetworkBroker, NetworkErrorKind, NetworkObservation,
        NetworkObserver, NetworkOutcome, NormalizedOrigin, OriginScheme, RunNetworkBudget,
        SystemResolver,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

/// Loopback port used by `socat` inside the VM's private network namespace.
/// Each Run has its own namespace, so the port cannot collide with another Run
/// and Chromium can be pinned to a stable literal proxy address.
pub const VM_PROXY_PORT: u16 = 31_281;

/// Maximum number of retained observations per Run.
pub const MAX_NETWORK_OBSERVATIONS: usize = 256;

fn invalid_config(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "invalid_config".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

pub(crate) fn network_unavailable(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "browser_network_unavailable".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// Stable code for a Run network budget that is exhausted or whose fixed
/// deadline has expired. This is deliberately distinct from
/// `browser_network_unavailable`, which means the isolation prerequisites
/// themselves could not be established.
pub(crate) fn network_budget(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "browser_network_budget".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// Strict, default-disabled browser network policy. An empty allowlist with
/// `public_web = false` denies all traffic; `public_web = true` still runs the
/// complete per-connection address and DNS checks. Limits are the existing
/// [`BrokerLimits`] so the Run budget, byte and time ceilings stay shared.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserNetworkSettings {
    pub enabled: bool,
    pub allowed_origins: Vec<String>,
    /// Allow any normalized HTTP(S) origin subject to full public-address and
    /// mixed-DNS-answer checks. Defaults to `false`.
    pub public_web: bool,
    /// One exact loopback HTTP(S) origin that must also appear in
    /// `allowed_origins`. Only loopback DNS answers are accepted for it.
    pub localhost_development_origin: Option<String>,
    pub limits: BrokerLimits,
}

impl BrowserNetworkSettings {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        // An empty allowlist is only meaningful while the policy is enabled;
        // the default-disabled configuration must stay valid. Every other
        // field is still validated so a bad value never hides behind
        // `enabled = false`.
        if self.enabled && !self.public_web && self.allowed_origins.is_empty() {
            return Err(invalid_config(
                "Browser network requires at least one allowed origin or public_web",
            ));
        }
        if self.allowed_origins.len() > 256 {
            return Err(invalid_config(
                "Browser network has too many allowed origins",
            ));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.allowed_origins {
            let origin = NormalizedOrigin::parse(entry)
                .map_err(|_| invalid_config("Browser network origin is invalid"))?;
            if !seen.insert(origin.clone()) {
                return Err(invalid_config("Browser network origins must be unique"));
            }
            // The loopback endpoint used as the proxy is not a page target.
            if origin.host == "127.0.0.1" && origin.port == VM_PROXY_PORT {
                return Err(invalid_config(
                    "Browser network origin collides with the in-VM proxy",
                ));
            }
        }
        let development = match &self.localhost_development_origin {
            Some(dev) => {
                let origin = NormalizedOrigin::parse(dev)
                    .map_err(|_| invalid_config("Browser development origin is invalid"))?;
                if !is_loopback_origin(&origin) || !seen.contains(&origin) {
                    return Err(invalid_config(
                        "Browser development origin must be loopback and listed",
                    ));
                }
                Some(origin)
            }
            None => None,
        };
        // A loopback entry that is not the single development exception would
        // always be denied by the address check; reject it instead of leaving
        // a silently unusable allowlist.
        for origin in &seen {
            if is_loopback_origin(origin) && development.as_ref() != Some(origin) {
                return Err(invalid_config(
                    "Loopback browser origins must be the development origin",
                ));
            }
        }
        self.to_policy().and_then(|policy| {
            policy
                .validate()
                .map_err(|_| invalid_config("Browser network limits are invalid"))
        })
    }

    /// Convert to the shared broker policy. `public_web` and the development
    /// origin are carried through unchanged; exact-origin behavior remains the
    /// default for every other broker user.
    pub fn to_policy(&self) -> Result<BrokerPolicy, BoundaryError> {
        Ok(BrokerPolicy {
            allowed_origins: self.allowed_origins.clone(),
            public_web: self.public_web,
            localhost_development_origin: self.localhost_development_origin.clone(),
            unix_socket: None,
            limits: self.limits.clone(),
        })
    }

    /// Host-side pre-check used before a navigation is dispatched. The broker
    /// still enforces the same rule on every connection and redirect hop.
    pub fn allows_origin(&self, origin: &NormalizedOrigin) -> bool {
        let local = self
            .localhost_development_origin
            .as_deref()
            .and_then(|value| NormalizedOrigin::parse(value).ok())
            .is_some_and(|allowed| allowed == *origin);
        self.public_web
            || local
            || self
                .allowed_origins
                .iter()
                .filter_map(|entry| NormalizedOrigin::parse(entry).ok())
                .any(|allowed| allowed == *origin)
    }
}

fn is_loopback_origin(origin: &NormalizedOrigin) -> bool {
    origin.host == "localhost"
        || origin
            .host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Bounded, host-owned observation log. Each entry is associated with the Run
/// that owns the provider and the VM worker that produced the traffic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNetworkObservation {
    pub run_id: String,
    pub worker_id: String,
    /// Normalized `scheme://host:port`; never a path, query or credential.
    /// `None` means the request was malformed or oversized before an origin
    /// could be identified, and no origin is fabricated.
    pub origin: Option<String>,
    pub outcome: String,
    /// True for CONNECT tunnels, where paths and queries are not visible.
    pub tunneled: bool,
    pub bytes: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug)]
pub struct BrowserNetworkLog {
    run_id: String,
    worker_id: Mutex<String>,
    entries: Mutex<VecDeque<BrowserNetworkObservation>>,
    limit: usize,
}

impl BrowserNetworkLog {
    pub fn new(run_id: &str, worker_id: &str, limit: usize) -> Self {
        Self {
            run_id: run_id.to_owned(),
            worker_id: Mutex::new(worker_id.to_owned()),
            entries: Mutex::new(VecDeque::new()),
            limit: limit.max(1),
        }
    }

    pub fn set_worker_pid(&self, pid: u32) {
        if let Ok(mut worker) = self.worker_id.lock() {
            *worker = format!("vm-{pid}");
        }
    }

    pub fn observations(&self) -> Vec<BrowserNetworkObservation> {
        self.entries
            .lock()
            .map(|entries| entries.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl NetworkObserver for BrowserNetworkLog {
    fn observe(&self, observation: NetworkObservation) {
        let worker_id = self
            .worker_id
            .lock()
            .map(|worker| worker.clone())
            .unwrap_or_else(|_| "unknown".into());
        let entry = BrowserNetworkObservation {
            run_id: self.run_id.clone(),
            worker_id,
            origin: observation.origin.as_ref().map(|origin| {
                format!(
                    "{}://{}",
                    match origin.scheme {
                        OriginScheme::Http => "http",
                        OriginScheme::Https => "https",
                    },
                    origin.authority()
                )
            }),
            outcome: outcome_name(observation.outcome).into(),
            tunneled: observation.tunneled,
            bytes: observation.bytes,
            elapsed_ms: observation.elapsed_ms,
        };
        if let Ok(mut entries) = self.entries.lock() {
            if entries.len() == self.limit {
                entries.pop_front();
            }
            entries.push_back(entry);
        }
    }
}

fn outcome_name(outcome: NetworkOutcome) -> &'static str {
    match outcome {
        NetworkOutcome::Allowed => "allowed",
        NetworkOutcome::Denied => "denied",
        NetworkOutcome::Upstream => "upstream_error",
        NetworkOutcome::Timeout => "timeout",
        NetworkOutcome::Limit => "limit",
        NetworkOutcome::Malformed => "malformed",
        NetworkOutcome::Cancelled => "cancelled",
    }
}

/// One per-worker-lifecycle broker plus its observation log. The listener is
/// owned until explicit shutdown; dropping the value only signals the broker
/// task, so callers should await [`Self::shutdown`] where a confirmed stop is
/// required.
pub struct BrowserNetwork {
    handle: Mutex<Option<crate::network::BrokerHandle>>,
    log: Arc<BrowserNetworkLog>,
    proxy_url: String,
    port: u16,
}

impl BrowserNetwork {
    /// Start a broker bound to dual-stack loopback on an ephemeral port. The
    /// endpoint is private to this Run and reached from the VM through the
    /// SSH reverse streamlocal forward created by the worker launcher.
    pub async fn start(
        settings: &BrowserNetworkSettings,
        run_id: &str,
        worker_id: &str,
        cancel: Arc<dyn Cancellation>,
    ) -> Result<Self, BoundaryError> {
        let log = Arc::new(BrowserNetworkLog::new(
            run_id,
            worker_id,
            MAX_NETWORK_OBSERVATIONS,
        ));
        Self::start_with_log(settings, log, cancel).await
    }

    /// Start using an existing log so a provider can expose observations even
    /// though each worker lifecycle owns a fresh broker listener.
    pub async fn start_with_log(
        settings: &BrowserNetworkSettings,
        log: Arc<BrowserNetworkLog>,
        cancel: Arc<dyn Cancellation>,
    ) -> Result<Self, BoundaryError> {
        settings.validate()?;
        let budget = RunNetworkBudget::new(&settings.limits)
            .map_err(|_| invalid_config("Browser network limits are invalid"))?;
        Self::start_with_log_and_budget(settings, log, budget, cancel).await
    }

    /// Start a listener while reusing a Run-scoped budget. The same cumulative
    /// bytes, connection attempts and fixed deadline therefore span every
    /// worker lifecycle and broker restart in this Run; a restart never resets
    /// a ceiling that was already exhausted.
    pub async fn start_with_log_and_budget(
        settings: &BrowserNetworkSettings,
        log: Arc<BrowserNetworkLog>,
        budget: Arc<RunNetworkBudget>,
        cancel: Arc<dyn Cancellation>,
    ) -> Result<Self, BoundaryError> {
        settings.validate()?;
        if !settings.enabled {
            return Err(invalid_config("Browser network is disabled"));
        }
        if cancel.is_cancelled() {
            return Err(BoundaryError(
                ErrorInfo {
                    code: "cancelled".into(),
                    message: "Browser network start cancelled".into(),
                },
                None,
                None,
            ));
        }
        let policy = settings.to_policy()?;
        let observer: Arc<dyn NetworkObserver> = log.clone();
        let handle = NetworkBroker::start_scoped_observed(
            policy,
            cancel,
            budget,
            Arc::new(SystemResolver),
            Some(observer),
        )
        .await
        .map_err(|error| match error.kind {
            NetworkErrorKind::Shutdown => BoundaryError(
                ErrorInfo {
                    code: "cancelled".into(),
                    message: "Browser network start cancelled".into(),
                },
                None,
                None,
            ),
            // The shared Run budget or its fixed deadline rejected the
            // listener before any isolation prerequisite was involved.
            NetworkErrorKind::LimitExceeded => network_budget(
                "Browser Run network budget is exhausted or its deadline has expired",
            ),
            _ => network_unavailable("Browser network broker could not start"),
        })?;
        let port = handle.tcp_endpoint().port();
        Ok(Self {
            handle: Mutex::new(Some(handle)),
            log,
            proxy_url: format!("http://127.0.0.1:{port}"),
            port,
        })
    }

    pub fn proxy_url(&self) -> &str {
        &self.proxy_url
    }

    /// Host TCP port that the SSH reverse forward targets. It is deliberately
    /// distinct from [`VM_PROXY_PORT`], the loopback port inside the VM.
    pub fn endpoint_port(&self) -> u16 {
        self.port
    }

    pub fn set_worker_pid(&self, pid: u32) {
        self.log.set_worker_pid(pid);
    }

    pub fn observations(&self) -> Vec<BrowserNetworkObservation> {
        self.log.observations()
    }

    pub fn observation_log(&self) -> Arc<BrowserNetworkLog> {
        self.log.clone()
    }

    /// Stop the listener and wait for every accepted connection task to end.
    pub async fn shutdown(&self) -> Result<(), BoundaryError> {
        let handle = self
            .handle
            .lock()
            .map_err(|_| network_unavailable("Browser network state is unavailable"))?
            .take();
        let Some(handle) = handle else {
            return Ok(());
        };
        handle
            .shutdown()
            .await
            .map_err(|_| network_unavailable("Browser network broker shutdown failed"))
    }

    /// Bounded shutdown used from cleanup paths where a nested error must not
    /// replace the primary failure.
    pub async fn shutdown_best_effort(&self) {
        let handle = self.handle.lock().ok().and_then(|mut slot| slot.take());
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(Duration::from_secs(5), handle.shutdown()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::NetworkObservation;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct NoCancel;
    impl Cancellation for NoCancel {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }

    struct SwitchCancel(Arc<AtomicBool>);
    impl Cancellation for SwitchCancel {
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            let flag = self.0.clone();
            Box::pin(async move {
                while !flag.load(Ordering::Acquire) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
        }
    }

    #[test]
    fn disabled_defaults_stay_valid_but_an_enabled_empty_allowlist_is_refused() {
        assert!(BrowserNetworkSettings::default().validate().is_ok());
        let enabled = BrowserNetworkSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(enabled.validate().is_err());
        let public = BrowserNetworkSettings {
            enabled: true,
            public_web: true,
            ..Default::default()
        };
        assert!(public.validate().is_ok());
        let proxy_collision = BrowserNetworkSettings {
            enabled: true,
            allowed_origins: vec![format!("http://127.0.0.1:{VM_PROXY_PORT}")],
            ..Default::default()
        };
        assert!(proxy_collision.validate().is_err());
        let duplicate = BrowserNetworkSettings {
            enabled: true,
            allowed_origins: vec![
                "https://example.com".into(),
                "https://EXAMPLE.com:443/".into(),
            ],
            ..Default::default()
        };
        assert!(duplicate.validate().is_err());
        let loopback_without_exception = BrowserNetworkSettings {
            enabled: true,
            allowed_origins: vec!["http://127.0.0.1:8080".into()],
            ..Default::default()
        };
        assert!(loopback_without_exception.validate().is_err());
        let bad_dev = BrowserNetworkSettings {
            enabled: true,
            allowed_origins: vec!["https://example.com".into()],
            localhost_development_origin: Some("https://example.com".into()),
            ..Default::default()
        };
        assert!(bad_dev.validate().is_err());
        let good_dev = BrowserNetworkSettings {
            enabled: true,
            allowed_origins: vec!["http://127.0.0.1:8080".into()],
            localhost_development_origin: Some("http://127.0.0.1:8080".into()),
            ..Default::default()
        };
        assert!(good_dev.validate().is_ok());
    }

    #[test]
    fn observations_are_bounded_and_never_include_paths_or_credentials() {
        let log = BrowserNetworkLog::new("run-7", "vm-starting", 2);
        log.set_worker_pid(1234);
        for _ in 0..3 {
            log.observe(NetworkObservation {
                origin: Some(NormalizedOrigin::parse("https://example.com").unwrap()),
                outcome: NetworkOutcome::Allowed,
                tunneled: false,
                bytes: 12,
                elapsed_ms: 7,
            });
        }
        let observed = log.observations();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[1].run_id, "run-7");
        assert_eq!(observed[1].worker_id, "vm-1234");
        assert_eq!(observed[1].origin.as_deref(), Some("https://example.com"));
        assert_eq!(observed[1].outcome, "allowed");
    }

    #[tokio::test]
    async fn broker_checks_the_origin_and_records_path_free_observations() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin_port = listener.local_addr().unwrap().port();
        let origin_url = format!("http://127.0.0.1:{origin_port}");
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer).await;
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    )
                    .await;
            }
        });
        let settings = BrowserNetworkSettings {
            enabled: true,
            allowed_origins: vec![origin_url.clone()],
            localhost_development_origin: Some(origin_url.clone()),
            ..Default::default()
        };
        let network = BrowserNetwork::start(&settings, "run-1", "vm-starting", Arc::new(NoCancel))
            .await
            .unwrap();

        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", network.endpoint_port()))
            .await
            .unwrap();
        let request = format!(
            "GET {origin_url}/private/path?token=secret HTTP/1.1\r\nHost: 127.0.0.1:{origin_port}\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8_lossy(&response).contains("200 OK"));

        // A second origin is denied by the exact allowlist.
        let mut denied = tokio::net::TcpStream::connect(("127.0.0.1", network.endpoint_port()))
            .await
            .unwrap();
        denied
            .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\nHost: denied.example:443\r\n\r\n")
            .await
            .unwrap();
        let mut rejected = Vec::new();
        denied.read_to_end(&mut rejected).await.unwrap();
        assert!(String::from_utf8_lossy(&rejected).contains("403"));

        let mut observed = Vec::new();
        for _ in 0..100 {
            observed = network.observations();
            if observed.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(observed.len(), 2, "both attempts are observed");
        assert_eq!(observed[0].origin.as_deref(), Some(origin_url.as_str()));
        assert_eq!(observed[0].outcome, "allowed");
        assert_eq!(observed[0].run_id, "run-1");
        assert!(observed[0].bytes > 0);
        assert_eq!(observed[1].outcome, "denied");
        assert_eq!(
            observed[1].origin.as_deref(),
            Some("https://denied.example")
        );
        for entry in observed {
            let origin = entry.origin.as_deref().unwrap_or_default();
            assert!(!origin.contains("secret"));
            assert!(!origin.contains("private"));
        }
        network.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn malformed_and_oversize_requests_are_observed_without_a_fabricated_origin() {
        let settings = BrowserNetworkSettings {
            enabled: true,
            public_web: true,
            ..Default::default()
        };
        let network = BrowserNetwork::start(
            &settings,
            "run-malformed",
            "vm-starting",
            Arc::new(NoCancel),
        )
        .await
        .unwrap();
        let endpoint = ("127.0.0.1", network.endpoint_port());

        let mut malformed = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        malformed.write_all(b"GARBAGE\r\n\r\n").await.unwrap();
        let _ = malformed.read_to_end(&mut Vec::new()).await;

        let mut oversize = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        oversize.write_all(&[b'X'; 20 * 1024]).await.unwrap();
        let _ = oversize.read_to_end(&mut Vec::new()).await;

        let mut observed = Vec::new();
        for _ in 0..200 {
            observed = network.observations();
            if observed.len() >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(observed.len(), 2, "{observed:?}");
        assert_eq!(observed[0].outcome, "malformed");
        assert_eq!(observed[0].origin, None);
        assert_eq!(observed[1].outcome, "limit");
        assert_eq!(observed[1].origin, None);
        network.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn connect_timeout_reports_the_bytes_already_charged() {
        let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = upstream.accept().await {
                // The broker sends no HTTP head upstream for CONNECT; it only
                // splices bytes after the tunnel is established. Send the
                // charged bytes immediately and stay open so the outer
                // connection timeout aborts the task after they were billed.
                let _ = stream.write_all(b"charged-upstream-bytes").await;
                let _ = stream.read(&mut [0u8; 1]).await;
            }
        });
        let settings = BrowserNetworkSettings {
            enabled: true,
            allowed_origins: vec![format!("https://127.0.0.1:{upstream_port}")],
            localhost_development_origin: Some(format!("https://127.0.0.1:{upstream_port}")),
            limits: BrokerLimits {
                connection_timeout_ms: 500,
                idle_timeout_ms: 5_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let network =
            BrowserNetwork::start(&settings, "run-connect", "vm-starting", Arc::new(NoCancel))
                .await
                .unwrap();
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", network.endpoint_port()))
            .await
            .unwrap();
        client
            .write_all(
                format!(
                    "CONNECT 127.0.0.1:{upstream_port} HTTP/1.1\r\nHost: 127.0.0.1:{upstream_port}\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut observation = None;
        for _ in 0..300 {
            if let Some(entry) = network
                .observations()
                .into_iter()
                .find(|entry| entry.tunneled && entry.outcome == "timeout")
            {
                observation = Some(entry);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let observation = observation.expect("CONNECT timeout observation");
        assert_eq!(
            observation.bytes,
            b"charged-upstream-bytes".len() as u64,
            "{observation:?}"
        );
        assert_eq!(
            observation.origin.as_deref(),
            Some(format!("https://127.0.0.1:{upstream_port}").as_str())
        );
        let _ = client.shutdown().await;
        network.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn run_cancellation_is_recorded_as_cancelled_not_timeout() {
        let settings = BrowserNetworkSettings {
            enabled: true,
            public_web: true,
            ..Default::default()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let token = SwitchCancel(cancel.clone());
        let network =
            BrowserNetwork::start(&settings, "run-cancel", "vm-starting", Arc::new(token))
                .await
                .unwrap();
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", network.endpoint_port()))
            .await
            .unwrap();
        // A partial head proves the connection was accepted and is blocked
        // before a terminal outcome, unlike a connection that never arrived.
        client.write_all(b"GET http://").await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.store(true, Ordering::Release);
        let mut entry = None;
        for _ in 0..300 {
            entry = network
                .observations()
                .into_iter()
                .find(|entry| entry.outcome == "cancelled");
            if entry.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let entry = entry.expect("Run cancellation observation");
        assert_eq!(entry.origin, None);
        assert!(
            network
                .observations()
                .iter()
                .all(|entry| entry.outcome != "timeout"),
            "cancellation must not be reported as a real timeout"
        );
        network.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn listener_and_shared_budget_rejections_are_observed_without_an_origin() {
        let settings = BrowserNetworkSettings {
            enabled: true,
            public_web: true,
            limits: BrokerLimits {
                max_total_connections: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let network =
            BrowserNetwork::start(&settings, "run-limits", "vm-starting", Arc::new(NoCancel))
                .await
                .unwrap();
        let endpoint = ("127.0.0.1", network.endpoint_port());

        let mut first = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        first
            .write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        first.read_to_end(&mut response).await.unwrap();
        assert!(String::from_utf8_lossy(&response).contains("403"));

        // The single allowed attempt is spent. This connection is rejected
        // before any request byte can identify an origin.
        let _second = tokio::net::TcpStream::connect(endpoint).await.unwrap();
        let mut limit = None;
        for _ in 0..300 {
            limit = network
                .observations()
                .into_iter()
                .find(|entry| entry.outcome == "limit");
            if limit.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let limit = limit.expect("shared-budget rejection observation");
        assert_eq!(limit.origin, None);
        assert!(!limit.tunneled);
        assert_eq!(limit.bytes, 0);
        network.shutdown().await.unwrap();
    }
}
