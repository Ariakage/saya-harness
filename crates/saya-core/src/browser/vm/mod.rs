//! Dedicated-VM browser worker backend.
//!
//! macOS Seatbelt cannot keep an outer sandbox while Chromium reinitializes
//! its own sandbox, and the Linux Bubblewrap profile used by the native
//! browser backend cannot satisfy Chromium's inner namespace setup. This module
//! keeps both sandboxes intact by running the pinned Playwright worker and
//! Chromium inside an explicitly provisioned Colima VM while the host only
//! runs a strictly confined `ssh` client.
//!
//! The VM is a host-provisioned input. Nothing here creates, installs or
//! updates it. The host SSH process is prepared through the existing
//! `PreparedEnvironment`/`ManagedProcess` boundary and may reach only the VM's
//! loopback SSH port. Its writable scope is a run-private directory; the SSH
//! identity, known-hosts file and binary are exposed read-only.
//!
//! VM network access for page traffic is not provided in this increment. The
//! backend never treats the VM's default egress as a controlled browser exit.

#[allow(unused_imports)]
pub(in crate::browser::vm) use crate::browser::{
    BrowserFailure,
    network::{
        BrowserNetwork, BrowserNetworkLog, BrowserNetworkSettings, MAX_NETWORK_OBSERVATIONS,
    },
    worker::{
        self, BrowserEnvironmentFactory, BrowserWorkspaceLease, CHROMIUM_REVISION,
        CHROMIUM_VERSION, ChromiumSandboxEvidence, ChromiumSandboxInspector, IdentityManifest,
        MAX_IDENTITY_FILE_BYTES, MAX_MANIFEST_BYTES, PLAYWRIGHT_MCP_VERSION, PLAYWRIGHT_VERSION,
        VerifiedWorkerIdentity, WorkerConnection, WorkerIdentityVerifier, WorkerLaunchPlanner,
        WorkerLaunchRequest, WorkerLauncher, parse_browsers, parse_package, sha256_hex,
        valid_sha256,
    },
};
#[allow(unused_imports)]
pub(in crate::browser::vm) use crate::{
    boundary::Cancellation,
    contract::{BoundaryError, ErrorInfo},
    execution::{
        self, BrowserSandboxDirectories, NetworkPolicy, PreparedEnvironment, SandboxPolicy,
    },
    mcp::{McpCatalog, McpLimits, McpToolResult},
    network::RunNetworkBudget,
    tools::exec::{ExecConfig, ExecPlan, ManagedProcess},
};
#[allow(unused_imports)]
pub(in crate::browser::vm) use async_trait::async_trait;
#[allow(unused_imports)]
pub(in crate::browser::vm) use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
pub(in crate::browser::vm) use serde_json::Value;
#[allow(unused_imports)]
pub(in crate::browser::vm) use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::{OsStr, OsString},
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
#[allow(unused_imports)]
pub(in crate::browser::vm) use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
};

mod launcher;
mod settings;
mod ssh;
#[cfg(test)]
mod tests;
mod verifier;

pub use launcher::{
    BrowserNetworkFactory, VmBrowserEnvironmentFactory, VmRemoteControl, VmWorkerLauncher,
    assemble_vm_browser_provider,
};
#[allow(unused_imports)]
pub(in crate::browser::vm) use launcher::{
    ConnectionCleanup, NeverCancel, PendingCleanup, RunnerRemoteControl, UPLOAD_SCRIPT,
    boundary_to_browser_failure, classify_launch_failure, parse_pid, schedule_drop_cleanup,
    worker_launch_arguments,
};
pub use settings::{BrowserVmBackend, BrowserVmSettings, VmLocalPaths};
#[allow(unused_imports)]
pub(in crate::browser::vm) use settings::{
    RemotePaths, identity_failure, invalid_config, normalized_loopback_host,
    remote_cleanup_failure, remote_failure, safe_token, validate_ssh_paths, vm_unavailable,
};
#[allow(unused_imports)]
pub(in crate::browser::vm) use ssh::{
    CAT_SCRIPT, CLEANUP_SCRIPT, INSTALLATION_PROBE, INSTALLATION_PROBE_SCRIPT, LAUNCH_SCRIPT,
    NODE_VERSION_SCRIPT, PID_SCRIPT, PREPARE_RUN_SCRIPT, PROBE_SCRIPT, ReverseForward,
    SANDBOX_SCRIPT, VmHostEnvironment, host_policy, open_directory, ssh_argv, strict_ssh_config,
    vm_command_outcome, vm_command_result, write_private,
};
pub use ssh::{SshVmCommandRunner, VmCommandOutput, VmCommandRunner, VmCommandSpec};
pub use verifier::{VmChromiumSandboxInspector, VmWorkerIdentityVerifier, VmWorkerLaunchPlanner};
#[allow(unused_imports)]
pub(in crate::browser::vm) use verifier::{headless_shell_path, worker_config_json};
