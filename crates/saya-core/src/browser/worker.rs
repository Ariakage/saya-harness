//! Host-managed Playwright MCP worker.
//!
//! This module owns the process/transport side of the browser provider. The
//! worker runtime, Node executable, Chromium executable and identity manifest
//! are supplied explicitly by the host. Nothing here installs, downloads or
//! discovers a package manager. Named files and the complete Node, worker and
//! Chromium installation trees are verified before launch.
//!
//! Model-facing tools remain the fixed [`super::BrowserProvider`] surface. The
//! worker catalogue is only used to prove that the pinned tools and schemas are
//! present; its additional tools are never mapped or exposed.

mod installation;
pub(crate) use installation::InstallationTrees;

use super::{
    BrowserAction, BrowserElement, BrowserFacts, BrowserFailure, BrowserOpened, BrowserProvider,
    BrowserSnapshot,
};
use crate::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo},
    execution::{
        BackendKind, BrowserSandboxDirectories, BrowserSandboxProfile, PreparedEnvironment,
        SandboxPolicy,
    },
    mcp::{McpCatalog, McpLimits, McpToolResult},
    tools::exec::{ExecConfig, ManagedProcess},
};
use async_trait::async_trait;
use cap_std::fs::{Dir, OpenOptions as CapOpenOptions, OpenOptionsExt as CapOpenOptionsExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsString,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex};

/// Pinned worker versions. The identity manifest must agree with these values
/// and with the files it names.
pub const PLAYWRIGHT_MCP_VERSION: &str = "0.0.85";
pub const PLAYWRIGHT_VERSION: &str = "1.66.0-alpha-2026-11-09";
pub const CHROMIUM_REVISION: u32 = 1254;
pub const CHROMIUM_VERSION: &str = "158.0.8127.18";

/// The protocol version the pinned worker echoes during initialization.
pub const WORKER_PROTOCOL_VERSION: &str = "2025-11-25";

/// Worker tools that this provider may call. Discovery of other worker tools
/// never adds a model tool.
pub const MAPPED_WORKER_TOOLS: [&str; 9] = [
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_press_key",
    "browser_wait_for",
    "browser_take_screenshot",
    "browser_close",
    "browser_file_upload",
];

pub(crate) const CLI_JS_PATH: &str = "node_modules/@playwright/mcp/cli.js";
pub(crate) const MCP_PACKAGE_PATH: &str = "node_modules/@playwright/mcp/package.json";
pub(crate) const PLAYWRIGHT_PACKAGE_PATH: &str = "node_modules/playwright/package.json";
pub(crate) const PLAYWRIGHT_CORE_PACKAGE_PATH: &str = "node_modules/playwright-core/package.json";
pub(crate) const BROWSERS_JSON_PATH: &str = "node_modules/playwright-core/browsers.json";
pub(crate) const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_IDENTITY_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_WORKER_RESULT_BYTES: usize = 4 * 1024 * 1024;
const WORKER_SESSION: &str = "worker_session";
const WORKER_PAGE: &str = "worker_page";
/// Bounded re-discovery attempts for a lifecycle operation that races the
/// worker's own catalogue update. The request is never dispatched while stale.
const CATALOG_RESYNC_ATTEMPTS: usize = 3;
/// Provider-side bound for a single download read. BrowserState enforces the
/// configured lower limit before storing the artifact.
const PROVIDER_MAX_DOWNLOAD_BYTES: usize = 16 * 1024 * 1024;

fn error(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn identity_error(message: &'static str) -> BoundaryError {
    error("worker_identity", message)
}

fn invalid_config(message: &'static str) -> BoundaryError {
    error("invalid_config", message)
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IdentityManifest {
    pub(crate) schema_version: u32,
    pub(crate) installation_trees: InstallationTrees,
    pub(crate) node: VersionedIdentity,
    pub(crate) worker_cli_js: FileIdentity,
    pub(crate) mcp_package: VersionedIdentity,
    pub(crate) playwright_package: VersionedIdentity,
    pub(crate) playwright_core_package: VersionedIdentity,
    pub(crate) playwright_core_browsers_json: FileIdentity,
    pub(crate) chromium: ChromiumIdentity,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileIdentity {
    pub(crate) sha256: String,
    pub(crate) size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedIdentity {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChromiumIdentity {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) version: String,
    pub(crate) revision: u32,
}

/// Absolute host inputs whose named files and installation trees were verified.
/// This does not attest the source or build provenance of an engine.
#[derive(Debug, Clone)]
pub struct VerifiedWorkerIdentity {
    pub node: PathBuf,
    pub worker_root: PathBuf,
    pub cli_js: PathBuf,
    pub chromium: PathBuf,
    pub manifest_sha256: String,
    pub(crate) local_images: Option<Arc<VerifiedLocalImages>>,
}

impl VerifiedWorkerIdentity {
    fn check_local_executables(
        &self,
        checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
    ) -> Result<(), BoundaryError> {
        let images = self.local_images.as_ref().ok_or_else(|| {
            identity_error("Browser worker local executable identities are unavailable")
        })?;
        images.node.check_named(&self.node, checkpoint)?;
        images.chromium.check_named(&self.chromium, checkpoint)
    }
}

#[derive(Debug)]
pub(crate) struct VerifiedLocalImages {
    node: PinnedWorkerImage,
    chromium: PinnedWorkerImage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkerImageIdentity {
    dev: u64,
    ino: u64,
    size: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl WorkerImageIdentity {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            links: metadata.nlink(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        }
    }
}

#[derive(Debug)]
struct PinnedWorkerImage {
    file: File,
    identity: WorkerImageIdentity,
    sha256: String,
}

impl PinnedWorkerImage {
    fn check_named(
        &self,
        path: &Path,
        checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
    ) -> Result<(), BoundaryError> {
        checkpoint()?;
        let named = open_absolute_file(path)?;
        self.check_metadata(&named)?;
        if self.identity.size > MAX_IDENTITY_FILE_BYTES {
            return Err(identity_error(
                "Browser worker executable exceeds its size limit",
            ));
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            checkpoint()?;
            let length = (self.identity.size - total).min(buffer.len() as u64).max(1) as usize;
            let count = self
                .file
                .read_at(&mut buffer[..length], total)
                .map_err(|_| identity_error("Browser worker executable cannot be reread"))?;
            if count == 0 {
                if total != self.identity.size {
                    return Err(identity_error("Browser worker executable ended early"));
                }
                break;
            }
            total += count as u64;
            if total > self.identity.size {
                return Err(identity_error(
                    "Browser worker executable grew after verification",
                ));
            }
            hasher.update(&buffer[..count]);
        }
        let digest = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if digest != self.sha256 {
            return Err(identity_error(
                "Browser worker executable content changed after verification",
            ));
        }
        self.check_metadata(&named)?;
        checkpoint()?;
        Ok(())
    }

    fn check_metadata(&self, named: &File) -> Result<(), BoundaryError> {
        for file in [&self.file, named, &self.file] {
            let metadata = file.metadata().map_err(|_| {
                identity_error("Browser worker executable identity cannot be rechecked")
            })?;
            if WorkerImageIdentity::from_metadata(&metadata) != self.identity {
                return Err(identity_error(
                    "Browser worker executable changed after verification",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub(crate) struct PackageJson {
    pub(crate) name: String,
    pub(crate) version: String,
}

#[derive(Deserialize)]
pub(crate) struct BrowsersJson {
    pub(crate) browsers: Vec<BrowserRecord>,
}

#[derive(Deserialize)]
pub(crate) struct BrowserRecord {
    pub(crate) name: String,
    pub(crate) revision: String,
    #[serde(rename = "browserVersion")]
    pub(crate) browser_version: Option<String>,
}

struct ExpectedFile<'a> {
    sha256: &'a str,
    size: u64,
}

fn expected(file: &FileIdentity) -> ExpectedFile<'_> {
    ExpectedFile {
        sha256: &file.sha256,
        size: file.size,
    }
}

fn expected_versioned(file: &VersionedIdentity) -> ExpectedFile<'_> {
    ExpectedFile {
        sha256: &file.sha256,
        size: file.size,
    }
}

fn expected_chromium(file: &ChromiumIdentity) -> ExpectedFile<'_> {
    ExpectedFile {
        sha256: &file.sha256,
        size: file.size,
    }
}

fn open_directory(path: &Path) -> Result<File, BoundaryError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
        .open(path)
        .map_err(|_| identity_error("Browser worker directory cannot be opened safely"))
}

fn open_absolute_file(path: &Path) -> Result<File, BoundaryError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    options
        .open(path)
        .map_err(|_| identity_error("Browser worker file cannot be opened safely"))
}

/// Open a worker-relative file by walking every component with `O_NOFOLLOW`.
/// The root may not be the final component of a symlink either.
fn open_under_root(root: &Path, relative: &Path) -> Result<File, BoundaryError> {
    let mut directory = Dir::from_std_file(open_directory(root)?);
    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty() {
        return Err(identity_error("Browser worker identity path is empty"));
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(identity_error(
                "Browser worker identity path is not relative",
            ));
        };
        let mut options = CapOpenOptions::new();
        options.read(true);
        if index + 1 == components.len() {
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
            let file = directory
                .open_with(name, &options)
                .map_err(|_| identity_error("Browser worker file changed or is unsafe"))?;
            return Ok(file.into_std());
        }
        options.custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        );
        let child = directory
            .open_with(name, &options)
            .map_err(|_| identity_error("Browser worker directory changed or is unsafe"))?
            .into_std();
        directory = Dir::from_std_file(child);
    }
    Err(identity_error("Browser worker identity path is empty"))
}

fn file_identity(metadata: &fs::Metadata) -> (u64, u64, u64, i64, i64, u32) {
    (
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.mode(),
    )
}

fn check_file_metadata(
    metadata: &fs::Metadata,
    expected: &ExpectedFile<'_>,
    executable: bool,
) -> Result<(), BoundaryError> {
    if !metadata.is_file() {
        return Err(identity_error(
            "Browser worker identity is not a regular file",
        ));
    }
    if metadata.len() != expected.size || metadata.len() > MAX_IDENTITY_FILE_BYTES {
        return Err(identity_error(
            "Browser worker identity size does not match its manifest",
        ));
    }
    let mode = metadata.permissions().mode();
    if mode & 0o022 != 0 || mode & 0o6000 != 0 {
        return Err(identity_error(
            "Browser worker identity is writable by another account or privileged",
        ));
    }
    if mode & 0o400 == 0 {
        return Err(identity_error("Browser worker identity is not readable"));
    }
    if executable && mode & 0o111 == 0 {
        return Err(identity_error(
            "Browser worker executable does not have an execute bit",
        ));
    }
    Ok(())
}

fn read_identity_bytes(
    mut file: File,
    expected: &ExpectedFile<'_>,
    checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
) -> Result<Vec<u8>, BoundaryError> {
    let before = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be inspected"))?;
    check_file_metadata(&before, expected, false)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut buffer = [0; 64 * 1024];
    loop {
        checkpoint()?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| identity_error("Browser worker identity cannot be read"))?;
        if count == 0 {
            break;
        }
        if bytes.len() as u64 + count as u64 > expected.size {
            return Err(identity_error(
                "Browser worker identity changed while being read",
            ));
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    let after = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be rechecked"))?;
    if file_identity(&before) != file_identity(&after) || bytes.len() as u64 != before.len() {
        return Err(identity_error(
            "Browser worker identity changed while being read",
        ));
    }
    if sha256_hex(&bytes) != expected.sha256 {
        return Err(identity_error(
            "Browser worker identity digest does not match its manifest",
        ));
    }
    Ok(bytes)
}

fn verify_identity_digest(
    mut file: File,
    expected: &ExpectedFile<'_>,
    executable: bool,
    checkpoint: &mut dyn FnMut() -> Result<(), BoundaryError>,
) -> Result<PinnedWorkerImage, BoundaryError> {
    let before = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be inspected"))?;
    check_file_metadata(&before, expected, executable)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        checkpoint()?;
        let count = file
            .read(&mut buffer)
            .map_err(|_| identity_error("Browser worker identity cannot be read"))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > expected.size {
            return Err(identity_error(
                "Browser worker identity size does not match its manifest",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    let after = file
        .metadata()
        .map_err(|_| identity_error("Browser worker identity cannot be rechecked"))?;
    let identity = WorkerImageIdentity::from_metadata(&before);
    if identity != WorkerImageIdentity::from_metadata(&after) || total != expected.size {
        return Err(identity_error(
            "Browser worker identity changed while being read",
        ));
    }
    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if digest != expected.sha256 {
        return Err(identity_error(
            "Browser worker identity digest does not match its manifest",
        ));
    }
    Ok(PinnedWorkerImage {
        file,
        identity,
        sha256: digest,
    })
}

pub(crate) fn parse_package(
    bytes: &[u8],
    expected_name: &str,
) -> Result<PackageJson, BoundaryError> {
    let package: PackageJson = serde_json::from_slice(bytes)
        .map_err(|_| identity_error("Browser worker package metadata is invalid"))?;
    if package.name != expected_name || package.version.is_empty() {
        return Err(identity_error("Browser worker package identity is wrong"));
    }
    Ok(package)
}

pub(crate) fn parse_browsers(bytes: &[u8]) -> Result<BrowsersJson, BoundaryError> {
    serde_json::from_slice(bytes)
        .map_err(|_| identity_error("Browser worker browsers.json is invalid"))
}

fn manifest_expected_digest(settings: &BrowserWorkerSettings) -> &str {
    settings.identity_sha256.as_deref().unwrap_or_default()
}

/// Verify named files and closed installation trees against a v2 manifest.
/// Reads are bounded by the configured connection deadline; no process is launched.
pub fn verify_worker_identity(
    settings: &BrowserWorkerSettings,
) -> Result<VerifiedWorkerIdentity, BoundaryError> {
    verify_worker_identity_checked(settings, None)
}

fn verify_worker_identity_checked(
    settings: &BrowserWorkerSettings,
    cancel: Option<&dyn Cancellation>,
) -> Result<VerifiedWorkerIdentity, BoundaryError> {
    settings.validate()?;
    let deadline = Instant::now() + Duration::from_millis(settings.connect_timeout_ms);
    let mut checkpoint = || {
        if cancel.is_some_and(Cancellation::is_cancelled) {
            return Err(error(
                "cancelled",
                "Browser worker identity verification cancelled",
            ));
        }
        if Instant::now() >= deadline {
            return Err(identity_error(
                "Browser worker identity verification exceeded its deadline",
            ));
        }
        Ok(())
    };
    checkpoint()?;
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
    let manifest_path = settings
        .identity_manifest
        .as_ref()
        .ok_or_else(|| invalid_config("Browser worker identity manifest is missing"))?;

    let mut manifest_file = open_absolute_file(manifest_path)?;
    let before = manifest_file
        .metadata()
        .map_err(|_| identity_error("Browser worker manifest cannot be inspected"))?;
    if !before.is_file() || before.len() > MAX_MANIFEST_BYTES {
        return Err(identity_error(
            "Browser worker manifest is not a bounded regular file",
        ));
    }
    let mode = before.permissions().mode();
    if mode & 0o022 != 0 || mode & 0o6000 != 0 || mode & 0o400 == 0 {
        return Err(identity_error(
            "Browser worker manifest permissions are unsafe",
        ));
    }
    let mut manifest_bytes = Vec::with_capacity(before.len() as usize);
    manifest_file
        .read_to_end(&mut manifest_bytes)
        .map_err(|_| identity_error("Browser worker manifest cannot be read"))?;
    let after = manifest_file
        .metadata()
        .map_err(|_| identity_error("Browser worker manifest cannot be rechecked"))?;
    if file_identity(&before) != file_identity(&after)
        || manifest_bytes.len() as u64 != before.len()
    {
        return Err(identity_error(
            "Browser worker manifest changed while being read",
        ));
    }
    let manifest_digest = sha256_hex(&manifest_bytes);
    if manifest_digest != manifest_expected_digest(settings) {
        return Err(identity_error(
            "Browser worker manifest digest does not match the host input",
        ));
    }
    let manifest: IdentityManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| identity_error("Browser worker manifest is invalid"))?;
    if manifest.schema_version != 2 {
        return Err(identity_error(
            "Browser worker manifest schema version is unsupported",
        ));
    }
    manifest.installation_trees.validate()?;
    for digest in [
        &manifest.node.sha256,
        &manifest.worker_cli_js.sha256,
        &manifest.mcp_package.sha256,
        &manifest.playwright_package.sha256,
        &manifest.playwright_core_package.sha256,
        &manifest.playwright_core_browsers_json.sha256,
        &manifest.chromium.sha256,
    ] {
        if !valid_sha256(digest) {
            return Err(identity_error(
                "Browser worker manifest contains an invalid digest",
            ));
        }
    }

    let node_image = verify_identity_digest(
        open_absolute_file(node)?,
        &expected_versioned(&manifest.node),
        true,
        &mut checkpoint,
    )?;
    if manifest.node.version.is_empty() {
        return Err(identity_error("Browser worker node version is missing"));
    }

    let cli_bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(CLI_JS_PATH))?,
        &expected(&manifest.worker_cli_js),
        &mut checkpoint,
    )?;
    if cli_bytes.is_empty() {
        return Err(identity_error("Browser worker CLI is empty"));
    }

    let mcp_package_bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(MCP_PACKAGE_PATH))?,
        &expected_versioned(&manifest.mcp_package),
        &mut checkpoint,
    )?;
    let mcp_package = parse_package(&mcp_package_bytes, "@playwright/mcp")?;
    if mcp_package.version != PLAYWRIGHT_MCP_VERSION
        || manifest.mcp_package.version != PLAYWRIGHT_MCP_VERSION
    {
        return Err(identity_error(
            "Browser worker @playwright/mcp version does not match the pinned version",
        ));
    }

    let playwright_package_bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(PLAYWRIGHT_PACKAGE_PATH))?,
        &expected_versioned(&manifest.playwright_package),
        &mut checkpoint,
    )?;
    let playwright_package = parse_package(&playwright_package_bytes, "playwright")?;
    if playwright_package.version != PLAYWRIGHT_VERSION
        || manifest.playwright_package.version != PLAYWRIGHT_VERSION
    {
        return Err(identity_error(
            "Browser worker playwright version does not match the pinned version",
        ));
    }

    let playwright_core_bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(PLAYWRIGHT_CORE_PACKAGE_PATH))?,
        &expected_versioned(&manifest.playwright_core_package),
        &mut checkpoint,
    )?;
    let playwright_core = parse_package(&playwright_core_bytes, "playwright-core")?;
    if playwright_core.version != PLAYWRIGHT_VERSION
        || manifest.playwright_core_package.version != PLAYWRIGHT_VERSION
    {
        return Err(identity_error(
            "Browser worker playwright-core version does not match the pinned version",
        ));
    }

    let browsers_bytes = read_identity_bytes(
        open_under_root(worker_root, Path::new(BROWSERS_JSON_PATH))?,
        &expected(&manifest.playwright_core_browsers_json),
        &mut checkpoint,
    )?;
    let browsers = parse_browsers(&browsers_bytes)?;
    let Some(record) = browsers
        .browsers
        .iter()
        .find(|record| record.name == "chromium")
    else {
        return Err(identity_error(
            "Browser worker browsers.json does not declare Chromium",
        ));
    };
    let revision = record
        .revision
        .parse::<u32>()
        .map_err(|_| identity_error("Browser worker Chromium revision is invalid"))?;
    if revision != CHROMIUM_REVISION
        || record.browser_version.as_deref() != Some(CHROMIUM_VERSION)
        || manifest.chromium.revision != CHROMIUM_REVISION
        || manifest.chromium.version != CHROMIUM_VERSION
    {
        return Err(identity_error(
            "Browser worker Chromium version does not match the pinned version",
        ));
    }

    let chromium_image = verify_identity_digest(
        open_absolute_file(chromium)?,
        &expected_chromium(&manifest.chromium),
        true,
        &mut checkpoint,
    )?;
    for (root, expected) in [
        (node.parent(), &manifest.installation_trees.node),
        (
            Some(worker_root.as_path()),
            &manifest.installation_trees.worker,
        ),
        (chromium.parent(), &manifest.installation_trees.chromium),
    ] {
        installation::verify_tree(
            root.ok_or_else(|| identity_error("Browser installation root is missing"))?,
            expected,
            &mut checkpoint,
        )?;
    }

    let identity = VerifiedWorkerIdentity {
        node: node.clone(),
        worker_root: worker_root.clone(),
        cli_js: worker_root.join(CLI_JS_PATH),
        chromium: chromium.clone(),
        manifest_sha256: manifest_digest,
        local_images: Some(Arc::new(VerifiedLocalImages {
            node: node_image,
            chromium: chromium_image,
        })),
    };
    checkpoint()?;
    identity.check_local_executables(&mut checkpoint)?;
    Ok(identity)
}

/// One private, host-owned temp tree for a worker lifecycle.
struct TempArtifacts {
    base: PathBuf,
    config_path: PathBuf,
    output_dir: PathBuf,
    profile_dir: PathBuf,
    work_dir: PathBuf,
    temp_dir: PathBuf,
    active: bool,
}

fn create_private_directory(path: &Path) -> Result<(), BoundaryError> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder.create(path).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker directory cannot be created",
        )
    })
}

impl TempArtifacts {
    fn create(settings: &BrowserWorkerSettings) -> Result<Self, BoundaryError> {
        let root = settings
            .temp_root
            .clone()
            .unwrap_or_else(std::env::temp_dir);
        let root = fs::canonicalize(&root).map_err(|_| {
            error(
                "worker_environment",
                "Browser worker temp root is unavailable",
            )
        })?;
        if !root.is_dir() {
            return Err(error(
                "worker_environment",
                "Browser worker temp root is not a directory",
            ));
        }
        for _ in 0..16 {
            let base = root.join(format!(
                "saya-browser-worker-{}",
                uuid::Uuid::now_v7().simple()
            ));
            match create_private_directory(&base) {
                Ok(()) => {
                    let config_dir = base.join("config");
                    let output_dir = base.join("output");
                    let profile_dir = base.join("profile");
                    let work_dir = base.join("work");
                    let temp_dir = work_dir.join("tmp");
                    let result = (|| {
                        for path in [&config_dir, &output_dir, &profile_dir, &work_dir, &temp_dir] {
                            create_private_directory(path)?;
                        }
                        create_private_directory(&profile_dir.join("cache"))?;
                        Ok(())
                    })();
                    if let Err(error) = result {
                        let _ = fs::remove_dir_all(&base);
                        return Err(error);
                    }
                    return Ok(Self {
                        config_path: config_dir.join("config.json"),
                        base,
                        output_dir,
                        profile_dir,
                        work_dir,
                        temp_dir,
                        active: true,
                    });
                }
                Err(_) if base.exists() => continue,
                Err(error) => return Err(error),
            }
        }
        Err(error(
            "worker_environment",
            "Browser worker temp directory could not be allocated",
        ))
    }

    fn remove(&mut self) -> Result<(), BoundaryError> {
        if !self.active {
            return Ok(());
        }
        match fs::remove_dir_all(&self.base) {
            Ok(()) => {
                self.active = false;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.active = false;
                Ok(())
            }
            Err(_) => Err(error(
                "worker_cleanup",
                "Browser worker temp files could not be removed",
            )),
        }
    }

    fn directories(&self) -> BrowserSandboxDirectories {
        BrowserSandboxDirectories {
            profile: self.profile_dir.clone(),
            output: self.output_dir.clone(),
            tmp: self.temp_dir.clone(),
            work: self.work_dir.clone(),
            config: self
                .config_path
                .parent()
                .unwrap_or(&self.base)
                .to_path_buf(),
        }
    }
}

impl Drop for TempArtifacts {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.base);
        }
    }
}

/// Validate a host-supplied upload name without rewriting it. The page must
/// receive exactly the artifact's stored name; only unsafe path components are
/// rejected.
fn validate_worker_filename(value: &str) -> Result<&str, BrowserFailure> {
    let name = value
        .rsplit(['/', '\\'])
        .next()
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| {
            BrowserFailure::new("file_name_invalid", "Browser upload name is invalid")
        })?;
    if name != value
        || name.starts_with('.')
        || name.len() > 128
        || name.chars().any(char::is_control)
        || name.contains('\0')
    {
        return Err(BrowserFailure::new(
            "file_name_invalid",
            "Browser upload name is unsafe",
        ));
    }
    Ok(name)
}

/// Remove a staging directory unless it was disarmed after a complete write.
struct UploadDirectoryGuard {
    path: Option<PathBuf>,
}

impl Drop for UploadDirectoryGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

fn stage_worker_upload(
    work_dir: &Path,
    filename: &str,
    bytes: &[u8],
    cancel: &dyn Cancellation,
) -> Result<PathBuf, BrowserFailure> {
    if cancel.is_cancelled() {
        return Err(BrowserFailure::new(
            "cancelled",
            "Browser upload staging cancelled",
        ));
    }
    let name = validate_worker_filename(filename)?;
    let uploads = work_dir.join("uploads");
    match DirBuilder::new().mode(0o700).create(&uploads) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser upload staging directory cannot be created",
            ));
        }
    }
    let metadata = fs::symlink_metadata(&uploads)
        .map_err(|_| BrowserFailure::new("file_broker", "Browser upload staging is unavailable"))?;
    // SAFETY: geteuid has no pointer arguments or ownership requirements.
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(BrowserFailure::new(
            "file_broker",
            "Browser upload staging must be a private owned directory",
        ));
    }
    // A per-upload directory keeps the page-visible basename identical to the
    // artifact name while remaining collision-free.
    let directory = uploads.join(format!("upload-{}", uuid::Uuid::now_v7().simple()));
    DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|_| {
            BrowserFailure::new(
                "file_broker",
                "Browser upload staging directory cannot be created",
            )
        })?;
    let mut guard = UploadDirectoryGuard {
        path: Some(directory.clone()),
    };
    let path = directory.join(name);
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(&path).map_err(|_| {
        BrowserFailure::new(
            "file_broker",
            "Browser upload staging file cannot be created",
        )
    })?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| {
            BrowserFailure::new(
                "file_broker",
                "Browser upload staging file cannot be written or synced",
            )
        })?;
    drop(file);
    if cancel.is_cancelled() {
        return Err(BrowserFailure::new(
            "cancelled",
            "Browser upload staging cancelled",
        ));
    }
    guard.path = None;
    Ok(path)
}

struct UploadCleanupCancel;
impl Cancellation for UploadCleanupCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

/// Best-effort staging cleanup for a cancelled or dropped upload. Failure is
/// never reported as a successful upload; the Run-private directory is also
/// covered by the worker/Run cleanup path.
struct RemoteUploadGuard {
    connection: Option<Arc<dyn WorkerConnection>>,
    path: String,
}

impl Drop for RemoteUploadGuard {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        let path = std::mem::take(&mut self.path);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let _ = connection.remove_upload(&path, &UploadCleanupCancel).await;
        });
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), BoundaryError> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options.open(path).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker config cannot be created",
        )
    })?;
    file.write_all(bytes).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker config cannot be written",
        )
    })?;
    Ok(())
}

/// A fixed host-generated launch request. The injectable launcher makes offline
/// tests deterministic; production assembly uses [`ManagedProcessLauncher`].
#[derive(Debug, Clone)]
pub struct WorkerLaunchRequest {
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
    pub stderr_limit: usize,
    pub mcp_limits: McpLimits,
    /// Run-private directory layout that the launch inputs were derived from.
    pub directories: BrowserSandboxDirectories,
    /// The fresh isolation environment for this worker lifecycle. Production
    /// launches require it; offline fixtures may omit it.
    pub execution: Option<PreparedEnvironment>,
    /// Identity verified immediately before this launch. Remote backends use
    /// these resolved paths instead of re-deriving them from settings.
    pub identity: Option<VerifiedWorkerIdentity>,
}

fn os_path(path: &Path) -> Result<&str, BoundaryError> {
    path.to_str()
        .ok_or_else(|| invalid_config("Browser worker paths must be UTF-8"))
}

fn build_launch_request(
    settings: &BrowserWorkerSettings,
    identity: &VerifiedWorkerIdentity,
    directories: &BrowserSandboxDirectories,
) -> Result<WorkerLaunchRequest, BoundaryError> {
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(settings.connect_timeout_ms))
        .ok_or_else(|| identity_error("Browser worker image recheck deadline is out of range"))?;
    let mut checkpoint = || {
        if Instant::now() >= deadline {
            return Err(identity_error(
                "Browser worker image recheck exceeded its deadline",
            ));
        }
        Ok(())
    };
    identity.check_local_executables(&mut checkpoint)?;
    // Chromium's own sandbox stays enabled. The browser worker network policy
    // is always decided by the host environment; `worker.proxy` is validated
    // input reserved for the later broker stage and must not be injected here.
    let launch_options = json!({
        "headless": true,
        "chromiumSandbox": true,
        "executablePath": os_path(&identity.chromium)?,
    });
    let config = json!({
        "browser": {
            "browserName": "chromium",
            "isolated": true,
            "launchOptions": launch_options,
        },
        "outputDir": os_path(&directories.output)?,
        "imageResponses": "omit",
        "webmcp": false,
    });
    let bytes = serde_json::to_vec_pretty(&config).map_err(|_| {
        error(
            "worker_environment",
            "Browser worker config cannot be encoded",
        )
    })?;
    let config_path = directories.config.join("config.json");
    write_private_file(&config_path, &bytes)?;

    let argv = vec![
        OsString::from(os_path(&identity.node)?),
        OsString::from(os_path(&identity.cli_js)?),
        OsString::from("--config"),
        config_path.into_os_string(),
        OsString::from("--headless"),
        OsString::from("--isolated"),
        OsString::from("--sandbox"),
        OsString::from("--block-service-workers"),
        OsString::from("--no-webmcp"),
        OsString::from("--ignore-roots"),
        OsString::from("--image-responses"),
        OsString::from("omit"),
        OsString::from("--output-dir"),
        directories.output.clone().into_os_string(),
        OsString::from("--executable-path"),
        identity.chromium.clone().into_os_string(),
    ];

    let mut env = BTreeMap::new();
    env.insert(OsString::from("PATH"), OsString::from("/usr/bin:/bin"));
    env.insert(
        OsString::from("HOME"),
        directories.profile.clone().into_os_string(),
    );
    env.insert(
        OsString::from("TMPDIR"),
        directories.tmp.clone().into_os_string(),
    );
    env.insert(
        OsString::from("XDG_CACHE_HOME"),
        directories.profile.join("cache").into_os_string(),
    );
    env.insert(
        OsString::from("PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD"),
        OsString::from("1"),
    );
    env.insert(OsString::from("LANG"), OsString::from("C"));
    env.insert(OsString::from("NO_COLOR"), OsString::from("1"));

    Ok(WorkerLaunchRequest {
        argv,
        cwd: directories.work.clone(),
        env,
        timeout: Duration::from_millis(settings.request_timeout_ms.saturating_add(30_000)),
        stderr_limit: settings.stderr_limit_bytes,
        mcp_limits: McpLimits {
            connect_timeout: Duration::from_millis(settings.connect_timeout_ms),
            request_timeout: Duration::from_millis(settings.request_timeout_ms),
            max_wire_bytes: MAX_WORKER_RESULT_BYTES.saturating_add(1024 * 1024),
            max_pages: 32,
            max_tools: 256,
            max_catalog_bytes: MAX_WORKER_RESULT_BYTES,
            max_result_bytes: MAX_WORKER_RESULT_BYTES,
            max_content_blocks: 256,
        },
        directories: directories.clone(),
        execution: None,
        identity: Some(identity.clone()),
    })
}

pub(crate) fn environment_digest(env: &BTreeMap<OsString, OsString>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"saya-browser-worker-env-v1\0");
    for (key, value) in env {
        hasher.update(key.as_os_str().as_bytes());
        hasher.update([0]);
        hasher.update(value.as_os_str().as_bytes());
        hasher.update([0]);
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn map_environment_failure(failure: &BoundaryError) -> BrowserFailure {
    match failure.0.code.as_str() {
        "browser_sandbox_unavailable" => BrowserFailure::new(
            "browser_sandbox_unavailable",
            "Host execution cannot provide a Chromium sandbox for the browser worker",
        ),
        "browser_vm_unavailable" => BrowserFailure::new(
            "browser_vm_unavailable",
            "The dedicated browser VM host channel is unavailable",
        ),
        _ => BrowserFailure::new(
            "worker_environment",
            "Browser sandbox environment could not be prepared",
        ),
    }
}

fn map_mcp_failure(failure: BoundaryError) -> BrowserFailure {
    match failure.0.code.as_str() {
        "outcome_unknown" => BrowserFailure::uncertain(
            "Browser worker call completion could not be confirmed. The call was not replayed",
        ),
        "cancelled" => {
            BrowserFailure::new("cancelled", "Browser worker call cancelled before dispatch")
        }
        "mcp_stale_catalog" => BrowserFailure::new(
            "stale_catalog",
            "Browser worker catalogue changed; begin a new session",
        ),
        "mcp_unknown_tool" => {
            BrowserFailure::new("worker_catalog", "Browser worker tool is unavailable")
        }
        "invalid_args" => BrowserFailure::new(
            "invalid_worker_args",
            "Browser worker rejected the host request before dispatch",
        ),
        "mcp_limit" => BrowserFailure::new(
            "worker_limit",
            "Browser worker request exceeded a host limit",
        ),
        "needs_input" => BrowserFailure::new(
            "worker_input",
            "Browser worker requested additional input, which this provider does not supply",
        ),
        "mcp_timeout" | "mcp_schema" => BrowserFailure::uncertain(
            "Browser worker call completion could not be confirmed. The call was not replayed",
        ),
        _ => BrowserFailure::new(
            "worker_transport",
            "Browser worker call failed before a terminal result",
        ),
    }
}

/// Evidence collected from the real Chromium renderer, not from configuration.
/// It contains process identifiers and sandbox flags only; no page content or
/// credentials are recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromiumSandboxEvidence {
    pub renderer_pid: u32,
    pub no_new_privs: bool,
    pub seccomp_mode: u32,
    /// Neither `--no-sandbox` nor `--disable-setuid-sandbox` appeared.
    pub sandbox_arguments_valid: bool,
    /// Chromium's own stderr mentioned `InitializeSandbox`.
    pub stderr_initializes_sandbox: bool,
}

/// Host-side verifier for Chromium's own sandbox. Injectable so tests can
/// exercise failure paths without a real browser.
#[async_trait]
pub trait ChromiumSandboxInspector: Send + Sync {
    async fn inspect(
        &self,
        worker_pid: u32,
        stderr_tail: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure>;
}

const MAX_SANDBOX_PROCESSES: usize = 512;
const MAX_SANDBOX_PROC_FILE_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy)]
struct RendererState {
    pid: u32,
    no_new_privs: bool,
    seccomp_mode: u32,
    sandbox_arguments_valid: bool,
}

/// Inspects descendants of the supervised worker through procfs. Only
/// processes reachable from the worker PID, with a matching recorded parent,
/// are considered. A bounded retry gives Chromium time to create a renderer.
pub struct ProcfsChromiumSandboxInspector {
    proc_root: PathBuf,
    timeout: Duration,
    interval: Duration,
}

impl Default for ProcfsChromiumSandboxInspector {
    fn default() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            timeout: Duration::from_secs(5),
            interval: Duration::from_millis(100),
        }
    }
}

impl ProcfsChromiumSandboxInspector {
    #[cfg(test)]
    fn with_limits(proc_root: PathBuf, timeout: Duration, interval: Duration) -> Self {
        Self {
            proc_root,
            timeout,
            interval,
        }
    }
}

#[async_trait]
impl ChromiumSandboxInspector for ProcfsChromiumSandboxInspector {
    async fn inspect(
        &self,
        worker_pid: u32,
        stderr_tail: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        if worker_pid == 0 {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker process identity is unavailable",
            ));
        }
        let stderr_evidence = stderr_has_initialize_sandbox(stderr_tail);
        let deadline = std::time::Instant::now() + self.timeout;
        loop {
            if cancel.is_cancelled() {
                return Err(BrowserFailure::new(
                    "cancelled",
                    "Chromium sandbox verification cancelled",
                ));
            }
            let renderers = inspect_renderers(&self.proc_root, worker_pid);
            let sandbox_arguments_rejected = renderers
                .iter()
                .any(|renderer| !renderer.sandbox_arguments_valid);
            let sandbox_active = renderers
                .iter()
                .find(|renderer| renderer.no_new_privs && renderer.seccomp_mode == 2)
                .copied();
            // The host browser profile already imposes NoNewPrivs and a
            // generic seccomp filter through Bubblewrap, so those fields alone
            // do not prove that Chromium initialized its own sandbox. Require
            // Chromium's own InitializeSandbox marker (or an equivalent
            // Chromium-specific signal) as the discriminator.
            if !sandbox_arguments_rejected
                && stderr_evidence
                && let Some(renderer) = sandbox_active
            {
                return Ok(ChromiumSandboxEvidence {
                    renderer_pid: renderer.pid,
                    no_new_privs: renderer.no_new_privs,
                    seccomp_mode: renderer.seccomp_mode,
                    sandbox_arguments_valid: renderer.sandbox_arguments_valid,
                    stderr_initializes_sandbox: stderr_evidence,
                });
            }
            if std::time::Instant::now() >= deadline {
                return Err(BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    if stderr_evidence {
                        "Chromium renderer sandbox evidence is insufficient"
                    } else {
                        "Chromium did not report its own sandbox initialization"
                    },
                ));
            }
            tokio::time::sleep(self.interval).await;
        }
    }
}

fn read_bounded_proc_file(path: &Path) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_SANDBOX_PROC_FILE_BYTES as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

fn proc_status(path: &Path) -> Option<(Option<u32>, bool, u32)> {
    let bytes = read_bounded_proc_file(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut parent = None;
    let mut no_new_privs = false;
    let mut seccomp_mode = 0;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("PPid:") {
            parent = value.trim().parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("NoNewPrivs:") {
            no_new_privs = value.trim() == "1";
        } else if let Some(value) = line.strip_prefix("Seccomp:") {
            seccomp_mode = value.trim().parse::<u32>().unwrap_or(0);
        }
    }
    Some((parent, no_new_privs, seccomp_mode))
}

fn task_children(proc_root: &Path, pid: u32) -> Vec<u32> {
    let task_root = proc_root.join(pid.to_string()).join("task");
    let Ok(tasks) = fs::read_dir(task_root) else {
        return Vec::new();
    };
    let mut children = Vec::new();
    for task in tasks.flatten() {
        let path = task.path().join("children");
        let Some(bytes) = read_bounded_proc_file(&path) else {
            continue;
        };
        for token in String::from_utf8_lossy(&bytes).split_whitespace() {
            if let Ok(child) = token.parse::<u32>()
                && child != 0
            {
                children.push(child);
            }
        }
    }
    children.sort_unstable();
    children.dedup();
    children
}

fn descendant_pids(proc_root: &Path, root: u32) -> Vec<u32> {
    let mut queue = VecDeque::from([root]);
    let mut seen = BTreeSet::from([root]);
    let mut descendants = Vec::new();
    while let Some(parent) = queue.pop_front() {
        if descendants.len() >= MAX_SANDBOX_PROCESSES {
            break;
        }
        for child in task_children(proc_root, parent) {
            if !seen.insert(child) {
                continue;
            }
            // A child file alone is not trusted: require the candidate to
            // still report the discovered parent, which bounds PID-reuse
            // misreads to the traversal that observed it.
            let status = proc_root.join(child.to_string()).join("status");
            let Some((Some(ppid), _, _)) = proc_status(&status) else {
                continue;
            };
            if ppid != parent {
                continue;
            }
            descendants.push(child);
            queue.push_back(child);
            if descendants.len() >= MAX_SANDBOX_PROCESSES {
                break;
            }
        }
    }
    descendants
}

fn cmdline_tokens(proc_root: &Path, pid: u32) -> Vec<String> {
    let path = proc_root.join(pid.to_string()).join("cmdline");
    let Some(bytes) = read_bounded_proc_file(&path) else {
        return Vec::new();
    };
    bytes
        .split(|byte| *byte == 0)
        .filter(|token| !token.is_empty())
        .map(|token| String::from_utf8_lossy(token).into_owned())
        .collect()
}

fn inspect_renderers(proc_root: &Path, worker_pid: u32) -> Vec<RendererState> {
    let mut renderers = Vec::new();
    for pid in descendant_pids(proc_root, worker_pid) {
        let tokens = cmdline_tokens(proc_root, pid);
        if !tokens.iter().any(|token| token == "--type=renderer") {
            continue;
        }
        let status = proc_root.join(pid.to_string()).join("status");
        let Some((_, no_new_privs, seccomp_mode)) = proc_status(&status) else {
            continue;
        };
        let sandbox_arguments_valid = !tokens
            .iter()
            .any(|token| matches!(token.as_str(), "--no-sandbox" | "--disable-setuid-sandbox"));
        renderers.push(RendererState {
            pid,
            no_new_privs,
            seccomp_mode,
            sandbox_arguments_valid,
        });
    }
    renderers
}

fn stderr_has_initialize_sandbox(stderr: &[u8]) -> bool {
    String::from_utf8_lossy(stderr).contains("InitializeSandbox")
}

/// Creates a fresh browser isolation environment for one worker lifecycle.
/// Implementations prepare a new environment for every launch; a closed
/// environment is never reused.
pub trait BrowserEnvironmentFactory: Send + Sync {
    fn prepare(&self) -> Result<Arc<dyn BrowserWorkspaceLease>, BoundaryError>;
}

/// One run-private workspace lease. `release` must reap nothing itself; the
/// provider reaps the worker and Chromium process group first, then calls this
/// to await lease cleanup and remove the private directories.
#[async_trait]
pub trait BrowserWorkspaceLease: Send + Sync {
    fn directories(&self) -> &BrowserSandboxDirectories;
    /// The prepared native environment for production launches. Offline test
    /// leases return `None`.
    fn execution(&self) -> Option<PreparedEnvironment>;
    async fn release(&self) -> Result<(), BoundaryError>;
}

struct NativeBrowserWorkspace {
    directories: BrowserSandboxDirectories,
    environment: PreparedEnvironment,
}

#[async_trait]
impl BrowserWorkspaceLease for NativeBrowserWorkspace {
    fn directories(&self) -> &BrowserSandboxDirectories {
        &self.directories
    }

    fn execution(&self) -> Option<PreparedEnvironment> {
        Some(self.environment.clone())
    }

    async fn release(&self) -> Result<(), BoundaryError> {
        self.environment.wait_for_process_cleanup().await?;
        self.environment.cleanup().map(|_| ())
    }
}

/// Production factory over the native browser sandbox. Construction performs a
/// preflight preparation and cleanup so unsupported hosts fail before a worker
/// or Chromium process can start.
pub struct NativeBrowserEnvironmentFactory {
    install_roots: Vec<PathBuf>,
    backend: BackendKind,
}

impl NativeBrowserEnvironmentFactory {
    pub fn new(install_roots: Vec<PathBuf>, backend: BackendKind) -> Result<Self, BoundaryError> {
        let factory = Self {
            install_roots,
            backend,
        };
        factory.preflight()?;
        Ok(factory)
    }

    fn policy(&self) -> SandboxPolicy {
        let mut policy = SandboxPolicy::browser(BrowserSandboxProfile {
            install_roots: self.install_roots.clone(),
        });
        policy.backend = self.backend;
        policy
    }

    fn preflight(&self) -> Result<(), BoundaryError> {
        let environment = crate::execution::prepare(&self.policy())?;
        environment.cleanup().map(|_| ())
    }
}

impl BrowserEnvironmentFactory for NativeBrowserEnvironmentFactory {
    fn prepare(&self) -> Result<Arc<dyn BrowserWorkspaceLease>, BoundaryError> {
        let environment = crate::execution::prepare(&self.policy())?;
        let Some(directories) = environment.browser_directories().cloned() else {
            let _ = environment.cleanup();
            return Err(error(
                "browser_sandbox_unavailable",
                "Browser sandbox did not establish run-private browser directories",
            ));
        };
        if !environment.is_browser() || environment.identity().browser.is_none() {
            let _ = environment.cleanup();
            return Err(error(
                "browser_sandbox_unavailable",
                "Browser sandbox did not establish a browser environment",
            ));
        }
        Ok(Arc::new(NativeBrowserWorkspace {
            directories,
            environment,
        }))
    }
}

/// Injectable worker launch/transport boundary.
#[async_trait]
pub trait WorkerLauncher: Send + Sync {
    async fn launch(
        &self,
        request: WorkerLaunchRequest,
        cancel: &dyn Cancellation,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure>;

    /// Retry cleanup that a previous launch could not confirm. Backends that
    /// keep no remote state succeed immediately. A failure keeps the handle
    /// registered, so a later shutdown or launch retries instead of reporting
    /// a second false success.
    async fn retry_pending_cleanup(&self) -> Result<(), BrowserFailure> {
        Ok(())
    }
}

/// Verifies the exact worker identity the selected backend will launch. The
/// local implementation reads host files; a backend with a remote worker
/// verifies the same manifest and named files through its controlled channel.
#[async_trait]
pub trait WorkerIdentityVerifier: Send + Sync {
    /// Construction-time check. Local backends verify files here; remote
    /// backends validate settings and local connection inputs without starting
    /// a worker.
    fn preflight(&self) -> Result<(), BoundaryError>;

    /// Verification immediately before a launch. Must not be satisfied by a
    /// transport-level success status alone.
    async fn verify(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<VerifiedWorkerIdentity, BrowserFailure>;
}

/// A backend-provided verifier over the host-named worker files.
pub struct LocalWorkerIdentityVerifier {
    settings: BrowserWorkerSettings,
}

impl LocalWorkerIdentityVerifier {
    pub fn new(settings: BrowserWorkerSettings) -> Self {
        Self { settings }
    }
}

#[async_trait]
impl WorkerIdentityVerifier for LocalWorkerIdentityVerifier {
    fn preflight(&self) -> Result<(), BoundaryError> {
        verify_worker_identity(&self.settings).map(|_| ())
    }

    async fn verify(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<VerifiedWorkerIdentity, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker identity check cancelled before dispatch",
            ));
        }
        verify_worker_identity_checked(&self.settings, Some(cancel)).map_err(|failure| {
            if failure.0.code == "cancelled" {
                BrowserFailure::new(
                    "cancelled",
                    "Browser worker identity verification cancelled before launch",
                )
            } else {
                BrowserFailure::new(
                    "worker_identity",
                    "Browser worker identity changed before launch",
                )
            }
        })
    }
}

/// Builds the exact launch request for a backend. Provider lifecycle,
/// catalogue validation, and tool mapping stay shared across backends.
pub trait WorkerLaunchPlanner: Send + Sync {
    fn build(
        &self,
        identity: &VerifiedWorkerIdentity,
        directories: &BrowserSandboxDirectories,
    ) -> Result<WorkerLaunchRequest, BoundaryError>;
}

/// Local planner over host-resolved Node, worker and Chromium paths.
pub struct LocalWorkerLaunchPlanner {
    settings: BrowserWorkerSettings,
}

impl LocalWorkerLaunchPlanner {
    pub fn new(settings: BrowserWorkerSettings) -> Self {
        Self { settings }
    }
}

impl WorkerLaunchPlanner for LocalWorkerLaunchPlanner {
    fn build(
        &self,
        identity: &VerifiedWorkerIdentity,
        directories: &BrowserSandboxDirectories,
    ) -> Result<WorkerLaunchRequest, BoundaryError> {
        build_launch_request(&self.settings, identity, directories)
    }
}

/// Host-only roles reported by the fixed private worker protocol.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerProcessInfo {
    pub schema_version: u32,
    pub node_pid: u32,
    pub node_pid_namespace: WorkerPidNamespace,
    pub browser_pid: u32,
    pub renderer_pids: Vec<u32>,
}

impl WorkerProcessInfo {
    pub(crate) fn decode(value: Value) -> Result<Self, BrowserFailure> {
        let info: Self = serde_json::from_value(value).map_err(|_| process_info_unavailable())?;
        let namespace_valid = [&info.node_pid_namespace.dev, &info.node_pid_namespace.ino]
            .into_iter()
            .all(|value| {
                value
                    .parse::<u64>()
                    .is_ok_and(|number| number != 0 && number.to_string() == *value)
            });
        let mut pids = BTreeSet::from([info.node_pid, info.browser_pid]);
        if info.schema_version != 1
            || info.node_pid == 0
            || info.browser_pid == 0
            || pids.len() != 2
            || !namespace_valid
            || info.renderer_pids.len() > MAX_SANDBOX_PROCESSES
            || info
                .renderer_pids
                .iter()
                .any(|pid| *pid == 0 || !pids.insert(*pid))
        {
            return Err(process_info_unavailable());
        }
        Ok(info)
    }
}

fn process_info_unavailable() -> BrowserFailure {
    BrowserFailure::new(
        "worker_process_info",
        "Browser worker process information is unavailable",
    )
}

pub(crate) fn process_info_failure(failure: BoundaryError) -> BrowserFailure {
    if failure.0.code == "cancelled" {
        BrowserFailure::new("cancelled", "Browser process information query cancelled")
    } else {
        process_info_unavailable()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkerPidNamespace {
    pub dev: String,
    pub ino: String,
}

/// A live worker connection. Implementations must send each call at most once.
#[async_trait]
pub trait WorkerConnection: Send + Sync {
    fn protocol_version(&self) -> Option<String>;
    fn generation(&self) -> u64;
    /// The supervised worker process-group leader, when this is a real
    /// process. Offline fixtures return `None` and skip procfs inspection.
    fn process_id(&self) -> Option<u32> {
        None
    }
    /// Bounded stderr tail used as supplemental sandbox evidence.
    fn stderr_tail(&self) -> Vec<u8> {
        Vec::new()
    }
    /// Read fixed private roles without creating a browser or exposing a tool.
    async fn process_info(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<WorkerProcessInfo, BrowserFailure> {
        Err(BrowserFailure::new(
            "worker_process_info",
            "Browser worker process information is unavailable",
        ))
    }
    async fn list_tools(&self, cancel: &dyn Cancellation) -> Result<McpCatalog, BrowserFailure>;
    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        generation: u64,
        cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure>;
    /// Copy one host-staged upload into the worker's Run-private staging area
    /// and return the absolute path the worker may pass to
    /// `browser_file_upload`.
    async fn put_upload(
        &self,
        _filename: &str,
        _bytes: &[u8],
        _cancel: &dyn Cancellation,
    ) -> Result<String, BrowserFailure> {
        Err(BrowserFailure::unsupported("upload staging"))
    }
    /// Remove one staging upload after the fixed upload tool call. The path is
    /// the value previously returned by `put_upload`.
    async fn remove_upload(
        &self,
        _path: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        Ok(())
    }
    /// Read one download file produced by this worker. Implementations must
    /// confine the path to the Run's output directory and stay bounded.
    async fn read_download(
        &self,
        _path: &str,
        _max_bytes: usize,
        _cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        Err(BrowserFailure::unsupported("download read"))
    }
    /// List regular files currently present in the Run-private download
    /// directory. Only the provider's own event for this click may name a
    /// download; this listing proves the named file is newly produced.
    async fn list_downloads(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        Err(BrowserFailure::unsupported("download listing"))
    }
    async fn shutdown(&self) -> Result<(), BrowserFailure>;
}

struct RealWorkerConnection {
    client: AsyncMutex<Option<crate::mcp::McpClient>>,
    process: AsyncMutex<Option<ManagedProcess>>,
    protocol: String,
    generation: std::sync::atomic::AtomicU64,
    pid: u32,
    stderr_tail: Arc<std::sync::Mutex<VecDeque<u8>>>,
    work_dir: PathBuf,
    output_dir: PathBuf,
}

#[async_trait]
impl WorkerConnection for RealWorkerConnection {
    fn protocol_version(&self) -> Option<String> {
        Some(self.protocol.clone())
    }

    fn generation(&self) -> u64 {
        self.generation.load(std::sync::atomic::Ordering::Acquire)
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_tail
            .lock()
            .map(|tail| tail.iter().copied().collect())
            .unwrap_or_default()
    }

    async fn process_info(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<WorkerProcessInfo, BrowserFailure> {
        let client = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(BrowserFailure::new("cancelled", "Browser process information query cancelled")),
            client = self.client.lock() => client,
        };
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser worker is closed"))?;
        let value = client
            .browser_process_info(cancel)
            .await
            .map_err(process_info_failure)?;
        WorkerProcessInfo::decode(value)
    }

    async fn list_tools(&self, cancel: &dyn Cancellation) -> Result<McpCatalog, BrowserFailure> {
        let client = self.client.lock().await;
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser worker is closed"))?;
        let catalog = client.list_tools(cancel).await.map_err(map_mcp_failure)?;
        self.generation
            .store(catalog.generation, std::sync::atomic::Ordering::Release);
        Ok(catalog)
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        generation: u64,
        cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure> {
        let client = self.client.lock().await;
        let client = client
            .as_ref()
            .ok_or_else(|| BrowserFailure::new("worker_closed", "Browser worker is closed"))?;
        client
            .call_tool(name, arguments, generation, cancel)
            .await
            .map_err(map_mcp_failure)
    }

    async fn put_upload(
        &self,
        filename: &str,
        bytes: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<String, BrowserFailure> {
        let path = stage_worker_upload(&self.work_dir, filename, bytes, cancel)?;
        path.to_str().map(str::to_owned).ok_or_else(|| {
            BrowserFailure::new("file_broker", "Browser upload staging path is not UTF-8")
        })
    }

    async fn remove_upload(
        &self,
        path: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        let uploads = fs::canonicalize(self.work_dir.join("uploads")).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser upload staging is unavailable")
        })?;
        let candidate = match fs::canonicalize(path) {
            Ok(candidate) => candidate,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                return Err(BrowserFailure::new(
                    "file_broker",
                    "Browser upload staging file is unavailable",
                ));
            }
        };
        if !candidate.starts_with(&uploads) {
            return Err(BrowserFailure::new(
                "file_path_denied",
                "Browser upload staging path is outside its directory",
            ));
        }
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => {
                return Err(BrowserFailure::new(
                    "file_broker",
                    "Browser upload staging file cannot be removed",
                ));
            }
        }
        // Each upload lives in its own private directory so the page receives
        // the original file name unchanged. Remove that directory as well.
        if let Some(directory) = candidate.parent()
            && directory.parent() == Some(uploads.as_path())
        {
            let _ = fs::remove_dir(directory);
        }
        Ok(())
    }

    async fn read_download(
        &self,
        path: &str,
        max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser download read cancelled",
            ));
        }
        let root = fs::canonicalize(&self.output_dir).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser output directory is unavailable")
        })?;
        let candidate = if path.contains(['/', '\\']) {
            fs::canonicalize(path).map_err(|_| {
                BrowserFailure::new("file_broker", "Browser download file is unavailable")
            })?
        } else {
            fs::canonicalize(root.join(path)).map_err(|_| {
                BrowserFailure::new("file_broker", "Browser download file is unavailable")
            })?
        };
        if !candidate.starts_with(&root) {
            return Err(BrowserFailure::new(
                "file_path_denied",
                "Browser download path is outside the Run output directory",
            ));
        }
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let file = options.open(&candidate).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser download file cannot be opened")
        })?;
        let metadata = file.metadata().map_err(|_| {
            BrowserFailure::new("file_broker", "Browser download file cannot be inspected")
        })?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes as u64 {
            return Err(BrowserFailure::new(
                "file_too_large",
                "Browser download file is not a bounded regular file",
            ));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        std::io::Read::read_to_end(&mut &file, &mut bytes).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser download file cannot be read")
        })?;
        if bytes.len() as u64 != metadata.len() || bytes.is_empty() {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser download file changed while being read",
            ));
        }
        Ok(bytes)
    }

    async fn list_downloads(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser download listing cancelled",
            ));
        }
        let root = fs::canonicalize(&self.output_dir).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser output directory is unavailable")
        })?;
        let entries = fs::read_dir(&root).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser output directory cannot be listed")
        })?;
        let mut names = BTreeSet::new();
        for entry in entries {
            let entry = entry.map_err(|_| {
                BrowserFailure::new("file_broker", "Browser output directory cannot be listed")
            })?;
            let file_type = entry.file_type().map_err(|_| {
                BrowserFailure::new("file_broker", "Browser output entry cannot be inspected")
            })?;
            if !file_type.is_file() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.')
                || name.contains('\0')
                || name.ends_with(".crdownload")
                || name.ends_with(".part")
                || name.ends_with(".tmp")
                || worker_metadata_file(&name)
            {
                continue;
            }
            names.insert(name);
        }
        Ok(names)
    }

    async fn shutdown(&self) -> Result<(), BrowserFailure> {
        let mut failure: Option<BrowserFailure> = None;
        let client = self.client.lock().await.take();
        if let Some(client) = client
            && client.shutdown().await.is_err()
        {
            failure = Some(BrowserFailure::uncertain(
                "Browser worker MCP shutdown could not be confirmed",
            ));
        }
        let process = self.process.lock().await.take();
        if let Some(mut process) = process
            && process.terminate().await.is_err()
        {
            failure = Some(BrowserFailure::uncertain(
                "Browser worker process termination could not be confirmed",
            ));
        }
        match failure {
            Some(failure) => Err(failure),
            None => Ok(()),
        }
    }
}

/// Production launcher. It composes with `PreparedEnvironment::prepare_command`
/// through `ManagedProcess`, so sandbox selection stays with the host. The
/// isolation environment is carried by each launch request so a fresh
/// environment is used for every worker lifecycle.
pub struct ManagedProcessLauncher {
    config: ExecConfig,
}

impl ManagedProcessLauncher {
    pub fn new(config: ExecConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl WorkerLauncher for ManagedProcessLauncher {
    async fn launch(
        &self,
        request: WorkerLaunchRequest,
        cancel: &dyn Cancellation,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker launch cancelled before dispatch",
            ));
        }
        let deadline = Instant::now()
            .checked_add(request.mcp_limits.connect_timeout)
            .ok_or_else(|| {
                BrowserFailure::new(
                    "worker_identity",
                    "Browser worker image recheck deadline is out of range",
                )
            })?;
        let mut checkpoint = || {
            if cancel.is_cancelled() {
                return Err(error("cancelled", "Browser worker image recheck cancelled"));
            }
            if Instant::now() >= deadline {
                return Err(identity_error(
                    "Browser worker image recheck exceeded its deadline",
                ));
            }
            Ok(())
        };
        request
            .identity
            .as_ref()
            .ok_or_else(|| {
                BrowserFailure::new(
                    "worker_identity",
                    "Browser worker launch identity is missing",
                )
            })?
            .check_local_executables(&mut checkpoint)
            .map_err(|failure| BrowserFailure::new(failure.0.code, failure.0.message))?;
        let Some(environment) = request.execution.clone() else {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser worker requires a prepared browser environment",
            ));
        };
        let argv = request
            .argv
            .iter()
            .map(|value| {
                value.to_str().map(str::to_owned).ok_or_else(|| {
                    BrowserFailure::new(
                        "worker_spawn",
                        "Browser worker argv is not representable for the host launcher",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let plan = crate::tools::exec::ExecPlan {
            argv,
            cwd: request.cwd.clone(),
            timeout: request.timeout,
            environment_digest: environment_digest(&request.env),
        };
        let cwd = {
            let mut options = OpenOptions::new();
            options.read(true).custom_flags(
                libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            );
            let file = options.open(&request.cwd).map_err(|_| {
                BrowserFailure::new(
                    "worker_spawn",
                    "Browser worker work directory is unavailable",
                )
            })?;
            Dir::from_std_file(file)
        };
        let mut config = self.config.clone();
        config.execution = Some(environment);
        let mut process = ManagedProcess::spawn(&plan, cwd, request.env.clone(), &config, true)
            .map_err(|_| {
                BrowserFailure::new(
                    "worker_spawn",
                    "Browser worker process could not be started",
                )
            })?;
        let pid = process.pid().as_raw() as u32;

        let Some(input) = process.take_stdin() else {
            let _ = process.terminate().await;
            return Err(BrowserFailure::new(
                "worker_spawn",
                "Browser worker stdin pipe is unavailable",
            ));
        };
        let Some(output) = process.take_stdout() else {
            let _ = process.terminate().await;
            return Err(BrowserFailure::new(
                "worker_spawn",
                "Browser worker stdout pipe is unavailable",
            ));
        };
        let stderr_tail = Arc::new(std::sync::Mutex::new(VecDeque::with_capacity(
            request.stderr_limit,
        )));
        if let Some(mut stderr) = process.take_stderr() {
            let keep = request.stderr_limit;
            let shared = stderr_tail.clone();
            tokio::spawn(async move {
                let mut buffer = [0u8; 8192];
                loop {
                    match stderr.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            if let Ok(mut tail) = shared.lock() {
                                for byte in &buffer[..count] {
                                    if tail.len() == keep {
                                        tail.pop_front();
                                    }
                                    tail.push_back(*byte);
                                }
                                if tail.capacity() > keep {
                                    tail.shrink_to(keep);
                                }
                            }
                            // The bounded tail is retained for sandbox evidence
                            // and never surfaced to the model.
                        }
                    }
                }
            });
        }

        let client = match crate::mcp::McpClient::connect_stdio(
            "saya-browser-worker",
            output,
            input,
            request.mcp_limits,
            cancel,
        )
        .await
        {
            Ok(client) => client,
            Err(failure) => {
                let code = failure.0.code.clone();
                if process.terminate().await.is_err() {
                    return Err(BrowserFailure::uncertain(
                        "Browser worker initialization failed and process cleanup is unknown",
                    ));
                }
                if code == "cancelled" {
                    return Err(BrowserFailure::new(
                        "cancelled",
                        "Browser worker connect cancelled before initialization completed",
                    ));
                }
                if code == "mcp_protocol_version" {
                    return Err(BrowserFailure::new(
                        "worker_protocol",
                        "Browser worker has no supported protocol version",
                    ));
                }
                return Err(BrowserFailure::new(
                    "worker_connect",
                    "Browser worker initialization failed",
                ));
            }
        };
        let protocol = client.protocol_version().unwrap_or_default();
        Ok(Arc::new(RealWorkerConnection {
            client: AsyncMutex::new(Some(client)),
            process: AsyncMutex::new(Some(process)),
            protocol,
            generation: std::sync::atomic::AtomicU64::new(0),
            pid,
            stderr_tail,
            work_dir: request.directories.work.clone(),
            output_dir: request.directories.output.clone(),
        }))
    }
}

fn expected_worker_schemas() -> Vec<(&'static str, Value)> {
    vec![
        (
            "browser_navigate",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {"url": {"description": "The URL to navigate to", "type": "string"}},
                "required": ["url"],
                "type": "object"
            }),
        ),
        (
            "browser_snapshot",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "boxes": {"description": "Include each element's bounding box as [box=x,y,width,height] in the snapshot. Coordinates are viewport-relative, in CSS pixels (Element.getBoundingClientRect)", "type": "boolean"},
                    "depth": {"description": "Limit the depth of the snapshot tree", "type": "number"},
                    "filename": {"description": "Save snapshot to a file instead of returning it in the response. Relative file names are resolved against the workspace root.", "type": "string"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"}
                },
                "type": "object"
            }),
        ),
        (
            "browser_click",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "button": {"description": "Button to click, defaults to left", "enum": ["left", "right", "middle"], "type": "string"},
                    "doubleClick": {"description": "Whether to perform a double click instead of a single click", "type": "boolean"},
                    "element": {"description": "Human-readable element description used to obtain permission to interact with the element", "type": "string"},
                    "modifiers": {"description": "Modifier keys to press", "items": {"enum": ["Alt", "Control", "ControlOrMeta", "Meta", "Shift"], "type": "string"}, "type": "array"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"}
                },
                "required": ["target"],
                "type": "object"
            }),
        ),
        (
            "browser_type",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "element": {"description": "Human-readable element description used to obtain permission to interact with the element", "type": "string"},
                    "slowly": {"description": "Whether to type one character at a time. Useful for triggering key handlers in the page. By default entire text is filled in at once.", "type": "boolean"},
                    "submit": {"description": "Whether to submit entered text (press Enter after)", "type": "boolean"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"},
                    "text": {"description": "Text to type into the element", "type": "string"}
                },
                "required": ["target", "text"],
                "type": "object"
            }),
        ),
        (
            "browser_press_key",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {"key": {"description": "Name of the key to press or a character to generate, such as `ArrowLeft` or `a`", "type": "string"}},
                "required": ["key"],
                "type": "object"
            }),
        ),
        (
            "browser_wait_for",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "text": {"description": "The text to wait for", "type": "string"},
                    "textGone": {"description": "The text to wait for to disappear", "type": "string"},
                    "time": {"description": "The time to wait in seconds, at most 30. When combined with text or textGone, serves as a timeout for them instead of the default action timeout", "type": "number"}
                },
                "type": "object"
            }),
        ),
        (
            // The pinned worker exposes no masking parameter. This schema is
            // pinned so identity checks are exact and so the absence of a mask
            // input is an explicit capability boundary, not an accident.
            "browser_take_screenshot",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {
                    "element": {"description": "Human-readable element description used to obtain permission to interact with the element", "type": "string"},
                    "target": {"description": "Exact target element reference from the page snapshot, or a unique element selector", "type": "string"},
                    "type": {"description": "Image format for the screenshot. If unset, inferred from the filename extension, otherwise png.", "enum": ["png", "jpeg", "webp"], "type": "string"},
                    "filename": {"description": "File name to save the screenshot to. Relative file names are resolved against the workspace root. If not specified, the screenshot is saved into the output directory as `page-{timestamp}.{png|jpeg|webp}`.", "type": "string"},
                    "fullPage": {"description": "When true, takes a screenshot of the full scrollable page, instead of the currently visible viewport. Cannot be used with element screenshots.", "type": "boolean"},
                    "scale": {"default": "css", "description": "Image resolution scale. \"css\" produces a screenshot sized in CSS pixels (smaller, consistent across devices). \"device\" produces a high-resolution screenshot using device pixels (larger, accounts for the device pixel ratio). Default is css.", "enum": ["css", "device"], "type": "string"}
                },
                "required": ["scale"],
                "type": "object"
            }),
        ),
        (
            "browser_close",
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "additionalProperties": false,
                "properties": {},
                "type": "object"
            }),
        ),
    ]
}

fn validate_catalog(catalog: &McpCatalog) -> Result<(), BrowserFailure> {
    let expected = expected_worker_schemas();
    let mapped = MAPPED_WORKER_TOOLS.iter().copied().collect::<BTreeSet<_>>();
    for (name, schema) in &expected {
        let tool = catalog
            .tools
            .iter()
            .find(|tool| tool.name == *name)
            .ok_or_else(|| {
                BrowserFailure::new(
                    "worker_catalog",
                    "Browser worker is missing a required tool",
                )
            })?;
        if tool.input_schema != *schema {
            return Err(BrowserFailure::new(
                "worker_catalog",
                "Browser worker tool schema differs from the pinned schema",
            ));
        }
        if !mapped.contains(name) {
            return Err(BrowserFailure::new(
                "worker_catalog",
                "Browser worker tool mapping is not pinned",
            ));
        }
    }
    // The file-input tool is mapped for the host file broker. Its exact
    // descriptive text is not part of the host contract, but the argument
    // shape must still be the pinned `paths: string[]` form.
    let upload = catalog
        .tools
        .iter()
        .find(|tool| tool.name == "browser_file_upload")
        .ok_or_else(|| {
            BrowserFailure::new(
                "worker_catalog",
                "Browser worker is missing its file upload tool",
            )
        })?;
    let paths = upload
        .input_schema
        .get("properties")
        .and_then(|properties| properties.get("paths"));
    if !paths.is_some_and(|paths| paths.get("type").and_then(Value::as_str) == Some("array")) {
        return Err(BrowserFailure::new(
            "worker_catalog",
            "Browser worker file upload schema is not the pinned shape",
        ));
    }
    Ok(())
}

fn first_text(result: &McpToolResult) -> Result<&str, BrowserFailure> {
    result
        .content
        .iter()
        .find_map(|content| match content {
            crate::mcp::McpContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .ok_or_else(|| {
            BrowserFailure::new(
                "invalid_provider_result",
                "Browser worker result did not contain text",
            )
        })
}

fn section_body<'a>(text: &'a str, heading: &str) -> Option<Vec<&'a str>> {
    let mut lines = text.lines();
    let mut started = false;
    let mut body = Vec::new();
    for line in lines.by_ref() {
        let line = line.trim_end_matches('\r');
        if !started {
            if line == heading {
                started = true;
            }
            continue;
        }
        if line.starts_with("### ") {
            break;
        }
        body.push(line);
    }
    started.then_some(body)
}

fn page_fields(text: &str) -> (Option<String>, Option<String>) {
    let Some(lines) = section_body(text, "### Page") else {
        return (None, None);
    };
    let mut url = None;
    let mut title = None;
    for line in lines {
        if let Some(value) = line.strip_prefix("- Page URL: ")
            && url.is_none()
        {
            url = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("- Page Title: ")
            && title.is_none()
        {
            title = Some(value.to_owned());
        }
    }
    (url, title)
}

fn result_detail(text: &str) -> Option<String> {
    section_body(text, "### Result").and_then(|lines| {
        lines
            .into_iter()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_owned)
    })
}

/// Playwright writes its own snapshot files into the same output directory.
/// They are not downloads and must not participate in the directory diff.
fn worker_metadata_file(name: &str) -> bool {
    name.starts_with("page-") && (name.ends_with(".yml") || name.ends_with(".yaml"))
}

/// Extract the worker's own download event from the fixed `### Download`
/// section. Page titles, snapshot bodies and prose elsewhere in the result are
/// never searched for a path. Exactly one well-formed event is accepted.
fn download_path(text: &str) -> Result<Option<String>, BrowserFailure> {
    let Some(lines) = section_body(text, "### Download") else {
        return Ok(None);
    };
    let mut paths = Vec::new();
    for line in lines {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        let Some(rest) = line.strip_prefix("- Downloaded file ") else {
            continue;
        };
        let Some(start) = rest.rfind(" to \"") else {
            continue;
        };
        let path = &rest[start + 5..];
        let Some(path) = path.strip_suffix('"') else {
            continue;
        };
        if path.is_empty()
            || path.len() > 4096
            || path.chars().any(char::is_control)
            || path.contains('\0')
        {
            continue;
        }
        paths.push(path.to_owned());
    }
    match paths.len() {
        0 => Ok(None),
        1 => Ok(paths.pop()),
        _ => Err(BrowserFailure::new(
            "invalid_provider_result",
            "Browser download section names more than one file",
        )),
    }
}

fn snapshot_yaml(text: &str) -> Option<String> {
    let lines = section_body(text, "### Snapshot")?;
    let mut body = Vec::new();
    let mut in_yaml = false;
    for line in lines {
        if !in_yaml {
            if line.starts_with("```yaml") {
                in_yaml = true;
            }
            continue;
        }
        if line.starts_with("```") {
            return Some(body.join("\n"));
        }
        body.push(line);
    }
    None
}

const MAX_ARIA_LINE_BYTES: usize = 16 * 1024;
const MAX_ARIA_NAME_BYTES: usize = 4096;
const MAX_ARIA_REF_BYTES: usize = 1024;

fn invalid_snapshot(message: &'static str) -> BrowserFailure {
    BrowserFailure::new("invalid_provider_result", message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ElementReference {
    Main,
    Frame(String),
}

/// Parse exactly the reference grammar emitted by the pinned provider:
/// `e<digits>` for the main document and `f<digits>e<digits>` for an element
/// owned by a child frame. A malformed value fails the whole snapshot instead
/// of being ignored with its subtree.
fn parse_element_reference(key: &str) -> Result<ElementReference, BrowserFailure> {
    if key.is_empty() || key.len() > MAX_ARIA_REF_BYTES {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element reference",
        ));
    }
    let bytes = key.as_bytes();
    let mut index = 0usize;
    let mut frame = None;
    if bytes[0] == b'f' {
        let start = index;
        index += 1;
        let digits = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index == digits {
            return Err(invalid_snapshot(
                "Browser snapshot contains an invalid frame reference",
            ));
        }
        frame = Some(key[start..index].to_owned());
    }
    if bytes.get(index) != Some(&b'e') {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element reference",
        ));
    }
    index += 1;
    let digits = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == digits || index != bytes.len() {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element reference",
        ));
    }
    Ok(match frame {
        Some(frame) => ElementReference::Frame(frame),
        None => ElementReference::Main,
    })
}

/// Decode one JSON (double-quoted) string and return the remaining input.
/// Accessible names are always JSON-encoded by the provider, so a bracket that
/// looks like `[ref=...]` inside the name is data, never a key.
fn parse_quoted(input: &str) -> Result<(String, &str), BrowserFailure> {
    if !input.starts_with('"') {
        return Err(invalid_snapshot(
            "Browser snapshot name is not a quoted string",
        ));
    }
    let tokens: Vec<(usize, char)> = input.char_indices().collect();
    let mut position = 1usize;
    let mut output = String::new();
    while position < tokens.len() {
        let (_, character) = tokens[position];
        position += 1;
        match character {
            '"' => {
                if output.len() > MAX_ARIA_NAME_BYTES {
                    return Err(invalid_snapshot("Browser snapshot name is too long"));
                }
                let rest = tokens.get(position).map_or("", |(byte, _)| &input[*byte..]);
                return Ok((output, rest));
            }
            '\\' => {
                let Some((_, escape)) = tokens.get(position).copied() else {
                    return Err(invalid_snapshot(
                        "Browser snapshot name has an incomplete escape",
                    ));
                };
                position += 1;
                match escape {
                    '"' => output.push('"'),
                    '\\' => output.push('\\'),
                    '/' => output.push('/'),
                    'b' => output.push('\u{8}'),
                    'f' => output.push('\u{c}'),
                    'n' => output.push('\n'),
                    'r' => output.push('\r'),
                    't' => output.push('\t'),
                    'u' => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let Some((_, digit)) = tokens.get(position).copied() else {
                                return Err(invalid_snapshot(
                                    "Browser snapshot name has an incomplete unicode escape",
                                ));
                            };
                            position += 1;
                            let Some(value) = digit.to_digit(16) else {
                                return Err(invalid_snapshot(
                                    "Browser snapshot name has an invalid unicode escape",
                                ));
                            };
                            code = code.saturating_mul(16).saturating_add(value);
                        }
                        let mut scalar = code;
                        if (0xd800..=0xdbff).contains(&code) {
                            let pair = tokens.get(position..position.saturating_add(6));
                            let mut paired = false;
                            if let Some(pair) = pair
                                && pair[0].1 == '\\'
                                && pair[1].1 == 'u'
                            {
                                let mut low = 0u32;
                                let mut valid = true;
                                for (_, digit) in &pair[2..] {
                                    match digit.to_digit(16) {
                                        Some(value) => {
                                            low = low.saturating_mul(16).saturating_add(value)
                                        }
                                        None => {
                                            valid = false;
                                            break;
                                        }
                                    }
                                }
                                if valid && (0xdc00..=0xdfff).contains(&low) {
                                    scalar = 0x10000u32
                                        .saturating_add((code - 0xd800) << 10)
                                        .saturating_add(low - 0xdc00);
                                    position += 6;
                                    paired = true;
                                }
                            }
                            if !paired {
                                scalar = 0xfffd;
                            }
                        } else if (0xdc00..=0xdfff).contains(&code) {
                            scalar = 0xfffd;
                        }
                        output.push(char::from_u32(scalar).unwrap_or('\u{fffd}'));
                    }
                    _ => {
                        return Err(invalid_snapshot(
                            "Browser snapshot name has an unsupported escape",
                        ));
                    }
                }
            }
            character if character < '\u{20}' => {
                return Err(invalid_snapshot(
                    "Browser snapshot name contains a control character",
                ));
            }
            other => output.push(other),
        }
    }
    Err(invalid_snapshot("Browser snapshot name is unterminated"))
}

/// Split a YAML key from its optional inline text. The serializer emits
/// `role "name" [props...]`, optionally followed by `: text`. Only the props
/// region may contain `[ref=...]`; brackets and quotes in the name are never
/// searched for keys.
fn split_key_and_text(content: &str) -> Result<(String, &str), BrowserFailure> {
    if content.len() > MAX_ARIA_LINE_BYTES {
        return Err(invalid_snapshot("Browser snapshot line is too long"));
    }
    if let Some(rest) = content.strip_prefix('\'') {
        // YAML single-quoted key. A doubled apostrophe is one literal quote.
        let mut key = String::new();
        let mut chars = rest.char_indices();
        while let Some((index, character)) = chars.next() {
            if character == '\'' {
                if let Some((_, '\'')) = chars.clone().next() {
                    chars.next();
                    key.push('\'');
                    continue;
                }
                let remainder = &rest[index + 1..];
                if !remainder.is_empty() && !remainder.starts_with(':') {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an unrecognized line",
                    ));
                }
                return Ok((key, remainder));
            }
            key.push(character);
        }
        return Err(invalid_snapshot("Browser snapshot key is unterminated"));
    }
    let mut in_name = false;
    let mut escaped = false;
    let mut bracket_depth = 0usize;
    for (index, character) in content.char_indices() {
        if in_name {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_name = false;
            }
            continue;
        }
        match character {
            '"' if bracket_depth == 0 => in_name = true,
            '[' => bracket_depth = bracket_depth.saturating_add(1),
            ']' => {
                if bracket_depth == 0 {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an unbalanced property",
                    ));
                }
                bracket_depth -= 1;
            }
            ':' if bracket_depth == 0 => {
                let remainder = &content[index + 1..];
                if !remainder.is_empty() && !remainder.starts_with(' ') {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an unrecognized line",
                    ));
                }
                return Ok((content[..index].to_owned(), remainder));
            }
            _ => {}
        }
    }
    if in_name || bracket_depth != 0 {
        return Err(invalid_snapshot(
            "Browser snapshot contains an unterminated property",
        ));
    }
    Ok((content.to_owned(), ""))
}

fn valid_property_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// Parse one `[key]` or `[key=value]` property and return its remainder.
fn parse_property(input: &str) -> Result<(&str, Option<&str>, &str), BrowserFailure> {
    let bytes = input.as_bytes();
    let mut index = 1usize;
    let mut quote = None;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(active) = quote {
            if byte == b'\\' {
                index = index.saturating_add(2);
                if index > bytes.len() {
                    return Err(invalid_snapshot(
                        "Browser snapshot property has an incomplete escape",
                    ));
                }
                continue;
            }
            if byte == active {
                quote = None;
            }
            index += 1;
            continue;
        }
        match byte {
            b'"' | b'\'' => {
                quote = Some(byte);
                index += 1;
            }
            b'[' => {
                return Err(invalid_snapshot("Browser snapshot property is not flat"));
            }
            b']' => {
                let inner = &input[1..index];
                if inner.is_empty() || inner.bytes().any(|byte| byte.is_ascii_control()) {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an invalid property",
                    ));
                }
                let (name, value) = match inner.split_once('=') {
                    Some((name, value)) => (name, Some(value)),
                    None => (inner, None),
                };
                if !valid_property_name(name) {
                    return Err(invalid_snapshot(
                        "Browser snapshot contains an invalid property",
                    ));
                }
                return Ok((name, value, &input[index + 1..]));
            }
            _ => index += 1,
        }
    }
    Err(invalid_snapshot(
        "Browser snapshot property is unterminated",
    ))
}

#[derive(Debug)]
struct SnapshotLine {
    role: String,
    name: String,
    reference: Option<String>,
    reference_kind: Option<ElementReference>,
    is_iframe: bool,
}

fn parse_element_line(key: &str) -> Result<SnapshotLine, BrowserFailure> {
    if key.is_empty() || key.len() > MAX_ARIA_LINE_BYTES {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element line",
        ));
    }
    let role_end = key.find([' ', '[', ':']).unwrap_or(key.len());
    let role = &key[..role_end];
    if role.is_empty()
        || role.len() > 64
        || !role
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid element role",
        ));
    }
    let mut rest = key[role_end..].trim_start_matches(' ');
    let mut name = String::new();
    if rest.starts_with('"') {
        let (parsed, remainder) = parse_quoted(rest)?;
        name = parsed;
        rest = remainder;
        if !rest.is_empty() && !rest.starts_with([' ', '[', ':']) {
            return Err(invalid_snapshot(
                "Browser snapshot contains an unrecognized name suffix",
            ));
        }
    }
    let mut reference = None;
    let mut reference_kind = None;
    loop {
        rest = rest.trim_start_matches(' ');
        if rest.is_empty() || rest.starts_with(':') {
            break;
        }
        if !rest.starts_with('[') {
            return Err(invalid_snapshot(
                "Browser snapshot contains an unrecognized property",
            ));
        }
        let (property, value, remainder) = parse_property(rest)?;
        rest = remainder;
        if property != "ref" {
            continue;
        }
        if reference.is_some() {
            return Err(invalid_snapshot(
                "Browser snapshot contains duplicate element references",
            ));
        }
        let Some(value) = value else {
            return Err(invalid_snapshot(
                "Browser snapshot contains an empty element reference",
            ));
        };
        reference_kind = Some(parse_element_reference(value)?);
        reference = Some(value.to_owned());
    }
    Ok(SnapshotLine {
        role: role.to_owned(),
        name,
        reference,
        reference_kind,
        is_iframe: role == "iframe",
    })
}

fn validate_property_line(content: &str) -> Result<(), BrowserFailure> {
    let Some((name, _value)) = content[1..].split_once(':') else {
        return Err(invalid_snapshot(
            "Browser snapshot contains an unrecognized property line",
        ));
    };
    if !valid_property_name(name) {
        return Err(invalid_snapshot(
            "Browser snapshot contains an invalid property line",
        ));
    }
    Ok(())
}

fn parse_elements(yaml: &str) -> Result<Vec<BrowserElement>, BrowserFailure> {
    let mut elements = Vec::new();
    let mut seen = BTreeSet::new();
    // (indent, frame label applying to deeper children)
    let mut scopes: Vec<(usize, Option<String>)> = Vec::new();
    let mut unnamed_frames = 0u64;
    for line in yaml.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len().saturating_sub(line.trim_start().len());
        while scopes
            .last()
            .is_some_and(|(child_indent, _)| *child_indent >= indent)
        {
            scopes.pop();
        }
        let Some(content) = line.trim_start().strip_prefix("- ") else {
            return Err(invalid_snapshot(
                "Browser snapshot contains an unrecognized line",
            ));
        };
        if content.starts_with('/') {
            validate_property_line(content)?;
            continue;
        }
        let (key, _text) = split_key_and_text(content)?;
        let parsed = parse_element_line(&key)?;
        // `- text: ...` is the serializer's text node, not an element.
        if parsed.role == "text" {
            continue;
        }
        let parent_frame = scopes.last().and_then(|(_, frame)| frame.clone());
        let (frame, child_frame) = if parsed.is_iframe {
            let child = match &parsed.reference_kind {
                Some(ElementReference::Frame(frame)) => frame.clone(),
                Some(ElementReference::Main) => {
                    format!("frame:{}", parsed.reference.as_deref().unwrap_or("unknown"))
                }
                None => {
                    unnamed_frames += 1;
                    format!("frame@{unnamed_frames}")
                }
            };
            // The iframe node itself is a child-frame boundary, so a credential
            // fill can never target it even at the top level.
            (
                parent_frame.clone().or_else(|| Some(child.clone())),
                Some(child),
            )
        } else {
            (parent_frame.clone(), parent_frame)
        };
        if let Some(reference) = parsed.reference {
            if !seen.insert(reference.clone()) {
                return Err(invalid_snapshot(
                    "Browser snapshot contains duplicate element references",
                ));
            }
            elements.push(BrowserElement {
                key: reference,
                role: parsed.role,
                name: parsed.name,
                frame,
            });
        }
        scopes.push((indent, child_frame));
    }
    Ok(elements)
}

fn parse_action(text: &str, fallback_url: Option<&str>) -> BrowserAction {
    let (url, _) = page_fields(text);
    BrowserAction {
        url: url.or_else(|| fallback_url.map(str::to_owned)),
        detail: result_detail(text),
    }
}

fn parse_snapshot(text: &str, result_truncated: bool) -> Result<BrowserSnapshot, BrowserFailure> {
    let (url, title) = page_fields(text);
    let url = url.ok_or_else(|| {
        BrowserFailure::new(
            "invalid_provider_result",
            "Browser worker snapshot did not include a page URL",
        )
    })?;
    let yaml = snapshot_yaml(text);
    let truncated = result_truncated || yaml.is_none();
    let body = yaml.unwrap_or_default();
    let elements = parse_elements(&body)?;
    Ok(BrowserSnapshot {
        url,
        title,
        elements,
        text: body,
        truncated,
    })
}

struct ActiveWorkspace {
    directories: BrowserSandboxDirectories,
    lease: Option<Arc<dyn BrowserWorkspaceLease>>,
    temp: Option<TempArtifacts>,
}

impl ActiveWorkspace {
    fn temp(artifacts: TempArtifacts) -> Self {
        let directories = artifacts.directories();
        Self {
            directories,
            lease: None,
            temp: Some(artifacts),
        }
    }

    fn prepared(lease: Arc<dyn BrowserWorkspaceLease>) -> Self {
        Self {
            directories: lease.directories().clone(),
            lease: Some(lease),
            temp: None,
        }
    }

    fn execution(&self) -> Option<PreparedEnvironment> {
        self.lease.as_ref().and_then(|lease| lease.execution())
    }

    /// Release the run-private workspace. Callers must reap the worker and
    /// Chromium process group before this; the environment refuses cleanup
    /// while an owned process lease is outstanding.
    async fn release(&mut self) -> Result<(), BrowserFailure> {
        if let Some(lease) = &self.lease {
            lease.release().await.map_err(|_| {
                BrowserFailure::uncertain("Browser sandbox cleanup could not be confirmed")
            })?;
            self.lease = None;
        }
        if let Some(temp) = &mut self.temp {
            temp.remove().map_err(|_| {
                BrowserFailure::uncertain("Browser worker temp cleanup could not be confirmed")
            })?;
            self.temp = None;
        }
        Ok(())
    }
}

struct ProviderInner {
    connection: Option<Arc<dyn WorkerConnection>>,
    generation: u64,
    session: Option<String>,
    page: Option<String>,
    /// Shared so a shutdown future that is dropped by an outer timeout never
    /// takes the only reference away. A later retry still sees the workspace.
    workspace: Option<Arc<AsyncMutex<ActiveWorkspace>>>,
    sandbox_evidence: Option<ChromiumSandboxEvidence>,
    sandbox_unusable: bool,
}

/// Playwright MCP implementation of [`BrowserProvider`].
pub struct PlaywrightMcpProvider {
    label: &'static str,
    identity_verifier: Arc<dyn WorkerIdentityVerifier>,
    launch_planner: Arc<dyn WorkerLaunchPlanner>,
    /// Only the host-local backend can fall back to host temp artifacts when
    /// no prepared environment factory is supplied. Offline tests use this.
    temp_settings: Option<BrowserWorkerSettings>,
    launcher: Arc<dyn WorkerLauncher>,
    environment_factory: Option<Arc<dyn BrowserEnvironmentFactory>>,
    sandbox_inspector: Arc<dyn ChromiumSandboxInspector>,
    /// Bounded host-side network observations for backends that use the
    /// browser broker. Native workers keep this unset.
    network_log: Option<Arc<super::network::BrowserNetworkLog>>,
    inner: AsyncMutex<ProviderInner>,
    call_lock: AsyncMutex<()>,
    shutdown_lock: AsyncMutex<()>,
}

impl std::fmt::Debug for PlaywrightMcpProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlaywrightMcpProvider")
            .field("backend", &self.label)
            .finish_non_exhaustive()
    }
}

impl PlaywrightMcpProvider {
    pub fn new(
        settings: BrowserWorkerSettings,
        launcher: Arc<dyn WorkerLauncher>,
    ) -> Result<Self, BoundaryError> {
        Self::build(settings, launcher, None)
    }

    /// Assemble the production provider. Every worker lifecycle receives a
    /// freshly prepared browser environment from `factory`; the launcher
    /// applies that environment through `PreparedEnvironment::prepare_command`.
    pub fn managed(
        settings: BrowserWorkerSettings,
        factory: Arc<dyn BrowserEnvironmentFactory>,
        config: ExecConfig,
    ) -> Result<Self, BoundaryError> {
        Self::build(
            settings,
            Arc::new(ManagedProcessLauncher::new(config)),
            Some(factory),
        )
    }

    /// Assemble a provider over a backend-supplied identity verifier, launch
    /// planner and launcher while sharing lifecycle, catalogue, tool mapping
    /// and cleanup behavior. The VM browser backend uses this entry point.
    pub fn with_worker_backend(
        label: &'static str,
        identity_verifier: Arc<dyn WorkerIdentityVerifier>,
        launch_planner: Arc<dyn WorkerLaunchPlanner>,
        launcher: Arc<dyn WorkerLauncher>,
        environment_factory: Arc<dyn BrowserEnvironmentFactory>,
        sandbox_inspector: Arc<dyn ChromiumSandboxInspector>,
    ) -> Result<Self, BoundaryError> {
        identity_verifier.preflight()?;
        Ok(Self::assemble(
            label,
            identity_verifier,
            launch_planner,
            None,
            launcher,
            Some(environment_factory),
            sandbox_inspector,
        ))
    }

    fn build(
        settings: BrowserWorkerSettings,
        launcher: Arc<dyn WorkerLauncher>,
        environment_factory: Option<Arc<dyn BrowserEnvironmentFactory>>,
    ) -> Result<Self, BoundaryError> {
        settings.validate()?;
        if !settings.enabled {
            return Err(invalid_config("Browser worker is disabled"));
        }
        let verifier = Arc::new(LocalWorkerIdentityVerifier::new(settings.clone()));
        verifier.preflight()?;
        let planner = Arc::new(LocalWorkerLaunchPlanner::new(settings.clone()));
        Ok(Self::assemble(
            "native",
            verifier,
            planner,
            Some(settings),
            launcher,
            environment_factory,
            Arc::new(ProcfsChromiumSandboxInspector::default()),
        ))
    }

    fn assemble(
        label: &'static str,
        identity_verifier: Arc<dyn WorkerIdentityVerifier>,
        launch_planner: Arc<dyn WorkerLaunchPlanner>,
        temp_settings: Option<BrowserWorkerSettings>,
        launcher: Arc<dyn WorkerLauncher>,
        environment_factory: Option<Arc<dyn BrowserEnvironmentFactory>>,
        sandbox_inspector: Arc<dyn ChromiumSandboxInspector>,
    ) -> Self {
        Self {
            label,
            identity_verifier,
            launch_planner,
            temp_settings,
            launcher,
            environment_factory,
            sandbox_inspector,
            network_log: None,
            inner: AsyncMutex::new(ProviderInner {
                connection: None,
                generation: 0,
                session: None,
                page: None,
                workspace: None,
                sandbox_evidence: None,
                sandbox_unusable: false,
            }),
            call_lock: AsyncMutex::new(()),
            shutdown_lock: AsyncMutex::new(()),
        }
    }

    /// Replace the sandbox verifier. Intended for host integration tests.
    pub fn with_sandbox_inspector(mut self, inspector: Arc<dyn ChromiumSandboxInspector>) -> Self {
        self.sandbox_inspector = inspector;
        self
    }

    /// Attach the bounded observation log used by the VM browser broker.
    pub fn with_network_log(mut self, log: Arc<super::network::BrowserNetworkLog>) -> Self {
        self.network_log = Some(log);
        self
    }

    /// Bounded, path-free network observations recorded for this Run.
    pub fn network_observations(&self) -> Vec<super::network::BrowserNetworkObservation> {
        self.network_log
            .as_ref()
            .map(|log| log.observations())
            .unwrap_or_default()
    }

    /// Actual evidence from the last completed Chromium sandbox verification.
    pub async fn sandbox_evidence(&self) -> Option<ChromiumSandboxEvidence> {
        self.inner.lock().await.sandbox_evidence.clone()
    }

    async fn connected(&self) -> Result<(Arc<dyn WorkerConnection>, u64), BrowserFailure> {
        let inner = self.inner.lock().await;
        if inner.sandbox_unusable {
            return Err(BrowserFailure::uncertain(
                "Browser sandbox verification or cleanup is incomplete; close the browser before reuse",
            ));
        }
        match &inner.connection {
            Some(connection) => Ok((connection.clone(), inner.generation)),
            None => Err(BrowserFailure::new(
                "worker_closed",
                "Browser worker is not running",
            )),
        }
    }

    /// Re-discover the worker catalogue after the worker itself changes it.
    ///
    /// Playwright publishes a new catalogue when it starts the browser, so the
    /// generation approved at launch is stale as soon as `open` creates its
    /// first page. Only lifecycle steps that cause the change call this;
    /// ordinary tool calls keep failing closed on a stale catalogue.
    async fn resync_catalog(
        &self,
        connection: &Arc<dyn WorkerConnection>,
        cancel: &dyn Cancellation,
    ) -> Result<u64, BrowserFailure> {
        let mut last = None;
        for _ in 0..CATALOG_RESYNC_ATTEMPTS {
            match connection.list_tools(cancel).await {
                Ok(catalog) => {
                    validate_catalog(&catalog)?;
                    self.inner.lock().await.generation = catalog.generation;
                    return Ok(catalog.generation);
                }
                Err(failure) if failure.code == "stale_catalog" => last = Some(failure),
                Err(failure) => return Err(failure),
            }
        }
        Err(last.unwrap_or_else(|| {
            BrowserFailure::new(
                "stale_catalog",
                "Browser worker catalogue changed; begin a new session",
            )
        }))
    }

    async fn ensure_launched(
        &self,
        inner: &mut ProviderInner,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        if inner.sandbox_unusable {
            return Err(BrowserFailure::uncertain(
                "Browser worker must finish shutdown before it can be relaunched",
            ));
        }
        if inner.connection.is_some() {
            return Ok(());
        }
        // A remote Run root whose cleanup was not confirmed must be retried
        // before starting another environment. The launcher keeps the handle
        // across launches, so a failure here is never a fresh success.
        self.launcher.retry_pending_cleanup().await.map_err(|_| {
            BrowserFailure::uncertain(
                "Browser worker pending remote cleanup could not be confirmed",
            )
        })?;
        // A previous cleanup failure must be resolved before starting a new
        // environment; a closed environment is never reused.
        if let Some(stale) = inner.workspace.clone() {
            let released = {
                let mut workspace = stale.lock().await;
                workspace.release().await.is_ok()
            };
            if !released {
                return Err(BrowserFailure::uncertain(
                    "Browser worker previous cleanup could not be confirmed",
                ));
            }
            inner.workspace = None;
        }
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker launch cancelled before dispatch",
            ));
        }
        // Recheck the named files immediately before launch so construction-time
        // identity cannot silently drift before the first process dispatch.
        let identity = self.identity_verifier.verify(cancel).await?;
        let workspace = Arc::new(AsyncMutex::new(match &self.environment_factory {
            Some(factory) => {
                let lease = factory
                    .prepare()
                    .map_err(|failure| map_environment_failure(&failure))?;
                ActiveWorkspace::prepared(lease)
            }
            None => {
                let settings = self.temp_settings.as_ref().ok_or_else(|| {
                    BrowserFailure::new(
                        "worker_environment",
                        "Browser worker requires a prepared environment factory",
                    )
                })?;
                ActiveWorkspace::temp(TempArtifacts::create(settings).map_err(|_| {
                    BrowserFailure::new("worker_environment", "Browser worker temp setup failed")
                })?)
            }
        }));
        // Register the workspace before any await so cancelling the launch
        // future cannot drop the only reference to an unreleased environment.
        inner.workspace = Some(workspace.clone());
        let directories = workspace.lock().await.directories.clone();
        let mut request = match self.launch_planner.build(&identity, &directories) {
            Ok(request) => request,
            Err(_) => {
                return Self::discard_workspace(
                    inner,
                    workspace,
                    BrowserFailure::new(
                        "worker_environment",
                        "Browser worker launch input is invalid",
                    ),
                )
                .await;
            }
        };
        request.execution = workspace.lock().await.execution();
        if cancel.is_cancelled() {
            return Self::discard_workspace(
                inner,
                workspace,
                BrowserFailure::new(
                    "cancelled",
                    "Browser worker launch cancelled before dispatch",
                ),
            )
            .await;
        }
        let connection = match self.launcher.launch(request, cancel).await {
            Ok(connection) => connection,
            Err(failure) => {
                return Self::discard_workspace(inner, workspace, failure).await;
            }
        };
        // Catalogue awaits must retain cleanup ownership if this future is dropped.
        inner.connection = Some(connection.clone());
        inner.sandbox_evidence = None;
        inner.sandbox_unusable = true;
        if connection.protocol_version().as_deref() != Some(WORKER_PROTOCOL_VERSION) {
            return self
                .discard_launch(
                    inner,
                    connection,
                    workspace,
                    BrowserFailure::new(
                        "worker_protocol",
                        "Browser worker protocol version is not supported",
                    ),
                )
                .await;
        }
        let catalog = match connection.list_tools(cancel).await {
            Ok(catalog) => catalog,
            Err(failure) => {
                return self
                    .discard_launch(inner, connection, workspace, failure)
                    .await;
            }
        };
        if let Err(failure) = validate_catalog(&catalog) {
            return self
                .discard_launch(inner, connection, workspace, failure)
                .await;
        }
        inner.connection = Some(connection);
        inner.generation = catalog.generation;
        inner.workspace = Some(workspace);
        Ok(())
    }

    async fn discard_workspace(
        inner: &mut ProviderInner,
        workspace: Arc<AsyncMutex<ActiveWorkspace>>,
        failure: BrowserFailure,
    ) -> Result<(), BrowserFailure> {
        // No worker process was accepted, so there is nothing to reap; the
        // prepared environment still has to release its scratch directory.
        let released = {
            let mut workspace = workspace.lock().await;
            workspace.release().await.is_ok()
        };
        if released {
            if inner
                .workspace
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &workspace))
            {
                inner.workspace = None;
            }
            return Err(failure);
        }
        inner.workspace = Some(workspace);
        Err(BrowserFailure::uncertain(
            "Browser worker startup cleanup could not be confirmed",
        ))
    }

    async fn discard_launch(
        &self,
        inner: &mut ProviderInner,
        connection: Arc<dyn WorkerConnection>,
        workspace: Arc<AsyncMutex<ActiveWorkspace>>,
        failure: BrowserFailure,
    ) -> Result<(), BrowserFailure> {
        // Reap the worker and Chromium group before releasing the environment.
        if connection.shutdown().await.is_err() {
            return Err(BrowserFailure::uncertain(
                "Browser worker startup cleanup could not be confirmed",
            ));
        }
        inner.connection = None;
        inner.generation = 0;
        let released = {
            let mut workspace = workspace.lock().await;
            workspace.release().await.is_ok()
        };
        if !released {
            // Keep the workspace for a retry; the connection was already
            // reaped, so a later shutdown only has to finish cleanup.
            inner.workspace = Some(workspace);
            return Err(BrowserFailure::uncertain(
                "Browser worker startup cleanup could not be confirmed",
            ));
        }
        if inner
            .workspace
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &workspace))
        {
            inner.workspace = None;
        }
        inner.sandbox_unusable = false;
        Err(failure)
    }

    async fn check_page(&self, session: &str, page: &str) -> Result<(), BrowserFailure> {
        let inner = self.inner.lock().await;
        if inner.session.as_deref() != Some(session) || inner.page.as_deref() != Some(page) {
            return Err(BrowserFailure::new(
                "invalid_provider_result",
                "Browser provider session or page is unknown",
            ));
        }
        Ok(())
    }

    async fn call(
        &self,
        name: &str,
        arguments: Value,
        cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure> {
        let _guard = self.call_lock.lock().await;
        let (connection, generation) = self.connected().await?;
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker call cancelled before dispatch",
            ));
        }
        let result = connection
            .call_tool(name, arguments, generation, cancel)
            .await?;
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker reported a tool error. The call was not replayed",
            ));
        }
        if let Err(failure) = self.verify_sandbox(&connection, cancel).await {
            if self.shutdown_resources().await.is_err() {
                return Err(BrowserFailure::uncertain(
                    "Browser sandbox verification failed and worker cleanup could not be confirmed",
                ));
            }
            return Err(failure);
        }
        Ok(result)
    }

    async fn verify_sandbox(
        &self,
        connection: &Arc<dyn WorkerConnection>,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        {
            let mut inner = self.inner.lock().await;
            inner.sandbox_evidence = None;
            // Dropping this future must not make an unverified lifecycle reusable.
            inner.sandbox_unusable = true;
        }
        let Some(pid) = connection.process_id() else {
            self.inner.lock().await.sandbox_unusable = false;
            return Ok(());
        };
        let stderr = connection.stderr_tail();
        let evidence = self.sandbox_inspector.inspect(pid, &stderr, cancel).await?;
        tracing::info!(
            target: "saya::browser",
            event = "browser.sandbox_verified",
            renderer_pid = evidence.renderer_pid,
            no_new_privs = evidence.no_new_privs,
            seccomp_mode = evidence.seccomp_mode,
            sandbox_arguments_valid = evidence.sandbox_arguments_valid,
            stderr_initializes_sandbox = evidence.stderr_initializes_sandbox
        );
        let mut inner = self.inner.lock().await;
        inner.sandbox_evidence = Some(evidence);
        inner.sandbox_unusable = false;
        Ok(())
    }

    /// Serialized cleanup shared by `shutdown` and sandbox-verification
    /// failure handling. Callers must hold `call_lock`, or have otherwise
    /// stopped dispatch, before invoking this.
    async fn shutdown_resources(&self) -> Result<(), BrowserFailure> {
        // Clone the shared handles instead of taking them. An outer timeout
        // may drop this future mid-await; the provider must still hold the
        // only connection/workspace so a retry can continue and cannot
        // report a second false success.
        let (connection, workspace) = {
            let mut inner = self.inner.lock().await;
            inner.sandbox_evidence = None;
            inner.sandbox_unusable = true;
            (inner.connection.clone(), inner.workspace.clone())
        };
        if connection.is_none() && workspace.is_none() {
            self.launcher.retry_pending_cleanup().await.map_err(|_| {
                BrowserFailure::uncertain(
                    "Browser worker pending remote cleanup could not be confirmed",
                )
            })?;
            let mut inner = self.inner.lock().await;
            inner.session = None;
            inner.page = None;
            inner.sandbox_evidence = None;
            inner.sandbox_unusable = false;
            return Ok(());
        }
        let mut process_failed = false;
        if let Some(connection) = &connection
            && connection.shutdown().await.is_err()
        {
            // Keep the private directories until process termination can be
            // confirmed on a retry.
            process_failed = true;
        }
        let mut cleanup_failed = false;
        if !process_failed && let Some(workspace) = &workspace {
            let mut workspace = workspace.lock().await;
            cleanup_failed = workspace.release().await.is_err();
        }
        if process_failed || cleanup_failed {
            return Err(BrowserFailure::uncertain(
                "Browser worker shutdown could not be confirmed",
            ));
        }
        {
            let mut inner = self.inner.lock().await;
            if let Some(connection) = &connection
                && inner
                    .connection
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, connection))
            {
                inner.connection = None;
            }
            if let Some(workspace) = &workspace
                && inner
                    .workspace
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, workspace))
            {
                inner.workspace = None;
            }
            inner.generation = 0;
            inner.session = None;
            inner.page = None;
            inner.sandbox_evidence = None;
            inner.sandbox_unusable = false;
        }
        self.launcher.retry_pending_cleanup().await.map_err(|_| {
            BrowserFailure::uncertain(
                "Browser worker pending remote cleanup could not be confirmed",
            )
        })?;
        Ok(())
    }
}

#[async_trait]
impl BrowserProvider for PlaywrightMcpProvider {
    fn facts(&self) -> BrowserFacts {
        BrowserFacts {
            provider: format!("playwright-mcp-{PLAYWRIGHT_MCP_VERSION}"),
            // The pinned schema has no `mask` input, so the worker cannot
            // perform the required pre-capture masking. The host refuses the
            // capability instead of returning an unmasked screenshot.
            screenshots: false,
            screenshot_masking: false,
            downloads: true,
            uploads: true,
        }
    }

    async fn open(
        &self,
        session: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserOpened, BrowserFailure> {
        let _guard = self.call_lock.lock().await;
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker open cancelled before dispatch",
            ));
        }
        {
            let inner = self.inner.lock().await;
            if inner.session.is_some() || inner.page.is_some() {
                return Err(BrowserFailure::new(
                    "browser_limit",
                    "This browser worker supports one page at a time",
                ));
            }
            if session.is_some() {
                return Err(BrowserFailure::new(
                    "invalid_provider_result",
                    "Browser provider has no session to resume",
                ));
            }
        }
        let (connection, generation) = {
            let mut inner = self.inner.lock().await;
            self.ensure_launched(&mut inner, cancel).await?;
            match &inner.connection {
                Some(connection) => (connection.clone(), inner.generation),
                None => {
                    return Err(BrowserFailure::new(
                        "worker_closed",
                        "Browser worker is not running",
                    ));
                }
            }
        };
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker open cancelled before dispatch",
            ));
        }
        let result = match connection
            .call_tool(
                "browser_navigate",
                json!({"url": "about:blank"}),
                generation,
                cancel,
            )
            .await
        {
            Ok(result) => result,
            Err(failure) if failure.code == "stale_catalog" => {
                // The stale check runs before dispatch, so nothing was sent.
                let generation = self.resync_catalog(&connection, cancel).await?;
                connection
                    .call_tool(
                        "browser_navigate",
                        json!({"url": "about:blank"}),
                        generation,
                        cancel,
                    )
                    .await?
            }
            Err(failure) => return Err(failure),
        };
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker could not create its initial page",
            ));
        }
        // Starting the browser publishes the worker's post-launch catalogue;
        // capture that generation so the page lifecycle stays approved.
        if let Err(failure) = self.resync_catalog(&connection, cancel).await {
            if self.shutdown_resources().await.is_err() {
                return Err(BrowserFailure::uncertain(
                    "Browser catalogue verification failed and worker cleanup could not be confirmed",
                ));
            }
            return Err(failure);
        }
        let text = first_text(&result).unwrap_or_default();
        let url = page_fields(text).0.unwrap_or_else(|| "about:blank".into());
        if let Err(failure) = self.verify_sandbox(&connection, cancel).await {
            if self.shutdown_resources().await.is_err() {
                return Err(BrowserFailure::uncertain(
                    "Browser sandbox verification failed and worker cleanup could not be confirmed",
                ));
            }
            return Err(failure);
        }
        let mut inner = self.inner.lock().await;
        inner.session = Some(WORKER_SESSION.into());
        inner.page = Some(WORKER_PAGE.into());
        Ok(BrowserOpened {
            session: WORKER_SESSION.into(),
            page: WORKER_PAGE.into(),
            url: Some(url),
        })
    }

    async fn close(&self, session: &str, cancel: &dyn Cancellation) -> Result<(), BrowserFailure> {
        let _guard = self.call_lock.lock().await;
        let sandbox_unusable = {
            let inner = self.inner.lock().await;
            if inner.session.is_none() && inner.page.is_none() {
                if !inner.sandbox_unusable {
                    return Ok(());
                }
            } else if inner.session.as_deref() != Some(session) {
                return Err(BrowserFailure::new(
                    "invalid_provider_result",
                    "Browser provider session is unknown",
                ));
            }
            inner.sandbox_unusable
        };
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker close cancelled before dispatch",
            ));
        }
        if sandbox_unusable {
            return self.shutdown_resources().await;
        }
        let (connection, _) = self.connected().await?;
        // `browser_close` is the lifecycle counterpart of `open`'s initial
        // navigation: re-discover first so a catalogue update published by the
        // launch cannot leave the close call stale.
        let mut generation = self.resync_catalog(&connection, cancel).await?;
        let mut result = None;
        for attempt in 0..CATALOG_RESYNC_ATTEMPTS {
            match connection
                .call_tool("browser_close", json!({}), generation, cancel)
                .await
            {
                Ok(value) => {
                    result = Some(value);
                    break;
                }
                Err(failure)
                    if failure.code == "stale_catalog" && attempt + 1 < CATALOG_RESYNC_ATTEMPTS =>
                {
                    generation = self.resync_catalog(&connection, cancel).await?;
                }
                Err(failure) => return Err(failure),
            }
        }
        let result = result.ok_or_else(|| {
            BrowserFailure::new(
                "stale_catalog",
                "Browser worker catalogue changed; begin a new session",
            )
        })?;
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker could not close its page",
            ));
        }
        let mut inner = self.inner.lock().await;
        inner.session = None;
        inner.page = None;
        inner.sandbox_evidence = None;
        Ok(())
    }

    async fn navigate(
        &self,
        session: &str,
        page: &str,
        url: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self
            .call("browser_navigate", json!({"url": url}), cancel)
            .await?;
        let action = parse_action(first_text(&result)?, Some(url));
        Ok(action)
    }

    async fn snapshot(
        &self,
        session: &str,
        page: &str,
        _max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserSnapshot, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self.call("browser_snapshot", json!({}), cancel).await?;
        parse_snapshot(first_text(&result)?, result.truncated)
    }

    async fn click(
        &self,
        session: &str,
        page: &str,
        element: &str,
        button: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let mut arguments = json!({"target": element});
        if let Some(button) = button {
            arguments["button"] = json!(button);
        }
        let result = self.call("browser_click", arguments, cancel).await?;
        Ok(parse_action(first_text(&result)?, None))
    }

    async fn fill(
        &self,
        session: &str,
        page: &str,
        element: &str,
        text: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self
            .call(
                "browser_type",
                json!({"target": element, "text": text}),
                cancel,
            )
            .await?;
        Ok(parse_action(first_text(&result)?, None))
    }

    async fn press(
        &self,
        session: &str,
        page: &str,
        key: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self
            .call("browser_press_key", json!({"key": key}), cancel)
            .await?;
        Ok(parse_action(first_text(&result)?, None))
    }

    async fn screenshot(
        &self,
        _session: &str,
        _page: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<super::BrowserScreenshot, BrowserFailure> {
        // `browser_take_screenshot` in the pinned worker has no mask input.
        // Returning bytes here would expose passwords and one-time codes, so
        // the provider reports an explicit disabled capability.
        Err(BrowserFailure::new(
            "browser_capability_disabled",
            "The pinned Playwright MCP worker cannot mask sensitive fields before capture",
        ))
    }
    async fn download(
        &self,
        session: &str,
        page: &str,
        element: &str,
        cancel: &dyn Cancellation,
    ) -> Result<super::BrowserDownload, BrowserFailure> {
        self.check_page(session, page).await?;
        let (connection, _) = self.connected().await?;
        // The download directory is the structural proof of which file this
        // click produced. No generic "last download" lookup is allowed.
        let before = connection.list_downloads(cancel).await?;
        // The click is the only action allowed to produce an accepted
        // download. Nothing is looked up from a previous action.
        let result = self
            .call("browser_click", json!({"target": element}), cancel)
            .await?;
        let mut text = first_text(&result)?.to_owned();
        let mut event = download_path(&text)?;
        let mut selected: Option<(String, String)> = None;
        for attempt in 0..20 {
            let after = connection.list_downloads(cancel).await?;
            let created: Vec<String> = after.difference(&before).cloned().collect();
            if created.len() > 1 {
                return Err(BrowserFailure::new(
                    "invalid_provider_result",
                    "Browser action created more than one new download file",
                ));
            }
            if let Some(name) = created.first() {
                if let Some(event_path) = &event {
                    if event_path.contains("..") {
                        return Err(BrowserFailure::new(
                            "file_path_denied",
                            "Browser download path contains traversal components",
                        ));
                    }
                    let event_name = event_path
                        .rsplit(['/', '\\'])
                        .next()
                        .filter(|event_name| !event_name.is_empty())
                        .ok_or_else(|| {
                            BrowserFailure::new(
                                "invalid_provider_result",
                                "Browser download path is invalid",
                            )
                        })?;
                    if event_name != name {
                        return Err(BrowserFailure::new(
                            "invalid_provider_result",
                            "Browser download event does not match the new file",
                        ));
                    }
                }
                selected = Some((name.clone(), name.clone()));
                break;
            }
            if attempt == 19 {
                break;
            }
            // A download may finish just after the click result; poll both the
            // trusted event section and the provider's output directory.
            let waited = self
                .call("browser_wait_for", json!({"time": 0.1}), cancel)
                .await?;
            text.push('\n');
            text.push_str(first_text(&waited).unwrap_or_default());
            event = download_path(&text)?;
        }
        let Some((filename, target)) = selected else {
            return Err(BrowserFailure::new(
                "download_missing",
                "The element action did not create exactly one new download file",
            ));
        };
        let bytes = connection
            .read_download(&target, PROVIDER_MAX_DOWNLOAD_BYTES, cancel)
            .await?;
        Ok(super::BrowserDownload {
            filename,
            // The host re-detects the real type from the magic bytes.
            mime: "application/octet-stream".into(),
            bytes,
        })
    }

    async fn upload(
        &self,
        session: &str,
        page: &str,
        element: &str,
        source: &super::BrowserUploadSource,
        cancel: &dyn Cancellation,
    ) -> Result<super::BrowserUpload, BrowserFailure> {
        self.check_page(session, page).await?;
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let mut file = options.open(&source.path).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser upload staging file is unavailable")
        })?;
        let metadata = file.metadata().map_err(|_| {
            BrowserFailure::new(
                "file_broker",
                "Browser upload staging file cannot be inspected",
            )
        })?;
        if !metadata.is_file() || metadata.len() != source.size {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser upload staging file changed",
            ));
        }
        let mut bytes = Vec::with_capacity(source.size as usize);
        std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser upload staging file cannot be read")
        })?;
        if bytes.len() as u64 != source.size || sha256_hex(&bytes) != source.sha256 {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser upload staging content changed",
            ));
        }
        // Open the page's file chooser with the explicit document element.
        self.call("browser_click", json!({"target": element}), cancel)
            .await?;
        let (connection, _) = self.connected().await?;
        let remote = connection
            .put_upload(&source.filename, &bytes, cancel)
            .await?;
        // If cancellation drops this future between staging and cleanup, the
        // guard still removes the Run-private staging directory.
        let mut guard = RemoteUploadGuard {
            connection: Some(connection.clone()),
            path: remote.clone(),
        };
        let result = self
            .call(
                "browser_file_upload",
                json!({"paths": [remote.clone()]}),
                cancel,
            )
            .await;
        let cleanup = connection.remove_upload(&remote, cancel).await;
        if cleanup.is_ok() {
            guard.connection = None;
        }
        let result = result?;
        // The file chooser already consumed the staged file. If cleanup cannot
        // be confirmed after a successful upload, the operation must not be
        // retried as an ordinary failure: the page may already hold the file.
        cleanup.map_err(|_| {
            BrowserFailure::uncertain(
                "Browser upload completed but staging cleanup could not be confirmed",
            )
        })?;
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker could not upload the file",
            ));
        }
        Ok(super::BrowserUpload {
            filename: source.filename.clone(),
            bytes_sent: source.size,
        })
    }

    async fn wait(
        &self,
        session: &str,
        page: &str,
        milliseconds: u64,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        if milliseconds == 0 {
            return Ok(BrowserAction {
                url: None,
                detail: None,
            });
        }
        let mut remaining = milliseconds;
        let mut detail = None;
        loop {
            let chunk = remaining.min(30_000);
            let seconds = chunk as f64 / 1000.0;
            let result = self
                .call("browser_wait_for", json!({"time": seconds}), cancel)
                .await?;
            if let Some(line) = result_detail(first_text(&result).unwrap_or_default()) {
                detail = Some(line);
            }
            remaining -= chunk;
            if remaining == 0 {
                break;
            }
        }
        Ok(BrowserAction { url: None, detail })
    }

    async fn shutdown(&self, _cancel: &dyn Cancellation) -> Result<(), BrowserFailure> {
        let _serial = self.shutdown_lock.lock().await;
        let _call = self.call_lock.lock().await;
        self.shutdown_resources().await
    }
}

#[cfg(test)]
mod tests;
