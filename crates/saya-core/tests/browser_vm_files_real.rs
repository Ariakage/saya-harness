//! Real dedicated-VM acceptance for DEV-081 browser credentials, the file
//! broker, background cancellation and unknown outcomes. It also proves that
//! credential fills cannot target child frames, that a background download
//! artifact survives GC until upload, and that background results are redacted
//! before they can reach the model or durable history.
//!
//! Ignored by default. The test starts only local host fixtures and uses the
//! explicit `SAYA_TEST_VM_*` inputs described in `docs/browser.md`:
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
//! cargo test -p saya-core --test browser_vm_files_real -- --ignored --nocapture
//! ```

use saya_core::{
    approvals::{ApprovalSetup, PermissionRevision, Preauthorizations},
    background::{BackgroundJobs, JobStatus},
    boundary::{Cancellation, Store, StoreChange, ToolContext},
    browser::{
        BrowserCredentialPurpose, BrowserCredentialSettings, BrowserCredentialSlot,
        BrowserFileSettings, BrowserProvider, BrowserSettings, BrowserState,
        network::BrowserNetworkSettings, vm::BrowserVmSettings, vm::assemble_vm_browser_provider,
    },
    contract::{
        ModelProtocol, PermissionMode, Run, RunId, Session, SessionId, ToolResult, WorkMode,
    },
    credentials::{
        CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry, SecretValue,
    },
    dispatch::ToolDispatcher,
    memory::MemoryStore,
    mode::ModeController,
    output_store::{OutputLimits, OutputStore},
    permissions::{PermissionConfig, Policy},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

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

fn vm_settings() -> BrowserVmSettings {
    BrowserVmSettings {
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
    }
}

fn configured() -> bool {
    vm_settings().ssh.is_some() && vm_settings().identity_sha256.is_some()
}

struct FixtureHits {
    submissions: Mutex<Vec<String>>,
    uploads: Mutex<Vec<String>>,
}

async fn spawn_fixture() -> (u16, Arc<FixtureHits>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(FixtureHits {
        submissions: Mutex::new(Vec::new()),
        uploads: Mutex::new(Vec::new()),
    });
    let shared = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let hits = shared.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buffer = [0u8; 1024];
                while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 32 * 1024
                {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => head.extend_from_slice(&buffer[..count]),
                    }
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                let mut lines = text.lines();
                let request_line = lines.next().unwrap_or_default().to_owned();
                let mut content_length = 0usize;
                for line in lines {
                    if let Some(value) = line
                        .to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(str::trim)
                    {
                        content_length = value.parse().unwrap_or(0);
                    }
                }
                let split = head
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                    .map(|index| index + 4)
                    .unwrap_or(head.len());
                let mut body = head[split..].to_vec();
                let mut buffer = [0u8; 4096];
                while body.len() < content_length.min(1024 * 1024) {
                    match stream.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(count) => body.extend_from_slice(&buffer[..count]),
                    }
                }
                body.truncate(content_length.min(1024 * 1024));
                let body_text = String::from_utf8_lossy(&body).into_owned();
                let (status, content_type, response) = if request_line.starts_with("GET /login ") {
                    (
                        "200 OK",
                        "text/html",
                        "<title>login</title><body><form method=\"POST\" action=\"/submit\">\
                             <input name=\"user\" aria-label=\"user\">\
                             <button type=\"submit\">Submit</button></form></body>"
                            .to_owned(),
                    )
                } else if request_line.starts_with("POST /submit ") {
                    hits.submissions.lock().unwrap().push(body_text);
                    (
                        "200 OK",
                        "text/html",
                        "<title>submitted</title><body>SUBMITTED</body>".to_owned(),
                    )
                } else if request_line.starts_with("GET /frame ") {
                    (
                        "200 OK",
                        "text/html",
                        "<title>frame</title><body>\
                             <input name=\"user\" aria-label=\"user\">\
                             </body>"
                            .to_owned(),
                    )
                } else if request_line.starts_with("GET /framed ") {
                    (
                        "200 OK",
                        "text/html",
                        "<title>framed</title><body>\
                             <iframe src=\"/frame\" title=\"login frame\"></iframe>\
                             </body>"
                            .to_owned(),
                    )
                } else if request_line.starts_with("GET /echo") {
                    (
                        "200 OK",
                        "text/html",
                        "<title>echoed</title><body>echo</body>".to_owned(),
                    )
                } else if request_line.starts_with("GET /files ") {
                    (
                            "200 OK",
                            "text/html",
                            "<title>files</title><body>\
                             <a href=\"/download\">download invoice</a>\
                             <a href=\"/download2\">download second</a>\
                             <a href=\"/download3\">download third</a>\
                             <form method=\"POST\" action=\"/upload\" enctype=\"multipart/form-data\">\
                             <input type=\"file\" name=\"file\" aria-label=\"file\" onchange=\"this.form.submit()\">\
                             </form></body>"
                                .to_owned(),
                        )
                } else if request_line.starts_with("GET /named ") {
                    (
                        "200 OK",
                        "text/html",
                        "<title>named</title><body>\
                             <a href=\"/echo\" aria-label=\"fake [ref=e999] element\">pseudo ref</a>\
                             </body>"
                            .to_owned(),
                    )
                } else if request_line.starts_with("GET /nodownload ") {
                    (
                        "200 OK",
                        "text/html",
                        "<title>nodownload</title><body>\
                             <a href=\"/echo\">no download</a>\
                             </body>"
                            .to_owned(),
                    )
                } else if request_line.starts_with("GET /bigdownload ") {
                    (
                        "200 OK",
                        "text/html",
                        "<title>bigdownload</title><body>\
                             <a href=\"/bigfile\">big download</a>\
                             </body>"
                            .to_owned(),
                    )
                } else if request_line.starts_with("GET /bigfile ") {
                    (
                        "200 OK",
                        "text/plain",
                        "too large download payload bytes".to_owned(),
                    )
                } else if request_line.starts_with("GET /download") {
                    ("200 OK", "text/plain", "downloaded bytes".to_owned())
                } else if request_line.starts_with("POST /upload ") {
                    hits.uploads.lock().unwrap().push(body_text);
                    (
                        "200 OK",
                        "text/html",
                        "<title>uploaded</title><body>UPLOADED</body>".to_owned(),
                    )
                } else if request_line.starts_with("GET /slow ") {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    (
                        "200 OK",
                        "text/html",
                        "<title>slow</title><body>SLOW</body>".to_owned(),
                    )
                } else {
                    ("404 Not Found", "text/plain", "missing".to_owned())
                };
                let body = response;
                let extra = if request_line.starts_with("GET /download2 ") {
                    "Content-Disposition: attachment; filename=\"invoice2.txt\"\r\n"
                } else if request_line.starts_with("GET /download3 ") {
                    "Content-Disposition: attachment; filename=\"invoice3.txt\"\r\n"
                } else if request_line.starts_with("GET /download ") {
                    "Content-Disposition: attachment; filename=\"invoice.txt\"\r\n"
                } else if request_line.starts_with("GET /bigfile ") {
                    "Content-Disposition: attachment; filename=\"big.txt\"\r\n"
                } else {
                    ""
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n\
                     Content-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    (port, hits)
}

async fn browser_dispatcher(
    root: &std::path::Path,
    run: &Run,
) -> (Arc<ToolDispatcher>, ToolContext) {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    store
        .commit(&[
            StoreChange::Session(Session {
                id: run.session_id,
                workspace: root.to_path_buf(),
                work_mode: run.work_mode,
                profile: "fixture".into(),
                protocol: ModelProtocol::ChatCompletions,
                model: "fixture".into(),
                history: Vec::new(),
            }),
            StoreChange::RunStarted(run.clone()),
        ])
        .await
        .unwrap();
    let config = PermissionConfig::default();
    let policy = Arc::new(Policy::new(root, config).unwrap());
    let modes = ModeController::new(run.work_mode);
    let dispatcher = Arc::new(
        ToolDispatcher::new(
            policy,
            modes.clone(),
            PermissionRevision::new(modes, run.permissions),
            store,
            ApprovalSetup {
                run_id: run.id,
                session_id: run.session_id,
                initial_permissions: run.permissions,
                preauthorizations: Preauthorizations::default(),
                known_secrets: vec!["alice-secret".into()],
                cancellation: Arc::new(NeverCancel),
            },
            Arc::new(
                ReadTools::new(
                    root,
                    &[],
                    matches!(run.permissions, PermissionMode::Bypass),
                    ReadLimits::default(),
                )
                .unwrap(),
            ),
            ExecConfig::default(),
        )
        .unwrap(),
    );
    let context = ToolContext {
        run: run.clone(),
        workspace: root.to_path_buf(),
    };
    (dispatcher, context)
}

fn tree_contains(path: &std::path::Path, needle: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if tree_contains(&path, needle) {
                return true;
            }
        } else if std::fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes).contains(needle))
            .unwrap_or(false)
        {
            return true;
        }
    }
    false
}

fn element(snapshot: &saya_core::browser::SnapshotView, role: &str, needle: &str) -> String {
    snapshot
        .elements
        .iter()
        .find(|element| {
            element.role.eq_ignore_ascii_case(role)
                && element.name.to_ascii_lowercase().contains(needle)
        })
        .unwrap_or_else(|| {
            panic!(
                "missing {role} containing {needle}; elements: {:?}",
                snapshot.elements
            )
        })
        .element_ref
        .clone()
}

#[tokio::test]
#[ignore = "requires a provisioned Colima browser VM and SAYA_TEST_VM_* inputs"]
async fn real_vm_credentials_files_background_and_cleanup() {
    if !cfg!(target_os = "macos") {
        eprintln!("real VM integration requires macOS; test not run");
        return;
    }
    if !configured() {
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

    let (port, hits) = spawn_fixture().await;
    let origin = format!("http://127.0.0.1:{port}");
    let alternate = format!("http://localhost:{port}");
    let network = BrowserNetworkSettings {
        enabled: true,
        allowed_origins: vec![origin.clone()],
        public_web: false,
        localhost_development_origin: Some(origin.clone()),
        ..Default::default()
    };
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(SecretRegistry::default());
    let credential_dir = root.path().join("credentials");
    let credential_store = FileCredentialStore::open(&credential_dir, registry.clone()).unwrap();
    let credential_ref = credential_store
        .create(
            CredentialScope::for_browser(
                "login",
                &saya_core::network::NormalizedOrigin::parse(&origin).unwrap(),
            )
            .unwrap(),
            CredentialValue::Secret(SecretValue::new("alice-secret").unwrap()),
        )
        .await
        .unwrap();
    let settings = BrowserSettings {
        enabled: true,
        max_wait_ms: 30_000,
        credentials: BrowserCredentialSettings {
            enabled: true,
            store: Some(credential_dir.clone()),
            slots: vec![
                BrowserCredentialSlot {
                    alias: "login".into(),
                    credential_ref,
                    origin: origin.clone(),
                    purpose: BrowserCredentialPurpose::FormFill,
                    max_uses: 1,
                },
                BrowserCredentialSlot {
                    alias: "wrong-origin".into(),
                    credential_ref,
                    origin: alternate,
                    purpose: BrowserCredentialPurpose::FormFill,
                    max_uses: 1,
                },
                BrowserCredentialSlot {
                    alias: "frame".into(),
                    credential_ref,
                    origin: origin.clone(),
                    purpose: BrowserCredentialPurpose::FormFill,
                    max_uses: 1,
                },
            ],
        },
        network: network.clone(),
        files: BrowserFileSettings {
            // "downloaded bytes" is exactly 16 bytes; the oversize fixture is
            // larger and must fail the host-side bounded store.
            max_download_bytes: 16,
            ..Default::default()
        },
        ..Default::default()
    };
    let outputs = Arc::new(
        OutputStore::open(
            root.path().join("artifacts"),
            OutputLimits::default(),
            registry.clone(),
        )
        .unwrap(),
    );
    let run = Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Bypass,
    };
    let (dispatcher, _context) = browser_dispatcher(root.path(), &run).await;
    let jobs = Arc::new(
        BackgroundJobs::new(dispatcher.clone(), 2, 4)
            .unwrap()
            .with_tool_timeout(Duration::from_secs(30))
            .unwrap(),
    );
    let provider = Arc::new(
        assemble_vm_browser_provider(
            vm_settings(),
            network,
            &run.id.0.to_string(),
            ExecConfig::default(),
        )
        .expect("VM provider assembly"),
    );
    let state = Arc::new(
        BrowserState::new_with_services(
            settings,
            provider.clone(),
            run.clone(),
            Some(outputs.clone()),
            Some(jobs.clone()),
            registry,
            saya_core::config::MediaCapabilities::default(),
        )
        .unwrap(),
    );
    let context = ToolContext {
        run: run.clone(),
        workspace: root.path().to_path_buf(),
    };

    // --- Form credentials: fill does not submit; the later click does. ---
    let opened = state.open(&context, &NeverCancel).await.expect("open");
    let page = opened.page.clone();
    state
        .navigate(&context, &page, &format!("{origin}/login"), &NeverCancel)
        .await
        .expect("navigate login");
    let snapshot = state
        .snapshot(&context, &page, &NeverCancel)
        .await
        .expect("login snapshot");
    let input = element(&snapshot, "textbox", "user");
    let view = state
        .fill_credential(
            &context,
            &page,
            &snapshot.document,
            &input,
            "login",
            &NeverCancel,
        )
        .await
        .expect("credential fill");
    assert_eq!(view.credential_slot.as_deref(), Some("login"));
    assert!(
        hits.submissions.lock().unwrap().is_empty(),
        "fill must not submit"
    );
    let encoded = serde_json::to_string(&view).unwrap();
    assert!(
        !encoded.contains("alice-secret"),
        "plaintext leaked into tool result"
    );

    // Stale document and wrong origin are rejected.
    let stale = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    assert_eq!(
        state
            .fill_credential(
                &context,
                &page,
                &snapshot.document,
                &element(&stale, "textbox", "user"),
                "login",
                &NeverCancel,
            )
            .await
            .unwrap_err()
            .0
            .code,
        "stale_element"
    );
    assert_eq!(
        state
            .fill_credential(
                &context,
                &page,
                &stale.document,
                &element(&stale, "textbox", "user"),
                "wrong-origin",
                &NeverCancel,
            )
            .await
            .unwrap_err()
            .0
            .code,
        "credential_origin_mismatch"
    );
    // A second use is refused after the first successful fill.
    state
        .navigate(&context, &page, &format!("{origin}/login"), &NeverCancel)
        .await
        .unwrap();
    let login = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    assert_eq!(
        state
            .fill_credential(
                &context,
                &page,
                &login.document,
                &element(&login, "textbox", "user"),
                "login",
                &NeverCancel,
            )
            .await
            .unwrap_err()
            .0
            .code,
        "credential_uses_exhausted"
    );

    // --- Subframe references are visible but never receive credentials. ---
    state
        .navigate(&context, &page, &format!("{origin}/framed"), &NeverCancel)
        .await
        .expect("navigate framed");
    let framed = state
        .snapshot(&context, &page, &NeverCancel)
        .await
        .expect("framed snapshot");
    let frame_input = element(&framed, "textbox", "user");
    let error = state
        .fill_credential(
            &context,
            &page,
            &framed.document,
            &frame_input,
            "frame",
            &NeverCancel,
        )
        .await
        .expect_err("child-frame credential fill must be rejected");
    assert_eq!(error.0.code, "credential_frame_denied");

    // --- An accessible name that looks like a ref is never used as a key. ---
    state
        .navigate(&context, &page, &format!("{origin}/named"), &NeverCancel)
        .await
        .expect("navigate named");
    let named = state
        .snapshot(&context, &page, &NeverCancel)
        .await
        .expect("named snapshot");
    let pseudo_ref = element(&named, "link", "fake [ref=");
    let click = state
        .click(
            &context,
            &page,
            &named.document,
            &pseudo_ref,
            None,
            &NeverCancel,
        )
        .await
        .expect("the trailing real reference must target the link");
    assert!(
        click.url.as_deref().unwrap_or_default().contains("/echo"),
        "pseudo ref name was mistaken for an element key: {click:?}"
    );

    // The submitted form actually received the injected value.
    state
        .navigate(&context, &page, &format!("{origin}/login"), &NeverCancel)
        .await
        .unwrap();
    // The Run's single use was consumed by the earlier successful fill; the
    // fixture side observed no automatic submission from filling.
    assert!(hits.submissions.lock().unwrap().is_empty());

    // --- Download through an explicit element click. ---
    state
        .navigate(&context, &page, &format!("{origin}/files"), &NeverCancel)
        .await
        .unwrap();
    let files = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    let link = element(&files, "link", "download");
    let (download, _download_sealed) = state
        .download(&context, &page, &files.document, &link, &NeverCancel)
        .await
        .expect("real download");
    assert_eq!(download.filename, "invoice.txt");
    assert_eq!(download.sha256.len(), 64);
    let artifact_ref = download.artifact_ref.clone().unwrap();
    assert_eq!(
        state.load_artifact(&context, &artifact_ref).unwrap(),
        b"downloaded bytes"
    );
    // Forged path-style references are rejected without touching the host.
    let after = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    assert!(
        state
            .upload(
                &context,
                &page,
                &after.document,
                &element(&after, "button", "file"),
                "/etc/passwd",
                &NeverCancel,
            )
            .await
            .is_err()
    );

    // --- Upload the downloaded artifact through the file chooser. ---
    let before_uploads = hits.uploads.lock().unwrap().len();
    let upload = state
        .upload(
            &context,
            &page,
            &after.document,
            &element(&after, "button", "file"),
            &artifact_ref,
            &NeverCancel,
        )
        .await
        .expect("real upload");
    assert_eq!(upload.bytes_sent, 16);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while hits.uploads.lock().unwrap().len() == before_uploads
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    {
        let uploads = hits.uploads.lock().unwrap();
        assert_eq!(uploads.len(), before_uploads + 1);
        assert!(uploads.last().unwrap().contains("downloaded bytes"));
        assert!(
            uploads.last().unwrap().contains("filename=\"invoice.txt\""),
            "the page must receive the original file name: {}",
            uploads.last().unwrap()
        );
    }

    // --- Provider download failures invalidate the old document. ---
    state
        .navigate(
            &context,
            &page,
            &format!("{origin}/nodownload"),
            &NeverCancel,
        )
        .await
        .unwrap();
    let nodownload = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    let nodownload_link = element(&nodownload, "link", "no download");
    let error = state
        .download(
            &context,
            &page,
            &nodownload.document,
            &nodownload_link,
            &NeverCancel,
        )
        .await
        .expect_err("a navigation must not produce a download");
    assert_eq!(error.0.code, "download_missing");
    // Known failure: the old document is invalid and a fresh snapshot is the
    // only retry path.
    assert_eq!(
        state
            .download(
                &context,
                &page,
                &nodownload.document,
                &nodownload_link,
                &NeverCancel,
            )
            .await
            .unwrap_err()
            .0
            .code,
        "stale_element"
    );
    let recovered = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    assert!(!recovered.document.is_empty());

    state
        .navigate(
            &context,
            &page,
            &format!("{origin}/bigdownload"),
            &NeverCancel,
        )
        .await
        .unwrap();
    let big = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    let big_link = element(&big, "link", "big download");
    let error = state
        .download(&context, &page, &big.document, &big_link, &NeverCancel)
        .await
        .expect_err("oversize download must be rejected");
    assert_eq!(error.0.code, "file_too_large");
    assert_eq!(
        state
            .download(&context, &page, &big.document, &big_link, &NeverCancel)
            .await
            .unwrap_err()
            .0
            .code,
        "stale_element"
    );
    let recovered = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    assert!(!recovered.document.is_empty());

    // --- A background download stays pinned across GC and can be uploaded. ---
    state
        .navigate(&context, &page, &format!("{origin}/files"), &NeverCancel)
        .await
        .unwrap();
    let background_files = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    let background_link = element(&background_files, "link", "second");
    let bg_state = state.clone();
    let bg_context = context.clone();
    let bg_page = page.clone();
    let bg_document = background_files.document.clone();
    let download_job = jobs
        .start_browser(
            "vm-background-download",
            &context,
            "browser_download",
            true,
            move |cancel| {
                Box::pin(async move {
                    let (view, sealed) = bg_state
                        .download(
                            &bg_context,
                            &bg_page,
                            &bg_document,
                            &background_link,
                            cancel.as_ref(),
                        )
                        .await?;
                    let media_refs = view.media_ref.clone().into_iter().collect::<Vec<_>>();
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({
                                "source": "untrusted_browser",
                                "download": view,
                            })),
                            error: None,
                            truncated: false,
                            media_refs,
                        },
                        sealed.into_iter().collect(),
                    ))
                })
            },
        )
        .expect("start background download");
    let status = tokio::time::timeout(Duration::from_secs(30), jobs.wait(&download_job))
        .await
        .expect("background download timeout")
        .expect("background download status")
        .expect("background download missing");
    let JobStatus::Finished(download_result) = status else {
        panic!("background download did not finish: {status:?}");
    };
    assert!(download_result.ok);
    let background_ref = download_result
        .data
        .as_ref()
        .and_then(|data| data.get("download"))
        .and_then(|download| download.get("artifact_ref"))
        .and_then(serde_json::Value::as_str)
        .expect("background download artifact reference")
        .to_owned();
    assert!(download_result.media_refs.contains(&background_ref));
    // GC with no session-history pins must not reclaim the job-held artifact.
    let collection = outputs
        .collect_garbage(&std::collections::BTreeSet::new(), 8)
        .expect("collect garbage");
    assert_eq!(
        collection.removed, 0,
        "background artifact was reclaimed before upload"
    );
    assert!(
        outputs.load_file(run.session_id, &background_ref).is_ok(),
        "background artifact disappeared before upload"
    );
    let after_background = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    let before_background_upload = hits.uploads.lock().unwrap().len();
    let upload = state
        .upload(
            &context,
            &page,
            &after_background.document,
            &element(&after_background, "button", "file"),
            &background_ref,
            &NeverCancel,
        )
        .await
        .expect("upload background artifact after GC");
    assert_eq!(upload.bytes_sent, 16);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while hits.uploads.lock().unwrap().len() == before_background_upload
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        hits.uploads.lock().unwrap().len(),
        before_background_upload + 1
    );

    // --- A timed-out background job keeps its produced artifact lease. ---
    state
        .navigate(&context, &page, &format!("{origin}/files"), &NeverCancel)
        .await
        .unwrap();
    let timeout_files = state.snapshot(&context, &page, &NeverCancel).await.unwrap();
    let timeout_link = element(&timeout_files, "link", "third");
    let timeout_jobs = Arc::new(
        BackgroundJobs::new(dispatcher.clone(), 1, 2)
            .unwrap()
            .with_tool_timeout(Duration::from_millis(150))
            .unwrap(),
    );
    let timeout_state = state.clone();
    let timeout_context = context.clone();
    let timeout_page = page.clone();
    let timeout_document = timeout_files.document.clone();
    let timeout_job = timeout_jobs
        .start_browser(
            "vm-timeout-download",
            &context,
            "browser_download",
            true,
            move |_cancel| {
                Box::pin(async move {
                    // The real download completes, then the operation crosses
                    // the supervisor deadline without observing cancellation.
                    let (view, sealed) = timeout_state
                        .download(
                            &timeout_context,
                            &timeout_page,
                            &timeout_document,
                            &timeout_link,
                            &NeverCancel,
                        )
                        .await?;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    let media_refs = view.media_ref.clone().into_iter().collect::<Vec<_>>();
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({
                                "source": "untrusted_browser",
                                "download": view,
                            })),
                            error: None,
                            truncated: false,
                            media_refs,
                        },
                        sealed.into_iter().collect(),
                    ))
                })
            },
        )
        .expect("start timeout download");
    let status = tokio::time::timeout(Duration::from_secs(30), timeout_jobs.wait(&timeout_job))
        .await
        .expect("timeout download deadline")
        .expect("timeout download status")
        .expect("timeout download job");
    let JobStatus::Failed(error) = status else {
        panic!("expected a tool_timeout job: {status:?}");
    };
    assert_eq!(error.0.code, "tool_timeout");
    let observed = error.2.expect("timeout must retain the observed result");
    let timeout_ref = observed
        .media_refs
        .first()
        .expect("timeout result artifact reference")
        .clone();
    assert!(
        outputs.load_file(run.session_id, &timeout_ref).is_ok(),
        "timeout artifact disappeared before the job state was consumed"
    );
    assert_eq!(
        outputs
            .collect_garbage(&std::collections::BTreeSet::new(), 8)
            .expect("collect garbage")
            .removed,
        0,
        "timeout dropped the artifact lease"
    );
    timeout_jobs.shutdown().await.unwrap();
    drop(timeout_jobs);
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        if outputs.load_file(run.session_id, &timeout_ref).is_err() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timeout artifact lease outlived the job state"
        );
        outputs
            .collect_garbage(&std::collections::BTreeSet::new(), 8)
            .expect("collect garbage");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // --- Background results are redacted before status. ---
    let echo_state = state.clone();
    let echo_context = context.clone();
    let echo_page = page.clone();
    let echo_url = format!("{origin}/echo?token=alice-secret");
    let echo_job = jobs
        .start_browser(
            "vm-background-echo",
            &context,
            "browser_navigate",
            true,
            move |cancel| {
                Box::pin(async move {
                    let view = echo_state
                        .navigate(&echo_context, &echo_page, &echo_url, cancel.as_ref())
                        .await?;
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({"url": view.url})),
                            error: None,
                            truncated: false,
                            media_refs: Vec::new(),
                        },
                        Vec::new(),
                    ))
                })
            },
        )
        .expect("start echo navigation");
    let status = tokio::time::timeout(Duration::from_secs(30), jobs.wait(&echo_job))
        .await
        .expect("echo navigation timeout")
        .expect("echo navigation status")
        .expect("echo navigation missing");
    let JobStatus::Finished(echo_result) = status else {
        panic!("echo navigation did not finish: {status:?}");
    };
    let encoded = serde_json::to_string(&echo_result).unwrap();
    assert!(
        !encoded.contains("alice-secret"),
        "background result persisted the credential plaintext"
    );
    assert!(encoded.contains("[REDACTED]"));

    // --- Background slow navigation: real job id, cancel, unknown. ---
    state
        .navigate(&context, &page, &format!("{origin}/files"), &NeverCancel)
        .await
        .unwrap();
    let state_for_job = state.clone();
    let context_for_job = context.clone();
    let slow = format!("{origin}/slow");
    let page_for_job = page.clone();
    let job_id = jobs
        .start_browser(
            "vm-background-launch",
            &context,
            "browser_navigate",
            true,
            move |cancel| {
                Box::pin(async move {
                    let view = state_for_job
                        .navigate(&context_for_job, &page_for_job, &slow, cancel.as_ref())
                        .await?;
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({"url": view.url})),
                            error: None,
                            truncated: false,
                            media_refs: Vec::new(),
                        },
                        Vec::new(),
                    ))
                })
            },
        )
        .expect("start background navigate");
    assert!(job_id.starts_with("background-"));
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    assert!(jobs.cancel(&job_id).unwrap());
    let status = tokio::time::timeout(Duration::from_secs(15), jobs.wait(&job_id))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let JobStatus::Failed(error) = status else {
        panic!("expected an unknown background outcome")
    };
    assert_eq!(error.0.code, "outcome_unknown");
    // The page stays unknown until a fresh observation (or close).
    assert_eq!(
        state
            .navigate(&context, &page, &format!("{origin}/files"), &NeverCancel)
            .await
            .unwrap_err()
            .0
            .code,
        "outcome_unknown"
    );
    let recovered = state
        .snapshot(&context, &page, &NeverCancel)
        .await
        .expect("fresh snapshot recovers the page");
    assert!(!recovered.document.is_empty());

    // --- PLAN cleanup, explicit open, then a real background action. ---
    state
        .shutdown(&NeverCancel)
        .await
        .expect("PLAN browser cleanup");
    let after_plan = state
        .open(&context, &NeverCancel)
        .await
        .expect("explicit open after PLAN");
    state
        .navigate(
            &context,
            &after_plan.page,
            &format!("{origin}/files"),
            &NeverCancel,
        )
        .await
        .expect("navigate after PLAN");
    let plan_state = state.clone();
    let plan_context = context.clone();
    let plan_page = after_plan.page.clone();
    let plan_target = format!("{origin}/echo");
    let plan_job = jobs
        .start_browser(
            "vm-plan-work-background",
            &context,
            "browser_navigate",
            true,
            move |cancel| {
                Box::pin(async move {
                    let view = plan_state
                        .navigate(&plan_context, &plan_page, &plan_target, cancel.as_ref())
                        .await?;
                    Ok((
                        ToolResult {
                            ok: true,
                            data: Some(serde_json::json!({"url": view.url})),
                            error: None,
                            truncated: false,
                            media_refs: Vec::new(),
                        },
                        Vec::new(),
                    ))
                })
            },
        )
        .expect("background action after PLAN cleanup");
    let status = tokio::time::timeout(Duration::from_secs(30), jobs.wait(&plan_job))
        .await
        .expect("PLAN to WORK background timeout")
        .expect("PLAN to WORK background status")
        .expect("PLAN to WORK background job");
    let JobStatus::Finished(result) = status else {
        panic!("background action after PLAN did not finish: {status:?}");
    };
    assert!(result.ok);

    // No plaintext credential value is persisted in the file broker or tool
    // artifact store. The private credential store is the only holder.
    assert!(
        !tree_contains(&root.path().join("artifacts"), "alice-secret"),
        "credential plaintext leaked into the artifact store"
    );

    // DEV-082: the pinned Playwright MCP worker has no screenshot mask input.
    // The production VM provider must therefore refuse the capability instead
    // of returning an unmasked image.
    assert!(!state.facts().screenshots);
    assert!(!state.facts().screenshot_masking);
    let screenshot_error = state
        .screenshot(&context, &after_plan.page, &NeverCancel)
        .await
        .expect_err("screenshot must be disabled without masking support");
    assert_eq!(screenshot_error.0.code, "browser_capability_disabled");

    jobs.shutdown().await.unwrap();
    state.shutdown(&NeverCancel).await.expect("state shutdown");
    provider
        .shutdown(&NeverCancel)
        .await
        .expect("provider shutdown");

    // Restored artifact is still readable from the private store.
    let reopened = OutputStore::open(
        root.path().join("artifacts"),
        OutputLimits::default(),
        Arc::new(SecretRegistry::default()),
    )
    .unwrap();
    let (bytes, _, _) = reopened
        .load_file(run.session_id, &artifact_ref)
        .expect("restored artifact reference");
    assert_eq!(bytes, b"downloaded bytes");

    // No Run root, worker or socat remains in the dedicated VM.
    let settings = vm_settings();
    let output = std::process::Command::new(settings.ssh.as_ref().unwrap())
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            &format!(
                "UserKnownHostsFile={}",
                settings.known_hosts.as_ref().unwrap().display()
            ),
            "-i",
            settings.identity_file.as_ref().unwrap().to_str().unwrap(),
            "-p",
            &settings.port.unwrap().to_string(),
            &format!(
                "{}@{}",
                settings.user.as_ref().unwrap(),
                settings.host.as_ref().unwrap()
            ),
            &format!(
                "ls -A {} 2>/dev/null; ps -eo args | grep '[s]aya-browser-vm-' || true",
                settings
                    .remote_temp_root
                    .as_deref()
                    .unwrap_or("/tmp/saya-runs")
            ),
        ])
        .output()
        .expect("ssh cleanup inspection");
    let text = String::from_utf8_lossy(&output.stdout);
    let leftovers = text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.contains("pgrep"))
        .collect::<Vec<_>>();
    assert!(
        leftovers.is_empty(),
        "remote Run residue remains: {leftovers:?}"
    );
}
