//! Real dedicated-VM browser worker integration entry point.
//!
//! Ignored by default. Run it explicitly on a macOS host with a provisioned
//! Colima profile created by `scripts/n5/browser_vm.py`:
//!
//! ```text
//! SAYA_TEST_EXECUTABLE=$PWD/target/release/saya \
//! SAYA_TEST_VM_COLIMA=/opt/homebrew/bin/colima \
//! SAYA_TEST_VM_SSH=/usr/bin/ssh \
//! SAYA_TEST_VM_PROFILE=saya-browser \
//! SAYA_TEST_VM_HOST=127.0.0.1 \
//! SAYA_TEST_VM_USER=ariakage \
//! SAYA_TEST_VM_PORT=58780 \
//! SAYA_TEST_VM_IDENTITY_FILE=$HOME/.colima/_lima/_config/user \
//! SAYA_TEST_VM_KNOWN_HOSTS=$HOME/.colima/_lima/colima-saya-browser/ssh.known_hosts \
//! SAYA_TEST_VM_NODE=/opt/saya-browser/bin/node \
//! SAYA_TEST_VM_WORKER_ROOT=/opt/saya-browser/worker \
//! SAYA_TEST_VM_CHROMIUM=/opt/saya-browser/chromium_headless_shell-1254/chrome-headless-shell-linux-arm64/chrome-headless-shell \
//! SAYA_TEST_VM_MANIFEST=/opt/saya-browser/worker-manifest.json \
//! SAYA_TEST_VM_MANIFEST_SHA256=e6af5a229682028f9be237a96c1e1f4699292d08b3b5bce88822e56a3d679b46 \
//! SAYA_TEST_VM_TEMP_ROOT=/tmp/saya-runs \
//! SAYA_TEST_VM_NETWORK_ORIGINS=https://example.com \
//! cargo test -p saya-core --test browser_vm_real -- --ignored --nocapture
//! ```
//!
//! These values are test inputs, never product defaults. When
//! `SAYA_TEST_VM_NETWORK_ORIGINS` is unset the test uses a legal,
//! non-contacted fixture origin (`https://example.com`); it never derives an
//! enabled network policy from an empty allowlist.

use saya_core::{
    boundary::Cancellation,
    browser::{BrowserProvider, network::BrowserNetworkSettings, vm::BrowserVmSettings},
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
    let Ok(launcher) = std::fs::canonicalize(launcher) else {
        eprintln!("real Saya launcher path is unavailable; test not run");
        return;
    };
    if saya_core::execution::pin_launcher_image_from(&launcher).is_err() {
        eprintln!("real Saya launcher cannot be pinned; test not run");
        return;
    }
    let origins = env("SAYA_TEST_VM_NETWORK_ORIGINS").unwrap_or_default();
    let allowed_origins = origins
        .split(',')
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    // A legal, non-contacted default fixture keeps an enabled VM network
    // policy valid when the test only inspects sandbox evidence. The worker
    // never navigates in this test.
    let allowed_origins = if allowed_origins.is_empty() {
        vec!["https://example.com".to_owned()]
    } else {
        allowed_origins
    };
    let network = BrowserNetworkSettings {
        enabled: true,
        allowed_origins,
        public_web: env("SAYA_TEST_VM_PUBLIC_WEB").is_some_and(|value| value == "1"),
        localhost_development_origin: env("SAYA_TEST_VM_DEV_ORIGIN"),
        ..Default::default()
    };
    let provider = match saya_core::browser::vm::assemble_vm_browser_provider(
        settings,
        network,
        "test-run",
        ExecConfig::default(),
    ) {
        Ok(provider) => Arc::new(provider),
        Err(error) => {
            eprintln!(
                "VM provider assembly is unavailable ({}); test not run",
                error.0.code
            );
            return;
        }
    };
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
