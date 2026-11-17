//! Production browser assembly boundaries: explicit enablement, Host/macOS
//! rejection before a worker starts, and default-off behavior.

use sha2::{Digest, Sha256};
use soyo_app::assemble_browser_provider;
use soyo_core::{
    browser::{
        vm::BrowserVmSettings,
        worker::{CHROMIUM_REVISION, CHROMIUM_VERSION, PLAYWRIGHT_MCP_VERSION, PLAYWRIGHT_VERSION},
    },
    config::{self, Environment, Overrides},
    tools::exec::ExecConfig,
};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn write(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn package(name: &str, version: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"name": name, "version": version})).unwrap()
}

struct Fixture {
    directory: TempDir,
    worker: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let install = root.join("install");
        fs::create_dir_all(&install).unwrap();
        let worker_root = install.join("worker");
        let node = install.join("node");
        let chromium = install.join("chromium");
        let manifest = install.join("identity.json");
        for path in [
            worker_root.join("node_modules/@playwright/mcp"),
            worker_root.join("node_modules/playwright"),
            worker_root.join("node_modules/playwright-core"),
        ] {
            fs::create_dir_all(path).unwrap();
        }
        write(&node, b"node binary", 0o755);
        write(&chromium, b"chromium binary", 0o755);
        write(
            &worker_root.join("node_modules/@playwright/mcp/cli.js"),
            b"// worker\n",
            0o644,
        );
        write(
            &worker_root.join("node_modules/@playwright/mcp/package.json"),
            &package("@playwright/mcp", PLAYWRIGHT_MCP_VERSION),
            0o644,
        );
        write(
            &worker_root.join("node_modules/playwright/package.json"),
            &package("playwright", PLAYWRIGHT_VERSION),
            0o644,
        );
        write(
            &worker_root.join("node_modules/playwright-core/package.json"),
            &package("playwright-core", PLAYWRIGHT_VERSION),
            0o644,
        );
        let browsers = serde_json::to_vec(&serde_json::json!({
            "browsers": [{
                "name": "chromium",
                "revision": CHROMIUM_REVISION.to_string(),
                "browserVersion": CHROMIUM_VERSION
            }]
        }))
        .unwrap();
        write(
            &worker_root.join("node_modules/playwright-core/browsers.json"),
            &browsers,
            0o644,
        );

        let node_bytes = fs::read(&node).unwrap();
        let chromium_bytes = fs::read(&chromium).unwrap();
        let cli_bytes = fs::read(worker_root.join("node_modules/@playwright/mcp/cli.js")).unwrap();
        let mcp_package =
            fs::read(worker_root.join("node_modules/@playwright/mcp/package.json")).unwrap();
        let playwright =
            fs::read(worker_root.join("node_modules/playwright/package.json")).unwrap();
        let playwright_core =
            fs::read(worker_root.join("node_modules/playwright-core/package.json")).unwrap();
        let identity = serde_json::json!({
            "schema_version": 1,
            "node": {"sha256": digest(&node_bytes), "size": node_bytes.len(), "version": "v22.23.0"},
            "worker_cli_js": {"sha256": digest(&cli_bytes), "size": cli_bytes.len()},
            "mcp_package": {"sha256": digest(&mcp_package), "size": mcp_package.len(), "version": PLAYWRIGHT_MCP_VERSION},
            "playwright_package": {"sha256": digest(&playwright), "size": playwright.len(), "version": PLAYWRIGHT_VERSION},
            "playwright_core_package": {"sha256": digest(&playwright_core), "size": playwright_core.len(), "version": PLAYWRIGHT_VERSION},
            "playwright_core_browsers_json": {"sha256": digest(&browsers), "size": browsers.len()},
            "chromium": {"sha256": digest(&chromium_bytes), "size": chromium_bytes.len(), "version": CHROMIUM_VERSION, "revision": CHROMIUM_REVISION},
        });
        let manifest_bytes = serde_json::to_vec_pretty(&identity).unwrap();
        write(&manifest, &manifest_bytes, 0o600);
        let worker = format!(
            r#"[browser.worker]
enabled=true
node="{}"
worker_root="{}"
chromium="{}"
identity_manifest="{}"
identity_sha256="{}"
"#,
            node.display(),
            worker_root.display(),
            chromium.display(),
            manifest.display(),
            digest(&manifest_bytes),
        );
        Self { directory, worker }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn load(&self, browser: &str, sandbox: &str) -> soyo_core::config::Config {
        let path = self.root().join("config.toml");
        fs::write(
            &path,
            format!(
                r#"
schema_version = 1
[defaults]
profile = "chat"
mode = "work"
permissions = "auto"
output = "text"
[profiles.chat]
protocol = "openai-chat-completions"
base_url = "https://example.invalid/v1"
model = "example"
auth = "bearer"
api_key_env = "EXAMPLE_KEY"
context_window_tokens = 100000
{sandbox}
{browser}
{worker}
"#,
                worker = self.worker
            ),
        )
        .unwrap();
        config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            Environment::from_pairs([
                (OsString::from("HOME"), self.root().as_os_str().to_owned()),
                (OsString::from("EXAMPLE_KEY"), OsString::from("test-key")),
            ]),
            self.root(),
        )
        .unwrap()
    }
}

#[test]
fn browser_default_off_assembles_nothing() {
    let fixture = Fixture::new();
    let config = fixture.load("[browser]\nenabled=false", "");
    let provider = assemble_browser_provider(&config, ExecConfig::default(), "test-run").unwrap();
    assert!(provider.is_none());
}

#[test]
fn enabled_browser_requires_the_worker() {
    let fixture = Fixture::new();
    let mut config = fixture.load("[browser]\nenabled=true", "");
    config.browser.worker.enabled = false;
    let error = assemble_browser_provider(&config, ExecConfig::default(), "test-run").unwrap_err();
    assert_eq!(error.0.code, "invalid_config");
}

#[test]
fn host_and_macos_reject_browser_before_any_worker_starts() {
    let fixture = Fixture::new();
    let host = fixture.load("[browser]\nenabled=true", "[sandbox]\nbackend=\"host\"");
    let error = assemble_browser_provider(&host, ExecConfig::default(), "test-run").unwrap_err();
    assert_eq!(error.0.code, "browser_sandbox_unavailable");

    #[cfg(target_os = "macos")]
    {
        let macos = fixture.load("[browser]\nenabled=true", "");
        let error =
            assemble_browser_provider(&macos, ExecConfig::default(), "test-run").unwrap_err();
        assert_eq!(error.0.code, "browser_sandbox_unavailable");
    }
}

#[test]
fn install_roots_never_include_the_workspace_home_or_saya_tree() {
    let fixture = Fixture::new();
    let config = fixture.load("[browser]\nenabled=true", "[sandbox]\nbackend=\"host\"");
    let roots = soyo_core::browser::worker::browser_install_roots(&config.browser.worker).unwrap();
    let workspace = config.cwd.clone();
    let home = PathBuf::from(fixture.root());
    assert!(!roots.contains(&workspace));
    assert!(!roots.contains(&home));
    assert!(!roots.iter().any(|root| root.ends_with(".soyo")));
}

fn full_vm_settings() -> BrowserVmSettings {
    BrowserVmSettings {
        enabled: true,
        colima: Some(PathBuf::from("/nonexistent/colima")),
        ssh: Some(PathBuf::from("/nonexistent/ssh")),
        profile: Some("soyo-browser".into()),
        host: Some("127.0.0.1".into()),
        user: Some("soyo".into()),
        port: Some(2222),
        identity_file: Some(PathBuf::from("/nonexistent/key")),
        known_hosts: Some(PathBuf::from("/nonexistent/known_hosts")),
        node: Some("/opt/soyo/node".into()),
        worker_root: Some("/opt/soyo/worker".into()),
        chromium: Some(
            "/opt/soyo/browsers/chromium_headless_shell-1254/chrome-headless-shell".into(),
        ),
        identity_manifest: Some("/opt/soyo/identity.json".into()),
        identity_sha256: Some("0".repeat(64)),
        remote_temp_root: Some("/tmp/saya-runs".into()),
        ..Default::default()
    }
}

#[test]
fn selecting_both_worker_backends_is_rejected() {
    let fixture = Fixture::new();
    let mut config = fixture.load("[browser]\nenabled=true", "");
    config.browser.vm = full_vm_settings();
    let error = assemble_browser_provider(&config, ExecConfig::default(), "test-run").unwrap_err();
    assert_eq!(error.0.code, "invalid_config");
}

#[test]
fn explicit_vm_selection_never_falls_back_to_native() {
    let fixture = Fixture::new();
    let mut config = fixture.load("[browser]\nenabled=true", "[sandbox]\nbackend=\"host\"");
    config.browser.worker.enabled = false;
    config.browser.vm = full_vm_settings();
    let error = assemble_browser_provider(&config, ExecConfig::default(), "test-run").unwrap_err();
    // Missing host inputs fail explicitly. The result must never be the
    // native/Host sandbox error, which would mean silent fallback.
    assert!(
        matches!(
            error.0.code.as_str(),
            "invalid_config" | "browser_vm_unavailable"
        ),
        "unexpected fallback error: {}",
        error.0.code
    );
}
