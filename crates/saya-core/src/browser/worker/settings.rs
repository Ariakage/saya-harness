use super::*;

/// Strict, default-disabled worker configuration. Enabling it never installs a
/// runtime; every path is an explicit host input and must be absolute.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowserWorkerSettings {
    pub enabled: bool,
    pub node: Option<PathBuf>,
    pub worker_root: Option<PathBuf>,
    pub chromium: Option<PathBuf>,
    pub identity_manifest: Option<PathBuf>,
    pub identity_sha256: Option<String>,
    /// Optional upstream proxy passed to Chromium's launch options and argv.
    pub proxy: Option<String>,
    /// Parent for the private config/output/profile/work directories.
    pub temp_root: Option<PathBuf>,
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub stderr_limit_bytes: usize,
}

impl Default for BrowserWorkerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            node: None,
            worker_root: None,
            chromium: None,
            identity_manifest: None,
            identity_sha256: None,
            proxy: None,
            temp_root: None,
            connect_timeout_ms: 20_000,
            request_timeout_ms: 120_000,
            stderr_limit_bytes: 64 * 1024,
        }
    }
}

impl BrowserWorkerSettings {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        for path in [
            &self.node,
            &self.worker_root,
            &self.chromium,
            &self.identity_manifest,
            &self.temp_root,
        ]
        .into_iter()
        .flatten()
        {
            if !path.is_absolute()
                || path.as_os_str().is_empty()
                || path.as_os_str().as_bytes().contains(&0)
                || path.to_str().is_none()
            {
                return Err(invalid_config(
                    "Browser worker paths must be absolute UTF-8 paths",
                ));
            }
        }
        if let Some(digest) = &self.identity_sha256
            && !valid_sha256(digest)
        {
            return Err(invalid_config(
                "Browser worker identity digest must be lowercase SHA-256",
            ));
        }
        if !(1_000..=120_000).contains(&self.connect_timeout_ms)
            || !(1_000..=600_000).contains(&self.request_timeout_ms)
            || !(4 * 1024..=1024 * 1024).contains(&self.stderr_limit_bytes)
        {
            return Err(invalid_config("Browser worker limits are invalid"));
        }
        if let Some(proxy) = &self.proxy {
            if proxy.is_empty()
                || proxy.len() > 2048
                || proxy.chars().any(char::is_control)
                || reqwest::Url::parse(proxy).is_err()
            {
                return Err(invalid_config("Browser worker proxy is invalid"));
            }
            let url = reqwest::Url::parse(proxy)
                .map_err(|_| invalid_config("Browser worker proxy is invalid"))?;
            let loopback = url
                .host_str()
                .map(|host| host.trim_start_matches('[').trim_end_matches(']'))
                .and_then(|host| host.parse::<std::net::IpAddr>().ok())
                .is_some_and(|address| address.is_loopback());
            if url.scheme() != "http"
                || !loopback
                || url.port().is_none_or(|port| port == 0)
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid_config(
                    "Browser worker proxy must be an explicit loopback HTTP endpoint without credentials",
                ));
            }
        }
        if self.enabled
            && (self.node.is_none()
                || self.worker_root.is_none()
                || self.chromium.is_none()
                || self.identity_manifest.is_none()
                || self.identity_sha256.is_none())
        {
            return Err(invalid_config(
                "Enabled browser worker requires node, worker root, Chromium, manifest and digest",
            ));
        }
        Ok(())
    }
}

/// Read-only installation roots needed by the pinned worker. The node
/// directory, the worker package tree and the Chromium package directory are
/// the minimum; nothing below the workspace, HOME, the Saya configuration tree
/// or credential directories is implied. Callers may pass these to
/// [`SandboxPolicy::browser`].
pub fn browser_install_roots(
    settings: &BrowserWorkerSettings,
) -> Result<Vec<PathBuf>, BoundaryError> {
    settings.validate()?;
    let node = settings
        .node
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker node path is missing"))?;
    let worker_root = settings
        .worker_root
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker root is missing"))?;
    let chromium = settings
        .chromium
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker Chromium path is missing"))?;
    let node_parent = node
        .parent()
        .ok_or_else(|| invalid_config("Browser worker node path has no parent"))?;
    let chromium_parent = chromium
        .parent()
        .ok_or_else(|| invalid_config("Browser worker Chromium path has no parent"))?;
    let mut roots = vec![
        node_parent.to_path_buf(),
        worker_root.clone(),
        chromium_parent.to_path_buf(),
    ];
    roots.sort();
    roots.dedup();
    Ok(roots)
}
