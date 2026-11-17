//! Real Playwright worker and Chromium integration entry point.
//!
//! These tests are ignored by default. Run them explicitly on a Linux host
//! with the pinned offline worker tree:
//!
//! ```text
//! SOYO_TEST_EXECUTABLE=/path/to/saya \
//! SOYO_TEST_NODE=/path/to/node \
//! SOYO_TEST_WORKER_ROOT=/path/to/worker-root \
//! SOYO_TEST_CHROMIUM_BIN=/path/to/chrome-headless-shell \
//! SOYO_TEST_IDENTITY_MANIFEST=/path/to/manifest.json \
//! cargo test -p soyo-core --test browser_worker_real -- --ignored --nocapture
//! ```
//!
//! The repository's macOS and Linux ARM test fixtures live under `target/n5`
//! and `/opt/browsers`; those paths are test inputs, never product defaults.

use sha2::{Digest, Sha256};
use soyo_core::{
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
use std::{path::PathBuf, process::Command, sync::Arc, time::Duration};

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

#[test]
fn real_worker_entry_rejects_missing_prerequisites() {
    let mut command = Command::new(std::env::current_exe().expect("test image"));
    command.args([
        "--ignored",
        "--exact",
        "real_playwright_worker_verifies_chromium_sandbox",
        "--nocapture",
    ]);
    for name in [
        "SOYO_TEST_EXECUTABLE",
        "SOYO_TEST_NODE",
        "SOYO_TEST_WORKER_ROOT",
        "SOYO_TEST_CHROMIUM_BIN",
        "SOYO_TEST_IDENTITY_MANIFEST",
    ] {
        command.env_remove(name);
    }
    let output = command.output().expect("real entry negative control");
    assert!(
        !output.status.success(),
        "missing real Browser prerequisites were counted as success: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("0 passed; 1 failed"));
}

fn required_path(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("missing explicit real Browser prerequisite {name}"))
}

#[tokio::test]
#[ignore = "requires explicit SOYO_TEST_EXECUTABLE and pinned offline worker inputs on a supported Linux host"]
async fn real_playwright_worker_verifies_chromium_sandbox() {
    assert_eq!(std::env::consts::OS, "linux", "real Browser requires Linux");
    let provider = native_worker_provider();
    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        let opened = provider
            .open(None, &NeverCancel)
            .await
            .map_err(|error| format!("real worker open: {error:?}"))?;
        let evidence = provider
            .sandbox_evidence()
            .await
            .ok_or_else(|| "Chromium sandbox evidence is missing".to_owned())?;
        provider
            .close(&opened.session, &NeverCancel)
            .await
            .map_err(|error| format!("real worker close: {error:?}"))?;
        Ok::<_, String>(evidence)
    })
    .await;
    tokio::time::timeout(Duration::from_secs(30), provider.shutdown(&NeverCancel))
        .await
        .expect("bounded real worker cleanup")
        .expect("real worker shutdown");
    let evidence = outcome
        .expect("bounded real Browser probe")
        .expect("real Browser probe");
    assert!(evidence.no_new_privs, "renderer NoNewPrivs is not 1");
    assert_eq!(
        evidence.seccomp_mode, 2,
        "renderer Seccomp is not filter mode"
    );
    assert!(
        evidence.sandbox_arguments_valid,
        "renderer command line disables its sandbox"
    );
    assert!(
        evidence.stderr_initializes_sandbox,
        "Chromium sandbox initialization was not witnessed"
    );
    eprintln!(
        "renderer_pid={} no_new_privs={} seccomp={} stderr_initialize_sandbox={}",
        evidence.renderer_pid,
        evidence.no_new_privs,
        evidence.seccomp_mode,
        evidence.stderr_initializes_sandbox
    );
}

fn native_worker_provider() -> PlaywrightMcpProvider {
    let image = std::fs::canonicalize(required_path("SOYO_TEST_EXECUTABLE"))
        .expect("explicit Soyo launcher path");
    soyo_core::execution::pin_launcher_image_from(&image).expect("real CLI launcher image");
    let node = required_path("SOYO_TEST_NODE");
    let worker_root = required_path("SOYO_TEST_WORKER_ROOT");
    let chromium = required_path("SOYO_TEST_CHROMIUM_BIN");
    let identity_manifest = required_path("SOYO_TEST_IDENTITY_MANIFEST");
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
        // Debug test builds hash the ~440 MB pinned Node/worker/Chromium trees unoptimized.
        connect_timeout_ms: 60_000,
        ..Default::default()
    };
    let install_roots = browser_install_roots(&settings).expect("installation roots");
    let factory = Arc::new(
        NativeBrowserEnvironmentFactory::new(install_roots, BackendKind::Auto)
            .expect("native browser environment"),
    );
    PlaywrightMcpProvider::managed(settings, factory, ExecConfig::default())
        .expect("production provider")
}

#[tokio::test]
#[ignore = "requires Linux Bubblewrap/Seccomp, SOYO_TEST_*, and the fixed capture engine + worker"]
async fn real_playwright_worker_captures_masked_png_in_native_sandbox() {
    assert_eq!(std::env::consts::OS, "linux", "real Browser requires Linux");
    let provider = native_worker_provider();
    let facts = provider.facts();
    assert!(facts.screenshots);
    assert!(facts.screenshot_masking);
    let outcome = tokio::time::timeout(Duration::from_secs(90), async {
        let opened = provider
            .open(None, &NeverCancel)
            .await
            .map_err(|error| format!("real worker open: {error:?}"))?;
        let shot = provider
            .screenshot(&opened.session, &opened.page, &NeverCancel)
            .await
            .map_err(|error| format!("real worker screenshot: {error:?}"))?;
        let evidence = provider
            .sandbox_evidence()
            .await
            .ok_or_else(|| "Chromium sandbox evidence is missing".to_owned())?;
        provider
            .close(&opened.session, &NeverCancel)
            .await
            .map_err(|error| format!("real worker close: {error:?}"))?;
        Ok::<_, String>((shot, evidence))
    })
    .await;
    tokio::time::timeout(Duration::from_secs(30), provider.shutdown(&NeverCancel))
        .await
        .expect("bounded real worker cleanup")
        .expect("real worker shutdown");
    let (shot, evidence) = outcome
        .expect("bounded native masked screenshot")
        .expect("native masked screenshot");
    assert!(shot.masked);
    assert_eq!(shot.mime, "image/png");
    assert!(shot.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(shot.width > 0 && shot.height > 0);
    assert_eq!(shot.url.as_deref(), Some("about:blank"));
    assert!(evidence.no_new_privs, "renderer NoNewPrivs is not 1");
    assert_eq!(evidence.seccomp_mode, 2);
    assert!(evidence.sandbox_arguments_valid);
    assert!(evidence.stderr_initializes_sandbox);
    eprintln!(
        "native_screenshot_width={} native_screenshot_height={} renderer_pid={} no_new_privs={} seccomp={} stderr_initialize_sandbox={}",
        shot.width,
        shot.height,
        evidence.renderer_pid,
        evidence.no_new_privs,
        evidence.seccomp_mode,
        evidence.stderr_initializes_sandbox
    );
}
