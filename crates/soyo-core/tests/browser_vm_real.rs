//! Real dedicated-VM browser worker integration entry point.
//!
//! Ignored by default. Run it explicitly on a macOS or Linux host with a provisioned
//! Colima profile created by `scripts/n5/browser_vm.py`:
//!
//! ```text
//! SOYO_TEST_EXECUTABLE=$PWD/target/release/saya \
//! SOYO_TEST_VM_COLIMA=/opt/homebrew/bin/colima \
//! SOYO_TEST_VM_SSH=/usr/bin/ssh \
//! SOYO_TEST_VM_PROFILE=soyo-browser \
//! SOYO_TEST_VM_HOST=127.0.0.1 \
//! SOYO_TEST_VM_USER=ariakage \
//! SOYO_TEST_VM_PORT=58780 \
//! SOYO_TEST_VM_IDENTITY_FILE=$HOME/.colima/_lima/_config/user \
//! SOYO_TEST_VM_KNOWN_HOSTS=$HOME/.colima/_lima/colima-soyo-browser/ssh.known_hosts \
//! SOYO_TEST_VM_NODE=/opt/soyo-browser/bin/node \
//! SOYO_TEST_VM_WORKER_ROOT=/opt/soyo-browser/worker \
//! SOYO_TEST_VM_CHROMIUM=/opt/soyo-browser/chromium_headless_shell-1254/chrome-headless-shell-linux-arm64/chrome-headless-shell \
//! SOYO_TEST_VM_MANIFEST=/opt/soyo-browser/worker-manifest.json \
//! SOYO_TEST_VM_MANIFEST_SHA256=e6af5a229682028f9be237a96c1e1f4699292d08b3b5bce88822e56a3d679b46 \
//! SOYO_TEST_VM_TEMP_ROOT=/tmp/saya-runs \
//! SOYO_TEST_VM_NETWORK_ORIGINS=https://example.com \
//! cargo test -p soyo-core --test browser_vm_real -- --ignored --nocapture
//! ```
//!
//! These values are test inputs, never product defaults. When
//! `SOYO_TEST_VM_NETWORK_ORIGINS` is unset the test uses a legal,
//! non-contacted fixture origin (`https://example.com`); it never derives an
//! enabled network policy from an empty allowlist.

use soyo_core::{
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

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("missing explicit test input {name}"))
}

fn path(name: &str) -> PathBuf {
    PathBuf::from(required(name))
}

#[tokio::test]
#[ignore = "requires a provisioned Colima browser VM and SOYO_TEST_VM_* inputs"]
async fn real_vm_worker_verifies_chromium_sandbox_inside_the_vm() {
    let launcher = std::fs::canonicalize(required("SOYO_TEST_EXECUTABLE"))
        .expect("explicit Soyo launcher path");
    soyo_core::execution::pin_launcher_image_from(&launcher)
        .expect("pin the explicit Soyo CLI instead of the libtest executable");
    let settings = BrowserVmSettings {
        enabled: true,
        colima: Some(path("SOYO_TEST_VM_COLIMA")),
        ssh: Some(path("SOYO_TEST_VM_SSH")),
        profile: Some(required("SOYO_TEST_VM_PROFILE")),
        host: Some(required("SOYO_TEST_VM_HOST")),
        user: Some(required("SOYO_TEST_VM_USER")),
        port: Some(
            required("SOYO_TEST_VM_PORT")
                .parse()
                .expect("valid SOYO_TEST_VM_PORT"),
        ),
        identity_file: Some(path("SOYO_TEST_VM_IDENTITY_FILE")),
        known_hosts: Some(path("SOYO_TEST_VM_KNOWN_HOSTS")),
        node: Some(required("SOYO_TEST_VM_NODE")),
        worker_root: Some(required("SOYO_TEST_VM_WORKER_ROOT")),
        chromium: Some(required("SOYO_TEST_VM_CHROMIUM")),
        identity_manifest: Some(required("SOYO_TEST_VM_MANIFEST")),
        identity_sha256: Some(required("SOYO_TEST_VM_MANIFEST_SHA256")),
        remote_temp_root: Some(required("SOYO_TEST_VM_TEMP_ROOT")),
        ..Default::default()
    };
    let origins = env("SOYO_TEST_VM_NETWORK_ORIGINS").unwrap_or_default();
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
        public_web: env("SOYO_TEST_VM_PUBLIC_WEB").is_some_and(|value| value == "1"),
        localhost_development_origin: env("SOYO_TEST_VM_DEV_ORIGIN"),
        ..Default::default()
    };
    let provider = Arc::new(
        soyo_core::browser::vm::assemble_vm_browser_provider(
            settings,
            network,
            "test-run",
            ExecConfig::default(),
        )
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
    assert_eq!(provider.sandbox_evidence().await, None);
    let reopened = provider
        .open(None, &NeverCancel)
        .await
        .expect("VM worker reopen");
    provider
        .snapshot(&reopened.session, &reopened.page, 64 * 1024, &NeverCancel)
        .await
        .expect("reopened VM page snapshot");
    let refreshed = provider
        .sandbox_evidence()
        .await
        .expect("sandbox evidence after reopening and observing the page");
    assert!(refreshed.no_new_privs);
    assert_eq!(refreshed.seccomp_mode, 2);
    assert!(refreshed.sandbox_arguments_valid);
    eprintln!(
        "vm reopened_renderer_pid={} no_new_privs={} seccomp={}",
        refreshed.renderer_pid, refreshed.no_new_privs, refreshed.seccomp_mode
    );
    provider
        .close(&reopened.session, &NeverCancel)
        .await
        .expect("reopened VM worker close");
    assert_eq!(provider.sandbox_evidence().await, None);
    provider
        .shutdown(&NeverCancel)
        .await
        .expect("VM worker shutdown");
}
