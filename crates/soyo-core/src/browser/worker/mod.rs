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

pub(in crate::browser::worker) use super::{
    BrowserAction, BrowserElement, BrowserFacts, BrowserFailure, BrowserOpened, BrowserProvider,
    BrowserSnapshot,
};
pub(in crate::browser::worker) use crate::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo},
    execution::{
        BackendKind, BrowserSandboxDirectories, BrowserSandboxProfile, PreparedEnvironment,
        SandboxPolicy,
    },
    mcp::{McpCatalog, McpLimits, McpToolResult},
    tools::exec::{ExecConfig, ManagedProcess},
};
pub(in crate::browser::worker) use async_trait::async_trait;
pub(in crate::browser::worker) use base64::{Engine as _, engine::general_purpose::STANDARD};
pub(in crate::browser::worker) use cap_std::fs::{
    Dir, OpenOptions as CapOpenOptions, OpenOptionsExt as CapOpenOptionsExt,
};
pub(in crate::browser::worker) use serde::{Deserialize, Serialize};
pub(in crate::browser::worker) use serde_json::{Value, json};
pub(in crate::browser::worker) use sha2::{Digest, Sha256};
#[cfg(target_os = "linux")]
pub(in crate::browser::worker) use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
pub(in crate::browser::worker) use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsString,
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
pub(in crate::browser::worker) use tokio::{io::AsyncReadExt, sync::Mutex as AsyncMutex};

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
pub(super) const MAX_WORKER_RESULT_BYTES: usize = 4 * 1024 * 1024;
pub(crate) const MAX_WORKER_SCREENSHOT_WIRE_BYTES: usize = 13 * 1024 * 1024;
pub(super) const MAX_SCREENSHOT_BASE64_BYTES: usize = 12 * 1024 * 1024;
pub(super) const MAX_SCREENSHOT_DIMENSION: u32 = 4096;
pub(super) const WORKER_SESSION: &str = "worker_session";
pub(super) const WORKER_PAGE: &str = "worker_page";
/// Bounded re-discovery attempts for a lifecycle operation that races the
/// worker's own catalogue update. The request is never dispatched while stale.
pub(super) const CATALOG_RESYNC_ATTEMPTS: usize = 3;
/// Provider-side bound for a single download read. BrowserState enforces the
/// configured lower limit before storing the artifact.
pub(super) const PROVIDER_MAX_DOWNLOAD_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn error(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

pub(super) fn identity_error(message: &'static str) -> BoundaryError {
    error("worker_identity", message)
}

pub(super) fn invalid_config(message: &'static str) -> BoundaryError {
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

mod environment;
mod identity;
mod launcher;
mod protocol;
mod provider;
mod sandbox;
mod settings;
#[cfg(test)]
mod tests;

pub use environment::{
    BrowserEnvironmentFactory, BrowserWorkspaceLease, LocalWorkerIdentityVerifier,
    LocalWorkerLaunchPlanner, NativeBrowserEnvironmentFactory, WorkerConnection,
    WorkerIdentityVerifier, WorkerLaunchPlanner, WorkerLaunchRequest, WorkerLauncher,
    WorkerPidNamespace, WorkerProcessInfo,
};
pub(in crate::browser::worker) use environment::{
    RemoteUploadGuard, TempArtifacts, map_environment_failure, map_mcp_failure,
    parse_captured_screenshot, stage_worker_upload,
};
pub(crate) use environment::{
    capture_screenshot_failure, environment_digest, process_info_failure,
};
#[cfg(test)]
pub(in crate::browser::worker) use identity::{ChromiumIdentity, FileIdentity, VersionedIdentity};
pub(crate) use identity::{IdentityManifest, VerifiedLocalImages, parse_browsers, parse_package};
pub(in crate::browser::worker) use identity::{
    PinnedWorkerImage, open_directory, verify_worker_identity_checked,
};
pub use identity::{VerifiedWorkerIdentity, verify_worker_identity};
pub use launcher::ManagedProcessLauncher;
#[cfg(test)]
pub(in crate::browser::worker) use launcher::RealWorkerConnection;
pub(in crate::browser::worker) use protocol::{
    download_path, first_text, page_fields, parse_action, parse_snapshot, result_detail,
    validate_catalog, worker_metadata_file,
};
#[cfg(test)]
pub(in crate::browser::worker) use protocol::{expected_worker_schemas, parse_elements};
pub use provider::PlaywrightMcpProvider;
pub use sandbox::{
    ChromiumSandboxEvidence, ChromiumSandboxInspector, NativeSandboxContext,
    ProcfsChromiumSandboxInspector,
};
#[cfg(test)]
pub(in crate::browser::worker) use sandbox::{
    KernelNamespaceId, KernelProcStatus, MAX_SANDBOX_PROC_FILE_BYTES, NativeKernelProbe,
    VerifiedNamespace, parse_cmdline_tokens, parse_proc_stat_starttime,
    read_bounded_proc_file_strict,
};
// The launch baseline probe runs only on Linux; the probe tests exercise the
// same types through a fake kernel on every platform.
#[cfg(target_os = "linux")]
pub(in crate::browser::worker) use identity::open_absolute_file;
#[cfg(any(test, target_os = "linux"))]
pub(in crate::browser::worker) use sandbox::{
    HostProcessPin, KernelNamespaceKind, NodeLaunchBaseline, SandboxProbeFailure,
};
pub(in crate::browser::worker) use sandbox::{MAX_SANDBOX_PROCESSES, NativeLaunchBaseline};
#[cfg(target_os = "linux")]
pub(in crate::browser::worker) use sandbox::{
    RealKernelProbe, capture_linux_process_pin, check_linux_process_pin, descendant_pids_strict,
    linux_pid_ns_owner_user_ns, open_linux_namespace, read_proc_status_file,
    verify_linux_process_exe, verify_namespace_descends,
};
pub use settings::{BrowserWorkerSettings, browser_install_roots};
