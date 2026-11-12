//! Real Playwright worker and Chromium integration entry point.
//!
//! These tests are ignored by default. Run them explicitly on a Linux host
//! with the pinned offline worker tree:
//!
//! ```text
//! SAYA_TEST_NODE=/path/to/node \
//! SAYA_TEST_WORKER_ROOT=/path/to/worker-root \
//! SAYA_TEST_CHROMIUM_BIN=/path/to/chrome-headless-shell \
//! SAYA_TEST_IDENTITY_MANIFEST=/path/to/manifest.json \
//! cargo test -p saya-core --test browser_worker_real -- --ignored --nocapture
//! ```
//!
//! The repository's macOS and Linux ARM test fixtures live under `target/n5`
//! and `/opt/browsers`; those paths are test inputs, never product defaults.

use saya_core::{
    boundary::Cancellation,
    browser::{
        BrowserProvider,
        worker::{
            BrowserWorkerSettings, NativeBrowserEnvironmentFactory, PlaywrightMcpProvider,
            browser_install_roots,
        },
    },
    execution::BackendKind,
    tools::exec::ExecConfig,
};
use sha2::{Digest, Sha256};
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

fn manifest_digest(path: &std::path::Path) -> String {
    let bytes = std::fs::read(path).expect("identity manifest is readable");
    Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
#[ignore = "requires SAYA_TEST_NODE, SAYA_TEST_WORKER_ROOT, SAYA_TEST_CHROMIUM_BIN and SAYA_TEST_IDENTITY_MANIFEST pointing at the pinned offline worker this host supports"]
async fn real_playwright_worker_verifies_chromium_sandbox() {
    if !cfg!(target_os = "linux") {
        eprintln!("real worker integration requires a Linux host; test not run");
        return;
    }
    let (Some(node), Some(worker_root), Some(chromium), Some(identity_manifest)) = (
        std::env::var_os("SAYA_TEST_NODE"),
        std::env::var_os("SAYA_TEST_WORKER_ROOT"),
        std::env::var_os("SAYA_TEST_CHROMIUM_BIN"),
        std::env::var_os("SAYA_TEST_IDENTITY_MANIFEST"),
    ) else {
        eprintln!("real worker environment is not configured; test not run");
        return;
    };
    let node = PathBuf::from(node);
    let worker_root = PathBuf::from(worker_root);
    let chromium = PathBuf::from(chromium);
    let identity_manifest = PathBuf::from(identity_manifest);
    assert!(node.is_file(), "Node is unavailable");
    assert!(worker_root.is_dir(), "worker root is unavailable");
    assert!(chromium.is_file(), "Chromium is unavailable");
    assert!(
        identity_manifest.is_file(),
        "identity manifest is unavailable"
    );

    let settings = BrowserWorkerSettings {
        enabled: true,
        node: Some(node),
        worker_root: Some(worker_root),
        chromium: Some(chromium),
        identity_manifest: Some(identity_manifest.clone()),
        identity_sha256: Some(manifest_digest(&identity_manifest)),
        proxy: None,
        temp_root: None,
        ..Default::default()
    };
    let install_roots = browser_install_roots(&settings).expect("installation roots");
    let factory = Arc::new(
        NativeBrowserEnvironmentFactory::new(install_roots, BackendKind::Auto)
            .expect("native browser environment"),
    );
    let provider = PlaywrightMcpProvider::managed(settings, factory, ExecConfig::default())
        .expect("production provider");

    let opened = provider
        .open(None, &NeverCancel)
        .await
        .expect("real worker open");
    let evidence = provider
        .sandbox_evidence()
        .await
        .expect("Chromium sandbox evidence");
    assert!(evidence.no_new_privs, "renderer NoNewPrivs is not 1");
    assert_eq!(
        evidence.seccomp_mode, 2,
        "renderer Seccomp is not filter mode"
    );
    assert!(
        evidence.sandbox_arguments_valid,
        "renderer command line disables its sandbox"
    );
    eprintln!(
        "renderer_pid={} no_new_privs={} seccomp={} stderr_initialize_sandbox={}",
        evidence.renderer_pid,
        evidence.no_new_privs,
        evidence.seccomp_mode,
        evidence.stderr_initializes_sandbox
    );

    provider
        .close(&opened.session, &NeverCancel)
        .await
        .expect("real worker close");
    provider
        .shutdown(&NeverCancel)
        .await
        .expect("real worker shutdown");
}
