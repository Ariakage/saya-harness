use super::*;

// ---------------------------------------------------------------------------
// Identity verification.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ProbeRecord {
    pub(super) size: u64,
    pub(super) mode: u32,
    pub(super) kind: String,
    pub(super) digest: String,
}

fn parse_probe(output: &[u8], count: usize) -> Result<Vec<ProbeRecord>, BrowserFailure> {
    let text = std::str::from_utf8(output)
        .map_err(|_| identity_failure("Browser VM identity probe returned non-UTF-8 text"))?;
    let mut lines = text.lines();
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let stat = lines
            .next()
            .ok_or_else(|| identity_failure("Browser VM identity probe is incomplete"))?;
        let mut fields = stat.split('|');
        let size = fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| identity_failure("Browser VM identity size is invalid"))?;
        let mode = fields
            .next()
            .and_then(|value| u32::from_str_radix(value, 8).ok())
            .ok_or_else(|| identity_failure("Browser VM identity mode is invalid"))?;
        let kind = fields
            .next()
            .ok_or_else(|| identity_failure("Browser VM identity type is missing"))?
            .to_owned();
        if fields.next().is_some() {
            return Err(identity_failure(
                "Browser VM identity probe line is malformed",
            ));
        }
        let hash_line = lines
            .next()
            .ok_or_else(|| identity_failure("Browser VM identity digest is missing"))?;
        let digest = hash_line
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        if !valid_sha256(&digest) {
            return Err(identity_failure("Browser VM identity digest is invalid"));
        }
        records.push(ProbeRecord {
            size,
            mode,
            kind,
            digest,
        });
    }
    if lines.next().is_some() {
        return Err(identity_failure(
            "Browser VM identity probe returned extra data",
        ));
    }
    Ok(records)
}

fn is_regular(kind: &str) -> bool {
    kind == "regular file" || kind == "regular empty file"
}

fn check_probe(
    label: &'static str,
    record: &ProbeRecord,
    expected_size: u64,
    expected_digest: &str,
    executable: bool,
) -> Result<(), BrowserFailure> {
    if !is_regular(&record.kind) {
        return Err(identity_failure(format!(
            "Browser VM {label} is not a regular file"
        )));
    }
    if record.size != expected_size || record.size > MAX_IDENTITY_FILE_BYTES {
        return Err(identity_failure(format!(
            "Browser VM {label} size does not match its manifest"
        )));
    }
    if record.digest != expected_digest {
        return Err(identity_failure(format!(
            "Browser VM {label} digest does not match its manifest"
        )));
    }
    if record.mode & 0o022 != 0 || record.mode & 0o6000 != 0 || record.mode & 0o400 == 0 {
        return Err(identity_failure(format!(
            "Browser VM {label} permissions are unsafe"
        )));
    }
    if executable && record.mode & 0o111 == 0 {
        return Err(identity_failure(format!(
            "Browser VM {label} is not executable"
        )));
    }
    Ok(())
}

fn parse_manifest(bytes: &[u8]) -> Result<IdentityManifest, BrowserFailure> {
    let manifest: IdentityManifest = serde_json::from_slice(bytes)
        .map_err(|_| identity_failure("Browser VM identity manifest is invalid"))?;
    if manifest.schema_version != 2 {
        return Err(identity_failure(
            "Browser VM identity manifest schema version is unsupported",
        ));
    }
    manifest
        .installation_trees
        .validate()
        .map_err(|_| identity_failure("Browser VM installation tree identity is invalid"))?;
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
            return Err(identity_failure(
                "Browser VM identity manifest contains an invalid digest",
            ));
        }
    }
    Ok(manifest)
}

pub(super) fn headless_shell_path(path: &str) -> bool {
    let path = Path::new(path);
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
    let revision_directory = format!("chromium_headless_shell-{CHROMIUM_REVISION}");
    name == "chrome-headless-shell"
        && path
            .components()
            .any(|component| component.as_os_str().to_str() == Some(revision_directory.as_str()))
}

/// Verify the remote manifest and every named file through the controlled VM
/// channel. Digests, sizes, permissions, versions and the headless-shell path
/// are checked locally against the returned bytes; a successful remote exit
/// status alone is never accepted.
pub struct VmWorkerIdentityVerifier {
    pub(super) settings: BrowserVmSettings,
    pub(super) runner: Arc<dyn VmCommandRunner>,
}

impl VmWorkerIdentityVerifier {
    pub fn new(settings: BrowserVmSettings, runner: Arc<dyn VmCommandRunner>) -> Self {
        Self { settings, runner }
    }

    pub(super) async fn read_remote_file(
        &self,
        path: &str,
        timeout: Duration,
        cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        let args = [path.to_owned()];
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: CAT_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM identity file read",
        )?;
        if !output.succeeded() {
            return Err(identity_failure("Browser VM identity file cannot be read"));
        }
        Ok(output.stdout)
    }
}

fn verify_manifest_probe_records(
    records: &[ProbeRecord],
    manifest: &IdentityManifest,
) -> Result<(), BrowserFailure> {
    let expected = [
        (
            "node",
            manifest.node.size,
            manifest.node.sha256.as_str(),
            true,
        ),
        (
            "worker CLI",
            manifest.worker_cli_js.size,
            manifest.worker_cli_js.sha256.as_str(),
            false,
        ),
        (
            "MCP package",
            manifest.mcp_package.size,
            manifest.mcp_package.sha256.as_str(),
            false,
        ),
        (
            "Playwright package",
            manifest.playwright_package.size,
            manifest.playwright_package.sha256.as_str(),
            false,
        ),
        (
            "Playwright core package",
            manifest.playwright_core_package.size,
            manifest.playwright_core_package.sha256.as_str(),
            false,
        ),
        (
            "browsers.json",
            manifest.playwright_core_browsers_json.size,
            manifest.playwright_core_browsers_json.sha256.as_str(),
            false,
        ),
        (
            "Chromium",
            manifest.chromium.size,
            manifest.chromium.sha256.as_str(),
            true,
        ),
    ];
    for ((label, size, sha256, executable), record) in expected.into_iter().zip(&records[1..]) {
        check_probe(label, record, size, sha256, executable)?;
    }
    Ok(())
}

impl VmWorkerIdentityVerifier {
    async fn read_and_verify_remote_manifest(
        &self,
        path: &str,
        expected_sha256: &str,
        record: &ProbeRecord,
        timeout: Duration,
        cancel: &dyn Cancellation,
    ) -> Result<IdentityManifest, BrowserFailure> {
        let manifest_bytes = self.read_remote_file(path, timeout, cancel).await?;
        if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES
            || manifest_bytes.len() as u64 != record.size
            || sha256_hex(&manifest_bytes) != expected_sha256
            || record.digest != expected_sha256
        {
            return Err(identity_failure(
                "Browser VM identity manifest digest does not match the host input",
            ));
        }
        if !is_regular(&record.kind) {
            return Err(identity_failure(
                "Browser VM identity manifest is not a regular file",
            ));
        }
        if record.mode & 0o022 != 0 || record.mode & 0o6000 != 0 || record.mode & 0o400 == 0 {
            return Err(identity_failure(
                "Browser VM identity manifest permissions are unsafe",
            ));
        }
        parse_manifest(&manifest_bytes)
    }

    async fn verify_remote_packages(
        &self,
        mcp_package: &str,
        playwright_package: &str,
        playwright_core: &str,
        manifest: &IdentityManifest,
        timeout: Duration,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        // Inspect the returned bytes instead of trusting the remote exit code.
        for (path, name, expected) in [
            (mcp_package, "@playwright/mcp", &manifest.mcp_package),
            (
                playwright_package,
                "playwright",
                &manifest.playwright_package,
            ),
            (
                playwright_core,
                "playwright-core",
                &manifest.playwright_core_package,
            ),
        ] {
            let bytes = self.read_remote_file(path, timeout, cancel).await?;
            if bytes.len() as u64 != expected.size || sha256_hex(&bytes) != expected.sha256 {
                return Err(identity_failure(
                    "Browser VM package bytes changed during verification",
                ));
            }
            let package = parse_package(&bytes, name)
                .map_err(|_| identity_failure("Browser VM package metadata is invalid"))?;
            if package.version != expected.version {
                return Err(identity_failure(
                    "Browser VM package version does not match its manifest",
                ));
            }
        }
        if manifest.mcp_package.version != PLAYWRIGHT_MCP_VERSION {
            return Err(identity_failure(
                "Browser VM @playwright/mcp version does not match the pinned version",
            ));
        }
        if manifest.playwright_package.version != PLAYWRIGHT_VERSION
            || manifest.playwright_core_package.version != PLAYWRIGHT_VERSION
        {
            return Err(identity_failure(
                "Browser VM Playwright version does not match the pinned version",
            ));
        }
        Ok(())
    }

    async fn verify_remote_browsers_json(
        &self,
        browsers_json: &str,
        manifest: &IdentityManifest,
        timeout: Duration,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        let browsers_bytes = self
            .read_remote_file(browsers_json, timeout, cancel)
            .await?;
        if browsers_bytes.len() as u64 != manifest.playwright_core_browsers_json.size
            || sha256_hex(&browsers_bytes) != manifest.playwright_core_browsers_json.sha256
        {
            return Err(identity_failure(
                "Browser VM browsers.json changed during verification",
            ));
        }
        let browsers = parse_browsers(&browsers_bytes)
            .map_err(|_| identity_failure("Browser VM browsers.json is invalid"))?;
        let record = browsers
            .browsers
            .iter()
            .find(|record| record.name == "chromium")
            .ok_or_else(|| {
                identity_failure("Browser VM browsers.json does not declare Chromium")
            })?;
        let revision = record
            .revision
            .parse::<u32>()
            .map_err(|_| identity_failure("Browser VM Chromium revision is invalid"))?;
        if revision != CHROMIUM_REVISION
            || record.browser_version.as_deref() != Some(CHROMIUM_VERSION)
            || manifest.chromium.revision != CHROMIUM_REVISION
            || manifest.chromium.version != CHROMIUM_VERSION
        {
            return Err(identity_failure(
                "Browser VM Chromium version does not match the pinned version",
            ));
        }
        Ok(())
    }

    async fn verify_remote_trees_and_node_version(
        &self,
        remote: &RemotePaths,
        manifest: &IdentityManifest,
        timeout: Duration,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        let parent = |path: &str| {
            Path::new(path)
                .parent()
                .and_then(Path::to_str)
                .map(str::to_owned)
                .ok_or_else(|| identity_failure("Browser VM installation root is missing"))
        };
        let tree_args = [
            remote.node.clone(),
            parent(&remote.node)?,
            remote.worker_root.clone(),
            parent(&remote.chromium)?,
            self.settings.connect_timeout_ms.to_string(),
        ];
        let tree_probe = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: INSTALLATION_PROBE_SCRIPT,
                        args: &tree_args,
                        stdin: Some(INSTALLATION_PROBE.as_bytes()),
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 4096,
                    },
                    cancel,
                )
                .await,
            "Browser VM installation tree probe",
        )?;
        if !tree_probe.succeeded()
            || !tree_probe.stderr.is_empty()
            || tree_probe.stdout.len() > 4096
        {
            return Err(identity_failure(
                "Browser VM installation tree probe failed",
            ));
        }
        let trees: worker::InstallationTrees = serde_json::from_slice(&tree_probe.stdout)
            .map_err(|_| identity_failure("Browser VM installation tree probe is invalid"))?;
        trees
            .validate()
            .map_err(|_| identity_failure("Browser VM installation tree probe is invalid"))?;
        if trees != manifest.installation_trees {
            return Err(identity_failure(
                "Browser VM installation tree differs from its manifest",
            ));
        }

        let version_args = [remote.node.clone()];
        let version = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: NODE_VERSION_SCRIPT,
                        args: &version_args,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM Node version probe",
        )?;
        if !version.succeeded() {
            return Err(identity_failure("Browser VM Node version cannot be read"));
        }
        let actual = std::str::from_utf8(&version.stdout)
            .map_err(|_| identity_failure("Browser VM Node version is not UTF-8"))?
            .trim();
        if actual.is_empty() || actual != manifest.node.version {
            return Err(identity_failure(
                "Browser VM Node version does not match its manifest",
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl WorkerIdentityVerifier for VmWorkerIdentityVerifier {
    fn preflight(&self) -> Result<(), BoundaryError> {
        self.settings.preflight_local()?;
        self.settings.resolved_remote_paths()?;
        Ok(())
    }

    async fn verify(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<VerifiedWorkerIdentity, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser VM identity check cancelled before dispatch",
            ));
        }
        let remote = self
            .settings
            .resolved_remote_paths()
            .map_err(|_| identity_failure("Browser VM remote identity input is invalid"))?;
        let root = remote.worker_root.trim_end_matches('/');
        let cli_js = format!("{root}/{}", worker::CLI_JS_PATH);
        let mcp_package = format!("{root}/{}", worker::MCP_PACKAGE_PATH);
        let playwright_package = format!("{root}/{}", worker::PLAYWRIGHT_PACKAGE_PATH);
        let playwright_core = format!("{root}/{}", worker::PLAYWRIGHT_CORE_PACKAGE_PATH);
        let browsers_json = format!("{root}/{}", worker::BROWSERS_JSON_PATH);
        if !headless_shell_path(&remote.chromium) {
            return Err(identity_failure(
                "Browser VM Chromium path is not the pinned headless-shell binary",
            ));
        }

        let files = [
            remote.identity_manifest.clone(),
            remote.node.clone(),
            cli_js.clone(),
            mcp_package.clone(),
            playwright_package.clone(),
            playwright_core.clone(),
            browsers_json.clone(),
            remote.chromium.clone(),
        ];
        let timeout = Duration::from_millis(self.settings.request_timeout_ms);
        let probe = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: PROBE_SCRIPT,
                        args: &files,
                        stdin: None,
                        timeout,
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM identity probe",
        )?;
        if !probe.succeeded() {
            return Err(identity_failure(
                "Browser VM identity probe could not inspect the pinned files",
            ));
        }
        let records = parse_probe(&probe.stdout, files.len())?;
        let manifest = self
            .read_and_verify_remote_manifest(
                &remote.identity_manifest,
                &remote.identity_sha256,
                &records[0],
                timeout,
                cancel,
            )
            .await?;
        verify_manifest_probe_records(&records, &manifest)?;
        self.verify_remote_packages(
            &mcp_package,
            &playwright_package,
            &playwright_core,
            &manifest,
            timeout,
            cancel,
        )
        .await?;
        self.verify_remote_browsers_json(&browsers_json, &manifest, timeout, cancel)
            .await?;
        self.verify_remote_trees_and_node_version(&remote, &manifest, timeout, cancel)
            .await?;

        Ok(VerifiedWorkerIdentity {
            node: PathBuf::from(remote.node),
            worker_root: PathBuf::from(remote.worker_root),
            cli_js: PathBuf::from(cli_js),
            chromium: PathBuf::from(remote.chromium),
            manifest_sha256: remote.identity_sha256,
            local_images: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Launch planning and execution.
// ---------------------------------------------------------------------------

/// Plans a VM request. The remote Run root is chosen by the launcher at each
/// lifecycle, so the request carries only shared limits and the run-private
/// host directory layout.
pub struct VmWorkerLaunchPlanner {
    pub(super) settings: BrowserVmSettings,
}

impl VmWorkerLaunchPlanner {
    pub fn new(settings: BrowserVmSettings) -> Self {
        Self { settings }
    }
}

impl WorkerLaunchPlanner for VmWorkerLaunchPlanner {
    fn build(
        &self,
        identity: &VerifiedWorkerIdentity,
        directories: &BrowserSandboxDirectories,
    ) -> Result<WorkerLaunchRequest, BoundaryError> {
        self.settings.validate()?;
        Ok(WorkerLaunchRequest {
            // The VM launcher builds its own SSH argv after creating the
            // remote Run root. These fields remain empty for that backend.
            argv: Vec::new(),
            cwd: directories.work.clone(),
            env: BTreeMap::new(),
            timeout: Duration::from_millis(self.settings.request_timeout_ms.saturating_add(30_000)),
            stderr_limit: self.settings.stderr_limit_bytes,
            mcp_limits: McpLimits {
                connect_timeout: Duration::from_millis(self.settings.connect_timeout_ms),
                request_timeout: Duration::from_millis(self.settings.request_timeout_ms),
                max_wire_bytes: worker::MAX_WORKER_SCREENSHOT_WIRE_BYTES,
                max_pages: 32,
                max_tools: 256,
                max_catalog_bytes: 4 * 1024 * 1024,
                max_result_bytes: 4 * 1024 * 1024,
                max_content_blocks: 256,
            },
            directories: directories.clone(),
            execution: None,
            identity: Some(identity.clone()),
        })
    }
}

pub(super) fn worker_config_json(
    settings: &BrowserVmSettings,
    run_root: &str,
    proxy_url: &str,
) -> Result<Vec<u8>, BoundaryError> {
    let chromium = settings
        .chromium
        .as_deref()
        .ok_or_else(|| invalid_config("Browser VM Chromium path is missing"))?;
    let config = serde_json::json!({
        "browser": {
            "browserName": "chromium",
            "isolated": true,
            "launchOptions": {
                "headless": true,
                "chromiumSandbox": true,
                "executablePath": chromium,
                // Playwright passes this to Chromium's --proxy-server. The
                // address is the loopback listener inside the Run's private
                // network namespace; no other egress exists there.
                "proxy": {"server": proxy_url},
                // No implicit loopback bypass, no QUIC/HTTP3, no direct or
                // non-proxied WebRTC UDP, and no local name resolution. The
                // broker performs all DNS work for allowed origins.
                "args": [
                    format!("--proxy-server={proxy_url}"),
                    "--proxy-bypass-list=<-loopback>".to_owned(),
                    "--disable-quic".to_owned(),
                    "--force-webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
                    "--webrtc-ip-handling-policy=disable_non_proxied_udp".to_owned(),
                    "--host-resolver-rules=EXCLUDE 127.0.0.1, MAP * ~NOTFOUND".to_owned(),
                    "--disable-features=DnsOverHttps".to_owned(),
                    "--no-pings".to_owned(),
                    "--disable-background-networking".to_owned(),
                ],
            },
        },
        "outputDir": format!("{run_root}/output"),
        "imageResponses": "omit",
        "webmcp": false,
    });
    serde_json::to_vec_pretty(&config)
        .map_err(|_| invalid_config("Browser VM worker config cannot be encoded"))
}

/// Verifies Chromium's sandbox from inside the VM. The worker PID is the
/// remote process-group leader; host `/proc` is never consulted.
pub struct VmChromiumSandboxInspector {
    pub(super) settings: BrowserVmSettings,
    pub(super) runner: Arc<dyn VmCommandRunner>,
}

impl VmChromiumSandboxInspector {
    pub fn new(settings: BrowserVmSettings, runner: Arc<dyn VmCommandRunner>) -> Self {
        Self { settings, runner }
    }
}

#[async_trait]
impl ChromiumSandboxInspector for VmChromiumSandboxInspector {
    async fn inspect(
        &self,
        worker_pid: u32,
        stderr_tail: &[u8],
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        let args = [worker_pid.to_string()];
        let output = vm_command_result(
            self.runner
                .run(
                    VmCommandSpec {
                        script: SANDBOX_SCRIPT,
                        args: &args,
                        stdin: None,
                        timeout: Duration::from_millis(
                            self.settings.cleanup_timeout_ms.max(15_000),
                        ),
                        stderr_limit: 16 * 1024,
                        stdout_limit: 512 * 1024,
                    },
                    cancel,
                )
                .await,
            "Browser VM sandbox inspection",
        )?;
        if !output.succeeded() {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser VM sandbox inspection did not complete",
            ));
        }
        let text = std::str::from_utf8(&output.stdout).unwrap_or_default();
        let mut renderer = None;
        let mut cmdline = None;
        let mut no_new_privs = false;
        let mut seccomp = 0u32;
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("renderer=") {
                renderer = value.parse::<u32>().ok();
            } else if let Some(value) = line.strip_prefix("cmdline=") {
                cmdline = Some(value.to_owned());
            } else if let Some(value) = line.strip_prefix("nonewprivs=") {
                no_new_privs = value.trim() == "1";
            } else if let Some(value) = line.strip_prefix("seccomp=") {
                seccomp = value.trim().parse::<u32>().unwrap_or(0);
            }
        }
        let Some(renderer) = renderer else {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser VM produced no Chromium renderer evidence",
            ));
        };
        let Some(cmdline) = cmdline else {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Browser VM renderer command line is unavailable",
            ));
        };
        let sandbox_arguments_valid =
            !cmdline.contains("--no-sandbox") && !cmdline.contains("--disable-setuid-sandbox");
        if !sandbox_arguments_valid || !no_new_privs || seccomp != 2 {
            return Err(BrowserFailure::new(
                "browser_sandbox_unavailable",
                "Chromium sandbox evidence inside the VM is insufficient",
            ));
        }
        Ok(ChromiumSandboxEvidence {
            renderer_pid: renderer,
            no_new_privs,
            seccomp_mode: seccomp,
            sandbox_arguments_valid,
            stderr_initializes_sandbox: stderr_tail
                .windows(b"InitializeSandbox".len())
                .any(|window| window == b"InitializeSandbox"),
        })
    }

    async fn inspect_connection(
        &self,
        connection: &dyn WorkerConnection,
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        let pid = connection
            .process_id()
            .filter(|pid| *pid != 0)
            .ok_or_else(|| {
                BrowserFailure::new(
                    "browser_sandbox_unavailable",
                    "Browser VM worker process identity is unavailable",
                )
            })?;
        self.inspect(pid, &connection.stderr_tail(), cancel).await
    }
}
