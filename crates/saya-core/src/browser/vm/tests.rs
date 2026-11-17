use super::*;
use crate::execution::BackendKind;
fn test_network_factory() -> Arc<BrowserNetworkFactory> {
    Arc::new(
        BrowserNetworkFactory::new(
            BrowserNetworkSettings {
                enabled: true,
                allowed_origins: vec!["https://example.com".into()],
                ..Default::default()
            },
            "test-run",
        )
        .unwrap(),
    )
}

struct Cancelled;
impl Cancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

use serde_json::json;
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;

#[test]
fn upload_script_preserves_original_name_and_cleans_partial_staging() {
    use std::{
        io::{ErrorKind, Write as _},
        process::{Command, Stdio},
    };

    let root = tempfile::tempdir().unwrap();
    let root_text = root.path().to_str().unwrap().to_owned();
    let run = |name: &str, input: &[u8], allow_early_exit: bool| {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(UPLOAD_SCRIPT)
            .arg("sh")
            .arg(&root_text)
            .arg(name)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        if let Some(mut stdin) = child.stdin.take() {
            match stdin.write_all(input) {
                Ok(()) => {}
                Err(error) if allow_early_exit && error.kind() == ErrorKind::BrokenPipe => {}
                Err(error) => panic!("failed to write upload payload for {name}: {error}"),
            }
        }
        child.wait_with_output().unwrap()
    };

    let output = run("invoice 2026.txt", b"payload", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        root.path()
            .join("uploads/invoice 2026.txt")
            .to_str()
            .unwrap()
    );
    assert_eq!(
        std::fs::read(root.path().join("uploads/invoice 2026.txt")).unwrap(),
        b"payload"
    );

    // A `..` inside a basename is not traversal and must be preserved.
    let output = run("archive..tar", b"dots", false);
    assert!(output.status.success());
    assert_eq!(
        std::fs::read(root.path().join("uploads/archive..tar")).unwrap(),
        b"dots"
    );

    // A refused move must remove the temporary file, not leave a partial.
    std::fs::create_dir(root.path().join("uploads/blocked.txt")).unwrap();
    let output = run("blocked.txt", b"payload", false);
    assert!(!output.status.success());
    let partial: Vec<String> = std::fs::read_dir(root.path().join("uploads"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".upload."))
        .collect();
    assert!(partial.is_empty(), "partial staging remained: {partial:?}");

    for name in ["../escape.txt", "dir/name.txt", ".hidden", "-rf", ".."] {
        assert!(
            !run(name, b"x", true).status.success(),
            "{name} was accepted"
        );
    }
}

use crate::network::BrokerLimits;

fn digest(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

fn package(name: &str, version: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"name": name, "version": version})).unwrap()
}

#[test]
fn pinned_headless_shell_path_accepts_the_actual_linux_arm_layout() {
    assert!(headless_shell_path(
        "/opt/saya-browser/chromium/chrome-headless-shell-linux-arm64/chrome-headless-shell"
            .replace("chromium/", "chromium_headless_shell-1254/")
            .as_str()
    ));
    assert!(headless_shell_path(
        "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell"
    ));
    assert!(!headless_shell_path(
        "/opt/saya-browser/chromium_headless_shell-1254/chrome-headless-shell-linux-arm64/chrome"
    ));
    assert!(!headless_shell_path(
        "/opt/saya-browser/chromium_headless_shell-9999/chrome-headless-shell-linux-arm64/chrome-headless-shell"
    ));
}

struct IdentityFixture {
    settings: BrowserVmSettings,
    probe: Vec<u8>,
    manifest: Vec<u8>,
    mcp_package: Vec<u8>,
    playwright_package: Vec<u8>,
    playwright_core_package: Vec<u8>,
    browsers: Vec<u8>,
    installation_probe: Vec<u8>,
    node_version: Vec<u8>,
}

impl IdentityFixture {
    fn new() -> Self {
        let manifest_path = "/opt/saya/identity.json";
        let node_path = "/opt/saya/node/bin/node";
        let worker_root = "/opt/saya/worker";
        let cli_path = format!("{worker_root}/{}", worker::CLI_JS_PATH);
        let mcp_path = format!("{worker_root}/{}", worker::MCP_PACKAGE_PATH);
        let playwright_path = format!("{worker_root}/{}", worker::PLAYWRIGHT_PACKAGE_PATH);
        let core_path = format!("{worker_root}/{}", worker::PLAYWRIGHT_CORE_PACKAGE_PATH);
        let browsers_path = format!("{worker_root}/{}", worker::BROWSERS_JSON_PATH);
        let chromium_path = "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell";

        let node = b"pinned node".to_vec();
        let cli = b"// pinned worker\n".to_vec();
        let mcp_package = package("@playwright/mcp", PLAYWRIGHT_MCP_VERSION);
        let playwright_package = package("playwright", PLAYWRIGHT_VERSION);
        let playwright_core_package = package("playwright-core", PLAYWRIGHT_VERSION);
        let browsers = serde_json::to_vec(&json!({
            "browsers": [{
                "name": "chromium",
                "revision": CHROMIUM_REVISION.to_string(),
                "browserVersion": CHROMIUM_VERSION,
            }]
        }))
        .unwrap();
        let chromium = b"pinned chromium".to_vec();
        let installation_trees = json!({
            "node": {"sha256": digest(b"node installation"), "entries": 2, "bytes": node.len()},
            "worker": {"sha256": digest(b"worker installation"), "entries": 12, "bytes": cli.len() + mcp_package.len() + playwright_package.len() + playwright_core_package.len() + browsers.len()},
            "chromium": {"sha256": digest(b"engine installation"), "entries": 2, "bytes": chromium.len()},
        });
        let manifest_bytes = serde_json::to_vec_pretty(&json!({
            "schema_version": 2,
            "installation_trees": installation_trees,
            "node": {"sha256": digest(&node), "size": node.len(), "version": "v22.23.0"},
            "worker_cli_js": {"sha256": digest(&cli), "size": cli.len()},
            "mcp_package": {"sha256": digest(&mcp_package), "size": mcp_package.len(), "version": PLAYWRIGHT_MCP_VERSION},
            "playwright_package": {"sha256": digest(&playwright_package), "size": playwright_package.len(), "version": PLAYWRIGHT_VERSION},
            "playwright_core_package": {"sha256": digest(&playwright_core_package), "size": playwright_core_package.len(), "version": PLAYWRIGHT_VERSION},
            "playwright_core_browsers_json": {"sha256": digest(&browsers), "size": browsers.len()},
            "chromium": {"sha256": digest(&chromium), "size": chromium.len(), "version": CHROMIUM_VERSION, "revision": CHROMIUM_REVISION},
        }))
        .unwrap();
        let manifest_digest = digest(&manifest_bytes);

        let entries = [
            (
                manifest_path,
                manifest_bytes.len(),
                0o600u32,
                digest(&manifest_bytes),
            ),
            (node_path, node.len(), 0o755, digest(&node)),
            (cli_path.as_str(), cli.len(), 0o644, digest(&cli)),
            (
                mcp_path.as_str(),
                mcp_package.len(),
                0o644,
                digest(&mcp_package),
            ),
            (
                playwright_path.as_str(),
                playwright_package.len(),
                0o644,
                digest(&playwright_package),
            ),
            (
                core_path.as_str(),
                playwright_core_package.len(),
                0o644,
                digest(&playwright_core_package),
            ),
            (
                browsers_path.as_str(),
                browsers.len(),
                0o644,
                digest(&browsers),
            ),
            (chromium_path, chromium.len(), 0o755, digest(&chromium)),
        ];
        let mut probe = Vec::new();
        for (_, size, mode, hash) in &entries {
            probe.extend_from_slice(
                format!("{size}|{mode:o}|regular file\n{hash}  file\n").as_bytes(),
            );
        }
        let settings = BrowserVmSettings {
            enabled: true,
            colima: Some(PathBuf::from("/usr/bin/ssh")),
            ssh: Some(PathBuf::from("/usr/bin/ssh")),
            profile: Some("saya-browser".into()),
            host: Some("127.0.0.1".into()),
            user: Some("saya".into()),
            port: Some(2222),
            identity_file: Some(PathBuf::from("/tmp/saya-test-key")),
            known_hosts: Some(PathBuf::from("/tmp/saya-test-known-hosts")),
            node: Some(node_path.into()),
            worker_root: Some(worker_root.into()),
            chromium: Some(chromium_path.into()),
            identity_manifest: Some(manifest_path.into()),
            identity_sha256: Some(manifest_digest),
            remote_temp_root: Some("/tmp/saya-runs".into()),
            ..Default::default()
        };
        Self {
            settings,
            probe,
            manifest: manifest_bytes,
            mcp_package,
            playwright_package,
            playwright_core_package,
            browsers,
            installation_probe: serde_json::to_vec(&installation_trees).unwrap(),
            node_version: b"v22.23.0\n".to_vec(),
        }
    }

    fn outputs(&self) -> VecDeque<VmCommandOutput> {
        [
            self.probe.clone(),
            self.manifest.clone(),
            self.mcp_package.clone(),
            self.playwright_package.clone(),
            self.playwright_core_package.clone(),
            self.browsers.clone(),
            self.installation_probe.clone(),
            self.node_version.clone(),
        ]
        .into_iter()
        .map(|stdout| VmCommandOutput {
            status: Some(0),
            stdout,
            stderr: Vec::new(),
            timed_out: false,
            cancelled: false,
        })
        .collect()
    }
}

struct QueueRunner {
    outputs: Mutex<VecDeque<VmCommandOutput>>,
    scripts: Mutex<Vec<String>>,
}

impl QueueRunner {
    fn new(outputs: VecDeque<VmCommandOutput>) -> Arc<Self> {
        Arc::new(Self {
            outputs: Mutex::new(outputs),
            scripts: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> usize {
        self.scripts.lock().unwrap().len()
    }

    fn scripts(&self) -> Vec<String> {
        self.scripts.lock().unwrap().clone()
    }
}

#[async_trait]
impl VmCommandRunner for QueueRunner {
    async fn run(
        &self,
        spec: VmCommandSpec<'_>,
        _cancel: &dyn Cancellation,
    ) -> Result<VmCommandOutput, BrowserFailure> {
        self.scripts.lock().unwrap().push(spec.script.to_owned());
        self.outputs
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| BrowserFailure::new("test_runner", "no queued output"))
    }
}

fn success(stdout: impl Into<Vec<u8>>) -> VmCommandOutput {
    VmCommandOutput {
        status: Some(0),
        stdout: stdout.into(),
        stderr: Vec::new(),
        timed_out: false,
        cancelled: false,
    }
}

#[test]
fn vm_settings_default_disabled_and_reject_partial_enablement() {
    let settings = BrowserVmSettings::default();
    assert!(!settings.enabled);
    settings.validate().unwrap();
    let enabled = BrowserVmSettings {
        enabled: true,
        ..Default::default()
    };
    assert!(enabled.validate().is_err());
    let relative = BrowserVmSettings {
        ssh: Some(PathBuf::from("ssh")),
        ..Default::default()
    };
    assert!(relative.validate().is_err());
}

#[test]
fn rejects_shell_injection_and_non_loopback_inputs() {
    let mut settings = IdentityFixture::new().settings;
    for bad in [
        "/opt/worker; rm -rf /",
        "/opt/$(id)",
        "/opt/`id`",
        "/opt/a\nb",
        "/opt/a b; c",
        "/opt/../root",
        "relative/path",
    ] {
        settings.node = Some(bad.into());
        assert!(settings.validate().is_err(), "{bad}");
    }
    settings.node = Some("/opt/node".into());
    settings.user = Some("root@evil".into());
    assert!(settings.validate().is_err());
    settings.user = Some("saya".into());
    settings.host = Some("example.com".into());
    assert!(settings.validate().is_err());
    settings.host = Some("127.0.0.1".into());
    settings.profile = Some("../escape".into());
    assert!(settings.validate().is_err());
    settings.profile = Some("saya-browser".into());
    settings.ssh = Some(PathBuf::from("/tmp/ssh\nProxyCommand=evil"));
    assert!(settings.validate().is_err());
    settings.ssh = Some(PathBuf::from("/usr/bin/ssh"));
    settings.validate().unwrap();
}

#[test]
fn ssh_config_is_strict_and_contains_no_secret_values() {
    let settings = IdentityFixture::new().settings;
    let paths = VmLocalPaths {
        ssh: PathBuf::from("/usr/bin/ssh"),
        identity_file: PathBuf::from("/private/tmp/saya-key"),
        known_hosts: PathBuf::from("/private/tmp/saya-known-hosts"),
        colima: PathBuf::from("/usr/local/bin/colima"),
    };
    let config = strict_ssh_config(&settings, &paths).unwrap();
    for required in [
        "BatchMode yes",
        "IdentitiesOnly yes",
        "StrictHostKeyChecking yes",
        "PasswordAuthentication no",
        "KbdInteractiveAuthentication no",
        "ForwardAgent no",
        "ClearAllForwardings yes",
        "ProxyCommand none",
        "ProxyJump none",
        "ControlMaster no",
        "UpdateHostKeys no",
    ] {
        assert!(config.contains(required), "{required}");
    }
    assert!(!config.contains("PRIVATE KEY"));
    let argv = ssh_argv(
        &settings,
        &paths,
        Path::new("/private/tmp/ssh_config"),
        PROBE_SCRIPT,
        &["/opt/file".into()],
        None,
    )
    .unwrap();
    let joined = argv.join(" ");
    assert!(joined.contains("ForwardAgent=no"));
    assert!(joined.contains("StrictHostKeyChecking=yes"));
    assert!(joined.contains("IdentitiesOnly=yes"));
    assert!(!joined.contains("ForwardAgent=yes"));
    assert!(joined.contains("sh -c '"));
}

#[test]
fn reverse_streamlocal_forward_replaces_clear_all_forwardings_and_fails_closed() {
    let settings = IdentityFixture::new().settings;
    let paths = VmLocalPaths {
        ssh: PathBuf::from("/usr/bin/ssh"),
        identity_file: PathBuf::from("/private/tmp/saya-key"),
        known_hosts: PathBuf::from("/private/tmp/saya-known-hosts"),
        colima: PathBuf::from("/usr/local/bin/colima"),
    };
    let base = ssh_argv(
        &settings,
        &paths,
        Path::new("/private/tmp/ssh_config"),
        PROBE_SCRIPT,
        &[],
        None,
    )
    .unwrap();
    // Plain identity commands keep every forwarding disabled.
    assert!(base.iter().any(|value| value == "ClearAllForwardings=yes"));
    assert!(!base.iter().any(|value| value == "-R"));
    assert!(!base.iter().any(|value| value == "ExitOnForwardFailure=yes"));

    let forwarded = ssh_argv(
        &settings,
        &paths,
        Path::new("/private/tmp/ssh_config"),
        PROBE_SCRIPT,
        &[],
        Some(&ReverseForward {
            remote_socket: "/tmp/run/proxy.sock".into(),
            local_port: 41_234,
        }),
    )
    .unwrap();
    // ssh uses the first obtained value, so the strict base value must be
    // replaced rather than shadowed by a later option.
    assert!(
        !forwarded
            .iter()
            .any(|value| value == "ClearAllForwardings=yes")
    );
    assert!(
        forwarded
            .iter()
            .any(|value| value == "ClearAllForwardings=no")
    );
    assert!(
        forwarded
            .iter()
            .any(|value| value == "ExitOnForwardFailure=yes")
    );
    assert!(
        forwarded
            .iter()
            .any(|value| value == "StreamLocalBindUnlink=yes")
    );
    assert_eq!(
        forwarded.iter().filter(|value| *value == "-R").count(),
        1,
        "a browser launch adds exactly one reverse forward: {forwarded:?}"
    );
    let index = forwarded
        .iter()
        .position(|value| value == "-R")
        .expect("-R");
    assert_eq!(forwarded[index + 1], "/tmp/run/proxy.sock:127.0.0.1:41234");
}

#[test]
fn host_channel_has_no_workspace_mount_and_only_vm_loopback_network() {
    let settings = IdentityFixture::new().settings;
    let workspace = Path::new("/private/tmp/saya-vm-host");
    let paths = VmLocalPaths {
        ssh: PathBuf::from("/usr/bin/ssh"),
        identity_file: PathBuf::from("/private/tmp/key"),
        known_hosts: PathBuf::from("/private/tmp/known_hosts"),
        colima: PathBuf::from("/usr/local/bin/colima"),
    };
    let policy = host_policy(&settings, workspace, &paths, None).unwrap();
    assert!(policy.read_roots.is_empty());
    assert_eq!(policy.write_roots, vec![workspace.to_path_buf()]);
    assert!(policy.browser.is_none());
    assert_eq!(policy.read_files.len(), 3);
    assert!(matches!(
        policy.network,
        NetworkPolicy::LoopbackPort { port, proxy_port, .. } if port == 2222 && proxy_port.is_none()
    ));
    assert!(!matches!(policy.network, NetworkPolicy::Proxy(_)));
    // The generic default is unchanged: only this purpose gains a
    // loopback destination, and it is never the VM's default egress.
    assert!(matches!(
        SandboxPolicy::workspace(PathBuf::from("/tmp")).network,
        NetworkPolicy::Deny
    ));
}

#[test]
fn host_channel_selects_the_native_backend_for_its_platform() {
    let settings = IdentityFixture::new().settings;
    let paths = VmLocalPaths {
        ssh: "/usr/bin/ssh".into(),
        identity_file: "/private/key".into(),
        known_hosts: "/private/known_hosts".into(),
        colima: "/usr/bin/colima".into(),
    };
    let policy = host_policy(&settings, Path::new("/private/vm-host"), &paths, None).unwrap();
    assert_eq!(policy.backend, BackendKind::Auto);
}

#[test]
fn host_channel_policy_preserves_the_exact_loopback_literal() {
    let mut settings = IdentityFixture::new().settings;
    let paths = VmLocalPaths {
        ssh: "/usr/bin/ssh".into(),
        identity_file: "/private/key".into(),
        known_hosts: "/private/known_hosts".into(),
        colima: "/usr/bin/colima".into(),
    };
    for (host, expected) in [("127.0.0.2", "127.0.0.2"), ("[::1]", "::1")] {
        settings.host = Some(host.into());
        let policy = host_policy(
            &settings,
            Path::new("/private/vm-host"),
            &paths,
            Some(31234),
        )
        .unwrap();
        let network = serde_json::to_value(policy.network).unwrap();
        assert_eq!(network["host"], expected);
        assert_eq!(network["proxy_port"], 31234);
    }
}

#[tokio::test]
async fn verifies_remote_identity_files_and_node_version() {
    let fixture = IdentityFixture::new();
    let runner = QueueRunner::new(fixture.outputs());
    let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner.clone());
    let identity = verifier.verify(&NeverCancel).await.unwrap();
    assert_eq!(
        identity.manifest_sha256,
        fixture.settings.identity_sha256.clone().unwrap()
    );
    assert_eq!(
        identity.chromium,
        PathBuf::from(fixture.settings.chromium.clone().unwrap())
    );
    assert_eq!(runner.calls(), 8);
}

#[tokio::test]
async fn rejects_remote_installation_tree_mismatches() {
    let fixture = IdentityFixture::new();
    let mut outputs = fixture.outputs();
    let mut trees: Value = serde_json::from_slice(&fixture.installation_probe).unwrap();
    trees["worker"]["sha256"] = json!("0".repeat(64));
    outputs[6] = success(serde_json::to_vec(&trees).unwrap());
    let verifier = VmWorkerIdentityVerifier::new(fixture.settings, QueueRunner::new(outputs));
    let failure = verifier.verify(&NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "worker_identity");
    assert!(failure.message.contains("installation tree"), "{failure:?}");
}

#[tokio::test]
async fn rejects_remote_digest_permission_and_version_mismatches() {
    let fixture = IdentityFixture::new();
    let mut outputs = fixture.outputs();
    outputs[0] = success(String::from_utf8(fixture.probe.clone()).unwrap().replacen(
        &fixture.settings.identity_sha256.clone().unwrap(),
        &"0".repeat(64),
        1,
    ));
    let runner = QueueRunner::new(outputs);
    let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner);
    assert_eq!(
        verifier.verify(&NeverCancel).await.unwrap_err().code,
        "worker_identity"
    );

    let fixture = IdentityFixture::new();
    let mut outputs = fixture.outputs();
    outputs[0] = success(String::from_utf8(fixture.probe.clone()).unwrap().replacen(
        "|644|regular file\n",
        "|666|regular file\n",
        1,
    ));
    let verifier =
        VmWorkerIdentityVerifier::new(fixture.settings.clone(), QueueRunner::new(outputs));
    assert_eq!(
        verifier.verify(&NeverCancel).await.unwrap_err().code,
        "worker_identity"
    );

    let fixture = IdentityFixture::new();
    let mut outputs = fixture.outputs();
    outputs[7] = success(b"v20.0.0\n".to_vec());
    let verifier =
        VmWorkerIdentityVerifier::new(fixture.settings.clone(), QueueRunner::new(outputs));
    assert_eq!(
        verifier.verify(&NeverCancel).await.unwrap_err().code,
        "worker_identity"
    );

    let mut settings = IdentityFixture::new().settings;
    settings.chromium = Some("/opt/saya/chromium".into());
    let runner = QueueRunner::new(IdentityFixture::new().outputs());
    let verifier = VmWorkerIdentityVerifier::new(settings, runner.clone());
    assert_eq!(
        verifier.verify(&NeverCancel).await.unwrap_err().code,
        "worker_identity"
    );
    assert_eq!(runner.calls(), 0);
}

#[test]
fn vm_launch_arguments_keep_chromium_sandbox_enabled() {
    let args = worker_launch_arguments(
        "/tmp/runs/one",
        "/tmp/runs/one/proxy.sock",
        31281,
        "/tmp/runs/one/worker.pid",
        "/opt/saya/node/bin/node",
        "/opt/saya/worker/node_modules/@playwright/mcp/cli.js",
        "/tmp/runs/one/config/config.json",
        "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell",
    );
    for required in [
        "--sandbox",
        "--config",
        "--executable-path",
        "--headless",
        "--isolated",
    ] {
        assert!(args.iter().any(|value| value == required), "{required}");
    }
    for forbidden in [
        "--no-sandbox",
        "--disable-setuid-sandbox",
        "--disable-namespace-sandbox",
        "--remote-debugging-port",
    ] {
        assert!(!args.iter().any(|value| value == forbidden), "{forbidden}");
    }
    assert_eq!(args[1], "/tmp/runs/one/proxy.sock");
    assert_eq!(args[2], "31281");
    assert_eq!(args[3], "/tmp/runs/one/worker.pid");
}

#[test]
fn worker_config_pins_chromium_to_the_run_proxy_without_bypasses() {
    let settings = IdentityFixture::new().settings;
    let bytes = worker_config_json(&settings, "/tmp/runs/one", "http://127.0.0.1:40123").unwrap();
    let config: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let options = &config["browser"]["launchOptions"];
    assert_eq!(options["proxy"]["server"], "http://127.0.0.1:40123");
    assert_eq!(options["chromiumSandbox"], true);
    let args = options["args"].as_array().unwrap();
    let args: Vec<&str> = args.iter().filter_map(|value| value.as_str()).collect();
    for required in [
        "--proxy-server=http://127.0.0.1:40123",
        "--proxy-bypass-list=<-loopback>",
        "--disable-quic",
        "--force-webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--webrtc-ip-handling-policy=disable_non_proxied_udp",
        "--host-resolver-rules=EXCLUDE 127.0.0.1, MAP * ~NOTFOUND",
        "--disable-features=DnsOverHttps",
    ] {
        assert!(args.contains(&required), "{required}");
    }
    assert!(!args.iter().any(|value| value.contains("direct")));
}

#[test]
fn launch_script_requires_a_private_network_namespace_and_never_falls_back() {
    for required in [
        "sudo -n unshare -n",
        "ip link set lo up",
        "ip -o route show",
        "socat TCP-LISTEN:",
        "UNIX-CONNECT:",
        "setpriv --reuid=",
        "setsid -w",
        "network=$tool-missing",
        "network=sudo-unavailable",
        "network=loopback-unavailable",
        "network=unexpected-route",
        "network=proxy-socket-missing",
        "network=proxy-start-failed",
        "network=proxy-not-listening",
    ] {
        assert!(LAUNCH_SCRIPT.contains(required), "{required}");
    }
    assert!(!LAUNCH_SCRIPT.contains("exit 0"));
}

#[test]
fn launch_failure_classification_is_explicit_and_never_generalized() {
    for sentinel in [
        "network=sudo-missing",
        "network=unshare-missing",
        "network=socat-missing",
        "network=setpriv-missing",
        "network=ip-missing",
        "network=sudo-unavailable",
        "network=loopback-unavailable",
        "network=unexpected-route",
        "network=proxy-socket-missing",
        "network=proxy-start-failed",
        "network=proxy-not-listening",
        "remote port forwarding failed for listen path /tmp/run/proxy.sock",
    ] {
        let failure = classify_launch_failure(&NeverCancel, sentinel.as_bytes());
        assert_eq!(
            failure.code, "browser_network_unavailable",
            "sentinel {sentinel}"
        );
    }
    let other = classify_launch_failure(&NeverCancel, b"worker crashed for another reason");
    assert_eq!(other.code, "browser_vm_transport");
    let unrelated = classify_launch_failure(&NeverCancel, b"network=unrelated-diagnostic");
    assert_eq!(
        unrelated.code, "browser_vm_transport",
        "only fixed network capability prefixes map to browser_network_unavailable"
    );
    let cancelled = classify_launch_failure(&Cancelled, b"network=sudo-missing");
    assert_eq!(cancelled.code, "cancelled");
}

#[tokio::test(start_paused = true)]
async fn network_factory_keeps_one_fixed_run_deadline_across_broker_restarts() {
    let factory = Arc::new(
        BrowserNetworkFactory::new(
            BrowserNetworkSettings {
                enabled: true,
                allowed_origins: vec!["https://example.com".into()],
                limits: BrokerLimits {
                    active_timeout_ms: 10,
                    ..Default::default()
                },
                ..Default::default()
            },
            "run-deadline",
        )
        .unwrap(),
    );
    let first = factory.start(&NeverCancel).await.unwrap();
    first.shutdown().await.unwrap();
    tokio::time::advance(Duration::from_millis(11)).await;
    let failure = match factory.start(&NeverCancel).await {
        Ok(_) => panic!("an expired Run deadline must not allow a restart"),
        Err(failure) => failure,
    };
    assert_eq!(
        failure.0.code, "browser_network_budget",
        "an expired fixed Run deadline is a budget failure, not an isolation failure"
    );
}

#[tokio::test]
async fn network_factory_never_resets_exhausted_connection_attempts() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let factory = Arc::new(
        BrowserNetworkFactory::new(
            BrowserNetworkSettings {
                enabled: true,
                allowed_origins: vec!["https://example.com".into()],
                limits: BrokerLimits {
                    max_total_connections: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
            "run-connections",
        )
        .unwrap(),
    );
    let first = factory.start(&NeverCancel).await.unwrap();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", first.endpoint_port()))
        .await
        .unwrap();
    stream
        .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\nHost: denied.example:443\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response).await;
    assert!(String::from_utf8_lossy(&response).contains("403"));
    first.shutdown().await.unwrap();
    drop(stream);

    let restarted = factory.start(&NeverCancel).await.unwrap();
    let mut denied = tokio::net::TcpStream::connect(("127.0.0.1", restarted.endpoint_port()))
        .await
        .unwrap();
    denied
        .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\nHost: denied.example:443\r\n\r\n")
        .await
        .unwrap();
    let mut second = Vec::new();
    let _ = denied.read_to_end(&mut second).await;
    assert!(
        second.is_empty(),
        "an exhausted Run budget must stay exhausted after a restart: {second:?}"
    );
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn network_factory_never_resets_spent_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let mut request = Vec::new();
            let _ = stream.read_to_end(&mut request).await;
            let _ = stream.write_all(response).await;
        }
    });
    let rewritten = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        origin.trim_start_matches("http://")
    );
    let capacity = rewritten.len() as u64 + response.len() as u64 + 1;
    let factory = Arc::new(
        BrowserNetworkFactory::new(
            BrowserNetworkSettings {
                enabled: true,
                allowed_origins: vec![origin.clone()],
                localhost_development_origin: Some(origin.clone()),
                limits: BrokerLimits {
                    max_total_bytes: capacity,
                    ..Default::default()
                },
                ..Default::default()
            },
            "run-bytes",
        )
        .unwrap(),
    );
    let send = |endpoint: u16, origin: String| async move {
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", endpoint))
            .await
            .unwrap();
        stream
            .write_all(
                format!(
                    "GET {origin}/ HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    origin.trim_start_matches("http://")
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
        response
    };
    let first = factory.start(&NeverCancel).await.unwrap();
    assert!(
        send(first.endpoint_port(), origin.clone())
            .await
            .starts_with(b"HTTP/1.1 200"),
        "the first request must fit the Run budget"
    );
    first.shutdown().await.unwrap();
    let restarted = factory.start(&NeverCancel).await.unwrap();
    assert!(
        !send(restarted.endpoint_port(), origin)
            .await
            .starts_with(b"HTTP/1.1 200"),
        "spent Run bytes must not reset when the broker restarts"
    );
    restarted.shutdown().await.unwrap();
}

#[test]
fn parses_remote_pid_only_for_the_pinned_worker() {
    let pid = parse_pid(b"pid=4242\ncmdline=/opt/saya/node /opt/saya/worker/cli.js\n").unwrap();
    assert_eq!(pid, 4242);
    assert!(parse_pid(b"pid=missing\n").is_none());
    assert!(parse_pid(b"pid=1\ncmdline=node cli.js\n").is_none());
    assert!(parse_pid(b"pid=42\ncmdline=node cli.js --no-sandbox\n").is_none());
    assert!(parse_pid(b"pid=42\ncmdline=node cli.js --disable-setuid-sandbox\n").is_none());
}

#[tokio::test]
async fn remote_cleanup_requires_explicit_confirmation() {
    let settings = IdentityFixture::new().settings;
    let runner = QueueRunner::new(
        [success(b"worker=alive\nrunroot=gone\n".to_vec())]
            .into_iter()
            .collect(),
    );
    let control = RunnerRemoteControl {
        settings: settings.clone(),
        runner: runner.clone(),
        run_root: "/tmp/saya-runs/one".into(),
        pid_file: "/tmp/saya-runs/one/worker.pid".into(),
        cli_js: "/opt/saya/worker/cli.js".into(),
    };
    let failure = control.terminate(true).await.unwrap_err();
    assert_eq!(failure.code, "outcome_unknown");
    assert!(failure.uncertain);
    assert_eq!(runner.scripts()[0], CLEANUP_SCRIPT);

    let runner = QueueRunner::new(
        [success(b"worker=gone\nrunroot=gone\n".to_vec())]
            .into_iter()
            .collect(),
    );
    let control = RunnerRemoteControl {
        settings,
        runner: QueueRunner::new(std::mem::take(&mut *runner.outputs.lock().unwrap())),
        run_root: "/tmp/saya-runs/one".into(),
        pid_file: "/tmp/saya-runs/one/worker.pid".into(),
        cli_js: "/opt/saya/worker/cli.js".into(),
    };
    control.terminate(true).await.unwrap();
}

#[tokio::test]
async fn vm_sandbox_evidence_comes_from_remote_proc() {
    let settings = IdentityFixture::new().settings;
    let runner = QueueRunner::new(
        [success(
            b"renderer=77\ncmdline=/opt/saya/chromium --type=renderer --no-zygote\nnonewprivs=1\nseccomp=2\n".to_vec(),
        )]
        .into_iter()
        .collect(),
    );
    let inspector = VmChromiumSandboxInspector::new(settings.clone(), runner);
    let evidence = inspector
        .inspect(42, b"InitializeSandbox() called", &NeverCancel)
        .await
        .unwrap();
    assert_eq!(evidence.renderer_pid, 77);
    assert!(evidence.no_new_privs);
    assert_eq!(evidence.seccomp_mode, 2);
    assert!(evidence.sandbox_arguments_valid);
    assert!(evidence.stderr_initializes_sandbox);

    let runner = QueueRunner::new(
        [success(
            b"renderer=77\ncmdline=/opt/saya/chromium --type=renderer --no-sandbox\nnonewprivs=1\nseccomp=2\n".to_vec(),
        )]
        .into_iter()
        .collect(),
    );
    let inspector = VmChromiumSandboxInspector::new(settings, runner);
    let failure = inspector.inspect(42, b"", &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
}

#[tokio::test]
async fn vm_sandbox_inspect_connection_requires_live_pid_and_delegates() {
    struct StubConnection {
        pid: Option<u32>,
        stderr: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl WorkerConnection for StubConnection {
        fn protocol_version(&self) -> Option<String> {
            Some("2025-11-25".to_owned())
        }
        fn generation(&self) -> u64 {
            0
        }
        fn process_id(&self) -> Option<u32> {
            self.pid
        }
        fn stderr_tail(&self) -> Vec<u8> {
            self.stderr.clone()
        }
        async fn list_tools(
            &self,
            _cancel: &dyn Cancellation,
        ) -> Result<crate::mcp::McpCatalog, BrowserFailure> {
            unreachable!()
        }
        async fn call_tool(
            &self,
            _name: &str,
            _arguments: serde_json::Value,
            _generation: u64,
            _cancel: &dyn Cancellation,
        ) -> Result<crate::mcp::McpToolResult, BrowserFailure> {
            unreachable!()
        }
        async fn shutdown(&self) -> Result<(), BrowserFailure> {
            Ok(())
        }
    }

    let settings = IdentityFixture::new().settings;
    for missing_pid in [None, Some(0)] {
        let runner = QueueRunner::new(VecDeque::new());
        let inspector = VmChromiumSandboxInspector::new(settings.clone(), runner.clone());
        let failure = inspector
            .inspect_connection(
                &StubConnection {
                    pid: missing_pid,
                    stderr: b"InitializeSandbox".to_vec(),
                },
                &NeverCancel,
            )
            .await
            .unwrap_err();
        assert_eq!(failure.code, "browser_sandbox_unavailable");
    }

    let runner = QueueRunner::new(
        [success(
            b"renderer=77\ncmdline=/opt/saya/chromium --type=renderer --no-zygote\nnonewprivs=1\nseccomp=2\n".to_vec(),
        )]
        .into_iter()
        .collect(),
    );
    let inspector = VmChromiumSandboxInspector::new(settings, runner);
    let evidence = inspector
        .inspect_connection(
            &StubConnection {
                pid: Some(42),
                stderr: b"InitializeSandbox() called".to_vec(),
            },
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(evidence.renderer_pid, 77);
    assert!(evidence.no_new_privs);
    assert_eq!(evidence.seccomp_mode, 2);
    assert!(evidence.sandbox_arguments_valid);
    assert!(evidence.stderr_initializes_sandbox);
}

#[tokio::test]
async fn cancelled_vm_launch_dispatches_no_remote_command() {
    let settings = IdentityFixture::new().settings;
    let runner = QueueRunner::new(VecDeque::new());
    let launcher = VmWorkerLauncher::new(
        settings,
        runner.clone(),
        ExecConfig::default(),
        test_network_factory(),
    );
    let cancel = AlreadyCancelled;
    let request = WorkerLaunchRequest {
        argv: Vec::new(),
        cwd: PathBuf::from("/tmp"),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(1),
        stderr_limit: 4096,
        mcp_limits: McpLimits::default(),
        directories: BrowserSandboxDirectories {
            profile: PathBuf::from("/tmp/p"),
            output: PathBuf::from("/tmp/o"),
            tmp: PathBuf::from("/tmp/t"),
            work: PathBuf::from("/tmp/w"),
            config: PathBuf::from("/tmp/c"),
        },
        execution: None,
        identity: None,
    };
    let failure = match launcher.launch(request, &cancel).await {
        Ok(_) => panic!("cancelled launch must not produce a connection"),
        Err(failure) => failure,
    };
    assert_eq!(failure.code, "cancelled");
    assert_eq!(runner.calls(), 0);
}

struct AlreadyCancelled;
impl Cancellation for AlreadyCancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

#[cfg(target_os = "macos")]
struct SwitchCancel(Arc<AtomicBool>);
#[cfg(target_os = "macos")]
impl Cancellation for SwitchCancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

/// Runner that flips cancellation after it confirms the remote Run root.
#[cfg(target_os = "macos")]
struct PrepareThenCancel {
    cancel: Arc<AtomicBool>,
    calls: AtomicUsize,
}

#[cfg(target_os = "macos")]
#[async_trait]
impl VmCommandRunner for PrepareThenCancel {
    async fn run(
        &self,
        spec: VmCommandSpec<'_>,
        _cancel: &dyn Cancellation,
    ) -> Result<VmCommandOutput, BrowserFailure> {
        let index = self.calls.fetch_add(1, Ordering::AcqRel);
        if index == 0 {
            self.cancel.store(true, Ordering::Release);
            Ok(success(spec.args[0].clone().into_bytes()))
        } else {
            Ok(success(b"worker=absent\nrunroot=gone\n".to_vec()))
        }
    }
}

#[cfg(target_os = "macos")]
struct HangPrepare {
    started: tokio::sync::Notify,
    calls: AtomicUsize,
}

#[cfg(target_os = "macos")]
#[async_trait]
impl VmCommandRunner for HangPrepare {
    async fn run(
        &self,
        _spec: VmCommandSpec<'_>,
        _cancel: &dyn Cancellation,
    ) -> Result<VmCommandOutput, BrowserFailure> {
        let index = self.calls.fetch_add(1, Ordering::AcqRel);
        if index == 0 {
            self.started.notify_one();
            std::future::pending::<()>().await;
            unreachable!("the hanging prepare never completes");
        }
        Ok(success(b"worker=absent\nrunroot=gone\n".to_vec()))
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn dropping_the_launch_future_keeps_the_remote_cleanup_handle() {
    let fixture = LocalFixture::new();
    let runner = Arc::new(HangPrepare {
        started: tokio::sync::Notify::new(),
        calls: AtomicUsize::new(0),
    });
    let launcher = Arc::new(VmWorkerLauncher::new(
        fixture.settings.clone(),
        runner.clone(),
        ExecConfig::default(),
        test_network_factory(),
    ));
    let workspace = tempfile::tempdir().unwrap();
    let policy = host_policy(&fixture.settings, workspace.path(), &fixture.paths, None).unwrap();
    let environment = execution::prepare(&policy).unwrap();
    let directories = BrowserSandboxDirectories {
        profile: workspace.path().join("profile"),
        output: workspace.path().join("output"),
        tmp: workspace.path().join("tmp"),
        work: workspace.path().join("work"),
        config: workspace.path().join("config"),
    };
    for directory in [
        &directories.profile,
        &directories.output,
        &directories.tmp,
        &directories.work,
        &directories.config,
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    let request = WorkerLaunchRequest {
        argv: Vec::new(),
        cwd: directories.work.clone(),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(1),
        stderr_limit: 4096,
        mcp_limits: McpLimits::default(),
        directories,
        execution: Some(environment.clone()),
        identity: Some(VerifiedWorkerIdentity {
            node: PathBuf::from("/opt/saya/node"),
            worker_root: PathBuf::from("/opt/saya/worker"),
            cli_js: PathBuf::from("/opt/saya/worker/node_modules/@playwright/mcp/cli.js"),
            chromium: PathBuf::from(
                "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell",
            ),
            manifest_sha256: fixture.settings.identity_sha256.clone().unwrap(),
            local_images: None,
        }),
    };
    let task_launcher = launcher.clone();
    let handle = tokio::spawn(async move {
        let cancel = NeverCancel;
        task_launcher.launch(request, &cancel).await
    });
    tokio::time::timeout(Duration::from_secs(5), runner.started.notified())
        .await
        .expect("the launch must reach the hanging prepare");
    handle.abort();
    let _ = handle.await;
    assert_eq!(
        launcher.pending_entries().len(),
        1,
        "the dropped launch must keep the remote cleanup handle"
    );
    launcher.retry_pending_cleanup().await.unwrap();
    assert!(launcher.pending_entries().is_empty());
    assert_eq!(runner.calls.load(Ordering::Acquire), 2);
    let _ = environment.cleanup();
}

#[cfg(target_os = "macos")]
struct LocalFixture {
    _directory: tempfile::TempDir,
    settings: BrowserVmSettings,
    paths: VmLocalPaths,
}

#[cfg(target_os = "macos")]
impl LocalFixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let identity = directory.path().join("id_ed25519");
        let known_hosts = directory.path().join("known_hosts");
        fs::write(&identity, b"private key placeholder").unwrap();
        fs::set_permissions(&identity, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(&known_hosts, b"127.0.0.1 ssh-ed25519 AAAA\n").unwrap();
        fs::set_permissions(&known_hosts, fs::Permissions::from_mode(0o644)).unwrap();
        let mut settings = IdentityFixture::new().settings;
        settings.ssh = Some(PathBuf::from("/usr/bin/ssh"));
        settings.colima = Some(PathBuf::from("/usr/bin/ssh"));
        settings.identity_file = Some(identity);
        settings.known_hosts = Some(known_hosts);
        let paths = settings.preflight_local().unwrap();
        Self {
            _directory: directory,
            settings,
            paths,
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn canonical_identity_paths_are_revalidated_after_symlink_resolution() {
    let fixture = LocalFixture::new();
    let root = fixture._directory.path();
    let target = root.join("key\nProxyCommand=evil");
    fs::write(&target, b"private key placeholder").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
    let link = root.join("identity-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut settings = fixture.settings.clone();
    settings.identity_file = Some(link);
    assert!(
        settings.preflight_local().is_err(),
        "a symlink target containing a newline must not reach ssh_config"
    );

    let known_target = root.join("known hosts\nProxyCommand=evil");
    fs::write(&known_target, b"127.0.0.1 ssh-ed25519 AAAA\n").unwrap();
    fs::set_permissions(&known_target, fs::Permissions::from_mode(0o644)).unwrap();
    let known_link = root.join("known-hosts-link");
    std::os::unix::fs::symlink(&known_target, &known_link).unwrap();
    settings = fixture.settings.clone();
    settings.known_hosts = Some(known_link);
    assert!(settings.preflight_local().is_err());
}

#[test]
fn bracketed_loopback_host_is_normalized_for_config_and_argv() {
    assert_eq!(normalized_loopback_host("[::1]").as_deref(), Some("::1"));
    assert_eq!(
        normalized_loopback_host("127.0.0.1").as_deref(),
        Some("127.0.0.1")
    );
    assert!(normalized_loopback_host("[::2]").is_none());
    assert!(normalized_loopback_host(" [::1]").is_none());

    let mut settings = IdentityFixture::new().settings;
    settings.host = Some("[::1]".into());
    settings.validate().unwrap();
    let paths = VmLocalPaths {
        ssh: PathBuf::from("/usr/bin/ssh"),
        identity_file: PathBuf::from("/private/tmp/key"),
        known_hosts: PathBuf::from("/private/tmp/known_hosts"),
        colima: PathBuf::from("/usr/local/bin/colima"),
    };
    let config = strict_ssh_config(&settings, &paths).unwrap();
    assert!(config.contains("HostName ::1"));
    assert!(!config.contains("[::1]"));
    let argv = ssh_argv(
        &settings,
        &paths,
        Path::new("/private/tmp/ssh_config"),
        PROBE_SCRIPT,
        &[],
        None,
    )
    .unwrap();
    assert!(argv.iter().any(|value| value == "::1"));
    assert!(!argv.iter().any(|value| value.contains("[::1]")));
}

#[test]
fn command_outcomes_distinguish_cancel_before_dispatch_from_unknown() {
    let cancelled = VmCommandOutput {
        status: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
        timed_out: false,
        cancelled: true,
    };
    let before = vm_command_outcome(cancelled.clone(), false, "probe").unwrap_err();
    assert_eq!(before.code, "cancelled");
    assert!(!before.uncertain);
    let after = vm_command_outcome(cancelled, true, "probe").unwrap_err();
    assert_eq!(after.code, "outcome_unknown");
    assert!(after.uncertain);

    let timed_out = VmCommandOutput {
        status: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
        timed_out: true,
        cancelled: false,
    };
    let failure = vm_command_outcome(timed_out, true, "cleanup").unwrap_err();
    assert_eq!(failure.code, "outcome_unknown");
    assert!(failure.uncertain);

    let known = vm_command_result(
        Err(BrowserFailure::new("worker_spawn", "spawn failed")),
        "identity probe",
    )
    .unwrap_err();
    assert_eq!(known.code, "worker_spawn");
    assert!(!known.uncertain, "a pre-dispatch failure stays known");
    let transport = vm_command_result(
        Err(BrowserFailure::uncertain("transport lost after dispatch")),
        "identity probe",
    )
    .unwrap_err();
    assert_eq!(transport.code, "outcome_unknown");
    assert!(transport.uncertain);
}

#[tokio::test]
async fn cancelled_or_timed_out_identity_probe_is_not_an_identity_failure() {
    let fixture = IdentityFixture::new();
    let mut outputs = fixture.outputs();
    outputs[0] = VmCommandOutput {
        status: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
        timed_out: true,
        cancelled: false,
    };
    let runner = QueueRunner::new(outputs);
    let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner);
    let failure = verifier.verify(&NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "outcome_unknown");
    assert!(failure.uncertain);

    let fixture = IdentityFixture::new();
    let mut outputs = fixture.outputs();
    outputs[0] = VmCommandOutput {
        status: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
        timed_out: false,
        cancelled: true,
    };
    let runner = QueueRunner::new(outputs);
    let verifier = VmWorkerIdentityVerifier::new(fixture.settings.clone(), runner);
    let failure = verifier.verify(&NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "outcome_unknown");
}

#[test]
fn prepare_script_creates_all_launch_directories_with_private_modes() {
    let directory = tempfile::tempdir().unwrap();
    let run_root = directory.path().join("run");
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg(PREPARE_RUN_SCRIPT)
        .arg("saya-browser-vm-cleanup")
        .arg(&run_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write as _;
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"{\"config\":true}")
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        run_root.display().to_string()
    );

    let mut expected = vec![
        "profile",
        "output",
        "tmp",
        "work",
        "config",
        "home",
        "home/cache",
        "home/data",
        "home/state",
        "home/runtime",
        "s",
    ];
    // Every XDG/HOME/TMPDIR/socket directory exported by the launch script
    // must already exist under the Run root.
    for line in LAUNCH_SCRIPT.lines() {
        let line = line.trim();
        let Some(value) = line.strip_prefix("export ") else {
            continue;
        };
        let Some((name, path)) = value.split_once('=') else {
            continue;
        };
        if !matches!(name, "HOME" | "TMPDIR" | "PWTEST_SOCKETS_DIR") && !name.starts_with("XDG_") {
            continue;
        }
        let path = path.trim_matches('"');
        if let Some(relative) = path.strip_prefix("$1/") {
            expected.push(relative);
        }
    }
    expected.sort_unstable();
    expected.dedup();
    for relative in expected {
        let path = run_root.join(relative);
        assert!(path.is_dir(), "missing prepared directory {relative}");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{relative} mode is {mode:o}");
    }
    assert_eq!(
        fs::read(run_root.join("config/config.json")).unwrap(),
        b"{\"config\":true}"
    );
}

struct CountingControl {
    calls: AtomicUsize,
    fail_times: AtomicUsize,
}

#[async_trait]
impl VmRemoteControl for CountingControl {
    async fn terminate(&self, _expect_worker: bool) -> Result<(), BrowserFailure> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        if self.fail_times.load(Ordering::Acquire) > 0 {
            self.fail_times.fetch_sub(1, Ordering::AcqRel);
            return Err(remote_cleanup_failure("test cleanup failure"));
        }
        Ok(())
    }
}

fn counting_control(fail_times: usize) -> Arc<CountingControl> {
    Arc::new(CountingControl {
        calls: AtomicUsize::new(0),
        fail_times: AtomicUsize::new(fail_times),
    })
}

struct DelayedControl {
    calls: AtomicUsize,
    delay_ms: AtomicU64,
}

impl DelayedControl {
    fn new(delay_ms: u64) -> Arc<Self> {
        Arc::new(Self {
            calls: AtomicUsize::new(0),
            delay_ms: AtomicU64::new(delay_ms),
        })
    }
}

#[async_trait]
impl VmRemoteControl for DelayedControl {
    async fn terminate(&self, _expect_worker: bool) -> Result<(), BrowserFailure> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        let delay = self.delay_ms.load(Ordering::Acquire);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn outer_timeout_keeps_every_pending_cleanup_handle_for_a_later_retry() {
    let settings = IdentityFixture::new().settings;
    let launcher = VmWorkerLauncher::new(
        settings,
        QueueRunner::new(VecDeque::new()),
        ExecConfig::default(),
        test_network_factory(),
    );
    let first = DelayedControl::new(30_000);
    let second = DelayedControl::new(30_000);

    // A ten-second outer timeout drops the cleanup future while the
    // remote command is still in flight. Both the current and the later
    // handle must stay registered.
    let timed = tokio::time::timeout(
        Duration::from_secs(10),
        launcher.cleanup_or_retain(first.clone(), false, remote_failure("first launch failed")),
    )
    .await;
    assert!(timed.is_err(), "the outer timeout must drop the future");
    let timed = tokio::time::timeout(
        Duration::from_secs(10),
        launcher.cleanup_or_retain(second.clone(), true, remote_failure("second launch failed")),
    )
    .await;
    assert!(timed.is_err(), "the outer timeout must drop the future");

    {
        let entries = launcher.pending_entries();
        assert_eq!(entries.len(), 2, "neither handle may be dropped");
        assert!(!entries[0].expect_worker.load(Ordering::Acquire));
        assert!(entries[1].expect_worker.load(Ordering::Acquire));
    }

    first.delay_ms.store(0, Ordering::Release);
    second.delay_ms.store(0, Ordering::Release);
    launcher.retry_pending_cleanup().await.unwrap();
    assert!(launcher.pending_entries().is_empty());
    assert_eq!(first.calls.load(Ordering::Acquire), 2);
    assert_eq!(second.calls.load(Ordering::Acquire), 2);
}

#[test]
fn drop_cleanup_without_a_tokio_runtime_reports_unknown_not_success() {
    let control = counting_control(0);
    let cleanup = Arc::new(ConnectionCleanup::new(control.clone()));
    assert!(!schedule_drop_cleanup(&cleanup));
    assert_eq!(control.calls.load(Ordering::Acquire), 0);
    assert!(
        cleanup.pending(),
        "without a runtime the cleanup stays unknown"
    );
}

#[tokio::test]
async fn drop_cleanup_spawns_bounded_work_when_a_runtime_exists() {
    let control = counting_control(0);
    let cleanup = Arc::new(ConnectionCleanup::new(control.clone()));
    assert!(schedule_drop_cleanup(&cleanup));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(control.calls.load(Ordering::Acquire), 1);
    assert!(!cleanup.pending());
}

#[tokio::test]
async fn failed_cleanup_stays_pending_until_a_retry_confirms_it() {
    let control = counting_control(1);
    let cleanup = ConnectionCleanup::new(control.clone());
    assert!(cleanup.terminate(true).await.is_err());
    assert!(cleanup.pending());
    cleanup.terminate(true).await.unwrap();
    assert!(!cleanup.pending());
    assert_eq!(control.calls.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn failed_launch_cleanup_is_retained_by_the_launcher_and_retried() {
    let settings = IdentityFixture::new().settings;
    let runner = QueueRunner::new(
        [
            success(b"worker=alive\nrunroot=gone\n".to_vec()),
            success(b"worker=alive\nrunroot=gone\n".to_vec()),
        ]
        .into_iter()
        .collect(),
    );
    let control: Arc<dyn VmRemoteControl> = Arc::new(RunnerRemoteControl {
        settings: settings.clone(),
        runner: runner.clone(),
        run_root: "/tmp/saya-runs/retained".into(),
        pid_file: "/tmp/saya-runs/retained/worker.pid".into(),
        cli_js: "/opt/saya/worker/cli.js".into(),
    });
    let launcher = VmWorkerLauncher::new(
        settings,
        runner.clone(),
        ExecConfig::default(),
        test_network_factory(),
    );
    let failure = launcher
        .cleanup_or_retain(control, false, remote_failure("prepare failed"))
        .await;
    assert_eq!(failure.code, "outcome_unknown");
    assert!(failure.uncertain);

    let retry = launcher.retry_pending_cleanup().await.unwrap_err();
    assert!(retry.uncertain);
    runner
        .outputs
        .lock()
        .unwrap()
        .push_back(success(b"worker=absent\nrunroot=gone\n".to_vec()));
    launcher.retry_pending_cleanup().await.unwrap();
}

#[test]
fn cleanup_script_treats_exited_workers_as_gone_and_removes_the_run_root() {
    // PHP-style zombie detection is documented in the script: a State of
    // Z counts as gone so a cleanly exited worker does not block removal.
    assert!(CLEANUP_SCRIPT.contains("\"$state\" != \"Z\""));
    assert!(CLEANUP_SCRIPT.contains("group_live"));

    let directory = tempfile::tempdir().unwrap();
    let run_root = directory.path().join("run");
    fs::create_dir(&run_root).unwrap();
    let pid_file = run_root.join("worker.pid");
    fs::write(&pid_file, b"999999").unwrap();
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(CLEANUP_SCRIPT)
        .arg("saya-browser-vm-cleanup")
        .arg(&run_root)
        .arg(&pid_file)
        .arg("/opt/saya/worker/cli.js")
        .arg("0")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{:?} {text}", output);
    assert!(
        text.contains("worker=gone") || text.contains("worker=absent"),
        "{text}"
    );
    assert!(text.contains("runroot=gone"), "{text}");
    assert!(!run_root.exists());
}

fn write_fake_proc(proc_root: &Path, pid: u32, command: &str, state: &str, children: &[u32]) {
    let directory = proc_root.join(pid.to_string());
    let task = directory.join("task").join(pid.to_string());
    fs::create_dir_all(&task).unwrap();
    fs::write(directory.join("cmdline"), command).unwrap();
    fs::write(directory.join("status"), format!("State:\t{state}\n")).unwrap();
    let children = children
        .iter()
        .map(|child| format!("{child} "))
        .collect::<String>();
    fs::write(task.join("children"), children).unwrap();
}

/// Start a process that is reparented after the launcher shell exits, so
/// killing it does not leave a zombie owned by the test process.
fn spawn_orphan_sleep() -> u32 {
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 30 >/dev/null 2>&1 & echo $!")
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap()
}

fn pid_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn wait_for_pid_gone(pid: u32) -> bool {
    for _ in 0..50 {
        if !pid_is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    !pid_is_alive(pid)
}

fn run_cleanup_script(
    run_root: &Path,
    pid_file: &Path,
    cli: &str,
    expect: &str,
    proc_root: &Path,
) -> std::process::Output {
    std::process::Command::new("sh")
        .arg("-c")
        .arg(CLEANUP_SCRIPT)
        .arg("saya-browser-vm-cleanup")
        .arg(run_root)
        .arg(pid_file)
        .arg(cli)
        .arg(expect)
        .arg(proc_root)
        .output()
        .unwrap()
}

#[test]
fn cleanup_script_scans_when_the_pid_file_is_missing() {
    let directory = tempfile::tempdir().unwrap();
    let run_root = directory.path().join("run");
    fs::create_dir(&run_root).unwrap();
    let proc_root = directory.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let pid_file = run_root.join("worker.pid");

    let output = run_cleanup_script(
        &run_root,
        &pid_file,
        "/opt/saya/worker/cli.js",
        "1",
        &proc_root,
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{:?} {text}", output);
    assert!(text.contains("worker=absent"), "{text}");
    assert!(!text.contains("worker=unknown"), "{text}");
    assert!(text.contains("runroot=gone"), "{text}");
    assert!(!run_root.exists());
}

#[test]
fn cleanup_script_terminates_related_processes_when_the_pid_file_is_missing() {
    let directory = tempfile::tempdir().unwrap();
    // Use the production Run-root shape: the worker and Chromium argv
    // contain this path, which also contains the `saya-browser-vm` prefix.
    // The cleanup shell's own label must be distinct so the scan does not
    // exclude the very processes it must terminate.
    let run_root = directory
        .path()
        .join("saya-runs")
        .join("saya-browser-vm-0123456789abcdef");
    fs::create_dir_all(&run_root).unwrap();
    let proc_root = directory.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let pid_file = run_root.join("worker.pid");
    assert!(CLEANUP_SCRIPT.contains("*saya-browser-vm-cleanup*"));

    let worker = spawn_orphan_sleep();
    let descendant = spawn_orphan_sleep();
    write_fake_proc(
        &proc_root,
        worker,
        &format!("node {}/config/config.json --sandbox", run_root.display()),
        "S",
        &[descendant],
    );
    write_fake_proc(
        &proc_root,
        descendant,
        &format!("chrome --user-data-dir={}/profile", run_root.display()),
        "S",
        &[],
    );

    let output = run_cleanup_script(
        &run_root,
        &pid_file,
        "/opt/saya/worker/cli.js",
        "1",
        &proc_root,
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{:?} {text}", output);
    assert!(text.contains("worker=gone"), "{text}");
    assert!(text.contains("runroot=gone"), "{text}");
    assert!(!run_root.exists());
    assert!(
        wait_for_pid_gone(worker),
        "scanned worker {worker} survived"
    );
    assert!(
        wait_for_pid_gone(descendant),
        "scanned descendant {descendant} survived"
    );
}

#[test]
fn cleanup_script_removes_the_run_root_after_a_crashed_worker() {
    let directory = tempfile::tempdir().unwrap();
    let run_root = directory.path().join("run");
    fs::create_dir(&run_root).unwrap();
    let proc_root = directory.path().join("proc");
    fs::create_dir(&proc_root).unwrap();

    let crashed = spawn_orphan_sleep();
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(crashed.to_string())
        .status();
    assert!(wait_for_pid_gone(crashed));
    let pid_file = run_root.join("worker.pid");
    fs::write(&pid_file, crashed.to_string()).unwrap();

    let output = run_cleanup_script(
        &run_root,
        &pid_file,
        "/opt/saya/worker/cli.js",
        "1",
        &proc_root,
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{:?} {text}", output);
    assert!(text.contains("worker=absent"), "{text}");
    assert!(text.contains("runroot=gone"), "{text}");
    assert!(!run_root.exists());
}

#[test]
fn cleanup_script_terminates_a_live_worker_and_its_descendants_from_the_pid_file() {
    let directory = tempfile::tempdir().unwrap();
    let run_root = directory.path().join("run");
    fs::create_dir(&run_root).unwrap();
    let proc_root = directory.path().join("proc");
    fs::create_dir(&proc_root).unwrap();
    let pid_file = run_root.join("worker.pid");

    let worker = spawn_orphan_sleep();
    let descendant = spawn_orphan_sleep();
    fs::write(&pid_file, worker.to_string()).unwrap();
    write_fake_proc(
        &proc_root,
        worker,
        "node /opt/saya/node /opt/saya/worker/cli.js --sandbox",
        "S",
        &[descendant],
    );
    write_fake_proc(
        &proc_root,
        descendant,
        &format!("chrome --user-data-dir={}/profile", run_root.display()),
        "S",
        &[],
    );

    let output = run_cleanup_script(
        &run_root,
        &pid_file,
        "/opt/saya/worker/cli.js",
        "1",
        &proc_root,
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{:?} {text}", output);
    assert!(text.contains("worker=gone"), "{text}");
    assert!(text.contains("runroot=gone"), "{text}");
    assert!(!run_root.exists());
    assert!(wait_for_pid_gone(worker), "worker {worker} survived");
    assert!(
        wait_for_pid_gone(descendant),
        "descendant {descendant} survived"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn cancel_after_remote_prepare_cleans_the_run_root() {
    let fixture = LocalFixture::new();
    let cancel = Arc::new(AtomicBool::new(false));
    let runner = Arc::new(PrepareThenCancel {
        cancel: cancel.clone(),
        calls: AtomicUsize::new(0),
    });
    let launcher = VmWorkerLauncher::new(
        fixture.settings.clone(),
        runner.clone(),
        ExecConfig::default(),
        test_network_factory(),
    );
    let workspace = tempfile::tempdir().unwrap();
    let policy = host_policy(&fixture.settings, workspace.path(), &fixture.paths, None).unwrap();
    let environment = execution::prepare(&policy).unwrap();
    let directories = BrowserSandboxDirectories {
        profile: workspace.path().join("profile"),
        output: workspace.path().join("output"),
        tmp: workspace.path().join("tmp"),
        work: workspace.path().join("work"),
        config: workspace.path().join("config"),
    };
    for directory in [
        &directories.profile,
        &directories.output,
        &directories.tmp,
        &directories.work,
        &directories.config,
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    let request = WorkerLaunchRequest {
        argv: Vec::new(),
        cwd: directories.work.clone(),
        env: BTreeMap::new(),
        timeout: Duration::from_secs(1),
        stderr_limit: 4096,
        mcp_limits: McpLimits::default(),
        directories,
        execution: Some(environment.clone()),
        identity: Some(VerifiedWorkerIdentity {
            node: PathBuf::from("/opt/saya/node"),
            worker_root: PathBuf::from("/opt/saya/worker"),
            cli_js: PathBuf::from("/opt/saya/worker/node_modules/@playwright/mcp/cli.js"),
            chromium: PathBuf::from(
                "/opt/saya/browsers/chromium_headless_shell-1254/chrome-headless-shell-linux64/chrome-headless-shell",
            ),
            manifest_sha256: fixture.settings.identity_sha256.clone().unwrap(),
            local_images: None,
        }),
    };
    let cancel_token = SwitchCancel(cancel);
    let failure = match launcher.launch(request, &cancel_token).await {
        Ok(_) => panic!("cancelled launch must not produce a connection"),
        Err(failure) => failure,
    };
    assert_eq!(failure.code, "cancelled");
    assert_eq!(runner.calls.load(Ordering::Acquire), 2);
    let _ = environment.cleanup();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn vm_factory_creates_and_releases_fresh_host_environments() {
    let fixture = LocalFixture::new();
    let factory = VmBrowserEnvironmentFactory::new(fixture.settings.clone()).unwrap();
    let first = factory.prepare().unwrap();
    let first_directories = first.directories().clone();
    assert!(first_directories.work.is_dir());
    let second = factory.prepare().unwrap();
    let second_directories = second.directories().clone();
    assert_ne!(first_directories.work, second_directories.work);
    assert!(second_directories.work.is_dir());
    first.release().await.unwrap();
    second.release().await.unwrap();
    assert!(!first_directories.work.exists());
    assert!(!second_directories.work.exists());
}
