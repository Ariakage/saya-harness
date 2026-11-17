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

fn canonical_tempdir() -> TempDir {
    // macOS default TMPDIR (/var/folders/...) traverses the /var -> private/var symlink;
    // canonicalize only the test root so ancestor O_NOFOLLOW checks still reject fixture links.
    let base = fs::canonicalize(std::env::temp_dir()).unwrap();
    tempfile::Builder::new().tempdir_in(base).unwrap()
}

fn fixture_identity() -> IdentityFixture {
    let directory = canonical_tempdir();
    let root = directory.path();
    let worker_root = root.join("worker");
    let node_path = root.join("node/bin/node");
    let chromium_path = root.join("engine/chrome-headless-shell");
    fs::create_dir_all(node_path.parent().unwrap()).unwrap();
    fs::create_dir_all(chromium_path.parent().unwrap()).unwrap();
    let manifest_path = root.join("worker-identity.json");
    let temp_root = root.join("temp");
    fs::create_dir(&temp_root).unwrap();
    for path in [
        worker_root.join("node_modules/@playwright/mcp"),
        worker_root.join("node_modules/playwright"),
        worker_root.join("node_modules/playwright-core/lib"),
    ] {
        fs::create_dir_all(path).unwrap();
    }
    write_mode(&node_path, b"node binary", 0o755);
    write_mode(&chromium_path, b"chromium binary", 0o755);
    write_mode(
        &worker_root.join("node_modules/playwright-core/lib/coreBundle.js"),
        b"pinned dependency",
        0o644,
    );
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
        schema_version: 2,
        installation_trees: InstallationTrees {
            node: serde_json::from_value(fixture_tree_identity(node_path.parent().unwrap()))
                .unwrap(),
            worker: serde_json::from_value(fixture_tree_identity(&worker_root)).unwrap(),
            chromium: serde_json::from_value(fixture_tree_identity(
                chromium_path.parent().unwrap(),
            ))
            .unwrap(),
        },
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

fn fixture_tree_identity(root: &Path) -> Value {
    fn text(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u32).to_be_bytes());
        hash.update(value);
    }
    fn walk(path: &Path, relative: &Path, hash: &mut Sha256, entries: &mut u64, bytes: &mut u64) {
        let metadata = fs::symlink_metadata(path).unwrap();
        *entries += 1;
        let kind = if metadata.is_dir() {
            b'D'
        } else if metadata.is_file() {
            b'F'
        } else {
            assert!(metadata.is_symlink());
            b'L'
        };
        hash.update([kind]);
        text(hash, relative.as_os_str().as_bytes());
        let mode = if kind == b'L' {
            0o777
        } else {
            metadata.mode() & 0o7777
        };
        hash.update(mode.to_be_bytes());
        if metadata.is_file() {
            let contents = fs::read(path).unwrap();
            *bytes += contents.len() as u64;
            hash.update((contents.len() as u64).to_be_bytes());
            hash.update(Sha256::digest(contents));
        } else if metadata.is_symlink() {
            text(hash, fs::read_link(path).unwrap().as_os_str().as_bytes());
        } else {
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                walk(
                    &path.join(&child),
                    &relative.join(child),
                    hash,
                    entries,
                    bytes,
                );
            }
        }
    }
    let mut hash = Sha256::new();
    hash.update(b"saya-browser-installation-v2\0");
    let mut entries = 0;
    let mut bytes = 0;
    walk(root, Path::new(""), &mut hash, &mut entries, &mut bytes);
    let digest = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    json!({"sha256": digest, "entries": entries, "bytes": bytes})
}

fn seal_fixture_installations(fixture: &mut IdentityFixture) {
    let mut manifest = serde_json::to_value(&fixture.manifest).unwrap();
    manifest["schema_version"] = json!(2);
    manifest["installation_trees"] = json!({
        "node": fixture_tree_identity(fixture.node_path.parent().unwrap()),
        "worker": fixture_tree_identity(&fixture.worker_root),
        "chromium": fixture_tree_identity(fixture.chromium_path.parent().unwrap()),
    });
    let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
    write_mode(&fixture.manifest_path, &bytes, 0o600);
    fixture.settings.identity_sha256 = Some(sha256_hex(&bytes));
}

async fn run_installation_probe(fixture: &IdentityFixture) -> std::process::Output {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    assert_eq!(
        std::env::consts::OS,
        "linux",
        "VM identity probes require Linux"
    );
    let node = PathBuf::from(
        std::env::var_os("SOYO_TEST_IDENTITY_NODE")
            .or_else(|| std::env::var_os("SAYA_TEST_IDENTITY_NODE"))
            .expect("explicit host Node is required for the real identity probe"),
    );
    assert!(
        node.is_absolute() && node.is_file(),
        "explicit host Node is unavailable"
    );
    let script = fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/browser/worker/installation_probe.js"),
    )
    .expect("production installation probe asset");
    let mut child = tokio::process::Command::new(node)
        .args(["--no-addons", "-"])
        .arg(fixture.node_path.parent().unwrap())
        .arg(&fixture.worker_root)
        .arg(fixture.chromium_path.parent().unwrap())
        .arg("20000")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &fixture.temp_root)
        .env("TMPDIR", &fixture.temp_root)
        .env("TMP", &fixture.temp_root)
        .env("TEMP", &fixture.temp_root)
        .current_dir(&fixture.temp_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("real Node probe process");
    let mut stdin = child.stdin.take().expect("probe stdin");
    stdin.write_all(&script).await.expect("probe script");
    drop(stdin);
    tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("bounded real installation probe")
        .expect("installation probe output")
}

#[tokio::test]
#[ignore = "requires explicit SOYO_TEST_IDENTITY_NODE on a Linux host"]
async fn installation_probe_matches_native_closed_trees() {
    let mut fixture = fixture_identity();
    write_mode(&fixture.worker_root.join("\u{e000}.js"), b"bmp", 0o644);
    write_mode(&fixture.worker_root.join("\u{10000}.js"), b"astral", 0o644);
    std::os::unix::fs::symlink(
        "node_modules/playwright-core/lib/coreBundle.js",
        fixture.worker_root.join("bundle-link.js"),
    )
    .unwrap();
    seal_fixture_installations(&mut fixture);
    verify_worker_identity(&fixture.settings).expect("native sealed tree verification");
    let output = run_installation_probe(&fixture).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert!(output.stdout.len() <= 4096);
    let actual: InstallationTrees = serde_json::from_slice(&output.stdout)
        .expect("installation probe must return complete tree identities");
    let manifest: IdentityManifest =
        serde_json::from_slice(&fs::read(&fixture.manifest_path).unwrap()).unwrap();
    assert_eq!(actual, manifest.installation_trees);
}

#[tokio::test]
#[ignore = "requires explicit SOYO_TEST_IDENTITY_NODE on a Linux host"]
async fn installation_probe_rejects_unsafe_members() {
    for case in [
        "escape", "dangling", "cycle", "hardlink", "fifo", "writable",
    ] {
        let fixture = fixture_identity();
        let path = fixture.worker_root.join("unsafe");
        match case {
            "escape" => std::os::unix::fs::symlink("../node/bin/node", &path).unwrap(),
            "dangling" => std::os::unix::fs::symlink("missing", &path).unwrap(),
            "cycle" => std::os::unix::fs::symlink("unsafe", &path).unwrap(),
            "hardlink" => fs::hard_link(
                fixture
                    .worker_root
                    .join("node_modules/playwright-core/lib/coreBundle.js"),
                fixture.directory.path().join("external-alias"),
            )
            .unwrap(),
            "fifo" => {
                let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                // SAFETY: the private fixture path remains alive throughout mkfifo.
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "writable" => {
                fs::set_permissions(&fixture.worker_root, fs::Permissions::from_mode(0o777))
                    .unwrap();
            }
            _ => unreachable!(),
        }
        let output = run_installation_probe(&fixture).await;
        assert!(
            !output.status.success(),
            "unsafe {case} member was accepted"
        );
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            b"Browser installation tree verification failed\n"
        );
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires explicit SOYO_TEST_IDENTITY_NODE on a Linux host"]
async fn installation_probe_preserves_utf8_bom_names_and_link_targets() {
    let mut fixture = fixture_identity();
    write_mode(
        &fixture.worker_root.join("\u{feff}asset.js"),
        b"export{}",
        0o644,
    );
    std::os::unix::fs::symlink("\u{feff}asset.js", fixture.worker_root.join("asset-link")).unwrap();
    seal_fixture_installations(&mut fixture);
    verify_worker_identity(&fixture.settings).expect("native UTF-8 installation identity");
    let output = run_installation_probe(&fixture).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let actual: InstallationTrees = serde_json::from_slice(&output.stdout).unwrap();
    let manifest: IdentityManifest =
        serde_json::from_slice(&fs::read(&fixture.manifest_path).unwrap()).unwrap();
    assert_eq!(actual, manifest.installation_trees);
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires explicit SOYO_TEST_IDENTITY_NODE on a Linux host"]
async fn installation_probe_rejects_in_place_mutation() {
    assert_probe_rejects_in_place_mutation(false).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires explicit SOYO_TEST_IDENTITY_NODE on a Linux host"]
async fn installation_probe_rejects_prefix_mutation_in_same_timestamp_tick() {
    assert_probe_rejects_in_place_mutation(true).await;
}

#[cfg(target_os = "linux")]
async fn assert_probe_rejects_in_place_mutation(same_timestamp_tick: bool) {
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    let fixture = fixture_identity();
    let target = fixture
        .worker_root
        .join("node_modules/playwright-core/lib/coreBundle.js");
    let audit = fixture.directory.path().join("mutation-audit.json");
    let node = PathBuf::from(
        std::env::var_os("SOYO_TEST_IDENTITY_NODE")
            .or_else(|| std::env::var_os("SAYA_TEST_IDENTITY_NODE"))
            .expect("explicit host Node is required for the real identity probe"),
    );
    assert!(node.is_absolute() && node.is_file());
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/browser/worker/installation_probe.js");
    // The same-tick case normalizes only timestamp observations, not file I/O.
    let bootstrap = br#"
'use strict';
const fs = require('node:fs');
const vm = require('node:vm');
const { performance } = require('node:perf_hooks');
const crypto = require('node:crypto');
require('node:util');
const [source, node, worker, chromium, target, audit, clock] = process.argv.slice(2);
const script = new vm.Script(fs.readFileSync(source, 'utf8'), {filename:source});
const context = vm.createContext({require, process, Buffer, performance});
const originalOpen = fs.openSync;
const originalRead = fs.readSync;
const originalLstat = fs.lstatSync;
const originalStat = fs.fstatSync;
const writer = originalOpen(target, 'r+');
const writerIdentity = originalStat(writer, {bigint:true});
const prefix = Buffer.alloc(512, 98);
const unchangedPrefix = Buffer.alloc(512, 97);
const originalContents = Buffer.alloc(1024, 97);
let scanner = null;
const report = {injected:false, refreshed:false, timestamp_observation:clock};
function signature(info) {
  return Object.fromEntries(['dev','ino','mode','nlink','size','mtimeNs','ctimeNs']
    .map(key => [key, String(info[key])]));
}
function observedMetadata(info) {
  if (clock === 'same-tick' && info.dev === writerIdentity.dev && info.ino === writerIdentity.ino) {
    info.mtimeNs = 0n;
    info.ctimeNs = 0n;
  }
  return info;
}
fs.fstatSync = function(...args) {
  const info = Reflect.apply(originalStat, fs, args);
  if (args[0] === scanner && !report.injected)
    report.before = signature(info);
  return observedMetadata(info);
};
fs.lstatSync = function(...args) {
  if (!report.refreshed && typeof args[0] === 'string' &&
      args[0].endsWith('/coreBundle.js') && fs.realpathSync(args[0]) === target) {
    report.refreshed_bytes = fs.writeSync(writer, unchangedPrefix, 0, unchangedPrefix.length, 0);
    report.refreshed = true;
  }
  return observedMetadata(Reflect.apply(originalLstat, fs, args));
};
fs.openSync = function(...args) {
  const fd = Reflect.apply(originalOpen, fs, args);
  if (fs.readlinkSync('/proc/self/fd/' + fd) === target) scanner = fd;
  return fd;
};
fs.readSync = function(...args) {
  const count = Reflect.apply(originalRead, fs, args);
  if (args[0] === scanner && count > 0 && !report.injected) {
    report.read_count = count;
    report.written = fs.writeSync(writer, prefix, 0, prefix.length, 0);
    report.after = signature(Reflect.apply(originalStat, fs, [scanner, {bigint:true}]));
    report.injected = true;
    report.offset = fs.readFileSync('/proc/self/fdinfo/' + scanner, 'utf8')
      .match(/^pos:\s*(\d+)/m)[1];
    fs.writeFileSync(audit, JSON.stringify(report));
  }
  return count;
};
process.argv = [process.execPath, '-', node, worker, chromium, '20000'];
try {
  fs.writeFileSync(writer, originalContents);
  script.runInContext(context, {timeout:25000});
} finally {
  fs.openSync = originalOpen;
  fs.readSync = originalRead;
  fs.lstatSync = originalLstat;
  fs.fstatSync = originalStat;
  fs.closeSync(writer);
  report.final_sha256 = crypto.createHash('sha256').update(fs.readFileSync(target)).digest('hex');
  fs.writeFileSync(audit, JSON.stringify(report));
}
"#;
    let mut child = tokio::process::Command::new(node)
        .args(["--no-addons", "-"])
        .arg(source)
        .arg(fixture.node_path.parent().unwrap())
        .arg(&fixture.worker_root)
        .arg(fixture.chromium_path.parent().unwrap())
        .arg(&target)
        .arg(&audit)
        .arg(if same_timestamp_tick {
            "same-tick"
        } else {
            "real"
        })
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &fixture.temp_root)
        .env("TMPDIR", &fixture.temp_root)
        .env("TMP", &fixture.temp_root)
        .env("TEMP", &fixture.temp_root)
        .current_dir(&fixture.temp_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("real Node probe process");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(bootstrap).await.unwrap();
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("bounded real installation probe")
        .unwrap();
    let observation: Value = serde_json::from_slice(&fs::read(audit).unwrap()).unwrap();
    assert_eq!(
        observation["timestamp_observation"],
        if same_timestamp_tick {
            "same-tick"
        } else {
            "real"
        },
        "{observation}"
    );
    assert_eq!(observation["refreshed"], true, "{observation}");
    assert_eq!(observation["refreshed_bytes"], 512, "{observation}");
    assert_eq!(observation["injected"], true, "{observation}");
    assert_eq!(observation["read_count"], 1024, "{observation}");
    assert_eq!(observation["offset"], "1024", "{observation}");
    assert_eq!(observation["written"], 512, "{observation}");
    let mut changed = vec![b'a'; 1024];
    changed[..512].fill(b'b');
    assert_eq!(fs::read(target).unwrap(), changed);
    assert_eq!(observation["final_sha256"], sha256_hex(&changed));
    eprintln!("in-place probe audit: {observation}");
    assert!(
        !output.status.success(),
        "changed tree accepted: {observation}; stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        b"Browser installation tree verification failed\n"
    );
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

enum FakeCatalogReply {
    Ready(Result<McpCatalog, BrowserFailure>),
    Hang,
}

struct FakeConnection {
    protocol: String,
    generation: AtomicU64,
    catalog: Mutex<McpCatalog>,
    catalog_replies: Mutex<VecDeque<FakeCatalogReply>>,
    calls: Mutex<Vec<(String, Value)>>,
    replies: Mutex<VecDeque<FakeReply>>,
    screenshot_replies: Mutex<VecDeque<Result<Value, BrowserFailure>>>,
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
            catalog_replies: Mutex::new(VecDeque::new()),
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(VecDeque::new()),
            screenshot_replies: Mutex::new(VecDeque::new()),
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

    fn push_screenshot(&self, reply: Result<Value, BrowserFailure>) {
        self.screenshot_replies.lock().unwrap().push_back(reply);
    }

    fn queue_catalog_reply(&self, reply: Result<McpCatalog, BrowserFailure>) {
        self.catalog_replies
            .lock()
            .unwrap()
            .push_back(FakeCatalogReply::Ready(reply));
    }

    fn queue_catalog_hang(&self) {
        self.catalog_replies
            .lock()
            .unwrap()
            .push_back(FakeCatalogReply::Hang);
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
        let reply = self.catalog_replies.lock().unwrap().pop_front();
        match reply {
            Some(FakeCatalogReply::Ready(reply)) => reply,
            Some(FakeCatalogReply::Hang) => std::future::pending().await,
            None => Ok(self.catalog.lock().unwrap().clone()),
        }
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

    async fn capture_screenshot(
        &self,
        _cancel: &dyn Cancellation,
    ) -> Result<Value, BrowserFailure> {
        self.screenshot_replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| {
                Err(BrowserFailure::new(
                    "browser_capability_disabled",
                    "fixture disabled",
                ))
            })
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
    let provider = PlaywrightMcpProvider::new(fixture.settings.clone(), launcher.clone())
        .unwrap()
        .with_sandbox_inspector(offline_sandbox_inspector());
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
fn installation_tree_matches_portable_golden_identity() {
    let directory = canonical_tempdir();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir(directory.path().join("lib")).unwrap();
    fs::set_permissions(
        directory.path().join("lib"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    write_mode(&directory.path().join("empty"), b"", 0o644);
    write_mode(&directory.path().join("lib/core.js"), b"export{}", 0o644);
    std::os::unix::fs::symlink("lib/core.js", directory.path().join("alias")).unwrap();
    let expected = installation::TreeIdentity {
        sha256: "7806fc4375d6d69b9139c1fe18869de403a12a33db8873cc53d74e3cea570b28".into(),
        entries: 5,
        bytes: 8,
    };
    installation::verify_tree(directory.path(), &expected, &mut || Ok(()))
        .expect("portable tree identity must normalize only symbolic-link mode");
}

#[cfg(target_os = "linux")]
#[test]
fn installation_tree_rejects_in_place_mutation() {
    use std::os::{fd::AsRawFd, unix::fs::FileExt};

    let directory = tempfile::tempdir().unwrap();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let path = directory.path().join("bundle.js");
    // A separate, smaller case than the historical 70,000-byte read witness.
    let original = vec![b'a'; 1024];
    // Independent wire encoding: D(empty,0755), F(bundle.js,0644,1024,sha256(a*1024)).
    assert_eq!(
        sha256_hex(&original),
        "2edc986847e209b4016e141a6dc8716d3207350f416969382d431539bf292e4a"
    );
    let expected = installation::TreeIdentity {
        sha256: "0aed29db35930c1ad59ab314baccd3ff2465931e049e60e6d10ea76fb765bc11".into(),
        entries: 2,
        bytes: 1024,
    };
    write_mode(&path, &original, 0o644);
    installation::verify_tree(directory.path(), &expected, &mut || Ok(())).unwrap();
    let writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
    let signature = |m: fs::Metadata| {
        (
            m.dev(),
            m.ino(),
            m.mode(),
            m.nlink(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.ctime(),
            m.ctime_nsec(),
        )
    };
    let mut injected = false;
    let mut observation = None;
    let mut reader_seen = false;
    let mut refreshes = 0;
    let writer_name = writer.as_raw_fd().to_string();
    // Refresh unchanged bytes only before the scanner opens its reader and captures metadata.
    // Once a reader exists, preserve all metadata and wait for a real read to complete.
    let result = installation::verify_tree(directory.path(), &expected, &mut || {
        if !injected {
            for entry in fs::read_dir("/proc/self/fd").unwrap() {
                let fd = entry.unwrap().path();
                if fd.file_name().unwrap() == writer_name.as_str()
                    || fs::read_link(&fd).ok().as_deref() != Some(path.as_path())
                {
                    continue;
                }
                reader_seen = true;
                let info = fs::read_to_string(
                    Path::new("/proc/self/fdinfo").join(fd.file_name().unwrap()),
                )
                .unwrap();
                if info
                    .lines()
                    .any(|line| line.split_whitespace().collect::<Vec<_>>() == ["pos:", "1024"])
                {
                    let before = signature(fs::metadata(&fd).unwrap());
                    writer.write_all_at(&[b'b'; 512], 0).unwrap();
                    let after = signature(fs::metadata(&fd).unwrap());
                    observation = Some((before, after));
                    injected = true;
                    break;
                }
            }
            if !reader_seen {
                writer.write_all_at(&[b'a'; 512], 0).unwrap();
                refreshes += 1;
            }
        }
        Ok(())
    });
    assert!(injected, "no real scanner read at offset 1024 observed");
    assert!(refreshes > 0);
    let mut changed = original;
    changed[..512].fill(b'b');
    assert_eq!(fs::read(&path).unwrap(), changed);
    eprintln!("in-place native audit: {observation:?}; result={result:?}");
    assert!(result.is_err(), "changed tree accepted: {observation:?}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[ignore = "requires explicit Linux user and mount namespace prerequisites"]
async fn installation_tree_rejects_cross_device_symlink() {
    use std::{
        ffi::CString,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
    };

    const PARENT_NAMESPACES: &str = "SOYO_TEST_INSTALLATION_PARENT_NAMESPACES";
    if let Ok(parent) = std::env::var(PARENT_NAMESPACES) {
        let parent: Value = serde_json::from_str(&parent).unwrap();
        // SAFETY: getuid has no pointer arguments or side effects.
        assert_eq!(unsafe { libc::getuid() }, 0);
        for name in ["user", "mnt"] {
            assert_ne!(
                fs::read_link(format!("/proc/self/ns/{name}")).unwrap(),
                PathBuf::from(parent[name].as_str().unwrap())
            );
        }
        let mut fixture = fixture_identity();
        let target = "node_modules/playwright-core/lib/coreBundle.js";
        let link = fixture.worker_root.join("foreign-link");
        std::os::unix::fs::symlink(target, &link).unwrap();
        seal_fixture_installations(&mut fixture);
        verify_worker_identity(&fixture.settings).expect("same-device positive control");
        let backing = fixture.directory.path().join("backing");
        fs::create_dir(&backing).unwrap();
        let backing_c = CString::new(backing.as_os_str().as_bytes()).unwrap();
        // SAFETY: C strings are live and the process has a private mount namespace.
        let mounted = unsafe {
            libc::mount(
                c"tmpfs".as_ptr(),
                backing_c.as_ptr(),
                c"tmpfs".as_ptr(),
                0,
                c"size=1m,mode=755".as_ptr().cast(),
            )
        };
        assert_eq!(mounted, 0, "{}", std::io::Error::last_os_error());
        let source = backing.join("link");
        std::os::unix::fs::symlink(target, &source).unwrap();
        let source_c = CString::new(source.as_os_str().as_bytes()).unwrap();
        // SAFETY: the live path is opened without following its final symlink.
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_open_tree,
                libc::AT_FDCWD,
                source_c.as_ptr(),
                1u32 | libc::O_CLOEXEC as u32 | libc::AT_SYMLINK_NOFOLLOW as u32,
            )
        };
        assert!(descriptor >= 0, "{}", std::io::Error::last_os_error());
        // SAFETY: a successful open_tree returns a new descriptor owned here.
        let descriptor = unsafe { OwnedFd::from_raw_fd(i32::try_from(descriptor).unwrap()) };
        let link_c = CString::new(link.as_os_str().as_bytes()).unwrap();
        // SAFETY: both paths are live and the cloned mount belongs to this namespace.
        let moved = unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                descriptor.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_FDCWD,
                link_c.as_ptr(),
                4u32,
            )
        };
        assert_eq!(moved, 0, "{}", std::io::Error::last_os_error());
        let root_dev = fs::metadata(&fixture.worker_root).unwrap().dev();
        let metadata = fs::symlink_metadata(&link).unwrap();
        assert!(metadata.is_symlink());
        assert_eq!(metadata.nlink(), 1);
        assert_ne!(metadata.dev(), root_dev);
        assert_eq!(fs::read_link(&link).unwrap(), Path::new(target));
        assert_eq!(
            fs::read(&link).unwrap(),
            fs::read(fixture.worker_root.join(target)).unwrap()
        );
        eprintln!(
            "cross-device installation link: root_dev={root_dev} link_dev={}",
            metadata.dev()
        );
        let failure = verify_worker_identity(&fixture.settings)
            .expect_err("a foreign-device installation symlink must be rejected");
        assert_eq!(failure.0.code, "worker_identity");
        return;
    }

    let directory = tempfile::tempdir().unwrap();
    let namespaces = json!({
        "user": fs::read_link("/proc/self/ns/user").unwrap(),
        "mnt": fs::read_link("/proc/self/ns/mnt").unwrap(),
    });
    let mut child = tokio::process::Command::new("/usr/bin/unshare");
    child
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--fork",
            "--kill-child",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "browser::worker::tests::installation_tree_rejects_cross_device_symlink",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            PARENT_NAMESPACES,
            serde_json::to_string(&namespaces).unwrap(),
        )
        .env("TMPDIR", directory.path())
        .env("TMP", directory.path())
        .env("TEMP", directory.path())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(60), child.output())
        .await
        .expect("bounded private namespace test")
        .expect("real namespace test child");
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn accepts_sealed_internal_installation_links() {
    let mut fixture = fixture_identity();
    std::os::unix::fs::symlink(
        "./coreBundle.js",
        fixture
            .worker_root
            .join("node_modules/playwright-core/lib/internal-link.js"),
    )
    .unwrap();
    seal_fixture_installations(&mut fixture);
    verify_worker_identity(&fixture.settings).expect("sealed internal installation link");
}

#[test]
fn rejects_sealed_escaping_installation_links() {
    let mut fixture = fixture_identity();
    std::os::unix::fs::symlink("../node/bin/node", fixture.worker_root.join("escape")).unwrap();
    seal_fixture_installations(&mut fixture);
    assert!(verify_worker_identity(&fixture.settings).is_err());
}

#[test]
fn rejects_sealed_cyclic_installation_links() {
    let mut fixture = fixture_identity();
    std::os::unix::fs::symlink("second", fixture.worker_root.join("first")).unwrap();
    std::os::unix::fs::symlink("first", fixture.worker_root.join("second")).unwrap();
    seal_fixture_installations(&mut fixture);
    assert!(verify_worker_identity(&fixture.settings).is_err());
}

#[test]
fn rejects_legacy_named_file_only_manifest() {
    let fixture = fixture_identity();
    let mut manifest = serde_json::to_value(&fixture.manifest).unwrap();
    manifest["schema_version"] = json!(1);
    manifest
        .as_object_mut()
        .unwrap()
        .remove("installation_trees");
    let bytes = serde_json::to_vec(&manifest).unwrap();
    write_mode(&fixture.manifest_path, &bytes, 0o600);
    let settings = BrowserWorkerSettings {
        identity_sha256: Some(sha256_hex(&bytes)),
        ..fixture.settings
    };
    assert!(
        verify_worker_identity(&settings).is_err(),
        "a named-file-only manifest was accepted"
    );
}

#[test]
fn rejects_unpinned_worker_dependency_changes() {
    let fixture = fixture_identity();
    write_mode(
        &fixture
            .worker_root
            .join("node_modules/playwright-core/lib/coreBundle.js"),
        b"unsafe dependency",
        0o644,
    );
    let error = verify_worker_identity(&fixture.settings)
        .expect_err("a changed executable dependency was accepted");
    assert_eq!(error.0.code, "worker_identity");
}

#[test]
fn rejects_unpinned_worker_installation_members() {
    let fixture = fixture_identity();
    write_mode(
        &fixture.worker_root.join("node_modules/injected.js"),
        b"unexpected dependency",
        0o644,
    );
    let error = verify_worker_identity(&fixture.settings)
        .expect_err("an extra installation member was accepted");
    assert_eq!(error.0.code, "worker_identity");
}

#[test]
fn rejects_unpinned_engine_installation_members() {
    let fixture = fixture_identity();
    write_mode(
        &fixture
            .chromium_path
            .parent()
            .unwrap()
            .join("resources.pak"),
        b"unexpected engine resource",
        0o644,
    );
    let error = verify_worker_identity(&fixture.settings)
        .expect_err("an extra engine resource was accepted");
    assert_eq!(error.0.code, "worker_identity");
}

#[test]
fn rejects_unpinned_node_installation_members() {
    let fixture = fixture_identity();
    write_mode(
        &fixture.node_path.parent().unwrap().join("injected.js"),
        b"unexpected node dependency",
        0o644,
    );
    assert!(
        verify_worker_identity(&fixture.settings).is_err(),
        "an extra Node installation member was accepted"
    );
}

#[test]
fn rejects_installation_files_with_external_hard_links() {
    let fixture = fixture_identity();
    fs::hard_link(
        fixture
            .worker_root
            .join("node_modules/playwright-core/lib/coreBundle.js"),
        fixture.directory.path().join("external-bundle-alias"),
    )
    .unwrap();
    assert!(
        verify_worker_identity(&fixture.settings).is_err(),
        "an installation file with an external alias was accepted"
    );
}

#[test]
fn rejects_writable_installation_directories() {
    let fixture = fixture_identity();
    fs::set_permissions(&fixture.worker_root, fs::Permissions::from_mode(0o777)).unwrap();
    assert!(
        verify_worker_identity(&fixture.settings).is_err(),
        "a writable installation directory was accepted"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn identity_verification_observes_cancellation_while_reading() {
    // Cancellation now reaches the hashing worker through a flag bridge: the
    // outer `cancelled()` resolves, `verify` flags the bridge, and the next
    // checkpoint inside `verify_worker_identity_checked` returns `cancelled`.
    struct YieldCancel;
    impl Cancellation for YieldCancel {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(async {
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
            })
        }
    }
    let fixture = slow_fixture_identity();
    let verifier = LocalWorkerIdentityVerifier::new(fixture.settings);
    let error = verifier
        .verify(&YieldCancel)
        .await
        .expect_err("identity verification ignored cancellation while reading");
    assert_eq!(error.code, "cancelled");
}

/// A Node image large enough that the identity hash takes several executor
/// turns; without it the worker could finish before a concurrent probe runs.
fn slow_fixture_identity() -> IdentityFixture {
    let mut fixture = fixture_identity();
    let node_bytes = vec![0xA5u8; 16 * 1024 * 1024];
    write_mode(&fixture.node_path, &node_bytes, 0o755);
    fixture.manifest.node.sha256 = sha256_hex(&node_bytes);
    fixture.manifest.node.size = node_bytes.len() as u64;
    seal_fixture_installations(&mut fixture);
    fixture
}

struct ImmediateCancel;
impl Cancellation for ImmediateCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::ready(()))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn identity_verification_reports_cancelled_when_the_signal_wins() {
    let fixture = slow_fixture_identity();
    let verifier = LocalWorkerIdentityVerifier::new(fixture.settings);
    let error = verifier
        .verify(&ImmediateCancel)
        .await
        .expect_err("a ready cancellation signal was ignored");
    assert_eq!(error.code, "cancelled");
}

#[tokio::test(flavor = "current_thread")]
async fn identity_verification_keeps_the_executor_responsive() {
    let fixture = slow_fixture_identity();
    let verifier = LocalWorkerIdentityVerifier::new(fixture.settings);
    let order = Arc::new(Mutex::new(Vec::new()));
    let verify = {
        let order = order.clone();
        async move {
            let _ = verifier.verify(&NeverCancel).await;
            order.lock().unwrap().push("verify");
        }
    };
    let probe = {
        let order = order.clone();
        async move {
            // One yield is enough for the executor to make progress while
            // hashing runs on a blocking thread; inline work cannot yield here.
            tokio::task::yield_now().await;
            order.lock().unwrap().push("probe");
        }
    };
    tokio::join!(verify, probe);
    assert_eq!(
        order.lock().unwrap().as_slice(),
        ["probe", "verify"],
        "identity hashing monopolized the current_thread executor"
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
    fixture.manifest.schema_version = 3;
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
async fn startup_rejection_cannot_reuse_a_connection_with_pending_cleanup() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let rejected = FakeConnection::new(pinned_catalog()).with_protocol("2099-01-01");
    rejected.shutdown_failure.store(true, Ordering::Release);
    let fresh = FakeConnection::new(pinned_catalog());
    let launcher = FakeLauncher::new(vec![Ok(rejected.clone()), Ok(fresh.clone())]);
    let provider = provider_with_factory(
        &fixture,
        launcher.clone(),
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );
    assert!(
        provider
            .open(None, &NeverCancel)
            .await
            .unwrap_err()
            .uncertain
    );
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert!(failure.uncertain);
    assert!(rejected.calls().is_empty());
    assert_eq!(launcher.launch_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);

    rejected.shutdown_failure.store(false, Ordering::Release);
    provider.shutdown(&NeverCancel).await.unwrap();
    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(launcher.launch_count(), 2);
    assert!(rejected.calls().is_empty());
    assert_eq!(fresh.calls().len(), 1);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(factory.releases.load(Ordering::Acquire), 2);
}

#[tokio::test]
async fn startup_discard_timeout_retains_the_connection_for_shutdown_retry() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_protocol("2099-01-01");
    connection.hang_next_shutdown();
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );
    let outcome =
        tokio::time::timeout(Duration::from_millis(50), provider.open(None, &NeverCancel)).await;
    assert!(outcome.is_err());
    assert_eq!(connection.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(connection.shutdown_count(), 2);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    assert!(connection.calls().is_empty());
}

#[tokio::test]
async fn startup_catalog_timeout_retains_the_unverified_connection() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog());
    connection.queue_catalog_hang();
    let fresh = FakeConnection::new(pinned_catalog());
    let launcher = FakeLauncher::new(vec![Ok(connection.clone()), Ok(fresh)]);
    let provider = provider_with_factory(
        &fixture,
        launcher.clone(),
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );
    let outcome =
        tokio::time::timeout(Duration::from_millis(50), provider.open(None, &NeverCancel)).await;
    assert!(outcome.is_err());
    assert!(
        provider
            .open(None, &NeverCancel)
            .await
            .unwrap_err()
            .uncertain
    );
    assert_eq!(launcher.launch_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);
    assert!(connection.calls().is_empty());

    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(connection.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(launcher.launch_count(), 2);
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn startup_post_navigation_catalog_timeout_requires_cleanup_before_reuse() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog());
    connection.queue_catalog_reply(Ok(pinned_catalog()));
    connection.queue_catalog_hang();
    let fresh = FakeConnection::new(pinned_catalog());
    let launcher = FakeLauncher::new(vec![Ok(connection.clone()), Ok(fresh.clone())]);
    let provider = provider_with_factory(
        &fixture,
        launcher.clone(),
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );
    let outcome =
        tokio::time::timeout(Duration::from_millis(50), provider.open(None, &NeverCancel)).await;
    assert!(outcome.is_err());
    assert_eq!(connection.calls().len(), 1);
    assert!(
        provider
            .open(None, &NeverCancel)
            .await
            .unwrap_err()
            .uncertain
    );
    assert_eq!(connection.calls().len(), 1);
    assert_eq!(launcher.launch_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);

    provider
        .close("worker_session", &NeverCancel)
        .await
        .unwrap();
    assert_eq!(connection.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(launcher.launch_count(), 2);
    assert_eq!(fresh.calls().len(), 1);
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn startup_catalog_resync_failure_with_unconfirmed_cleanup_is_unknown() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog());
    connection.queue_catalog_reply(Ok(pinned_catalog()));
    connection.queue_catalog_reply(Err(BrowserFailure::new(
        "worker_transport",
        "Catalogue refresh failed before a session was confirmed",
    )));
    connection.shutdown_failure.store(true, Ordering::Release);
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    );
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(failure.code, "outcome_unknown");
    assert_eq!(connection.calls().len(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);
    connection.shutdown_failure.store(false, Ordering::Release);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(connection.shutdown_count(), 2);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
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
    let provider = PlaywrightMcpProvider::new(fixture.settings.clone(), launcher.clone())
        .unwrap()
        .with_sandbox_inspector(offline_sandbox_inspector());

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

#[test]
fn launch_planner_rejects_replaced_verified_executables() {
    for node in [true, false] {
        let fixture = fixture_identity();
        let identity = verify_worker_identity(&fixture.settings).unwrap();
        let artifacts = TempArtifacts::create(&fixture.settings).unwrap();
        let path = if node {
            &fixture.node_path
        } else {
            &fixture.chromium_path
        };
        let before = fs::metadata(path).unwrap();
        let replacement = path.with_extension("replacement");
        write_mode(&replacement, &fs::read(path).unwrap(), 0o755);
        fs::rename(replacement, path).unwrap();
        assert_ne!(fs::metadata(path).unwrap().ino(), before.ino());

        let failure = LocalWorkerLaunchPlanner::new(fixture.settings.clone())
            .build(&identity, &artifacts.directories())
            .expect_err("a replaced verified executable must not produce a launch request");
        assert_eq!(failure.0.code, "worker_identity");
        assert!(!artifacts.config_path.exists());
    }
}

#[test]
fn launch_planner_rejects_backdated_verified_executable_mutations() {
    for node in [true, false] {
        let fixture = fixture_identity();
        let identity = verify_worker_identity(&fixture.settings).unwrap();
        let artifacts = TempArtifacts::create(&fixture.settings).unwrap();
        let path = if node {
            &fixture.node_path
        } else {
            &fixture.chromium_path
        };
        let before = fs::metadata(path).unwrap();
        let mut changed = fs::read(path).unwrap();
        changed[0] ^= 1;
        fs::write(path, changed).unwrap();
        File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(before.modified().unwrap()))
            .unwrap();
        let after = fs::metadata(path).unwrap();
        assert_eq!(after.ino(), before.ino());
        assert_eq!(after.len(), before.len());
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());

        let failure = LocalWorkerLaunchPlanner::new(fixture.settings.clone())
            .build(&identity, &artifacts.directories())
            .expect_err("a backdated executable mutation must not produce a launch request");
        assert_eq!(failure.0.code, "worker_identity");
        assert!(!artifacts.config_path.exists());
    }
}

#[tokio::test]
async fn production_launcher_rechecks_images_changed_after_planning() {
    for node in [true, false] {
        let fixture = fixture_identity();
        let identity = verify_worker_identity(&fixture.settings).unwrap();
        let artifacts = TempArtifacts::create(&fixture.settings).unwrap();
        let request = LocalWorkerLaunchPlanner::new(fixture.settings.clone())
            .build(&identity, &artifacts.directories())
            .unwrap();
        let launcher = ManagedProcessLauncher::new(ExecConfig::default());
        let no_environment = launcher
            .launch(request.clone(), &NeverCancel)
            .await
            .err()
            .expect("a native environment is still mandatory");
        assert_eq!(no_environment.code, "browser_sandbox_unavailable");
        let path = if node {
            &fixture.node_path
        } else {
            &fixture.chromium_path
        };
        let replacement = path.with_extension("replacement");
        write_mode(&replacement, &fs::read(path).unwrap(), 0o755);
        fs::rename(replacement, path).unwrap();

        let failure = launcher
            .launch(request, &NeverCancel)
            .await
            .err()
            .expect("a changed image must be rejected before environment dispatch");
        assert_eq!(failure.code, "worker_identity");
        assert!(!failure.uncertain);
    }
}

#[tokio::test]
async fn production_launcher_honors_cancellation_after_entry() {
    struct CancelAfterEntry(AtomicUsize);
    impl Cancellation for CancelAfterEntry {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, Ordering::AcqRel) > 0
        }

        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }

    let fixture = fixture_identity();
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let artifacts = TempArtifacts::create(&fixture.settings).unwrap();
    let request = LocalWorkerLaunchPlanner::new(fixture.settings.clone())
        .build(&identity, &artifacts.directories())
        .unwrap();
    let failure = ManagedProcessLauncher::new(ExecConfig::default())
        .launch(request, &CancelAfterEntry(AtomicUsize::new(0)))
        .await
        .err()
        .expect("cancellation during image recheck must stop dispatch");
    assert_eq!(failure.code, "cancelled");
    assert!(!failure.uncertain);
}

#[tokio::test]
async fn production_launcher_honors_image_recheck_deadline() {
    let fixture = fixture_identity();
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let artifacts = TempArtifacts::create(&fixture.settings).unwrap();
    let mut request = LocalWorkerLaunchPlanner::new(fixture.settings.clone())
        .build(&identity, &artifacts.directories())
        .unwrap();
    request.mcp_limits.connect_timeout = Duration::from_nanos(1);
    let failure = ManagedProcessLauncher::new(ExecConfig::default())
        .launch(request, &NeverCancel)
        .await
        .err()
        .expect("an expired image recheck must stop dispatch");
    assert_eq!(failure.code, "worker_identity");
    assert!(!failure.uncertain);
}

#[tokio::test]
async fn production_launcher_rejects_a_missing_verified_identity() {
    let fixture = fixture_identity();
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let artifacts = TempArtifacts::create(&fixture.settings).unwrap();
    let mut request = LocalWorkerLaunchPlanner::new(fixture.settings.clone())
        .build(&identity, &artifacts.directories())
        .unwrap();
    request.identity = None;
    let failure = ManagedProcessLauncher::new(ExecConfig::default())
        .launch(request, &NeverCancel)
        .await
        .err()
        .expect("a local launch requires the verified executable identities");
    assert_eq!(failure.code, "worker_identity");
    assert!(!failure.uncertain);
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

fn offline_sandbox_inspector() -> Arc<FixedInspector> {
    FixedInspector::new(Ok(ChromiumSandboxEvidence {
        renderer_pid: 400,
        no_new_privs: true,
        seccomp_mode: 2,
        sandbox_arguments_valid: true,
        stderr_initializes_sandbox: true,
    }))
}

fn provider_with_factory(
    fixture: &IdentityFixture,
    launcher: Arc<dyn WorkerLauncher>,
    factory: Arc<dyn BrowserEnvironmentFactory>,
) -> PlaywrightMcpProvider {
    PlaywrightMcpProvider::build(fixture.settings.clone(), launcher, Some(factory))
        .unwrap()
        .with_sandbox_inspector(offline_sandbox_inspector())
}

struct FixedInspector {
    evidence: Mutex<Result<ChromiumSandboxEvidence, BrowserFailure>>,
    calls: AtomicUsize,
    hang_next: AtomicBool,
}

impl FixedInspector {
    fn new(evidence: Result<ChromiumSandboxEvidence, BrowserFailure>) -> Arc<Self> {
        Arc::new(Self {
            evidence: Mutex::new(evidence),
            calls: AtomicUsize::new(0),
            hang_next: AtomicBool::new(false),
        })
    }

    fn set_evidence(&self, evidence: Result<ChromiumSandboxEvidence, BrowserFailure>) {
        *self.evidence.lock().unwrap() = evidence;
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
        if self.hang_next.swap(false, Ordering::AcqRel) {
            std::future::pending::<()>().await;
        }
        self.evidence.lock().unwrap().clone()
    }

    async fn inspect_connection(
        &self,
        connection: &dyn WorkerConnection,
        cancel: &dyn Cancellation,
    ) -> Result<ChromiumSandboxEvidence, BrowserFailure> {
        self.inspect(
            connection.process_id().unwrap_or(0),
            &connection.stderr_tail(),
            cancel,
        )
        .await
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
async fn sandbox_evidence_tracks_the_renderer_after_navigation() {
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
    assert_eq!(provider.sandbox_evidence().await, Some(evidence.clone()));
    let next = ChromiumSandboxEvidence {
        renderer_pid: 79,
        ..evidence
    };
    inspector.set_evidence(Ok(next.clone()));
    provider
        .navigate(
            &opened.session,
            &opened.page,
            "https://example.test/next",
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(provider.sandbox_evidence().await, Some(next));
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn sandbox_evidence_is_invalidated_when_the_page_closes() {
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
    provider.close(&opened.session, &NeverCancel).await.unwrap();
    assert_eq!(provider.sandbox_evidence().await, None);

    let next = ChromiumSandboxEvidence {
        renderer_pid: 79,
        ..evidence
    };
    inspector.set_evidence(Ok(next.clone()));
    provider.open(None, &NeverCancel).await.unwrap();
    assert_eq!(provider.sandbox_evidence().await, Some(next));
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn sandbox_failure_after_navigation_terminates_the_lifecycle() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_pid(77);
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let inspector = FixedInspector::new(Ok(ChromiumSandboxEvidence {
        renderer_pid: 78,
        no_new_privs: true,
        seccomp_mode: 2,
        sandbox_arguments_valid: true,
        stderr_initializes_sandbox: true,
    }));
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    )
    .with_sandbox_inspector(inspector.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();
    inspector.set_evidence(Err(BrowserFailure::new(
        "browser_sandbox_unavailable",
        "Replacement renderer sandbox was not witnessed",
    )));
    let failure = provider
        .navigate(
            &opened.session,
            &opened.page,
            "https://example.test/next",
            &NeverCancel,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert_eq!(provider.sandbox_evidence().await, None);
    assert_eq!(connection.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    assert!(
        provider
            .snapshot(&opened.session, &opened.page, 1024, &NeverCancel)
            .await
            .is_err()
    );
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn sandbox_failure_with_pending_cleanup_blocks_further_dispatch() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_pid(77);
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
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
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    )
    .with_sandbox_inspector(inspector.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();
    connection.shutdown_failure.store(true, Ordering::Release);
    inspector.set_evidence(Err(BrowserFailure::new(
        "browser_sandbox_unavailable",
        "Replacement renderer sandbox was not witnessed",
    )));
    assert!(
        provider
            .snapshot(&opened.session, &opened.page, 1024, &NeverCancel)
            .await
            .is_err()
    );
    assert_eq!(provider.sandbox_evidence().await, None);
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);
    inspector.set_evidence(Ok(evidence));
    let dispatched = connection.calls().len();
    let failure = provider
        .click(&opened.session, &opened.page, "e1", None, &NeverCancel)
        .await
        .unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(connection.calls().len(), dispatched);
    connection.shutdown_failure.store(false, Ordering::Release);
    provider.close(&opened.session, &NeverCancel).await.unwrap();
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    assert_eq!(connection.calls().len(), dispatched);
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[tokio::test]
async fn sandbox_failure_during_open_reports_unconfirmed_cleanup_as_unknown() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_pid(77);
    connection.shutdown_failure.store(true, Ordering::Release);
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let inspector = FixedInspector::new(Err(BrowserFailure::new(
        "browser_sandbox_unavailable",
        "Initial renderer sandbox was not witnessed",
    )));
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    )
    .with_sandbox_inspector(inspector);
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert!(
        failure.uncertain,
        "unconfirmed cleanup requires full shutdown recovery"
    );
    assert_eq!(factory.releases.load(Ordering::Acquire), 0);
    connection.shutdown_failure.store(false, Ordering::Release);
    provider.shutdown(&NeverCancel).await.unwrap();
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn sandbox_verification_dropped_by_timeout_quarantines_the_lifecycle() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_pid(77);
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let inspector = FixedInspector::new(Ok(ChromiumSandboxEvidence {
        renderer_pid: 78,
        no_new_privs: true,
        seccomp_mode: 2,
        sandbox_arguments_valid: true,
        stderr_initializes_sandbox: true,
    }));
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    )
    .with_sandbox_inspector(inspector.clone());
    let opened = provider.open(None, &NeverCancel).await.unwrap();
    inspector.hang_next.store(true, Ordering::Release);
    let outcome = tokio::time::timeout(
        Duration::from_millis(20),
        provider.snapshot(&opened.session, &opened.page, 1024, &NeverCancel),
    )
    .await;
    assert!(
        outcome.is_err(),
        "the new verification must be awaiting evidence"
    );
    assert_eq!(provider.sandbox_evidence().await, None);
    let dispatched = connection.calls().len();
    let failure = provider
        .press(&opened.session, &opened.page, "Enter", &NeverCancel)
        .await
        .unwrap_err();
    assert!(failure.uncertain);
    assert_eq!(connection.calls().len(), dispatched);
    provider.close(&opened.session, &NeverCancel).await.unwrap();
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    provider.shutdown(&NeverCancel).await.unwrap();
}

#[derive(Debug, Clone)]
struct FakeProcNode {
    ppid: u32,
    nspid: Vec<u32>,
    no_new_privs: bool,
    seccomp_mode: u32,
    seccomp_filters: Option<u32>,
    cap_eff_zero: bool,
    cmdline: Vec<String>,
    children: Vec<u32>,
    starttime: u64,
    alive: bool,
    exe: &'static str,
    pid_ns: u64,
    user_ns: u64,
}

#[derive(Default)]
struct FakeKernelState {
    processes: BTreeMap<u32, FakeProcNode>,
    pid_ns_parents: BTreeMap<u64, u64>,
    pid_ns_owners: BTreeMap<u64, u64>,
    user_ns_parents: BTreeMap<u64, u64>,
    ns_eperm: BTreeSet<u64>,
    owner_eperm: BTreeSet<u64>,
    hard_status_error_pids: BTreeSet<u32>,
}

struct FakeKernelProbe {
    state: Mutex<FakeKernelState>,
    node_path: PathBuf,
    chromium_path: PathBuf,
}

impl FakeKernelProbe {
    fn new(node_path: PathBuf, chromium_path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeKernelState::default()),
            node_path,
            chromium_path,
        })
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, FakeKernelState> {
        self.state.lock().unwrap()
    }
}

fn test_pin(pid: u32, starttime: u64) -> HostProcessPin {
    HostProcessPin {
        pid,
        starttime,
        #[cfg(target_os = "linux")]
        pidfd: None,
    }
}

fn test_verified_ns(ino: u64) -> VerifiedNamespace {
    VerifiedNamespace {
        id: KernelNamespaceId { dev: 4, ino },
        #[cfg(target_os = "linux")]
        fd: None,
    }
}

impl NativeKernelProbe for FakeKernelProbe {
    fn check_supervisor_pin(&self, pin: &HostProcessPin) -> Result<(), SandboxProbeFailure> {
        self.check_process_pin(pin)
    }

    fn capture_process_pin(&self, pid: u32) -> Result<HostProcessPin, SandboxProbeFailure> {
        let state = self.locked();
        let node = state
            .processes
            .get(&pid)
            .ok_or(SandboxProbeFailure::Hard("Simulated process is missing"))?;
        if !node.alive {
            return Err(SandboxProbeFailure::Hard("Simulated process has exited"));
        }
        Ok(test_pin(pid, node.starttime))
    }

    fn check_process_pin(&self, pin: &HostProcessPin) -> Result<(), SandboxProbeFailure> {
        let state = self.locked();
        let node = state
            .processes
            .get(&pin.pid)
            .ok_or(SandboxProbeFailure::Hard("Simulated process disappeared"))?;
        if !node.alive || node.starttime != pin.starttime {
            return Err(SandboxProbeFailure::Hard(
                "Simulated process exited or recycled",
            ));
        }
        Ok(())
    }

    fn read_status(&self, pid: u32) -> Result<KernelProcStatus, SandboxProbeFailure> {
        let state = self.locked();
        if state.hard_status_error_pids.contains(&pid) {
            return Err(SandboxProbeFailure::Hard("Simulated procfs read failure"));
        }
        let node = state
            .processes
            .get(&pid)
            .ok_or(SandboxProbeFailure::Transient(
                "Simulated process disappeared",
            ))?;
        Ok(KernelProcStatus {
            ppid: Some(node.ppid),
            nspid: node.nspid.clone(),
            no_new_privs: node.no_new_privs,
            seccomp_mode: node.seccomp_mode,
            seccomp_filters: node.seccomp_filters,
            cap_eff_zero: node.cap_eff_zero,
        })
    }

    fn read_cmdline(&self, pid: u32) -> Result<Vec<String>, SandboxProbeFailure> {
        let state = self.locked();
        let node = state
            .processes
            .get(&pid)
            .ok_or(SandboxProbeFailure::Transient(
                "Simulated process disappeared",
            ))?;
        Ok(node.cmdline.clone())
    }

    fn task_children(&self, pid: u32) -> Result<Vec<u32>, SandboxProbeFailure> {
        let state = self.locked();
        let node = state
            .processes
            .get(&pid)
            .ok_or(SandboxProbeFailure::Transient(
                "Simulated parent disappeared",
            ))?;
        Ok(node.children.clone())
    }

    fn verify_exe(
        &self,
        pid: u32,
        expected_path: &Path,
        _pinned: &PinnedWorkerImage,
    ) -> Result<(), SandboxProbeFailure> {
        let state = self.locked();
        let node = state
            .processes
            .get(&pid)
            .ok_or(SandboxProbeFailure::Hard("Simulated exe process missing"))?;
        let expected_tag = if expected_path == self.node_path {
            "node"
        } else if expected_path == self.chromium_path {
            "chromium"
        } else {
            return Err(SandboxProbeFailure::Hard("Unexpected executable path"));
        };
        if node.exe == expected_tag {
            Ok(())
        } else {
            Err(SandboxProbeFailure::Hard(
                "Simulated process executable mismatch",
            ))
        }
    }

    fn inspect_namespace(
        &self,
        pid: u32,
        kind: KernelNamespaceKind,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure> {
        let state = self.locked();
        let node = state
            .processes
            .get(&pid)
            .ok_or(SandboxProbeFailure::Transient(
                "Simulated namespace process disappeared",
            ))?;
        let ino = match kind {
            KernelNamespaceKind::Pid => node.pid_ns,
            KernelNamespaceKind::User => node.user_ns,
        };
        Ok(test_verified_ns(ino))
    }

    fn parent_namespace(
        &self,
        ns: &VerifiedNamespace,
        expected_kind: KernelNamespaceKind,
    ) -> Result<Option<VerifiedNamespace>, SandboxProbeFailure> {
        let state = self.locked();
        if state.ns_eperm.contains(&ns.id.ino) {
            return Err(SandboxProbeFailure::PermissionDenied);
        }
        let table = match expected_kind {
            KernelNamespaceKind::Pid => &state.pid_ns_parents,
            KernelNamespaceKind::User => &state.user_ns_parents,
        };
        Ok(table.get(&ns.id.ino).copied().map(test_verified_ns))
    }

    fn pid_ns_owner_user_ns(
        &self,
        pid_ns: &VerifiedNamespace,
    ) -> Result<VerifiedNamespace, SandboxProbeFailure> {
        let state = self.locked();
        if state.owner_eperm.contains(&pid_ns.id.ino) {
            return Err(SandboxProbeFailure::PermissionDenied);
        }
        let owner =
            state
                .pid_ns_owners
                .get(&pid_ns.id.ino)
                .copied()
                .ok_or(SandboxProbeFailure::Hard(
                    "Simulated pid namespace owner missing",
                ))?;
        Ok(test_verified_ns(owner))
    }
}

struct TestNativeConnection {
    process: Arc<AsyncMutex<Option<ManagedProcess>>>,
    local_images: Arc<VerifiedLocalImages>,
    node_path: PathBuf,
    chromium_path: PathBuf,
    baseline: NativeLaunchBaseline,
    stderr: Mutex<Vec<u8>>,
    replies: Mutex<VecDeque<Result<WorkerProcessInfo, BrowserFailure>>>,
    steady_reply: Mutex<Option<Result<WorkerProcessInfo, BrowserFailure>>>,
    delays: Mutex<VecDeque<Duration>>,
    hang_on_call: Mutex<Option<usize>>,
    process_info_calls: AtomicUsize,
}

impl TestNativeConnection {
    fn from_identity(
        identity: VerifiedWorkerIdentity,
        baseline: NativeLaunchBaseline,
        stderr: &[u8],
        reply: WorkerProcessInfo,
    ) -> Self {
        Self {
            process: Arc::new(AsyncMutex::new(None)),
            local_images: identity.local_images.expect("local images verified"),
            node_path: identity.node,
            chromium_path: identity.chromium,
            baseline,
            stderr: Mutex::new(stderr.to_vec()),
            replies: Mutex::new(VecDeque::new()),
            steady_reply: Mutex::new(Some(Ok(reply))),
            delays: Mutex::new(VecDeque::new()),
            hang_on_call: Mutex::new(None),
            process_info_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl WorkerConnection for TestNativeConnection {
    fn protocol_version(&self) -> Option<String> {
        Some(WORKER_PROTOCOL_VERSION.to_owned())
    }

    fn generation(&self) -> u64 {
        0
    }

    fn process_id(&self) -> Option<u32> {
        Some(self.baseline.supervisor.pid)
    }

    fn stderr_tail(&self) -> Vec<u8> {
        self.stderr.lock().unwrap().clone()
    }

    async fn native_sandbox_context(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<NativeSandboxContext<'_>, BrowserFailure> {
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new("cancelled", "cancelled"));
        }
        let guard = self.process.clone().lock_owned().await;
        Ok(NativeSandboxContext {
            _process_guard: guard,
            local_images: self.local_images.clone(),
            node_path: &self.node_path,
            chromium_path: &self.chromium_path,
            baseline: &self.baseline,
        })
    }

    async fn process_info(
        &self,
        cancel: &dyn Cancellation,
    ) -> Result<WorkerProcessInfo, BrowserFailure> {
        let call_index = self.process_info_calls.fetch_add(1, Ordering::AcqRel) + 1;
        let next_delay = self.delays.lock().unwrap().pop_front();
        if let Some(delay) = next_delay {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(BrowserFailure::new("cancelled", "cancelled")),
                _ = tokio::time::sleep(delay) => {}
            }
        }
        let should_hang = *self.hang_on_call.lock().unwrap() == Some(call_index);
        if should_hang {
            cancel.cancelled().await;
            return Err(BrowserFailure::new("cancelled", "cancelled"));
        }
        let queued = self.replies.lock().unwrap().pop_front();
        if let Some(next) = queued {
            return next;
        }
        self.steady_reply
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Err(BrowserFailure::new("worker_process_info", "missing reply")))
    }

    async fn list_tools(&self, _cancel: &dyn Cancellation) -> Result<McpCatalog, BrowserFailure> {
        unreachable!()
    }

    async fn call_tool(
        &self,
        _name: &str,
        _arguments: Value,
        _generation: u64,
        _cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure> {
        unreachable!()
    }

    async fn shutdown(&self) -> Result<(), BrowserFailure> {
        Ok(())
    }
}

fn native_role_fixture(
    renderer_depth_k2: bool,
) -> (
    IdentityFixture,
    Arc<FakeKernelProbe>,
    NativeLaunchBaseline,
    WorkerProcessInfo,
) {
    let fixture = fixture_identity();
    let probe = FakeKernelProbe::new(fixture.node_path.clone(), fixture.chromium_path.clone());
    let mut state = probe.locked();
    let (renderer_pid_ns, renderer_nspid, renderer_ppid, browser_child) = if renderer_depth_k2 {
        state.pid_ns_parents.insert(30, 20);
        state.pid_ns_parents.insert(20, 10);
        state.pid_ns_owners.insert(30, 1001);
        state.pid_ns_owners.insert(20, 1001);
        state.processes.insert(
            500,
            FakeProcNode {
                ppid: 300,
                nspid: vec![500, 6, 1],
                no_new_privs: true,
                seccomp_mode: 2,
                seccomp_filters: Some(2),
                cap_eff_zero: true,
                cmdline: vec!["/chromium".into(), "--type=zygote".into()],
                children: vec![63582],
                starttime: 50,
                alive: true,
                exe: "chromium",
                pid_ns: 20,
                user_ns: 1001,
            },
        );
        (30, vec![63582, 74, 4, 1], 500, 500)
    } else {
        state.pid_ns_parents.insert(20, 10);
        state.pid_ns_owners.insert(20, 1001);
        (20, vec![63582, 74, 1], 300, 63582)
    };
    state.user_ns_parents.insert(1001, 1002);
    state.user_ns_parents.insert(1002, 1000);
    state.pid_ns_owners.insert(10, 1000);

    state.processes.insert(
        100,
        FakeProcNode {
            ppid: 1,
            nspid: vec![100],
            no_new_privs: false,
            seccomp_mode: 0,
            seccomp_filters: None,
            cap_eff_zero: false,
            cmdline: vec!["bwrap".into()],
            children: vec![200],
            starttime: 10,
            alive: true,
            exe: "supervisor",
            pid_ns: 1,
            user_ns: 999,
        },
    );
    state.processes.insert(
        200,
        FakeProcNode {
            ppid: 100,
            nspid: vec![200, 3],
            no_new_privs: true,
            seccomp_mode: 2,
            seccomp_filters: Some(2),
            cap_eff_zero: true,
            cmdline: vec!["node".into(), "cli.js".into()],
            children: vec![300],
            starttime: 20,
            alive: true,
            exe: "node",
            pid_ns: 10,
            user_ns: 1000,
        },
    );
    state.processes.insert(
        300,
        FakeProcNode {
            ppid: 200,
            nspid: vec![300, 15],
            no_new_privs: true,
            seccomp_mode: 2,
            seccomp_filters: Some(2),
            cap_eff_zero: true,
            cmdline: vec!["/chromium".into(), "--headless".into()],
            children: vec![browser_child],
            starttime: 30,
            alive: true,
            exe: "chromium",
            pid_ns: 10,
            user_ns: 1000,
        },
    );
    state.processes.insert(
        63582,
        FakeProcNode {
            ppid: renderer_ppid,
            nspid: renderer_nspid,
            no_new_privs: true,
            seccomp_mode: 2,
            seccomp_filters: Some(3),
            cap_eff_zero: true,
            cmdline: vec!["/chromium".into(), "--type=renderer".into()],
            children: vec![],
            starttime: 60,
            alive: true,
            exe: "chromium",
            pid_ns: renderer_pid_ns,
            user_ns: 1001,
        },
    );
    drop(state);

    let baseline = NativeLaunchBaseline {
        supervisor: test_pin(100, 10),
        node: NodeLaunchBaseline {
            pin: test_pin(200, 20),
            ns_pid: 3,
            pid_ns: KernelNamespaceId { dev: 4, ino: 10 },
            user_ns: KernelNamespaceId { dev: 4, ino: 1000 },
            seccomp_filters: 2,
        },
    };
    let info = WorkerProcessInfo {
        schema_version: 1,
        node_pid: 3,
        node_pid_namespace: WorkerPidNamespace {
            dev: "4".into(),
            ino: "10".into(),
        },
        browser_pid: 15,
        renderer_pids: vec![74],
    };
    (fixture, probe, baseline, info)
}

#[tokio::test]
async fn procfs_sandbox_evidence_requires_a_real_renderer() {
    let (fixture, probe, baseline, info) = native_role_fixture(false);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection = TestNativeConnection::from_identity(
        identity,
        baseline,
        b"Chromium InitializeSandbox() called",
        info,
    );
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(200),
        Duration::from_millis(10),
    );
    let evidence = inspector
        .inspect_connection(&connection, &NeverCancel)
        .await
        .unwrap();
    assert_eq!(evidence.renderer_pid, 74);
    assert!(evidence.no_new_privs);
    assert_eq!(evidence.seccomp_mode, 2);
    assert!(evidence.sandbox_arguments_valid);
    assert!(evidence.stderr_initializes_sandbox);
}

#[tokio::test]
async fn procfs_sandbox_evidence_rejects_bubblewrap_only_markers() {
    let (fixture, probe, baseline, info) = native_role_fixture(false);
    probe
        .locked()
        .processes
        .get_mut(&63582)
        .unwrap()
        .seccomp_filters = Some(2);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection = TestNativeConnection::from_identity(
        identity,
        baseline,
        b"bubblewrap seccomp installed",
        info,
    );
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(100),
        Duration::from_millis(10),
    );
    let failure = inspector
        .inspect_connection(&connection, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
}

#[tokio::test]
async fn procfs_sandbox_evidence_rejects_disabled_or_unrelated_processes() {
    for (cmdline, nnp, seccomp) in [
        (
            vec!["/chromium", "--type=renderer", "--no-sandbox"],
            true,
            2,
        ),
        (
            vec!["/chromium", "--type=renderer", "--disable-setuid-sandbox"],
            true,
            2,
        ),
        (vec!["/chromium", "--type=renderer"], false, 2),
        (vec!["/chromium", "--type=renderer"], true, 1),
        (
            vec!["/chromium --type=renderer --no-sandbox --headless=old"],
            true,
            2,
        ),
        (
            vec!["/chromium --type=renderer --disable-setuid-sandbox=1 --headless=old"],
            true,
            2,
        ),
    ] {
        let (fixture, probe, baseline, info) = native_role_fixture(false);
        {
            let mut state = probe.locked();
            let renderer = state.processes.get_mut(&63582).unwrap();
            renderer.cmdline = cmdline.into_iter().map(str::to_owned).collect();
            renderer.no_new_privs = nnp;
            renderer.seccomp_mode = seccomp;
        }
        let identity = verify_worker_identity(&fixture.settings).unwrap();
        let connection =
            TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
        let inspector = ProcfsChromiumSandboxInspector::with_probe(
            probe,
            Duration::from_millis(50),
            Duration::from_millis(10),
        );
        let failure = inspector
            .inspect_connection(&connection, &NeverCancel)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "browser_sandbox_unavailable");
    }

    // Mismatched PPid between children list and child status must not be accepted.
    let (fixture, probe, baseline, info) = native_role_fixture(false);
    {
        let mut state = probe.locked();
        state.processes.get_mut(&63582).unwrap().ppid = 999;
    }
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(50),
        Duration::from_millis(10),
    );
    assert_eq!(
        inspector
            .inspect_connection(&connection, &NeverCancel)
            .await
            .unwrap_err()
            .code,
        "browser_sandbox_unavailable"
    );

    let (fixture, probe, baseline, info) = native_role_fixture(false);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(200),
        Duration::from_millis(10),
    );
    assert_eq!(
        inspector
            .inspect_connection(&connection, &SwitchCancel(Arc::new(AtomicBool::new(true))))
            .await
            .unwrap_err()
            .code,
        "cancelled"
    );
}

fn test_png_bytes(width: u32, height: u32, extra_tail: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(33 + 12 + extra_tail);
    bytes.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 2, 0, 0, 0]);
    bytes.extend_from_slice(&[0; 4]);
    bytes.resize(bytes.len() + extra_tail, b'x');
    bytes.extend_from_slice(&[0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xae, 0x42, 0x60, 0x82]);
    bytes
}

fn captured_screenshot_reply(width: u32, height: u32, extra_tail: usize) -> Value {
    json!({
        "schema_version": 1,
        "protocol_version": 1,
        "mime": "image/png",
        "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, test_png_bytes(width, height, extra_tail)),
        "width": width,
        "height": height,
        "masked": true,
        "url": "http://allowed.example/"
    })
}

#[tokio::test]
async fn pinned_worker_declares_masked_screenshot_capability_and_fails_closed() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    let (provider, _) = fake_provider(&fixture, connection.clone());
    let facts = provider.facts();
    assert!(facts.screenshots);
    assert!(facts.screenshot_masking);
    assert!(facts.downloads);
    assert!(facts.uploads);
    let cancel = SwitchCancel(Arc::new(AtomicBool::new(false)));
    let opened = provider.open(None, &cancel).await.unwrap();

    // Runtime capability still fails closed if worker/engine refuses masking.
    let error = provider
        .screenshot(&opened.session, &opened.page, &cancel)
        .await
        .unwrap_err();
    assert_eq!(error.code, "browser_capability_disabled");

    // Valid masked screenshot succeeds and decodes verified PNG + IHDR dimensions.
    connection.push_screenshot(Ok(captured_screenshot_reply(320, 240, 0)));
    let shot = provider
        .screenshot(&opened.session, &opened.page, &cancel)
        .await
        .unwrap();
    assert_eq!(shot.mime, "image/png");
    assert_eq!(shot.width, 320);
    assert_eq!(shot.height, 240);
    assert!(shot.masked);
    assert_eq!(shot.url.as_deref(), Some("http://allowed.example/"));
    provider.shutdown(&cancel).await.unwrap();
}

#[test]
fn install_roots_cover_node_worker_and_chromium_package_only() {
    let fixture = fixture_identity();
    let roots = browser_install_roots(&fixture.settings).unwrap();
    assert!(roots.contains(&fixture.node_path.parent().unwrap().to_path_buf()));
    assert!(roots.contains(&fixture.worker_root));
    assert!(roots.contains(&fixture.chromium_path.parent().unwrap().to_path_buf()));
    assert!(!roots.iter().any(|root| root.ends_with(".soyo")));
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

struct ProcessInfoFixture {
    connection: Arc<RealWorkerConnection>,
    requests: Arc<Mutex<Vec<Value>>>,
    received: tokio::sync::mpsc::Receiver<Value>,
    task: tokio::task::JoinHandle<()>,
    _directory: TempDir,
}

impl ProcessInfoFixture {
    async fn observed_method(&mut self, method: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(1), async {
            while let Some(message) = self.received.recv().await {
                if message["method"] == method {
                    return message;
                }
            }
            panic!("wire closed before {method}");
        })
        .await
        .unwrap_or_else(|_| panic!("wire method {method} did not arrive"))
    }
}

impl Drop for ProcessInfoFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn process_info_reply() -> Value {
    json!({
        "schema_version": 1,
        "node_pid": 3,
        "node_pid_namespace": { "dev": "4", "ino": "4026532444" },
        "browser_pid": 15,
        "renderer_pids": [74, 81]
    })
}

async fn process_info_fixture(reply: Value) -> ProcessInfoFixture {
    process_info_fixture_with_limits(
        Some(reply),
        McpLimits {
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(1),
            ..McpLimits::default()
        },
    )
    .await
}

async fn process_info_fixture_with_limits(
    reply: Option<Value>,
    limits: McpLimits,
) -> ProcessInfoFixture {
    process_info_fixture_transport(reply, limits, false).await
}

async fn process_info_fixture_transport(
    reply: Option<Value>,
    limits: McpLimits,
    stall_after_query: bool,
) -> ProcessInfoFixture {
    let mut replies = VecDeque::new();
    if let Some(reply) = reply {
        replies.push_back(Ok(reply));
    }
    custom_method_fixture(
        "soyo/browser/processInfo",
        replies,
        limits,
        stall_after_query,
    )
    .await
}

type CustomRpcReply = Result<Value, (i32, &'static str, Option<Value>)>;

async fn custom_method_fixture(
    custom_method: &'static str,
    mut replies: VecDeque<CustomRpcReply>,
    limits: McpLimits,
    stall_after_query: bool,
) -> ProcessInfoFixture {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (client, server) = tokio::io::duplex(if stall_after_query { 1 } else { 64 * 1024 });
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, mut server_write) = tokio::io::split(server);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = requests.clone();
    let (sent, received) = tokio::sync::mpsc::channel(64);
    let task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_read).lines();
        while let Some(line) = lines.next_line().await.unwrap() {
            let request: Value = serde_json::from_str(&line).unwrap();
            observed.lock().unwrap().push(request.clone());
            if sent.send(request.clone()).await.is_err() {
                break;
            }
            let Some(id) = request.get("id") else {
                continue;
            };
            let method = request["method"].as_str().unwrap();
            let response = match method {
                "server/discover" => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "legacy fixture" }
                }),
                "initialize" => json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "protocolVersion": "2025-11-25", "capabilities": {},
                        "serverInfo": { "name": "process-info-fixture", "version": "1" }
                    }
                }),
                _ if method == custom_method => match replies.pop_front() {
                    Some(Ok(reply)) => json!({
                        "jsonrpc": "2.0", "id": id, "result": reply
                    }),
                    Some(Err((code, message, data))) => {
                        let mut error = json!({ "code": code, "message": message });
                        if let Some(data) = data {
                            error["data"] = data;
                        }
                        json!({ "jsonrpc": "2.0", "id": id, "error": error })
                    }
                    None if stall_after_query => std::future::pending().await,
                    None => continue,
                },
                _ => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": "unexpected method" }
                }),
            };
            if server_write
                .write_all(format!("{response}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let client = crate::mcp::McpClient::connect_stdio(
        "soyo-browser-worker",
        client_read,
        client_write,
        limits,
        &NeverCancel,
    )
    .await
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    ProcessInfoFixture {
        connection: Arc::new(RealWorkerConnection {
            client: AsyncMutex::new(Some(client)),
            process: Arc::new(AsyncMutex::new(None)),
            protocol: WORKER_PROTOCOL_VERSION.to_owned(),
            generation: AtomicU64::new(0),
            pid: 0,
            stderr_tail: Arc::new(Mutex::new(VecDeque::new())),
            work_dir: directory.path().to_path_buf(),
            output_dir: directory.path().to_path_buf(),
            local_images: None,
            node_path: PathBuf::new(),
            chromium_path: PathBuf::new(),
            baseline: None,
            test_sandbox_ready: false,
        }),
        requests,
        received,
        task,
        _directory: directory,
    }
}

#[tokio::test]
async fn real_worker_process_info_uses_one_fixed_private_request_without_catalogue() {
    let fixture = process_info_fixture(process_info_reply()).await;
    let info = fixture
        .connection
        .process_info(&NeverCancel)
        .await
        .expect("fixed private process query must be connected");
    assert_eq!(info.schema_version, 1);
    assert_eq!(info.node_pid, 3);
    assert_eq!(info.node_pid_namespace.dev, "4");
    assert_eq!(info.node_pid_namespace.ino, "4026532444");
    assert_eq!(info.browser_pid, 15);
    assert_eq!(info.renderer_pids, [74, 81]);
    let requests = fixture.requests.lock().unwrap().clone();
    let private: Vec<_> = requests
        .iter()
        .filter(|request| request["method"] == "soyo/browser/processInfo")
        .collect();
    assert_eq!(private.len(), 1);
    assert_eq!(
        private[0]["params"],
        json!({ "_meta": { "progressToken": 0 } })
    );
    assert!(!requests.iter().any(|request| {
        matches!(
            request["method"].as_str(),
            Some("tools/list" | "tools/call")
        )
    }));
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_worker_process_info_cancels_the_dispatched_request() {
    let mut fixture = process_info_fixture_with_limits(None, McpLimits::default()).await;
    let connection = fixture.connection.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let query =
        tokio::spawn(async move { connection.process_info(&SwitchCancel(cancelled)).await });
    let request = fixture.observed_method("soyo/browser/processInfo").await;
    flag.store(true, Ordering::Release);
    let failure = tokio::time::timeout(Duration::from_secs(1), query)
        .await
        .expect("cancelled query did not return")
        .unwrap()
        .expect_err("cancelled query returned role evidence");
    assert_eq!(failure.code, "cancelled");
    assert!(!failure.uncertain);
    let notification = fixture.observed_method("notifications/cancelled").await;
    assert_eq!(notification["params"]["requestId"], request["id"]);
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|message| { message["method"] == "soyo/browser/processInfo" })
            .count(),
        1,
        "cancellation must not replay the private query"
    );
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_worker_process_info_timeout_cancels_the_dispatched_request() {
    let mut fixture = process_info_fixture_with_limits(
        None,
        McpLimits {
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_millis(50),
            ..McpLimits::default()
        },
    )
    .await;
    let connection = fixture.connection.clone();
    let query = tokio::spawn(async move { connection.process_info(&NeverCancel).await });
    let request = fixture.observed_method("soyo/browser/processInfo").await;
    let failure = tokio::time::timeout(Duration::from_secs(1), query)
        .await
        .expect("timed-out query did not return")
        .unwrap()
        .expect_err("timed-out query returned role evidence");
    assert_eq!(failure.code, "worker_process_info");
    assert!(!failure.uncertain);
    let notification = fixture.observed_method("notifications/cancelled").await;
    assert_eq!(notification["params"]["requestId"], request["id"]);
    assert_eq!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|message| { message["method"] == "soyo/browser/processInfo" })
            .count(),
        1,
        "a timed-out private query must not be replayed"
    );
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_worker_process_info_stalled_cancellation_closes_the_connection() {
    let mut fixture = process_info_fixture_transport(None, McpLimits::default(), true).await;
    let connection = fixture.connection.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let query =
        tokio::spawn(async move { connection.process_info(&SwitchCancel(cancelled)).await });
    fixture.observed_method("soyo/browser/processInfo").await;
    flag.store(true, Ordering::Release);
    let failure = tokio::time::timeout(Duration::from_secs(8), query)
        .await
        .expect("stalled cancellation exceeded its cleanup bound")
        .unwrap()
        .expect_err("stalled cancellation returned role evidence");
    assert_eq!(failure.code, "cancelled");
    assert!(
        !fixture
            .connection
            .client
            .lock()
            .await
            .as_ref()
            .unwrap()
            .is_connected(),
        "unfinished cancellation left the connection reusable"
    );
    fixture.task.abort();
    let _ = (&mut fixture.task).await;
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_worker_process_info_dropped_query_closes_the_connection() {
    let mut fixture = process_info_fixture_with_limits(None, McpLimits::default()).await;
    let connection = fixture.connection.clone();
    let query = tokio::spawn(async move { connection.process_info(&NeverCancel).await });
    fixture.observed_method("soyo/browser/processInfo").await;
    query.abort();
    assert!(query.await.unwrap_err().is_cancelled());
    assert!(
        !fixture
            .connection
            .client
            .lock()
            .await
            .as_ref()
            .unwrap()
            .is_connected(),
        "dropped query left an in-flight request on a reusable connection"
    );
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_worker_process_info_rejects_a_valid_role_reply_above_the_byte_limit() {
    let mut reply = process_info_reply();
    reply["renderer_pids"] = json!((1000..1100).collect::<Vec<u32>>());
    let fixture = process_info_fixture_with_limits(
        Some(reply),
        McpLimits {
            max_result_bytes: 256,
            ..McpLimits::default()
        },
    )
    .await;
    let failure = fixture
        .connection
        .process_info(&NeverCancel)
        .await
        .expect_err("private role query bypassed its result byte limit");
    assert_eq!(failure.code, "worker_process_info");
    assert!(!failure.uncertain);
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_worker_process_info_closed_connection_does_not_dispatch() {
    let fixture = process_info_fixture(process_info_reply()).await;
    fixture.connection.shutdown().await.unwrap();
    let failure = fixture
        .connection
        .process_info(&NeverCancel)
        .await
        .expect_err("closed connection returned role evidence");
    assert_eq!(failure.code, "worker_closed");
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|message| { message["method"] == "soyo/browser/processInfo" })
    );
}

#[tokio::test]
async fn real_worker_process_info_pre_cancel_does_not_dispatch() {
    let fixture = process_info_fixture(process_info_reply()).await;
    let failure = fixture
        .connection
        .process_info(&SwitchCancel(Arc::new(AtomicBool::new(true))))
        .await
        .expect_err("pre-cancelled query returned role evidence");
    assert_eq!(failure.code, "cancelled");
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|message| { message["method"] == "soyo/browser/processInfo" })
    );
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_worker_process_info_rejects_untrusted_role_replies() {
    let mut cases = Vec::new();
    for (field, value) in [
        ("schema_version", json!(2)),
        ("node_pid", json!(0)),
        ("browser_pid", json!(0)),
        ("browser_pid", json!(3)),
        ("renderer_pids", json!([74, 74])),
        ("renderer_pids", json!([0])),
        ("renderer_pids", json!([15])),
        ("renderer_pids", json!([3])),
        ("renderer_pids", json!([4294967296_u64])),
        ("renderer_pids", json!(["74"])),
        ("renderer_pids", json!([74.5])),
        ("renderer_pids", json!((1000..1513).collect::<Vec<_>>())),
        ("script", json!("not a process field")),
    ] {
        let mut reply = process_info_reply();
        reply[field] = value;
        cases.push(reply);
    }
    for namespace in [
        json!({ "dev": "4", "ino": "" }),
        json!({ "dev": "-4", "ino": "4026532444" }),
        json!({ "dev": "04", "ino": "4026532444" }),
        json!({ "dev": "4", "ino": "18446744073709551616" }),
        json!({ "dev": 4, "ino": "4026532444" }),
        json!({ "dev": "4", "ino": "4026532444", "other": 1 }),
    ] {
        let mut reply = process_info_reply();
        reply["node_pid_namespace"] = namespace;
        cases.push(reply);
    }
    for reply in cases {
        let fixture = process_info_fixture(reply.clone()).await;
        let failure = fixture
            .connection
            .process_info(&NeverCancel)
            .await
            .expect_err("invalid private role reply was accepted");
        assert_eq!(failure.code, "worker_process_info", "reply={reply}");
        assert!(!failure.uncertain);
        fixture.connection.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn verify_sandbox_without_fixed_inspector_rejects_fake_connection_and_raw_pid_inspect() {
    let fixture = fixture_identity();
    let connection = FakeConnection::new(pinned_catalog());
    assert_eq!(connection.process_id(), None);
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let provider = PlaywrightMcpProvider::new(fixture.settings.clone(), launcher).unwrap();
    let failure = provider.open(None, &NeverCancel).await.unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert_eq!(connection.shutdown_count(), 1);

    let inspector = ProcfsChromiumSandboxInspector::default();
    let direct_failure = inspector
        .inspect(100, b"InitializeSandbox", &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(direct_failure.code, "browser_sandbox_unavailable");

    let cancelled_failure = inspector
        .inspect(
            100,
            b"InitializeSandbox",
            &SwitchCancel(Arc::new(AtomicBool::new(true))),
        )
        .await
        .unwrap_err();
    assert_eq!(cancelled_failure.code, "cancelled");
}

#[tokio::test]
async fn real_worker_connection_without_process_rejects_native_sandbox_context() {
    let fixture = process_info_fixture(process_info_reply()).await;
    let context_err = fixture
        .connection
        .native_sandbox_context(&NeverCancel)
        .await
        .err()
        .expect("missing ManagedProcess must reject native sandbox context");
    assert_eq!(context_err.code, "browser_sandbox_unavailable");

    let inspector_err = ProcfsChromiumSandboxInspector::default()
        .inspect_connection(fixture.connection.as_ref(), &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(inspector_err.code, "browser_sandbox_unavailable");
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_inspector_rejects_empty_renderers_and_q1_q2_mismatch_immediately() {
    let (fixture, probe, baseline, mut info) = native_role_fixture(true);
    info.renderer_pids.clear();
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let empty_conn =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_secs(5),
        Duration::from_millis(10),
    );
    let started = Instant::now();
    let failure = inspector
        .inspect_connection(&empty_conn, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert_eq!(empty_conn.process_info_calls.load(Ordering::Acquire), 1);
    assert!(started.elapsed() < Duration::from_secs(1));

    let (fixture, probe, baseline, q1) = native_role_fixture(true);
    let mut q2 = q1.clone();
    q2.renderer_pids = vec![75];
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let mismatch_conn =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", q1.clone());
    *mismatch_conn.replies.lock().unwrap() = VecDeque::from([Ok(q1), Ok(q2)]);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_secs(5),
        Duration::from_millis(10),
    );
    let started = Instant::now();
    let failure = inspector
        .inspect_connection(&mismatch_conn, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert_eq!(mismatch_conn.process_info_calls.load(Ordering::Acquire), 2);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn native_inspector_maps_nested_nspid_at_depth_k2() {
    assert_eq!(
        parse_cmdline_tokens(
            b"/home/saya-test/chrome-headless-shell --type=renderer --headless=old\0\0\0"
        ),
        vec![
            "/home/saya-test/chrome-headless-shell",
            "--type=renderer",
            "--headless=old",
        ]
    );

    let (fixture, probe, baseline, info) = native_role_fixture(true);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(200),
        Duration::from_millis(10),
    );
    let evidence = inspector
        .inspect_connection(&connection, &NeverCancel)
        .await
        .expect("k=2 nested renderer topology must succeed");
    assert_eq!(evidence.renderer_pid, 74);
    assert_eq!(connection.process_info_calls.load(Ordering::Acquire), 2);

    // Space-collapsed setproctitle `/proc/<pid>/cmdline` from Chromium's Linux zygote must match `--type=renderer`.
    let (fixture, probe, baseline, info) = native_role_fixture(true);
    {
        let mut state = probe.locked();
        state.processes.get_mut(&63582).unwrap().cmdline =
            vec!["/home/saya-test/chrome-headless-shell --type=renderer --headless=old".into()];
    }
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(200),
        Duration::from_millis(10),
    );
    let evidence = inspector
        .inspect_connection(&connection, &NeverCancel)
        .await
        .expect("setproctitle space-collapsed renderer cmdline must succeed");
    assert_eq!(evidence.renderer_pid, 74);
}

#[tokio::test]
async fn native_inspector_verifies_every_renderer_and_rejects_unreported_or_weak_second_renderer() {
    let add_second_renderer = |probe: &Arc<FakeKernelProbe>, info: &mut WorkerProcessInfo| {
        info.renderer_pids = vec![74, 75];
        let mut state = probe.locked();
        state.pid_ns_parents.insert(31, 20);
        state.pid_ns_owners.insert(31, 1001);
        state.processes.get_mut(&500).unwrap().children.push(63583);
        state.processes.insert(
            63583,
            FakeProcNode {
                ppid: 500,
                nspid: vec![63583, 75, 5, 1],
                no_new_privs: true,
                seccomp_mode: 2,
                seccomp_filters: Some(3),
                cap_eff_zero: true,
                cmdline: vec!["/chromium".into(), "--type=renderer".into()],
                children: vec![],
                starttime: 61,
                alive: true,
                exe: "chromium",
                pid_ns: 31,
                user_ns: 1001,
            },
        );
    };

    // Two compliant renderers pass.
    let (fixture, probe, baseline, mut info) = native_role_fixture(true);
    add_second_renderer(&probe, &mut info);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(100),
        Duration::from_millis(10),
    );
    inspector
        .inspect_connection(&connection, &NeverCancel)
        .await
        .expect("two locked-down renderers should succeed");

    // Any failure on the second renderer fails closed.
    for mutation in [
        "cap_eff",
        "no_new_privs",
        "seccomp",
        "weak_seccomp_filters",
        "no_sandbox_arg",
        "unreported_renderer",
    ] {
        let (fixture, probe, baseline, mut info) = native_role_fixture(true);
        add_second_renderer(&probe, &mut info);
        {
            let mut state = probe.locked();
            match mutation {
                "cap_eff" => state.processes.get_mut(&63583).unwrap().cap_eff_zero = false,
                "no_new_privs" => state.processes.get_mut(&63583).unwrap().no_new_privs = false,
                "seccomp" => state.processes.get_mut(&63583).unwrap().seccomp_mode = 1,
                "weak_seccomp_filters" => {
                    state.processes.get_mut(&63583).unwrap().seccomp_filters = Some(2);
                }
                "no_sandbox_arg" => state
                    .processes
                    .get_mut(&63583)
                    .unwrap()
                    .cmdline
                    .push("--disable-setuid-sandbox=1".into()),
                "unreported_renderer" => info.renderer_pids = vec![74],
                _ => unreachable!(),
            }
        }
        let identity = verify_worker_identity(&fixture.settings).unwrap();
        let connection =
            TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
        let inspector = ProcfsChromiumSandboxInspector::with_probe(
            probe,
            Duration::from_millis(60),
            Duration::from_millis(10),
        );
        let failure = inspector
            .inspect_connection(&connection, &NeverCancel)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "browser_sandbox_unavailable", "{mutation}");
    }
}

#[tokio::test]
async fn native_inspector_fails_closed_immediately_on_eperm_same_userns_or_deep_ancestry() {
    for case in [
        "pid_ns_eperm",
        "user_ns_eperm",
        "owner_eperm",
        "same_user_ns",
        "same_pid_ns",
        "owner_same_user_ns",
        "too_deep_9_hops",
    ] {
        let (fixture, probe, baseline, info) = native_role_fixture(true);
        {
            let mut state = probe.locked();
            match case {
                "pid_ns_eperm" => {
                    state.ns_eperm.insert(30);
                }
                "user_ns_eperm" => {
                    state.ns_eperm.insert(1002);
                }
                "owner_eperm" => {
                    state.owner_eperm.insert(30);
                }
                "same_user_ns" => {
                    state.processes.get_mut(&63582).unwrap().user_ns = 1000;
                }
                "same_pid_ns" => {
                    state.processes.get_mut(&63582).unwrap().pid_ns = 10;
                }
                "owner_same_user_ns" => {
                    state.pid_ns_owners.insert(30, 1000);
                }
                "too_deep_9_hops" => {
                    // Build a 9-hop user_ns chain (> MAX_NAMESPACE_HOPS = 8).
                    state.user_ns_parents.clear();
                    for hop in 0..8_u64 {
                        state.user_ns_parents.insert(2000 + hop, 2001 + hop);
                    }
                    state.user_ns_parents.insert(2008, 1000);
                    state.processes.get_mut(&63582).unwrap().user_ns = 2000;
                    state.pid_ns_owners.insert(30, 2000);
                }
                _ => unreachable!(),
            }
        }
        let identity = verify_worker_identity(&fixture.settings).unwrap();
        let connection =
            TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
        let inspector = ProcfsChromiumSandboxInspector::with_probe(
            probe,
            Duration::from_secs(5),
            Duration::from_millis(10),
        );
        let started = Instant::now();
        let failure = inspector
            .inspect_connection(&connection, &NeverCancel)
            .await
            .unwrap_err();
        assert_eq!(failure.code, "browser_sandbox_unavailable", "{case}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{case} did not fail immediately"
        );
    }
}

#[tokio::test]
async fn native_inspector_rejects_oversized_procfs_and_process_count_above_limit() {
    let dir = tempfile::tempdir().unwrap();
    let exact_path = dir.path().join("exact");
    fs::write(&exact_path, vec![b'a'; MAX_SANDBOX_PROC_FILE_BYTES]).unwrap();
    assert_eq!(
        read_bounded_proc_file_strict(&exact_path).unwrap().len(),
        MAX_SANDBOX_PROC_FILE_BYTES
    );

    let over_path = dir.path().join("over");
    fs::write(&over_path, vec![b'a'; MAX_SANDBOX_PROC_FILE_BYTES + 1]).unwrap();
    assert!(matches!(
        read_bounded_proc_file_strict(&over_path),
        Err(SandboxProbeFailure::Hard(_))
    ));

    assert_eq!(
        parse_proc_stat_starttime(
            b"63582 (Web (Content) Helper) S 500 500 500 0 -1 4194560 1 0 0 0 1 0 0 0 20 0 4 0 987654 0 0",
            63582
        ),
        Ok(987654)
    );

    // Populate 513 total descendants under supervisor (> MAX_SANDBOX_PROCESSES = 512).
    let (fixture, probe, baseline, info) = native_role_fixture(true);
    {
        let mut state = probe.locked();
        let extra_start = 10_000_u32;
        let extra_count = (MAX_SANDBOX_PROCESSES + 1) as u32;
        let browser = state.processes.get_mut(&300).unwrap();
        for offset in 0..extra_count {
            browser.children.push(extra_start + offset);
        }
        for offset in 0..extra_count {
            let pid = extra_start + offset;
            state.processes.insert(
                pid,
                FakeProcNode {
                    ppid: 300,
                    nspid: vec![pid, 1000 + offset],
                    no_new_privs: true,
                    seccomp_mode: 2,
                    seccomp_filters: Some(2),
                    cap_eff_zero: true,
                    cmdline: vec!["/chromium".into(), "--type=utility".into()],
                    children: vec![],
                    starttime: 200 + u64::from(offset),
                    alive: true,
                    exe: "chromium",
                    pid_ns: 10,
                    user_ns: 1000,
                },
            );
        }
    }
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let connection =
        TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_secs(5),
        Duration::from_millis(10),
    );
    let started = Instant::now();
    let failure = inspector
        .inspect_connection(&connection, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn native_inspector_enforces_single_deadline_vs_cancellation_across_q1_and_q2() {
    // q1 hangs -> deadline expires as browser_sandbox_unavailable (not cancelled).
    let (fixture, probe, baseline, info) = native_role_fixture(true);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let conn = TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    *conn.hang_on_call.lock().unwrap() = Some(1);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(50),
        Duration::from_millis(10),
    );
    let failure = inspector
        .inspect_connection(&conn, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");

    // q1 hangs -> caller cancellation overrides before deadline.
    let (fixture, probe, baseline, info) = native_role_fixture(true);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let conn = TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    *conn.hang_on_call.lock().unwrap() = Some(1);
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let notifier = cancel_flag.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(15)).await;
        notifier.store(true, Ordering::Release);
    });
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(200),
        Duration::from_millis(10),
    );
    let failure = inspector
        .inspect_connection(&conn, &SwitchCancel(cancel_flag))
        .await
        .unwrap_err();
    assert_eq!(failure.code, "cancelled");

    // q1 takes 30ms of a 70ms total budget; q2 hangs and times out at the shared 70ms deadline.
    let (fixture, probe, baseline, info) = native_role_fixture(true);
    let identity = verify_worker_identity(&fixture.settings).unwrap();
    let conn = TestNativeConnection::from_identity(identity, baseline, b"InitializeSandbox", info);
    *conn.delays.lock().unwrap() = VecDeque::from([Duration::from_millis(30)]);
    *conn.hang_on_call.lock().unwrap() = Some(2);
    let inspector = ProcfsChromiumSandboxInspector::with_probe(
        probe,
        Duration::from_millis(70),
        Duration::from_millis(10),
    );
    let started = Instant::now();
    let failure = inspector
        .inspect_connection(&conn, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert_eq!(conn.process_info_calls.load(Ordering::Acquire), 2);
    assert!(started.elapsed() < Duration::from_millis(130));
}

#[test]
fn parse_captured_screenshot_validates_masking_ihdr_url_and_limits() {
    let valid = parse_captured_screenshot(captured_screenshot_reply(128, 64, 0)).unwrap();
    assert_eq!(valid.width, 128);
    assert_eq!(valid.height, 64);
    assert!(valid.masked);
    assert_eq!(valid.url.as_deref(), Some("http://allowed.example/"));

    let mut unmasked = captured_screenshot_reply(128, 64, 0);
    unmasked["masked"] = json!(false);
    assert_eq!(
        parse_captured_screenshot(unmasked).unwrap_err().code,
        "browser_capability_disabled"
    );

    let mut bad_mime = captured_screenshot_reply(128, 64, 0);
    bad_mime["mime"] = json!("image/jpeg");
    assert_eq!(
        parse_captured_screenshot(bad_mime).unwrap_err().code,
        "invalid_provider_result"
    );

    let mut bad_base64 = captured_screenshot_reply(128, 64, 0);
    bad_base64["data"] = json!("@@@not-base64@@@");
    assert_eq!(
        parse_captured_screenshot(bad_base64).unwrap_err().code,
        "invalid_provider_result"
    );

    let mut bad_ihdr_dim = captured_screenshot_reply(128, 64, 0);
    bad_ihdr_dim["width"] = json!(256);
    assert_eq!(
        parse_captured_screenshot(bad_ihdr_dim).unwrap_err().code,
        "invalid_provider_result"
    );

    let mut bad_url = captured_screenshot_reply(128, 64, 0);
    bad_url["url"] = json!("");
    assert_eq!(
        parse_captured_screenshot(bad_url).unwrap_err().code,
        "invalid_provider_result"
    );

    let mut oversized_decoded = captured_screenshot_reply(128, 64, 0);
    oversized_decoded["data"] = json!(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        test_png_bytes(128, 64, (crate::media::MAX_IMAGE_BYTES as usize) + 1)
    ));
    assert_eq!(
        parse_captured_screenshot(oversized_decoded)
            .unwrap_err()
            .code,
        "browser_limit"
    );
}

#[tokio::test]
async fn real_worker_capture_screenshot_supports_large_wire_payload_and_maps_errors() {
    let large_payload = captured_screenshot_reply(800, 600, 4_500_000);
    assert!(large_payload["data"].as_str().unwrap().len() > 5 * 1024 * 1024);

    let replies = VecDeque::from([
        Ok(large_payload),
        Err((-32601, "Method not found", None)),
        Err((
            -32000,
            "Unsupported",
            Some(json!({ "reason": "capture_unsupported" })),
        )),
        Err((
            -32000,
            "Timeout",
            Some(json!({ "reason": "soyo_capture_timeout" })),
        )),
        Err((
            -32000,
            "Stale",
            Some(json!({ "reason": "soyo_capture_stale_context" })),
        )),
        Err((
            -32000,
            "Incomplete",
            Some(json!({ "reason": "soyo_capture_incomplete" })),
        )),
        Err((
            -32000,
            "Busy",
            Some(json!({ "reason": "soyo_capture_busy" })),
        )),
        Err((
            -32000,
            "Disconnected",
            Some(json!({ "reason": "soyo_capture_disconnected" })),
        )),
        Err((
            -32000,
            "Oversized",
            Some(json!({ "reason": "browser_limit" })),
        )),
    ]);
    let fixture = custom_method_fixture(
        "soyo/browser/captureScreenshot",
        replies,
        McpLimits {
            connect_timeout: Duration::from_secs(3),
            request_timeout: Duration::from_secs(5),
            max_wire_bytes: MAX_WORKER_SCREENSHOT_WIRE_BYTES,
            ..McpLimits::default()
        },
        false,
    )
    .await;

    let value = fixture
        .connection
        .capture_screenshot(&NeverCancel)
        .await
        .expect("6 MiB base64 PNG must cross updated stdio wire limit");
    let shot = parse_captured_screenshot(value).unwrap();
    assert_eq!(shot.width, 800);
    assert_eq!(shot.height, 600);
    assert!(shot.masked);

    for expected in [
        "browser_capability_disabled",
        "browser_capability_disabled",
        "soyo_capture_timeout",
        "soyo_capture_stale_context",
        "soyo_capture_incomplete",
        "soyo_capture_busy",
        "worker_closed",
        "browser_limit",
    ] {
        let error = fixture
            .connection
            .capture_screenshot(&NeverCancel)
            .await
            .expect_err("expected mapped error code");
        assert_eq!(error.code, expected);
    }
    fixture.connection.shutdown().await.unwrap();
}

#[tokio::test]
async fn screenshot_sandbox_verification_failure_cleans_worker_and_environment() {
    let fixture = fixture_identity();
    let environment_root = tempfile::tempdir().unwrap();
    let factory = FakeEnvironmentFactory::new(environment_root.path().to_path_buf());
    let connection = FakeConnection::new(pinned_catalog()).with_pid(4242);
    connection.push_screenshot(Ok(captured_screenshot_reply(320, 240, 0)));
    let launcher = FakeLauncher::new(vec![Ok(connection.clone())]);
    let inspector = FixedInspector::new(Ok(ChromiumSandboxEvidence {
        renderer_pid: 4243,
        no_new_privs: true,
        seccomp_mode: 2,
        sandbox_arguments_valid: true,
        stderr_initializes_sandbox: true,
    }));
    let provider = provider_with_factory(
        &fixture,
        launcher,
        factory.clone() as Arc<dyn BrowserEnvironmentFactory>,
    )
    .with_sandbox_inspector(inspector.clone());

    let opened = provider.open(None, &NeverCancel).await.unwrap();
    inspector.set_evidence(Err(BrowserFailure::new(
        "browser_sandbox_unavailable",
        "Chromium renderer sandbox evidence is insufficient",
    )));

    let failure = provider
        .screenshot(&opened.session, &opened.page, &NeverCancel)
        .await
        .unwrap_err();
    assert_eq!(failure.code, "browser_sandbox_unavailable");
    assert_eq!(connection.shutdown_count(), 1);
    assert_eq!(factory.releases.load(Ordering::Acquire), 1);
    provider.shutdown(&NeverCancel).await.unwrap();
}
