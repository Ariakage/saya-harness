use super::*;
use crate::mcp::{McpContent, McpTool};
use std::{
    collections::{BTreeSet, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};
use tempfile::TempDir;

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

struct SwitchCancel(Arc<AtomicBool>);
impl Cancellation for SwitchCancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        let flag = self.0.clone();
        Box::pin(async move {
            while !flag.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
    }
}

const SNAPSHOT_TEXT: &str = r#"### Page
- Page URL: https://example.test/path
- Page Title: Example page
### Snapshot
```yaml
- main [ref=e2]:
  - heading "Hello \"world\"" [level=1] [ref=e3]
  - button "Submit" [ref=e4]
  - textbox [ref=e5]: synthetic
  - link "Docs" [ref=e6]:
```
"#;

struct IdentityFixture {
    directory: TempDir,
    settings: BrowserWorkerSettings,
    manifest: IdentityManifest,
    manifest_path: PathBuf,
    node_path: PathBuf,
    chromium_path: PathBuf,
    worker_root: PathBuf,
    temp_root: PathBuf,
}

fn write_mode(path: &Path, bytes: &[u8], mode: u32) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn package(name: &str, version: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"name": name, "version": version})).unwrap()
}

fn fixture_identity() -> IdentityFixture {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let worker_root = root.join("worker");
    let node_path = root.join("node");
    let chromium_path = root.join("chromium");
    let manifest_path = root.join("worker-identity.json");
    let temp_root = root.join("temp");
    fs::create_dir(&temp_root).unwrap();
    for path in [
        worker_root.join("node_modules/@playwright/mcp"),
        worker_root.join("node_modules/playwright"),
        worker_root.join("node_modules/playwright-core"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    write_mode(&node_path, b"node binary", 0o755);
    write_mode(&chromium_path, b"chromium binary", 0o755);
    write_mode(&worker_root.join(CLI_JS_PATH), b"// pinned worker\n", 0o644);
    write_mode(
        &worker_root.join(MCP_PACKAGE_PATH),
        &package("@playwright/mcp", PLAYWRIGHT_MCP_VERSION),
        0o644,
    );
    write_mode(
        &worker_root.join(PLAYWRIGHT_PACKAGE_PATH),
        &package("playwright", PLAYWRIGHT_VERSION),
        0o644,
    );
    write_mode(
        &worker_root.join(PLAYWRIGHT_CORE_PACKAGE_PATH),
        &package("playwright-core", PLAYWRIGHT_VERSION),
        0o644,
    );
    write_mode(
        &worker_root.join(BROWSERS_JSON_PATH),
        &serde_json::to_vec(&json!({
            "browsers": [{
                "name": "chromium",
                "revision": CHROMIUM_REVISION.to_string(),
                "browserVersion": CHROMIUM_VERSION
            }]
        }))
        .unwrap(),
        0o644,
    );

    let node_bytes = fs::read(&node_path).unwrap();
    let cli_bytes = fs::read(worker_root.join(CLI_JS_PATH)).unwrap();
    let mcp_package = fs::read(worker_root.join(MCP_PACKAGE_PATH)).unwrap();
    let playwright_package = fs::read(worker_root.join(PLAYWRIGHT_PACKAGE_PATH)).unwrap();
    let playwright_core = fs::read(worker_root.join(PLAYWRIGHT_CORE_PACKAGE_PATH)).unwrap();
    let browsers = fs::read(worker_root.join(BROWSERS_JSON_PATH)).unwrap();
    let chromium_bytes = fs::read(&chromium_path).unwrap();

    let manifest = IdentityManifest {
        schema_version: 1,
        node: VersionedIdentity {
            sha256: sha256_hex(&node_bytes),
            size: node_bytes.len() as u64,
            version: "v22.14.0".into(),
        },
        worker_cli_js: FileIdentity {
            sha256: sha256_hex(&cli_bytes),
            size: cli_bytes.len() as u64,
        },
        mcp_package: VersionedIdentity {
            sha256: sha256_hex(&mcp_package),
            size: mcp_package.len() as u64,
            version: PLAYWRIGHT_MCP_VERSION.into(),
        },
        playwright_package: VersionedIdentity {
            sha256: sha256_hex(&playwright_package),
            size: playwright_package.len() as u64,
            version: PLAYWRIGHT_VERSION.into(),
        },
        playwright_core_package: VersionedIdentity {
            sha256: sha256_hex(&playwright_core),
            size: playwright_core.len() as u64,
            version: PLAYWRIGHT_VERSION.into(),
        },
        playwright_core_browsers_json: FileIdentity {
            sha256: sha256_hex(&browsers),
            size: browsers.len() as u64,
        },
        chromium: ChromiumIdentity {
            sha256: sha256_hex(&chromium_bytes),
            size: chromium_bytes.len() as u64,
            version: CHROMIUM_VERSION.into(),
            revision: CHROMIUM_REVISION,
        },
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    write_mode(&manifest_path, &manifest_bytes, 0o600);
    let settings = BrowserWorkerSettings {
        enabled: true,
        node: Some(node_path.clone()),
        worker_root: Some(worker_root.clone()),
        chromium: Some(chromium_path.clone()),
        identity_manifest: Some(manifest_path.clone()),
        identity_sha256: Some(sha256_hex(&manifest_bytes)),
        proxy: None,
        temp_root: Some(temp_root.clone()),
        ..Default::default()
    };
    IdentityFixture {
        directory,
        settings,
        manifest,
        manifest_path,
        node_path,
        chromium_path,
        worker_root,
        temp_root,
    }
}

impl IdentityFixture {
    fn rewrite_manifest(&mut self) {
        let bytes = serde_json::to_vec_pretty(&self.manifest).unwrap();
        write_mode(&self.manifest_path, &bytes, 0o600);
        self.settings.identity_sha256 = Some(sha256_hex(&bytes));
    }
}

fn pinned_catalog() -> McpCatalog {
    let mut tools = expected_worker_schemas()
        .into_iter()
        .map(|(name, input_schema)| McpTool {
            name: name.into(),
            description: String::new(),
            input_schema,
            output_schema: None,
            annotations: None,
        })
        .collect::<Vec<_>>();
    tools.push(McpTool {
        name: "browser_file_upload".into(),
        description: String::new(),
        input_schema: json!({
            "type": "object",
            "properties": {"paths": {"type": "array", "items": {"type": "string"}}}
        }),
        output_schema: None,
        annotations: None,
    });
    tools.push(McpTool {
        name: "browser_evaluate".into(),
        description: String::new(),
        input_schema: json!({"type": "object", "properties": {}}),
        output_schema: None,
        annotations: None,
    });
    McpCatalog {
        generation: 1,
        tools,
    }
}

fn text_result(text: impl Into<String>) -> McpToolResult {
    McpToolResult {
        content: vec![McpContent::Text { text: text.into() }],
        structured_content: None,
        is_error: false,
        truncated: false,
    }
}

#[derive(Clone)]
enum FakeReply {
    Text(String),
    ToolError,
    Uncertain,
    Known,
    Cancelled,
}

struct FakeConnection {
    protocol: String,
    generation: AtomicU64,
    catalog: Mutex<McpCatalog>,
    calls: Mutex<Vec<(String, Value)>>,
    replies: Mutex<VecDeque<FakeReply>>,
    shutdowns: AtomicUsize,
    shutdown_failure: AtomicBool,
    shutdown_hangs: AtomicBool,
    pid: Option<u32>,
    stderr: Vec<u8>,
    downloads: Mutex<BTreeSet<String>>,
    fail_upload_cleanup: AtomicBool,
}

impl FakeConnection {
    fn new(catalog: McpCatalog) -> Arc<Self> {
        Arc::new(Self {
            protocol: WORKER_PROTOCOL_VERSION.into(),
            generation: AtomicU64::new(catalog.generation),
            catalog: Mutex::new(catalog),
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(VecDeque::new()),
            shutdowns: AtomicUsize::new(0),
            shutdown_failure: AtomicBool::new(false),
            shutdown_hangs: AtomicBool::new(false),
            pid: None,
            stderr: Vec::new(),
            downloads: Mutex::new(BTreeSet::new()),
            fail_upload_cleanup: AtomicBool::new(false),
        })
    }

    fn with_pid(mut self: Arc<Self>, pid: u32) -> Arc<Self> {
        let connection = Arc::get_mut(&mut self).expect("unique fake connection");
        connection.pid = Some(pid);
        self
    }

    fn with_protocol(mut self: Arc<Self>, protocol: &str) -> Arc<Self> {
        let connection = Arc::get_mut(&mut self).expect("unique fake connection");
        connection.protocol = protocol.into();
        self
    }

    fn push(&self, reply: FakeReply) {
        self.replies.lock().unwrap().push_back(reply);
    }

    fn set_generation(&self, generation: u64) {
        self.generation.store(generation, Ordering::Release);
    }

    fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().unwrap().clone()
    }

    fn shutdown_count(&self) -> usize {
        self.shutdowns.load(Ordering::Acquire)
    }

    fn hang_next_shutdown(&self) {
        self.shutdown_hangs.store(true, Ordering::Release);
    }

    fn preset_download(&self, name: &str) {
        self.downloads.lock().unwrap().insert(name.into());
    }

    fn fail_upload_cleanup(&self) {
        self.fail_upload_cleanup.store(true, Ordering::Release);
    }

    fn default_text(&self, name: &str, arguments: &Value) -> String {
        match name {
            "browser_snapshot" => SNAPSHOT_TEXT.into(),
            "browser_click" => {
                let target = arguments
                    .get("target")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if target.contains("download-two") {
                    let mut downloads = self.downloads.lock().unwrap();
                    downloads.insert("invoice.txt".into());
                    downloads.insert("stray.txt".into());
                    "### Page\n- Page URL: http://allowed.example/files\n- Page Title: Files\n### Download\n- Downloaded file invoice.txt to \"/run/output/invoice.txt\"\n".into()
                } else if target.contains("download") {
                    self.downloads.lock().unwrap().insert("invoice.txt".into());
                    "### Page\n- Page URL: http://allowed.example/files\n- Page Title: Files\n### Download\n- Downloaded file invoice.txt to \"/run/output/invoice.txt\"\n".into()
                } else {
                    "### Page\n- Page URL: http://allowed.example/\n- Page Title: Fixture\n".into()
                }
            }
            "browser_close" => {
                "### Result\nNo open tabs. Navigate to a URL to create one.\n".into()
            }
            _ => {
                let url = arguments
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or("about:blank");
                format!("### Page\n- Page URL: {url}\n- Page Title: Fixture\n")
            }
        }
    }
}

#[async_trait]
impl WorkerConnection for FakeConnection {
    fn protocol_version(&self) -> Option<String> {
        Some(self.protocol.clone())
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn process_id(&self) -> Option<u32> {
        self.pid
    }

    fn stderr_tail(&self) -> Vec<u8> {
        self.stderr.clone()
    }

    async fn list_tools(&self, _cancel: &dyn Cancellation) -> Result<McpCatalog, BrowserFailure> {
        Ok(self.catalog.lock().unwrap().clone())
    }

    async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        generation: u64,
        _cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure> {
        if generation != self.generation.load(Ordering::Acquire) {
            return Err(BrowserFailure::new(
                "stale_catalog",
                "Browser worker catalogue changed",
            ));
        }
        self.calls
            .lock()
            .unwrap()
            .push((name.to_owned(), arguments.clone()));
        let reply = self.replies.lock().unwrap().pop_front();
        match reply {
            Some(FakeReply::Text(text)) => Ok(text_result(text)),
            Some(FakeReply::ToolError) => Ok(McpToolResult {
                content: vec![McpContent::Text {
                    text: "failed".into(),
                }],
                structured_content: None,
                is_error: true,
                truncated: false,
            }),
            Some(FakeReply::Uncertain) => Err(BrowserFailure::uncertain(
                "MCP tool completion could not be confirmed",
            )),
            Some(FakeReply::Known) => Err(BrowserFailure::new(
                "worker_transport",
                "Browser worker call failed before dispatch",
            )),
            Some(FakeReply::Cancelled) => Err(BrowserFailure::new(
                "cancelled",
                "Browser worker call cancelled",
            )),
            None => Ok(text_result(self.default_text(name, &arguments))),
        }
    }

    async fn put_upload(
        &self,
        filename: &str,
        bytes: &[u8],
        _cancel: &dyn Cancellation,
    ) -> Result<String, BrowserFailure> {
        Ok(format!(
            "/run/uploads/{}",
            crate::browser::worker::sha256_hex(bytes)
                .get(..8)
                .map(|prefix| format!("{prefix}_{filename}"))
                .unwrap_or_else(|| filename.to_owned())
        ))
    }

    async fn list_downloads(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<BTreeSet<String>, BrowserFailure> {
        Ok(self.downloads.lock().unwrap().clone())
    }

    async fn remove_upload(
        &self,
        _path: &str,
        _cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        if self.fail_upload_cleanup.load(Ordering::Acquire) {
            Err(BrowserFailure::new("file_broker", "staging cleanup failed"))
        } else {
            Ok(())
        }
    }

    async fn read_download(
        &self,
        path: &str,
        _max_bytes: usize,
        _cancel: &dyn Cancellation,
    ) -> Result<Vec<u8>, BrowserFailure> {
        if path != "/run/output/invoice.txt" && path != "invoice.txt" {
            return Err(BrowserFailure::new(
                "file_path_denied",
                "unexpected download path",
            ));
        }
        Ok(b"worker download".to_vec())
    }

    async fn shutdown(&self) -> Result<(), BrowserFailure> {
        self.shutdowns.fetch_add(1, Ordering::AcqRel);
        if self.shutdown_hangs.swap(false, Ordering::AcqRel) {
            // Simulate a provider cleanup that outlives an outer timeout.
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        if self.shutdown_failure.load(Ordering::Acquire) {
            Err(BrowserFailure::uncertain("shutdown failed"))
        } else {
            Ok(())
        }
    }
}

struct FakeLauncher {
    launches: Mutex<Vec<WorkerLaunchRequest>>,
    connections: Mutex<VecDeque<Result<Arc<FakeConnection>, BrowserFailure>>>,
}

impl FakeLauncher {
    fn new(connections: Vec<Result<Arc<FakeConnection>, BrowserFailure>>) -> Arc<Self> {
        Arc::new(Self {
            launches: Mutex::new(Vec::new()),
            connections: Mutex::new(connections.into()),
        })
    }

    fn launch_count(&self) -> usize {
        self.launches.lock().unwrap().len()
    }

    fn request(&self, index: usize) -> WorkerLaunchRequest {
        self.launches.lock().unwrap()[index].clone()
    }
}

#[async_trait]
impl WorkerLauncher for FakeLauncher {
    async fn launch(
        &self,
        request: WorkerLaunchRequest,
        _cancel: &dyn Cancellation,
    ) -> Result<Arc<dyn WorkerConnection>, BrowserFailure> {
        self.launches.lock().unwrap().push(request);
        self.connections
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Err(BrowserFailure::new("worker_spawn", "no fake connection")))
            .map(|connection| connection as Arc<dyn WorkerConnection>)
    }
}

fn fake_provider(
    fixture: &IdentityFixture,
    connection: Arc<FakeConnection>,
) -> (PlaywrightMcpProvider, Arc<FakeLauncher>) {
    let launcher = FakeLauncher::new(vec![Ok(connection)]);
    let provider = PlaywrightMcpProvider::new(fixture.settings.clone(), launcher.clone()).unwrap();
    (provider, launcher)
}

#[test]
fn verifies_each_pinned_identity_file() {
    let fixture = fixture_identity();
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    assert_eq!(identity.node, fixture.node_path);
    assert_eq!(identity.chromium, fixture.chromium_path);
    assert_eq!(identity.worker_root, fixture.worker_root);
    assert_eq!(identity.cli_js, fixture.worker_root.join(CLI_JS_PATH));
    assert_eq!(
        identity.manifest_sha256,
        fixture.settings.identity_sha256.clone().unwrap()
    );
}

#[test]
fn rejects_manifest_and_file_digest_mismatches() {
    let mut fixture = fixture_identity();
    fixture.manifest.node.sha256 = "0".repeat(64);
    fixture.rewrite_manifest();
    assert_eq!(
        verify_worker_identity(&fixture.settings)
            .unwrap_err()
            .0
            .code,
        "worker_identity"
    );

    let mut fixture = fixture_identity();
    fixture.settings.identity_sha256 = Some("1".repeat(64));
    assert_eq!(
        verify_worker_identity(&fixture.settings)
            .unwrap_err()
            .0
            .code,
        "worker_identity"
    );

    let mut fixture = fixture_identity();
    fixture.manifest.mcp_package.size += 1;
    fixture.rewrite_manifest();
    assert!(verify_worker_identity(&fixture.settings).is_err());
}

#[test]
fn rejects_symlinked_writable_and_non_executable_identities() {
    let mut fixture = fixture_identity();
    let link = fixture.directory.path().join("node-link");
    std::os::unix::fs::symlink(&fixture.node_path, &link).unwrap();
    fixture.settings.node = Some(link);
    assert!(verify_worker_identity(&fixture.settings).is_err());

    let mut fixture = fixture_identity();
    let manifest_link = fixture.directory.path().join("manifest-link");
    std::os::unix::fs::symlink(&fixture.manifest_path, &manifest_link).unwrap();
    fixture.settings.identity_manifest = Some(manifest_link);
    assert!(verify_worker_identity(&fixture.settings).is_err());

    let fixture = fixture_identity();
    fs::set_permissions(
        fixture.worker_root.join(MCP_PACKAGE_PATH),
        fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(verify_worker_identity(&fixture.settings).is_err());

    let fixture = fixture_identity();
    fs::set_permissions(&fixture.chromium_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(verify_worker_identity(&fixture.settings).is_err());

    let fixture = fixture_identity();
    fs::set_permissions(&fixture.node_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(verify_worker_identity(&fixture.settings).is_err());
}

#[test]
fn rejects_unpinned_versions_and_manifest_fields() {
    let mut fixture = fixture_identity();
    write_mode(
        &fixture.worker_root.join(MCP_PACKAGE_PATH),
        &package("@playwright/mcp", "0.0.84"),
        0o644,
    );
    let bytes = fs::read(fixture.worker_root.join(MCP_PACKAGE_PATH)).unwrap();
    fixture.manifest.mcp_package.sha256 = sha256_hex(&bytes);
    fixture.manifest.mcp_package.size = bytes.len() as u64;
    fixture.manifest.mcp_package.version = "0.0.84".into();
    fixture.rewrite_manifest();
    assert!(verify_worker_identity(&fixture.settings).is_err());

    let mut fixture = fixture_identity();
    write_mode(
        &fixture.worker_root.join(BROWSERS_JSON_PATH),
        &serde_json::to_vec(&json!({
            "browsers": [{
                "name": "chromium",
                "revision": "1255",
                "browserVersion": CHROMIUM_VERSION
            }]
        }))
        .unwrap(),
        0o644,
    );
    let bytes = fs::read(fixture.worker_root.join(BROWSERS_JSON_PATH)).unwrap();
    fixture.manifest.playwright_core_browsers_json.sha256 = sha256_hex(&bytes);
    fixture.manifest.playwright_core_browsers_json.size = bytes.len() as u64;
    fixture.rewrite_manifest();
    assert!(verify_worker_identity(&fixture.settings).is_err());

    let mut fixture = fixture_identity();
    fixture.manifest.schema_version = 2;
    fixture.rewrite_manifest();
    assert!(verify_worker_identity(&fixture.settings).is_err());
}

#[tokio::test]
async fn launch_argv_and_config_are_fixed_and_sandboxed() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, launcher) = fake_provider(&fixture, connection);
    provider.open(None, &NeverCancel).await.unwrap();
    let request = launcher.request(0);
    let argv = request
        .argv
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(argv[0], fixture.node_path.to_str().unwrap());
    assert_eq!(
        argv[1],
        fixture.worker_root.join(CLI_JS_PATH).to_str().unwrap()
    );
    for required in [
        "--headless",
        "--isolated",
        "--sandbox",
        "--block-service-workers",
        "--no-webmcp",
        "--ignore-roots",
        "--image-responses",
        "--output-dir",
        "--executable-path",
        "--config",
    ] {
        assert!(argv.iter().any(|value| value == required), "{required}");
    }
    assert!(!argv.iter().any(|value| value == "--no-sandbox"));
    assert!(!argv.iter().any(|value| value.contains("npx")));
    assert!(!argv.iter().any(|value| value.contains("npm")));
    assert!(!argv.iter().any(|value| value == "--cdp-endpoint"));
    assert!(
        !argv
            .iter()
            .any(|value| value.contains("remote-debugging-port"))
    );
    let config_index = argv.iter().position(|value| value == "--config").unwrap() + 1;
    let config: Value = serde_json::from_slice(&fs::read(&argv[config_index]).unwrap()).unwrap();
    assert_eq!(config["browser"]["isolated"], true);
    assert_eq!(config["browser"]["launchOptions"]["headless"], true);
    assert_eq!(config["browser"]["launchOptions"]["chromiumSandbox"], true);
    assert_eq!(config["imageResponses"], "omit");
    assert_eq!(config["webmcp"], false);
    assert_eq!(
        config["browser"]["launchOptions"]["executablePath"],
        fixture.chromium_path.to_str().unwrap()
    );
    assert!(
        request
            .cwd
            .starts_with(fs::canonicalize(&fixture.temp_root).unwrap())
    );
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[test]
fn maps_snapshot_and_action_text_to_provider_values() {
    let snapshot = parse_snapshot(SNAPSHOT_TEXT, false).unwrap();
    assert_eq!(snapshot.url, "https://example.test/path");
    assert_eq!(snapshot.title.as_deref(), Some("Example page"));
    assert!(snapshot.text.contains("- main [ref=e2]:"));
    assert_eq!(snapshot.elements.len(), 5);
    assert_eq!(snapshot.elements[0].key, "e2");
    assert_eq!(snapshot.elements[0].role, "main");
    assert_eq!(snapshot.elements[1].key, "e3");
    assert_eq!(snapshot.elements[1].role, "heading");
    assert_eq!(snapshot.elements[1].name, "Hello \"world\"");
    assert_eq!(snapshot.elements[2].key, "e4");
    assert_eq!(snapshot.elements[2].name, "Submit");
    assert_eq!(snapshot.elements[3].key, "e5");
    assert_eq!(snapshot.elements[3].role, "textbox");
    assert_eq!(snapshot.elements[4].name, "Docs");

    let action = parse_action(
        "### Page\n- Page URL: https://example.test/next\n- Page Title: Next\n",
        None,
    );
    assert_eq!(action.url.as_deref(), Some("https://example.test/next"));
    assert!(action.detail.is_none());

    let close = parse_action(
        "### Result\nNo open tabs. Navigate to a URL to create one.\n",
        None,
    );
    assert!(close.url.is_none());
    assert_eq!(
        close.detail.as_deref(),
        Some("No open tabs. Navigate to a URL to create one.")
    );

    assert!(parse_snapshot("### Snapshot\n```yaml\n- main [ref=e2]\n```\n", false).is_err());
}

#[tokio::test]
async fn provider_maps_each_supported_worker_tool() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, _launcher) = fake_provider(&fixture, connection.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();

    connection.push(FakeReply::Text(
        "### Page\n- Page URL: https://example.test/one\n- Page Title: One\n".into(),
    ));
    let action = provider
        .navigate(
            &opened.session,
            &opened.page,
            "https://example.test/one",
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(action.url.as_deref(), Some("https://example.test/one"));

    let snapshot = provider
        .snapshot(&opened.session, &opened.page, 4096, &NeverCancel)
        .await
        .unwrap();
    assert_eq!(snapshot.url, "https://example.test/path");
    assert_eq!(snapshot.elements.len(), 5);

    connection.push(FakeReply::Text(
        "### Page\n- Page URL: https://example.test/one\n".into(),
    ));
    provider
        .click(
            &opened.session,
            &opened.page,
            "e4",
            Some("left"),
            &NeverCancel,
        )
        .await
        .unwrap();
    connection.push(FakeReply::Text(
        "### Page\n- Page URL: https://example.test/one\n".into(),
    ));
    provider
        .fill(&opened.session, &opened.page, "e5", "value", &NeverCancel)
        .await
        .unwrap();
    provider
        .press(&opened.session, &opened.page, "Enter", &NeverCancel)
        .await
        .unwrap();
    provider
        .wait(&opened.session, &opened.page, 250, &NeverCancel)
        .await
        .unwrap();
    provider.close(&opened.session, &NeverCancel).await.unwrap();

    let calls = connection.calls();
    let names = calls
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        vec![
            "browser_navigate",
            "browser_navigate",
            "browser_snapshot",
            "browser_click",
            "browser_type",
            "browser_press_key",
            "browser_wait_for",
            "browser_close",
        ]
    );
    assert_eq!(calls[0].1, json!({"url": "about:blank"}));
    assert_eq!(calls[3].1, json!({"target": "e4", "button": "left"}));
    assert_eq!(calls[4].1, json!({"target": "e5", "text": "value"}));
    assert_eq!(calls[5].1, json!({"key": "Enter"}));
    assert_eq!(calls[6].1, json!({"time": 0.25}));
    assert!(
        !names
            .iter()
            .any(|name| matches!(*name, "browser_evaluate" | "browser_run_code_unsafe"))
    );
}

#[tokio::test]
async fn provider_download_requires_this_action_and_upload_uses_worker_staging() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, _launcher) = fake_provider(&fixture, connection.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();

    let download = provider
        .download(&opened.session, &opened.page, "e-download", &NeverCancel)
        .await
        .unwrap();
    assert_eq!(download.filename, "invoice.txt");
    assert_eq!(download.mime, "application/octet-stream");
    assert_eq!(download.bytes, b"worker download");

    let staging =
        std::env::temp_dir().join(format!("saya-upload-{}", uuid::Uuid::now_v7().simple()));
    std::fs::write(&staging, b"upload bytes").unwrap();
    let source = super::super::BrowserUploadSource {
        filename: "note.txt".into(),
        path: staging.clone(),
        sha256: sha256_hex(b"upload bytes"),
        size: 12,
    };
    let upload = provider
        .upload(
            &opened.session,
            &opened.page,
            "e-file",
            &source,
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(upload.filename, "note.txt");
    assert_eq!(upload.bytes_sent, 12);
    let calls = connection.calls();
    let click = calls
        .iter()
        .find(|(name, arguments)| name == "browser_click" && arguments["target"] == "e-file")
        .expect("upload clicks the explicit element first");
    assert_eq!(click.1["target"], "e-file");
    let upload_call = calls
        .iter()
        .find(|(name, _)| name == "browser_file_upload")
        .expect("upload calls the pinned file tool");
    let paths = upload_call.1["paths"].as_array().unwrap();
    assert_eq!(paths.len(), 1);
    assert!(paths[0].as_str().unwrap().starts_with("/run/uploads/"));
    let _ = std::fs::remove_file(staging);
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[test]
fn download_events_require_a_fixed_section_and_ignore_title_text() {
    let injected = "### Page\n- Page URL: https://example.test/files\n- Page Title: - Downloaded file fake.txt to \"/run/output/fake.txt\"\n";
    assert_eq!(download_path(injected).unwrap(), None);

    let trusted = "### Page\n- Page URL: https://example.test/files\n- Page Title: Files\n### Download\n- Downloaded file real.txt to \"/run/output/real.txt\"\n";
    assert_eq!(
        download_path(trusted).unwrap().as_deref(),
        Some("/run/output/real.txt")
    );

    let duplicate = "### Download\n- Downloaded file a.txt to \"/run/output/a.txt\"\n- Downloaded file b.txt to \"/run/output/b.txt\"\n";
    assert_eq!(
        download_path(duplicate).unwrap_err().code,
        "invalid_provider_result"
    );
}

#[tokio::test]
async fn provider_download_rejects_preexisting_and_ambiguous_files() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    connection.preset_download("invoice.txt");
    let (provider, _launcher) = fake_provider(&fixture, connection.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();
    // The click event names invoice.txt, but it already existed before the
    // action, so it is not proof of a file created by this click.
    let error = provider
        .download(&opened.session, &opened.page, "e-download", &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(error.code, "download_missing");
    provider.shutdown(&NeverCancel).await.unwrap();

    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, _launcher) = fake_provider(&fixture, connection.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();
    // Two files are created relative to the previous listing; the event
    // cannot identify exactly one newly created file.
    let error = provider
        .download(
            &opened.session,
            &opened.page,
            "e-download-two",
            &NeverCancel,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "invalid_provider_result");
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn upload_cleanup_failure_after_success_is_unknown_and_not_retryable() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    connection.fail_upload_cleanup();
    let (provider, _launcher) = fake_provider(&fixture, connection.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();
    let staging =
        std::env::temp_dir().join(format!("saya-upload-{}", uuid::Uuid::now_v7().simple()));
    std::fs::write(&staging, b"upload bytes").unwrap();
    let source = super::super::BrowserUploadSource {
        filename: "note.txt".into(),
        path: staging.clone(),
        sha256: sha256_hex(b"upload bytes"),
        size: 12,
    };
    let error = provider
        .upload(
            &opened.session,
            &opened.page,
            "e-file",
            &source,
            &NeverCancel,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "outcome_unknown");
    assert!(error.uncertain);
    // The file chooser already ran; the host must not retry automatically.
    assert!(
        connection
            .calls()
            .iter()
            .any(|(name, _)| name == "browser_file_upload")
    );
    let _ = std::fs::remove_file(staging);
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn catalog_missing_schema_drift_and_protocol_fail_closed() {
    let fixture = fixture_identity();

    let mut missing = pinned_catalog();
    missing.tools.retain(|tool| tool.name != "browser_click");
    let connection = FakeConnection::new(missing);
    let (provider, _) = fake_provider(&fixture, connection.clone());
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "worker_catalog");
    assert!(!failure.uncertain);
    assert_eq!(connection.shutdown_count(), 1);

    let mut drift = pinned_catalog();
    drift
        .tools
        .iter_mut()
        .find(|tool| tool.name == "browser_navigate")
        .unwrap()
        .input_schema = json!({"type": "object", "properties": {}, "required": []});
    let connection = FakeConnection::new(drift);
    let (provider, _) = fake_provider(&fixture, connection.clone());
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "worker_catalog");
    assert!(!failure.uncertain);

    let connection = FakeConnection::new(pinned_catalog()).with_protocol("2099-01-01");
    let (provider, _) = fake_provider(&fixture, connection.clone());
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "worker_protocol");
    assert!(!failure.uncertain);
    assert_eq!(connection.shutdown_count(), 1);

    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, _) = fake_provider(&fixture, connection);
    provider.open(None, &NeverCancel).await.unwrap();
    for forbidden in [
        "browser_evaluate",
        "browser_run_code_unsafe",
        "browser_install",
        "browser_network_requests",
        "browser_console_messages",
        "browser_tabs",
    ] {
        assert!(!MAPPED_WORKER_TOOLS.contains(&forbidden));
    }
    // `browser_file_upload` is host-internal: only the file broker may call it
    // with a Run-private staged path, and it never becomes a model tool.
    assert!(MAPPED_WORKER_TOOLS.contains(&"browser_file_upload"));
}

#[tokio::test]
async fn dispatched_unknown_cancel_and_catalog_changes_are_not_retried() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, _) = fake_provider(&fixture, connection.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();

    connection.push(FakeReply::Uncertain);
    let failure = provider
        .navigate(
            &opened.session,
            &opened.page,
            "https://example.test",
            &NeverCancel,
        )
        .await
        .unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(failure.code, "outcome_unknown");

    connection.push(FakeReply::Known);
    let failure = provider
        .click(&opened.session, &opened.page, "e4", None, &NeverCancel)
        .await
        .unwrap_err();
    assert!(!failure.uncertain);
    assert_eq!(failure.code, "worker_transport");

    connection.push(FakeReply::Cancelled);
    let failure = provider
        .fill(&opened.session, &opened.page, "e5", "x", &NeverCancel)
        .await
        .unwrap_err();
    assert!(!failure.uncertain);
    assert_eq!(failure.code, "cancelled");

    connection.push(FakeReply::ToolError);
    let failure = provider
        .press(&opened.session, &opened.page, "Enter", &NeverCancel)
        .await
        .unwrap_err();
    assert!(!failure.uncertain);
    assert_eq!(failure.code, "worker_tool_error");

    connection.set_generation(2);
    let failure = provider
        .snapshot(&opened.session, &opened.page, 4096, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "stale_catalog");
    assert!(!failure.uncertain);

    let calls = connection.calls().len();
    assert_eq!(
        calls, 5,
        "each dispatched failure is sent once; stale catalogue checks precede dispatch"
    );
}

#[tokio::test]
async fn cancellation_before_dispatch_is_known_and_not_sent() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, _) = fake_provider(&fixture, connection.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();
    let cancelled = Arc::new(AtomicBool::new(true));
    let failure = provider
        .navigate(
            &opened.session,
            &opened.page,
            "https://example.test",
            &SwitchCancel(cancelled),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.code, "cancelled");
    assert!(!failure.uncertain);
    assert_eq!(connection.calls().len(), 1);
}

#[tokio::test]
async fn shutdown_reaps_cleans_and_can_restart() {
    let fixture = fixture_identity();
    let first = FakeConnection::new(pinned_catalog());
    let second = FakeConnection::new(pinned_catalog());
    let launcher = FakeLauncher::new(vec![Ok(first.clone()), Ok(second.clone())]);
    let provider = PlaywrightMcpProvider::new(fixture.settings.clone(), launcher.clone()).unwrap();

    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(launcher.launch_count(), 1);
    assert_eq!(fs::read_dir(&fixture.temp_root).unwrap().count(), 1);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(first.shutdown_count(), 1);
    assert_eq!(fs::read_dir(&fixture.temp_root).unwrap().count(), 0);

    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(launcher.launch_count(), 2);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(second.shutdown_count(), 1);
    assert_eq!(fs::read_dir(&fixture.temp_root).unwrap().count(), 0);
}

#[tokio::test]
async fn shutdown_failure_stays_retryable_and_explicitly_unknown() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    connection.shutdown_failure.store(true, Ordering::Release);
    let (provider, _) = fake_provider(&fixture, connection.clone());
    provider.open(None, &NeverCancel).await.unwrap();
    let failure = provider.shutdown(&NeverCancel).await.unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(fs::read_dir(&fixture.temp_root).unwrap().count(), 1);

    connection.shutdown_failure.store(false, Ordering::Release);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(connection.shutdown_count(), 2);
    assert_eq!(fs::read_dir(&fixture.temp_root).unwrap().count(), 0);
}

#[tokio::test]
async fn launch_failure_cleans_temp_files() {
    let fixture = fixture_identity();
    let launcher = FakeLauncher::new(vec![Err(BrowserFailure::new("worker_spawn", "no process"))]);
    let provider = PlaywrightMcpProvider::new(fixture.settings.clone(), launcher).unwrap();
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "worker_spawn");
    assert!(!failure.uncertain);
    assert_eq!(fs::read_dir(&fixture.temp_root).unwrap().count(), 0);
}

#[test]
fn worker_settings_default_disabled_and_reject_partial_enablement() {
    let settings = BrowserWorkerSettings::default();
    assert!(!settings.enabled);
    settings.validate().unwrap();

    let enabled = BrowserWorkerSettings {
        enabled: true,
        ..Default::default()
    };
    assert!(enabled.validate().is_err());

    let relative = BrowserWorkerSettings {
        node: Some(PathBuf::from("node")),
        ..Default::default()
    };
    assert!(relative.validate().is_err());
}

#[test]
fn temp_artifacts_disappear_on_drop() {
    let fixture = fixture_identity();
    {
        let artifacts = TempArtifacts::create(&fixture.settings).unwrap();
        assert!(artifacts.base.exists());
    }
    assert_eq!(fs::read_dir(&fixture.temp_root).unwrap().count(), 0);
}

#[tokio::test]
async fn identity_is_rechecked_immediately_before_launch() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, launcher) = fake_provider(&fixture, connection);
    write_mode(&fixture.node_path, b"changed node binary", 0o755);
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "worker_identity");
    assert!(!failure.uncertain);
    assert_eq!(launcher.launch_count(), 0);
}

struct FakeLease {
    directories: BrowserSandboxDirectories,
    releases: Arc<AtomicUsize>,
    fail_release: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl BrowserWorkspaceLease for FakeLease {
    fn directories(&self) -> &BrowserSandboxDirectories {
        &self.directories
    }

    fn execution(&self) -> Option<PreparedEnvironment> {
        None
    }

    async fn release(&self) -> Result<(), BoundaryError> {
        if self.fail_release.load(Ordering::Acquire) {
            return Err(BoundaryError(
                ErrorInfo {
                    code: "worker_cleanup".into(),
                    message: "synthetic cleanup failure".into(),
                },
                None,
                None,
            ));
        }
        self.releases.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

struct FakeEnvironmentFactory {
    root: PathBuf,
    creates: AtomicUsize,
    releases: Arc<AtomicUsize>,
    fail_release: Arc<AtomicBool>,
}

impl FakeEnvironmentFactory {
    fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root,
            creates: AtomicUsize::new(0),
            releases: Arc::new(AtomicUsize::new(0)),
            fail_release: Arc::new(AtomicBool::new(false)),
        })
    }

    fn creates(&self) -> usize {
        self.creates.load(Ordering::Acquire)
    }
}

impl BrowserEnvironmentFactory for FakeEnvironmentFactory {
    fn prepare(&self) -> Result<Arc<dyn BrowserWorkspaceLease>, BoundaryError> {
        let index = self.creates.fetch_add(1, Ordering::AcqRel);
        let base = self.root.join(format!("environment-{index}"));
        let directories = BrowserSandboxDirectories {
            profile: base.join("profile"),
            output: base.join("output"),
            tmp: base.join("tmp"),
            work: base.join("work"),
            config: base.join("config"),
        };
        for path in [
            &directories.profile,
            &directories.output,
            &directories.tmp,
            &directories.work,
            &directories.config,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        Ok(Arc::new(FakeLease {
            directories,
            releases: self.releases.clone(),
            fail_release: self.fail_release.clone(),
        }))
    }
}

fn provider_with_factory(
    fixture: &IdentityFixture,
    launcher: Arc<dyn WorkerLauncher>,
    factory: Arc<dyn BrowserEnvironmentFactory>,
) -> PlaywrightMcpProvider {
    PlaywrightMcpProvider::build(fixture.settings.clone(), launcher, Some(factory)).unwrap()
}

struct FixedInspector {
    evidence: Result<ChromiumSandboxEvidence, BrowserFailure>,
    calls: AtomicUsize,
}

impl FixedInspector {
    fn new(evidence: Result<ChromiumSandboxEvidence, BrowserFailure>) -> Arc<Self> {
        Arc::new(Self {
            evidence,
            calls: AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

#[async_trait::async_trait]
impl ChromiumSandboxInspector for FixedInspector {
    async fn inspect(
        &self,
        _worker_pid: u32,
        _stderr_tail: &[u8],
        _cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.evidence.clone()
    }
}

#[tokio::test]
async fn production_launch_uses_prepared_directories_and_fresh_environments() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let first = FakeConnection::new(pinned_catalog());
    let second = FakeConnection::new(pinned_catalog());
    let launcher = FakeLauncher::new(vec![Ok(first.clone()), Ok(second.clone())]);
    let provider = provider_with_factory(
        &fixture,
        launcher.clone(),
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );

    provider.open(None, &NeverCancel).await.unwrap();
    let first_request = launcher.request(0);
    assert_eq!(first_request.cwd, first_request.directories.work);
    assert!(
        first_request
            .directories
            .config
            .join("config.json")
            .is_file()
    );
    assert_eq!(
        first_request.env.get(&std::ffi::OsString::from("HOME")),
        Some(&first_request.directories.profile.clone().into_os_string())
    );
    assert_eq!(
        first_request.env.get(&std::ffi::OsString::from("TMPDIR")),
        Some(&first_request.directories.tmp.clone().into_os_string())
    );
    assert!(
        first_request
            .directories
            .output
            .starts_with(environment_root.path())
    );
    assert!(first_request.execution.is_none());
    let argv = first_request
        .argv
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let output_index = argv
        .iter()
        .position(|value| value == "--output-dir")
        .expect("output directory argument")
        + 1;
    assert_eq!(
        argv[output_index],
        first_request.directories.output.to_str().unwrap()
    );
    let config_index = argv
        .iter()
        .position(|value| value == "--config")
        .expect("config argument")
        + 1;
    assert_eq!(
        std::path::Path::new(&argv[config_index]),
        first_request.directories.config.join("config.json")
    );
    assert_eq!(factory.creates(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);

    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(first.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);

    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(launcher.launch_count(), 2);
    let second_request = launcher.request(1);
    assert_ne!(
        first_request.directories.profile, second_request.directories.profile,
        "a fresh environment is prepared for each lifecycle"
    );
    assert_eq!(factory.creates(), 2);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(second.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn sandbox_verification_failure_cleans_worker_and_environment() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_pid(4242);
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let inspector = FixedInspector::new(Err(BrowserFailure::new(
        "browser_sandbox_unavailable",
        "Chromium renderer sandbox evidence is insufficient",
    )));
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    )
    .with_sandbox_inspector(inspector.clone());

    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert_eq!(inspector.calls(), 1);
    assert_eq!(connection.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn sandbox_evidence_is_recorded_once_after_real_open() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_pid(77);
    let launcher = FakeLauncher::new(vec![Ok(connection)]);
    let evidence = ChromiumSandboxEvidence {
        renderer_pid: 78,
        no_new_privs: true,
        seccomp_mode: 2,
        sandbox_arguments_valid: true,
        stderr_initializes_sandbox: true,
    };
    let inspector = FixedInspector::new(Ok(evidence.clone()));
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory as Arc<dyn BrowserEnvironmentFactory>,
    )
    .with_sandbox_inspector(inspector.clone());

    let opened = provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(provider.sandbox_evidence().await, Some(evidence));
    assert_eq!(inspector.calls(), 1);
    provider
        .navigate(
            &opened.session,
            &opened.page,
            "https://example.test/next",
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(
        inspector.calls(),
        1,
        "evidence is verified once per worker lifecycle"
    );
    provider.shutdown(&NeverCancel).await.unwrap();
}

fn proc_entry(
    root: &Path,
    pid: u32,
    ppid: u32,
    no_new_privs: u32,
    seccomp: u32,
    cmdline: &[&str],
    children: &[u32],
) {
    let directory = root.join(pid.to_string());
    fs::create_dir_all(directory.join("task").join(pid.to_string())).unwrap();
    fs::write(
        directory.join("status"),
        format!(
            "Name:\tprocess\nPPid:\t{ppid}\nNoNewPrivs:\t{no_new_privs}\nSeccomp:\t{seccomp}\n"
        ),
    )
    .unwrap();
    let mut bytes = Vec::new();
    for token in cmdline {
        bytes.extend_from_slice(token.as_bytes());
        bytes.push(0);
    }
    fs::write(directory.join("cmdline"), bytes).unwrap();
    fs::write(
        directory
            .join("task")
            .join(pid.to_string())
            .join("children"),
        children
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(" "),
    )
    .unwrap();
}

fn renderer_fixture(root: &Path, renderer_cmdline: &[&str], nnp: u32, seccomp: u32) {
    proc_entry(root, 100, 1, 0, 0, &["/usr/bin/node"], &[200]);
    proc_entry(
        root,
        200,
        100,
        0,
        0,
        &["/chromium", "--type=browser"],
        &[400],
    );
    proc_entry(root, 400, 200, nnp, seccomp, renderer_cmdline, &[]);
}

#[tokio::test]
async fn procfs_sandbox_evidence_requires_a_real_renderer() {
    let root = tempfile::tempdir().unwrap();
    renderer_fixture(
        root.path(),
        &["/chromium", "--type=renderer", "--enable-sandbox"],
        1,
        2,
    );
    let inspector = ProcfsChromiumSandboxInspector::with_limits(
        root.path().to_path_buf(),
        Duration::from_millis(200),
        Duration::from_millis(10),
    );
    let evidence = inspector
        .inspect(100, b"Chromium InitializeSandbox() called", &NeverCancel)
        .await
        .unwrap();
    assert_eq!(evidence.renderer_pid, 400);
    assert!(evidence.no_new_privs);
    assert_eq!(evidence.seccomp_mode, 2);
    assert!(evidence.sandbox_arguments_valid);
    assert!(evidence.stderr_initializes_sandbox);
}

#[tokio::test]
async fn procfs_sandbox_evidence_rejects_bubblewrap_only_markers() {
    // Bubblewrap gives every confined process NoNewPrivs and a seccomp filter.
    // Without Chromium's own InitializeSandbox marker those fields must not be
    // accepted as proof of Chromium's sandbox.
    let root = tempfile::tempdir().unwrap();
    renderer_fixture(
        root.path(),
        &["/chromium", "--type=renderer", "--enable-sandbox"],
        1,
        2,
    );
    let inspector = ProcfsChromiumSandboxInspector::with_limits(
        root.path().to_path_buf(),
        Duration::from_millis(100),
        Duration::from_millis(10),
    );
    let failure = inspector
        .inspect(100, b"bubblewrap seccomp installed", &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
}

#[tokio::test]
async fn procfs_sandbox_evidence_rejects_disabled_or_unrelated_processes() {
    for (cmdline, nnp, seccomp) in [
        (vec!["/chromium", "--type=renderer", "--no-sandbox"], 1, 2),
        (
            vec!["/chromium", "--type=renderer", "--disable-setuid-sandbox"],
            1,
            2,
        ),
        (vec!["/chromium", "--type=renderer"], 0, 2),
        (vec!["/chromium", "--type=renderer"], 1, 1),
    ] {
        let root = tempfile::tempdir().unwrap();
        renderer_fixture(root.path(), &cmdline, nnp, seccomp);
        let inspector = ProcfsChromiumSandboxInspector::with_limits(
            root.path().to_path_buf(),
            Duration::from_millis(50),
            Duration::from_millis(10),
        );
        let failure = inspector.inspect(100, b"", &NeverCancel).await.unwrap_err();
        assert_eq!(failure.code, "browser_sandbox_unavailable");
    }

    // A children file alone is not trusted when the candidate reports another
    // parent, which is the shape of a PID-reuse misread.
    let root = tempfile::tempdir().unwrap();
    proc_entry(root.path(), 100, 1, 0, 0, &["/usr/bin/node"], &[500]);
    proc_entry(
        root.path(),
        500,
        999,
        1,
        2,
        &["/chromium", "--type=renderer"],
        &[],
    );
    let inspector = ProcfsChromiumSandboxInspector::with_limits(
        root.path().to_path_buf(),
        Duration::from_millis(50),
        Duration::from_millis(10),
    );
    assert_eq!(
        inspector
            .inspect(100, b"", &NeverCancel)
            .await
            .unwrap_err()
            .code,
        "browser_sandbox_unavailable"
    );

    let cancelled = Arc::new(AtomicBool::new(true));
    let root = tempfile::tempdir().unwrap();
    renderer_fixture(root.path(), &["/chromium", "--type=renderer"], 1, 2);
    let inspector = ProcfsChromiumSandboxInspector::with_limits(
        root.path().to_path_buf(),
        Duration::from_millis(200),
        Duration::from_millis(10),
    );
    assert_eq!(
        inspector
            .inspect(100, b"", &SwitchCancel(cancelled))
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
}

#[test]
fn install_roots_cover_node_worker_and_chromium_package_only() {
    let fixture = fixture_identity();
    let roots = browser_install_roots(&fixture.settings).unwrap();
    assert!(roots.contains(&fixture.node_path.parent().unwrap().to_path_buf()));
    assert!(roots.contains(&fixture.worker_root));
    assert!(roots.contains(&fixture.chromium_path.parent().unwrap().to_path_buf()));
    assert!(!roots.iter().any(|root| root.ends_with(".saya")));
}

#[test]
fn worker_proxy_accepts_only_explicit_loopback_http_endpoints() {
    let fixture = fixture_identity();
    for proxy in ["http://127.0.0.1:3128", "http://[::1]:3128"] {
        let mut settings = fixture.settings.clone();
        settings.proxy = Some(proxy.into());
        settings.validate().unwrap();
    }
    for proxy in [
        "https://127.0.0.1:3128",
        "http://192.0.2.1:3128",
        "http://localhost:3128",
        "http://user:pass@127.0.0.1:3128",
        "http://127.0.0.1:3128/path",
        "socks5://127.0.0.1:1080",
    ] {
        let mut settings = fixture.settings.clone();
        settings.proxy = Some(proxy.into());
        assert!(settings.validate().is_err(), "{proxy}");
    }
}

#[tokio::test]
async fn outer_timeout_keeps_connection_and_workspace_for_a_retry() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog());
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );
    provider.open(None, &NeverCancel).await.unwrap();

    connection.hang_next_shutdown();
    let timed_out =
        tokio::time::timeout(Duration::from_millis(20), provider.shutdown(&NeverCancel)).await;
    assert!(timed_out.is_err(), "the hanging shutdown must time out");
    // Dropping the shutdown future must not take the connection or workspace
    // away. A retry continues on the same resources and confirms cleanup.
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(connection.shutdown_count(), 2);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn cleanup_failure_is_retryable_and_never_reuses_a_closed_environment() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let first = FakeConnection::new(pinned_catalog());
    let second = FakeConnection::new(pinned_catalog());
    let launcher = FakeLauncher::new(vec![Ok(first.clone()), Ok(second.clone())]);
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );

    provider.open(None, &NeverCancel).await.unwrap();
    factory.fail_release.store(true, Ordering::Release);
    let failure = provider.shutdown(&NeverCancel).await.unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(first.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);

    factory.fail_release.store(false, Ordering::Release);
    // Retrying shutdown completes cleanup without launching a new worker.
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    assert_eq!(factory.creates(), 1);
    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(
        factory.creates(),
        2,
        "a later open prepares a new environment after confirmed cleanup"
    );
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(factory.releases.load(Ordering::Acquire), 2);
}

#[test]
fn snapshot_frame_identity_is_structural_not_ref_prefix_only() {
    let yaml = "- main [ref=e2]:\n  - textbox \"user\" [ref=e3]\n  - iframe \"login frame\" [ref=f1e20]:\n    - textbox \"user\" [ref=f1e21]\n";
    let elements = parse_elements(yaml).unwrap();
    assert_eq!(elements[0].frame, None);
    assert_eq!(elements[1].frame, None);
    // The iframe node itself and every descendant belong to a child frame.
    assert_eq!(elements[2].frame.as_deref(), Some("f1"));
    assert_eq!(elements[3].frame.as_deref(), Some("f1"));

    // A ref that carries an `f`-prefixed id but sits at the top level is the
    // main document. Providers may renumber frame ids after navigation.
    let flattened = parse_elements("- textbox \"user\" [ref=f1e2]\n").unwrap();
    assert_eq!(flattened[0].frame, None);
}

#[test]
fn snapshot_ignores_pseudo_refs_inside_accessible_names() {
    let yaml = "- button \"See [ref=e9] now\" [ref=e3]\n\
                - link \"quoted \\\" [ref=e8] \\\" end\" [ref=e4]\n";
    let elements = parse_elements(yaml).unwrap();
    assert_eq!(elements.len(), 2);
    assert_eq!(elements[0].key, "e3");
    assert_eq!(elements[0].name, "See [ref=e9] now");
    assert_eq!(elements[1].key, "e4");
    assert_eq!(elements[1].name, "quoted \" [ref=e8] \" end");

    // Only the trailing properties region may name a reference.
    let text = "### Page\n- Page URL: https://example.test/\n### Snapshot\n```yaml\n\
                - button \"fake [ref=e1]\" [ref=e2]\n```\n";
    let snapshot = parse_snapshot(text, false).unwrap();
    assert_eq!(snapshot.elements.len(), 1);
    assert_eq!(snapshot.elements[0].key, "e2");
    assert_eq!(snapshot.elements[0].name, "fake [ref=e1]");
}

#[test]
fn snapshot_rejects_illegal_frame_and_element_references() {
    for yaml in [
        "- iframe \"frame\" [ref=fxe1]\n",
        "- iframe \"frame\" [ref=f1]\n",
        "- iframe \"frame\" [ref=f1e]\n",
        "- textbox \"bad\" [ref=f1e2x]\n",
        "- textbox \"bad\" [ref=e]\n",
        "- textbox \"bad\" [ref=../../etc]\n",
        "- textbox \"bad\" [ref=e1 e2]\n",
    ] {
        assert!(
            parse_elements(yaml).is_err(),
            "malformed reference was accepted: {yaml}"
        );
    }
    let text = "### Page\n- Page URL: https://example.test/\n### Snapshot\n```yaml\n\
                - iframe \"frame\" [ref=fxe1]\n    - textbox \"user\" [ref=f1e2]\n```\n";
    let error = parse_snapshot(text, false).unwrap_err();
    assert_eq!(error.code, "invalid_provider_result");
}

#[test]
fn snapshot_rejects_duplicate_references_without_dropping_subtrees() {
    let error = parse_elements(
        "- main [ref=e2]:\n  - button \"first\" [ref=e3]\n  - button \"second\" [ref=e3]\n",
    )
    .unwrap_err();
    assert_eq!(error.code, "invalid_provider_result");
    // A duplicate property inside one line is also fatal.
    assert!(parse_elements("- button \"x\" [ref=e3] [ref=e4]\n").is_err());
}

#[test]
fn snapshot_keeps_ref_less_containers_and_quoted_keys() {
    // The provider assigns references only to interactable nodes. Containers
    // without a ref must still carry their child-frame scope.
    let yaml =
        "- generic:\n  - iframe \"login frame\" [ref=f1e20]:\n    - textbox \"user\" [ref=f1e21]\n";
    let elements = parse_elements(yaml).unwrap();
    assert_eq!(elements.len(), 2);
    assert_eq!(elements[0].key, "f1e20");
    assert_eq!(elements[0].frame.as_deref(), Some("f1"));
    assert_eq!(elements[1].key, "f1e21");
    assert_eq!(elements[1].frame.as_deref(), Some("f1"));

    // YAML quotes a whole key when the accessible name contains `: ` or a
    // backtick. Single quotes are doubled inside the scalar.
    let yaml = "- 'button \"it''s: here\" [ref=e3]'\n- 'heading \"`code`\" [ref=e4]'\n";
    let elements = parse_elements(yaml).unwrap();
    assert_eq!(elements[0].name, "it's: here");
    assert_eq!(elements[0].key, "e3");
    assert_eq!(elements[1].name, "`code`");
    assert_eq!(elements[1].key, "e4");

    // Provider property lines are metadata, not elements.
    let yaml = "- generic:\n  - /url: /login\n  - link \"Docs\" [ref=e5]:\n";
    let elements = parse_elements(yaml).unwrap();
    assert_eq!(elements.len(), 1);
    assert_eq!(elements[0].key, "e5");
}

#[test]
fn snapshot_rejects_unrecognized_lines_and_unbalanced_properties() {
    for yaml in [
        "- button \"x\" [ref=e3\n",
        "- button \"x\" [ref=e3] trailing\n",
        "- button \"x\n",
        "- [ref=e3]\n",
        "not a list item\n",
        "- button \"x\" [bad property=e3]\n",
        "- button\n  continuation\n",
    ] {
        assert!(
            parse_elements(yaml).is_err(),
            "malformed snapshot line was accepted: {yaml:?}"
        );
    }
}

#[test]
fn upload_staging_preserves_page_visible_name_and_never_leaves_partials() {
    let root = tempfile::tempdir().unwrap();
    let path =
        stage_worker_upload(root.path(), "invoice 2026.txt", b"payload", &NeverCancel).unwrap();
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("invoice 2026.txt"),
        "the page-visible basename must not gain a host prefix"
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"payload");
    assert!(path.starts_with(root.path().join("uploads")));
    let directory = path.parent().unwrap();
    // A `..` inside a basename is not traversal and must survive verbatim.
    let dotted = stage_worker_upload(root.path(), "archive..tar", b"x", &NeverCancel).unwrap();
    assert_eq!(
        dotted.file_name().and_then(|name| name.to_str()),
        Some("archive..tar")
    );
    assert_eq!(
        std::fs::metadata(directory).unwrap().permissions().mode() & 0o077,
        0,
        "staging directories must stay private"
    );
    for name in ["../escape.txt", "a/b", ".hidden", "..", "name\0"] {
        assert!(
            stage_worker_upload(root.path(), name, b"x", &NeverCancel).is_err(),
            "unsafe name {name:?} was staged"
        );
    }
    // A cancellation observed after the synchronous write must remove the
    // partial directory rather than leaving it for the next upload.
    let cancelled = SwitchCancel(Arc::new(AtomicBool::new(true)));
    assert!(
        stage_worker_upload(root.path(), "cancelled.txt", b"x", &cancelled).is_err(),
        "cancelled upload must fail"
    );
    let directories = std::fs::read_dir(root.path().join("uploads"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .is_ok_and(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        })
        .count();
    assert_eq!(
        directories, 2,
        "only the committed upload directories remain"
    );
    assert!(!directory.join("cancelled.txt").exists());
}
