//! Real dedicated-VM browser worker integration entry point.
//!
//! Ignored by default. Run it explicitly on a macOS host with a provisioned
//! Colima profile created by `scripts/n5/browser_vm.py`:
//!
//! ```text
//! SAYA_TEST_VM_COLIMA=/opt/homebrew/bin/colima \
//! SAYA_TEST_VM_SSH=/usr/bin/ssh \
//! SAYA_TEST_VM_PROFILE=saya-browser \
//! SAYA_TEST_VM_HOST=127.0.0.1 \
//! SAYA_TEST_VM_USER=saya \
//! SAYA_TEST_VM_PORT=2222 \
//! SAYA_TEST_VM_IDENTITY_FILE=/path/to/id_ed25519 \
//! SAYA_TEST_VM_KNOWN_HOSTS=/path/to/known_hosts \
//! SAYA_TEST_VM_NODE=/opt/saya/node/bin/node \
//! SAYA_TEST_VM_WORKER_ROOT=/opt/saya/worker \
//! SAYA_TEST_VM_CHROMIUM=/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell \
//! SAYA_TEST_VM_MANIFEST=/opt/saya/identity.json \
//! SAYA_TEST_VM_MANIFEST_SHA256=<64-hex> \
//! SAYA_TEST_VM_TEMP_ROOT=/tmp/saya-runs \
//! cargo test -p saya-core --test browser_vm_real -- --ignored --nocapture
//! ```
//!
//! These values are test inputs, never product defaults.

use saya_core::{
    boundary::Cancellation,
    browser::{BrowserProvider, vm::BrowserVmSettings},
    tools::exec::ExecConfig,
};
use std::{path::PathBuf, sync::Arc};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn path(name: &str) -> Option<PathBuf> {
    env(name).map(PathBuf::from)
}

#[tokio::test]
#[ignore = "requires a provisioned Colima browser VM and SAYA_TEST_VM_* inputs"]
async fn real_vm_worker_verifies_chromium_sandbox_inside_the_vm() {
    if !cfg!(target_os = "macos") {
        eprintln!("real VM integration requires macOS; test not run");
        return;
    }
    let settings = BrowserVmSettings {
        enabled: true,
        colima: path("SAYA_TEST_VM_COLIMA"),
        ssh: path("SAYA_TEST_VM_SSH"),
        profile: env("SAYA_TEST_VM_PROFILE"),
        host: env("SAYA_TEST_VM_HOST"),
        user: env("SAYA_TEST_VM_USER"),
        port: env("SAYA_TEST_VM_PORT").and_then(|port| port.parse().ok()),
        identity_file: path("SAYA_TEST_VM_IDENTITY_FILE"),
        known_hosts: path("SAYA_TEST_VM_KNOWN_HOSTS"),
        node: env("SAYA_TEST_VM_NODE"),
        worker_root: env("SAYA_TEST_VM_WORKER_ROOT"),
        chromium: env("SAYA_TEST_VM_CHROMIUM"),
        identity_manifest: env("SAYA_TEST_VM_MANIFEST"),
        identity_sha256: env("SAYA_TEST_VM_MANIFEST_SHA256"),
        remote_temp_root: env("SAYA_TEST_VM_TEMP_ROOT"),
        ..Default::default()
    };
    if settings.ssh.is_none() || settings.identity_sha256.is_none() {
        eprintln!("SAYA_TEST_VM_* environment is not configured; test not run");
        return;
    }
    let launcher = env("SAYA_TEST_EXECUTABLE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("target/release/saya"));
    let launcher = std::fs::canonicalize(launcher).expect("real Saya launcher path");
    saya_core::execution::pin_launcher_image_from(&launcher)
        .expect("pin the real Saya CLI instead of the libtest executable");
    let provider = Arc::new(
        saya_core::browser::vm::assemble_vm_browser_provider(settings, ExecConfig::default())
            .expect("VM provider assembly"),
    );
    let opened = provider
        .open(None, &NeverCancel)
        .await
        .expect("VM worker open");
    let evidence = provider
        .sandbox_evidence()
        .await
        .expect("Chromium sandbox evidence from inside the VM");
    assert!(evidence.no_new_privs, "VM renderer NoNewPrivs is not 1");
    assert_eq!(
        evidence.seccomp_mode, 2,
        "VM renderer Seccomp is not filter mode"
    );
    assert!(
        evidence.sandbox_arguments_valid,
        "VM renderer command line disables its sandbox"
    );
    eprintln!(
        "vm renderer_pid={} no_new_privs={} seccomp={} stderr_initialize_sandbox={}",
        evidence.renderer_pid,
        evidence.no_new_privs,
        evidence.seccomp_mode,
        evidence.stderr_initializes_sandbox
    );
    provider
        .close(&opened.session, &NeverCancel)
        .await
        .expect("VM worker close");
    provider
        .shutdown(&NeverCancel)
        .await
        .expect("VM worker shutdown");
}
