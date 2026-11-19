//! `__soyo-js-worker` and `js_repl` end-to-end coverage.
//!
//! The stdio tests exercise the private worker entry point inside the real
//! `soyo` binary without configuration or app state, mirroring the
//! `__soyo-file-worker` precedent. The runtime tests drive a full
//! `AgentRuntime` run with the native prepared environment on this host.

use serde_json::{Value, json};
use soyo_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
    },
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, PermissionMode, Run, RunId,
        RunStatus, Session, SessionId, ToolCall, ToolResult, WorkMode,
    },
    credentials::SecretRegistry,
    hook_store::{HookStore, HookStoreLimits},
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    plugin_manifest::{MANIFEST_NAME, PackageSource, SourceKind, inspect_package},
    plugin_runtime::{ApprovedRuntime, PluginHostBindings, ViewLimits, ViewPool},
    plugin_store::{EnableGrant, PluginScope, PluginStore, StoreLimits},
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{exec::ExecConfig, read::ReadLimits, read::ReadTools},
};
use std::{
    collections::{BTreeSet, VecDeque},
    fs,
    future::{Future, pending},
    io::{BufRead, BufReader, Write},
    path::Path,
    pin::Pin,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

fn init(script: &str) -> Value {
    json!({
        "schema_version": 1,
        "script": script,
        "memory_bytes": 64 * 1024 * 1024,
        "stack_bytes": 1024 * 1024,
        "max_calls": 8,
    })
}

fn spawn_worker(args: &[&str], workspace: &Path) -> (Child, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let poisoned = home.path().join("invalid-config.toml");
    fs::write(&poisoned, "SYNTHETIC-CONFIG-MUST-NOT-BE-READ").unwrap();
    let state = home.path().join("state");
    let child = Command::new(env!("CARGO_BIN_EXE_soyo"))
        .args(args)
        .current_dir(workspace)
        .env_clear()
        .env("HOME", home.path())
        .env("SOYO_CONFIG", poisoned)
        .env("XDG_STATE_HOME", &state)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    (child, home)
}

fn worker_output(child: Child, home: &tempfile::TempDir) -> std::process::Output {
    let output = child.wait_with_output().unwrap();
    assert!(
        !home.path().join("state").exists(),
        "js worker opened app state"
    );
    output
}

fn send_line(child: &mut Child, value: &Value) {
    let mut bytes = serde_json::to_vec(value).unwrap();
    bytes.push(b'\n');
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&bytes)
        .expect("write worker frame");
    child.stdin.as_mut().unwrap().flush().unwrap();
}

fn read_line(child: &mut Child) -> Value {
    let stdout = child.stdout.as_mut().unwrap();
    let mut line = String::new();
    assert_ne!(
        BufReader::new(stdout).read_line(&mut line).unwrap(),
        0,
        "worker closed stdout without a message"
    );
    serde_json::from_str(line.trim()).expect("worker emitted invalid protocol JSON")
}

#[test]
fn js_worker_evaluates_scripts_over_bounded_stdio() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut child, home) = spawn_worker(&["__soyo-js-worker"], workspace.path());
    send_line(&mut child, &init("return 21 * 2;"));
    let message = read_line(&mut child);
    assert_eq!(message, json!({"kind": "done", "value": 42}));
    let output = worker_output(child, &home);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[test]
fn js_worker_bridge_emits_calls_and_consumes_replies() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut child, home) = spawn_worker(&["__soyo-js-worker"], workspace.path());
    send_line(
        &mut child,
        &init(
            "const a = await soyo.callTool('read_file', {path: 'a.txt'}); \
             const b = await soyo.callTool('exec', {argv: ['id']}); \
             return {a: a.data, denied: b.error.code};",
        ),
    );
    let first = read_line(&mut child);
    assert_eq!(first["kind"], "call");
    assert_eq!(first["seq"], 0);
    assert_eq!(first["name"], "read_file");
    assert_eq!(first["arguments"], "{\"path\":\"a.txt\"}");
    send_line(
        &mut child,
        &json!({
            "kind": "result", "seq": 0,
            "result": {"ok": true, "data": {"content": "hello"}, "error": null, "truncated": false}
        }),
    );
    let second = read_line(&mut child);
    assert_eq!(second["kind"], "call");
    assert_eq!(second["seq"], 1);
    assert_eq!(second["name"], "exec");
    send_line(
        &mut child,
        &json!({
            "kind": "result", "seq": 1,
            "result": {"ok": false, "data": null,
                "error": {"code": "permission_denied", "message": "operation denied"},
                "truncated": false}
        }),
    );
    let last = read_line(&mut child);
    assert_eq!(
        last,
        json!({"kind": "done", "value": {"a": {"content": "hello"}, "denied": "permission_denied"}})
    );
    let output = worker_output(child, &home);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[test]
fn js_worker_rejects_bad_init_extra_args_and_reports_script_failures() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut child, home) = spawn_worker(&["__soyo-js-worker"], workspace.path());
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"not a protocol frame\n")
        .unwrap();
    let output = worker_output(child, &home);
    assert_eq!(output.status.code(), Some(64));
    let (mut child, home) = spawn_worker(&["__soyo-js-worker"], workspace.path());
    send_line(&mut child, &init("throw new Error('boom');"));
    let message = read_line(&mut child);
    assert_eq!(message["kind"], "failed");
    assert!(message["message"].as_str().unwrap().contains("boom"));
    let output = worker_output(child, &home);
    assert!(output.status.success());
    let (child, home) = spawn_worker(&["__soyo-js-worker", "unexpected"], workspace.path());
    let output = worker_output(child, &home);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stdout.is_empty());
}

// ---- Full-runtime coverage: real native sandbox, real worker subprocess ----

struct NoCancel;
impl Cancellation for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct Stream(Option<Vec<HistoryItem>>);
impl ModelStream for Stream {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            Ok(self
                .0
                .take()
                .map(|items| ModelUpdate::Finished { items, usage: None }))
        })
    }
}
struct FakeModel {
    turns: Mutex<VecDeque<Vec<HistoryItem>>>,
}
impl Model for FakeModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        let _ = request;
        Box::pin(async move {
            let items = self
                .turns
                .lock()
                .map_err(|_| test_err("model"))?
                .pop_front()
                .ok_or_else(|| test_err("unexpected_request"))?;
            Ok(Box::new(Stream(Some(items))) as Box<dyn ModelStream>)
        })
    }
}
fn test_err(code: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: code.into(),
        },
        None,
        None,
    )
}
fn fixture(root: &Path, permissions: PermissionMode) -> (Session, Run) {
    let session = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fake".into(),
        protocol: ModelProtocol::Responses,
        model: "fake".into(),
        history: vec![HistoryItem::UserText("task".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions,
    };
    (session, run)
}
fn script_call(id: &str, script: &str) -> HistoryItem {
    HistoryItem::ToolCall(ToolCall {
        call_id: id.into(),
        name: "js_repl".into(),
        arguments: json!({"script": script}).to_string(),
    })
}
fn runtime(root: &Path, run: Run, model: Arc<dyn Model>) -> Result<AgentRuntime, BoundaryError> {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::default());
    let policy = Arc::new(Policy::new(root, PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(root, &[], false, ReadLimits::default()).unwrap());
    AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store,
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
}
fn tool_result<'a>(session: &'a Session, call_id: &str) -> Option<&'a ToolResult> {
    session.history.iter().find_map(|item| {
        if let HistoryItem::ToolResult {
            call_id: id,
            result,
        } = item
            && id == call_id
        {
            return Some(result);
        }
        None
    })
}
fn native_environment(runtime: AgentRuntime, workspace: &Path) -> AgentRuntime {
    soyo_core::execution::pin_launcher_image_from(Path::new(env!("CARGO_BIN_EXE_soyo"))).unwrap();
    runtime
        .with_execution_settings(
            soyo_core::execution::SandboxPolicy::workspace(workspace.into()),
            None,
        )
        .unwrap()
}
fn native_environment_with(
    runtime: AgentRuntime,
    policy: soyo_core::execution::SandboxPolicy,
) -> AgentRuntime {
    soyo_core::execution::pin_launcher_image_from(Path::new(env!("CARGO_BIN_EXE_soyo"))).unwrap();
    runtime.with_execution_settings(policy, None).unwrap()
}
/// Locate a `node` binary on PATH for the optional B2 engine coverage. The
/// test is skipped silently on hosts without Node.
fn node_program() -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("node"))
            .find(|candidate| candidate.is_file())
            .and_then(|candidate| std::fs::canonicalize(candidate).ok())
    })
}
/// Read-only sandbox exposures a real Node install needs beyond the pinned
/// binary. Self-contained builds need nothing extra; keg/distro layouts
/// (Homebrew `node@22`, prefix installs) link shared libraries the sandbox
/// must expose explicitly — the same surface `[js_repl.node] roots` models.
/// Enumerate `otool -L`/`ldd` dependencies and expose every existing
/// non-system file exactly, including canonicalized symlink targets.
fn node_exposure(program: &std::path::Path) -> (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) {
    let mut files = std::collections::BTreeSet::from([program.to_path_buf()]);
    let mut roots = std::collections::BTreeSet::new();
    let bin_dir = program.parent().map(std::path::Path::to_path_buf);
    let mut expose = |candidate: std::path::PathBuf| {
        let Ok(real) = std::fs::canonicalize(&candidate) else {
            return;
        };
        if real.is_dir() {
            roots.insert(real);
            return;
        }
        if real.is_file() {
            files.insert(real);
        }
        // Symlink components on the recorded chain (keg `opt/` → `Cellar/`)
        // are only readable when their parent directory is a read root;
        // read_files entries must stay regular files for policy validation.
        for ancestor in candidate.ancestors() {
            if std::fs::symlink_metadata(ancestor)
                .map(|meta| meta.file_type().is_symlink())
                .unwrap_or(false)
                && let Some(parent) = ancestor.parent()
                && let Ok(parent) = std::fs::canonicalize(parent)
            {
                roots.insert(parent);
            }
        }
    };
    if let Some(bin) = &bin_dir {
        expose(bin.join("../lib").components().collect());
        expose(bin.join("lib"));
    }
    let listing = if cfg!(target_os = "macos") {
        std::process::Command::new("otool")
            .arg("-L")
            .arg(program)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        std::process::Command::new("ldd")
            .arg(program)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    if let Some(text) = listing {
        for token in text.split_whitespace() {
            let dep = if token.starts_with('@') {
                bin_dir.as_ref().map(|bin| {
                    let basename = token.rsplit('/').next().unwrap_or(token);
                    bin.join("../lib").join(basename).components().collect()
                })
            } else if token.starts_with('/') {
                Some(std::path::PathBuf::from(token))
            } else {
                None
            };
            let Some(dep) = dep else { continue };
            let text = dep.to_string_lossy();
            if text.starts_with("/usr/lib") || text.starts_with("/System") {
                continue;
            }
            expose(dep);
        }
    }
    (files.into_iter().collect(), roots.into_iter().collect())
}

#[tokio::test]
async fn js_repl_stays_unknown_when_not_enabled() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", "return 1;")],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let mut runtime = runtime(workspace.path(), run, model).unwrap();
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(!result.ok);
    assert_eq!(result.error.as_ref().unwrap().code, "unknown_tool");
}

#[tokio::test]
async fn js_repl_requires_a_native_environment() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", "return 1;")],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let mut runtime = runtime(workspace.path(), run, model)
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap();
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(!result.ok);
    assert_eq!(
        result.error.as_ref().unwrap().code,
        "sandbox_capability_unavailable"
    );
}

#[tokio::test]
async fn js_repl_runs_scripts_and_bridges_read_only_tools_inside_the_sandbox() {
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("seed.txt"), "SYNTHETIC-SEED-42").unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let script = "const hit = await soyo.callTool('read_file', {path: 'seed.txt', start_line: 1, line_count: 10}); \
                  return {content: hit.data.content, ok: hit.ok};";
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", script)],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let runtime = runtime(workspace.path(), run, model)
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap();
    let mut runtime = native_environment(runtime, workspace.path());
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(result.ok, "repl result: {result:?}");
    let data = result.data.as_ref().unwrap();
    assert_eq!(data["ok"], true);
    assert_eq!(data["content"], "SYNTHETIC-SEED-42");
    assert!(
        !session
            .history
            .iter()
            .any(|item| matches!(item, HistoryItem::ToolResult { call_id, .. } if call_id.contains(":repl:"))),
        "bridged child calls must never enter model history: {:?}",
        session.history
    );
}

#[tokio::test]
async fn js_repl_bridged_denials_reach_the_script_as_failures() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let script = concat!(
        "const w = await soyo.callTool('write_file', {path: 'escaped.txt', content: 'x'});",
        "const e = await soyo.callTool('exec', {argv: ['/bin/id']});",
        "const r = await soyo.callTool('js_repl', {script: 'return 1'});",
        "const u = await soyo.callTool('mcp__missing__tool_x', {});",
        "return [w.error.code, e.error.code, r.error.code, u.error.code];",
    );
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", script)],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let runtime = runtime(workspace.path(), run, model)
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap();
    let mut runtime = native_environment(runtime, workspace.path());
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(result.ok, "repl result: {result:?}");
    let codes = result.data.as_ref().unwrap().as_array().unwrap();
    assert_eq!(codes.len(), 4);
    assert!(codes.iter().all(|code| code == "permission_denied"));
    assert!(!workspace.path().join("escaped.txt").exists());
}

#[tokio::test]
async fn js_repl_malformed_arguments_and_timeout_fail_closed() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let bad = HistoryItem::ToolCall(ToolCall {
        call_id: "repl-bad".into(),
        name: "js_repl".into(),
        arguments: json!({"script": ""}).to_string(),
    });
    let hang = HistoryItem::ToolCall(ToolCall {
        call_id: "repl-hang".into(),
        name: "js_repl".into(),
        arguments: json!({"script": "while (true) {}", "timeout_seconds": 5}).to_string(),
    });
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![bad],
                vec![hang],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let runtime = runtime(workspace.path(), run, model)
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap();
    let mut runtime = native_environment(runtime, workspace.path());
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let bad = tool_result(&session, "repl-bad").expect("bad-args result");
    assert!(!bad.ok);
    assert_eq!(bad.error.as_ref().unwrap().code, "invalid_args");
    let hang = tool_result(&session, "repl-hang").expect("timeout result");
    assert!(!hang.ok);
    assert_eq!(hang.error.as_ref().unwrap().code, "tool_timeout");
}

// ---- DEV-096 joint acceptance: hooks, dynamic deny injection, on/off compare ----

#[derive(Default)]
struct RecordingStore {
    inner: MemoryStore,
    changes: Mutex<Vec<StoreChange>>,
}
impl Store for RecordingStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            self.inner.commit(changes).await?;
            self.changes.lock().unwrap().extend_from_slice(changes);
            Ok(())
        })
    }
}

fn recording_runtime(
    root: &Path,
    run: Run,
    model: Arc<dyn Model>,
    store: Arc<RecordingStore>,
) -> Result<AgentRuntime, BoundaryError> {
    let policy = Arc::new(Policy::new(root, PermissionConfig::default())?);
    let files = Arc::new(ReadTools::new(root, &[], false, ReadLimits::default()).unwrap());
    AgentRuntime::new(RuntimeSetup {
        run,
        model,
        store: store as Arc<dyn Store>,
        policy,
        files,
        cancellation: Arc::new(NoCancel),
        review: ReviewConfig::new("fake".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "task".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
}

fn read_call(id: &str, path: &str) -> HistoryItem {
    HistoryItem::ToolCall(ToolCall {
        call_id: id.into(),
        name: "read_file".into(),
        arguments: json!({"path": path, "start_line": 1, "line_count": 10}).to_string(),
    })
}
fn child_intents(store: &RecordingStore, prefix: &str) -> Vec<String> {
    store
        .changes
        .lock()
        .unwrap()
        .iter()
        .filter_map(|change| match change {
            StoreChange::ToolIntent { call, .. } if call.call_id.starts_with(prefix) => {
                Some(call.call_id.clone())
            }
            _ => None,
        })
        .collect()
}
fn child_finished(store: &RecordingStore, call_id: &str) -> Option<ToolResult> {
    store
        .changes
        .lock()
        .unwrap()
        .iter()
        .find_map(|change| match change {
            StoreChange::ToolFinished {
                call_id: id,
                result,
                ..
            } if id == call_id => Some(result.clone()),
            _ => None,
        })
}
fn history_tool_calls(session: &Session) -> Vec<String> {
    session
        .history
        .iter()
        .filter_map(|item| match item {
            HistoryItem::ToolCall(call) => Some(call.call_id.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn js_repl_dynamically_computed_calls_still_face_the_gate() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let script = concat!(
        "const parts = ['e', 'xec'];",
        "const path = '..' + '/escape.txt';",
        "const e = await soyo.callTool(parts.join(''), {argv: ['/bin/id']});",
        "const r = await soyo.callTool('read_' + 'file', {path, start_line: 1, line_count: 5});",
        "return [e.error.code, r.error.code];",
    );
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", script)],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let store = Arc::new(RecordingStore::default());
    let runtime = recording_runtime(workspace.path(), run, model, store.clone())
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap();
    let mut runtime = native_environment(runtime, workspace.path());
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(result.ok, "repl result: {result:?}");
    let codes = result.data.as_ref().unwrap().as_array().unwrap();
    assert_eq!(
        codes,
        &vec![json!("permission_denied"), json!("path_denied")]
    );
    let intents = child_intents(&store, "repl-1:repl:");
    assert_eq!(intents.len(), 2, "both blocked calls keep durable intent");
    for call_id in &intents {
        let finished = child_finished(&store, call_id).expect("finished evidence");
        assert!(!finished.ok);
    }
}

struct HookFixture {
    _root: tempfile::TempDir,
    store: PluginStore,
    pool: Arc<ViewPool>,
    journal: Arc<HookStore>,
    bindings: PluginHostBindings,
    secrets: Arc<SecretRegistry>,
}
impl HookFixture {
    fn new(workspace: &Path) -> Self {
        let root = tempfile::tempdir().unwrap();
        let secrets = Arc::new(SecretRegistry::default());
        let store = PluginStore::open(
            root.path().join("store"),
            secrets.clone(),
            StoreLimits::default(),
        )
        .unwrap();
        let pool =
            Arc::new(ViewPool::open(&root.path().join("views"), ViewLimits::default()).unwrap());
        let journal = Arc::new(
            HookStore::open(
                &root.path().join("hooks"),
                secrets.clone(),
                HookStoreLimits::default(),
            )
            .unwrap(),
        );
        let source = root.path().join("package");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("check.sh"),
            "#!/bin/sh\nIFS= read -r event || :\ncase \"$event\" in\n  *'\"tool\":\"read_file\"'*) printf '{\"decision\":\"deny\",\"annotations\":[{\"level\":\"warning\",\"message\":\"fixture veto\"}]}' ;;\n  *) printf '{\"decision\":\"pass\"}' ;;\nesac\n",
        )
        .unwrap();
        fs::write(
            source.join(MANIFEST_NAME),
            "schema_version=1\n[plugin]\nid=\"io.soyo.repl-hook-fixture\"\nversion=\"1.0.0\"\ndescription=\"check hook fixture\"\nlicense=\"MIT\"\n[compatibility]\nrequires_soyo=\">=0.2.0, <1.0.0\"\nrequires_features=[\"plugin-manifest-v1\",\"hook-events-v1\"]\n[capabilities]\nrequested=[\"process.exec\",\"hook.check\"]\n[[hooks]]\nid=\"veto\"\nevent=\"tool.before_dispatch\"\nkind=\"check\"\nprogram=\"shell\"\nargs=[\"@plugin/check.sh\"]\ntimeout_ms=5000\n",
        )
        .unwrap();
        let package = inspect_package(&source).unwrap();
        store
            .install(
                &package,
                PackageSource {
                    kind: SourceKind::LocalDirectory,
                    reference: "./hook-fixture".into(),
                },
            )
            .unwrap();
        let features =
            BTreeSet::from(["plugin-manifest-v1".to_owned(), "hook-events-v1".to_owned()]);
        store
            .enable(
                package.digest(),
                PluginScope::workspace(workspace).unwrap(),
                EnableGrant::requested(package.manifest()),
                "0.2.0",
                &features,
            )
            .unwrap();
        let mut bindings = PluginHostBindings::default();
        bindings.runtimes.insert(
            "shell".into(),
            ApprovedRuntime {
                program: std::fs::canonicalize("/bin/sh").unwrap(),
                ..ApprovedRuntime::default()
            },
        );
        Self {
            _root: root,
            store,
            pool,
            journal,
            bindings,
            secrets,
        }
    }
}

#[tokio::test]
async fn js_repl_bridged_calls_pass_check_hooks_individually() {
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("seed.txt"), "HOOK-VISIBLE").unwrap();
    let hooks = HookFixture::new(workspace.path());
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let script = concat!(
        "const denied = await soyo.callTool('read_file', {path: 'seed.txt', start_line: 1, line_count: 5});",
        "const allowed = await soyo.callTool('list_dir', {path: '.'});",
        "return {denied: denied.error.code, allowed: allowed.ok};",
    );
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", script)],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let store = Arc::new(RecordingStore::default());
    let runtime = recording_runtime(workspace.path(), run, model, store.clone())
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap()
        .with_plugins(
            hooks.store.clone(),
            hooks.pool.clone(),
            hooks.bindings.clone(),
            hooks.journal.clone(),
        )
        .unwrap()
        .with_mcp_settings(
            soyo_core::mcp::config::McpSettings::default(),
            soyo_core::config::Environment::from_pairs([]),
            None,
            hooks.secrets.clone(),
        )
        .unwrap();
    let mut runtime = native_environment(runtime, workspace.path());
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(result.ok, "repl result: {result:?}");
    let data = result.data.as_ref().unwrap();
    assert_eq!(data["denied"], "hook_denied", "full data: {data}");
    assert_eq!(data["allowed"], true);
    let denied = child_finished(&store, "repl-1:repl:0").expect("denied child evidence");
    assert_eq!(denied.error.as_ref().unwrap().code, "hook_denied");
    let allowed = child_finished(&store, "repl-1:repl:1").expect("allowed child evidence");
    assert!(allowed.ok);
}

#[tokio::test]
async fn js_repl_multi_file_comparison_against_per_call_dispatch() {
    let workspace = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("alpha.txt", "CONTENT-ALPHA"),
        ("beta.txt", "CONTENT-BETA"),
        ("gamma.txt", "CONTENT-GAMMA"),
    ] {
        fs::write(workspace.path().join(name), body).unwrap();
    }

    // Enabled: one model-visible call; three bridged reads stay off history.
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let script = concat!(
        "const a = await soyo.callTool('read_file', {path: 'alpha.txt', start_line: 1, line_count: 5});",
        "const b = await soyo.callTool('read_file', {path: 'beta.txt', start_line: 1, line_count: 5});",
        "const c = await soyo.callTool('read_file', {path: 'gamma.txt', start_line: 1, line_count: 5});",
        "return [a.data.content, b.data.content, c.data.content];",
    );
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", script)],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let store = Arc::new(RecordingStore::default());
    let runtime = recording_runtime(workspace.path(), run, model, store.clone())
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap();
    let mut runtime = native_environment(runtime, workspace.path());
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let repl_result = tool_result(&session, "repl-1").expect("repl result");
    let repl_contents = repl_result
        .data
        .as_ref()
        .unwrap()
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(history_tool_calls(&session), vec!["repl-1".to_owned()]);
    assert_eq!(child_intents(&store, "repl-1:repl:").len(), 3);

    // Disabled: the same three reads are three model-visible calls.
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![
                    read_call("read-1", "alpha.txt"),
                    read_call("read-2", "beta.txt"),
                    read_call("read-3", "gamma.txt"),
                ],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let store = Arc::new(RecordingStore::default());
    let mut runtime = recording_runtime(workspace.path(), run, model, store.clone())
        .unwrap()
        .with_js_repl(None)
        .unwrap();
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let direct_contents: Vec<Value> = ["read-1", "read-2", "read-3"]
        .iter()
        .map(|id| tool_result(&session, id).unwrap().data.as_ref().unwrap()["content"].clone())
        .collect();
    assert_eq!(repl_contents, direct_contents);
    assert_eq!(
        history_tool_calls(&session),
        vec![
            "read-1".to_owned(),
            "read-2".to_owned(),
            "read-3".to_owned()
        ]
    );
}

#[tokio::test]
async fn js_repl_bridged_calls_carry_durable_evidence_without_history() {
    let workspace = tempfile::tempdir().unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let script = "await soyo.callTool('list_dir', {path: '.'}); return 'child-evidence';";
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", script)],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let runtime = runtime(workspace.path(), run, model)
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig::quickjs(
            soyo_core::js_repl::ReplLimits::default(),
        )))
        .unwrap();
    let mut runtime = native_environment(runtime, workspace.path());
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert_eq!(result.data.as_ref().unwrap(), "child-evidence");
    let child_in_history = session.history.iter().any(|item| match item {
        HistoryItem::ToolCall(call) => call.call_id == "repl-1:repl:0",
        HistoryItem::ToolResult { call_id, .. } => call_id == "repl-1:repl:0",
        _ => false,
    });
    assert!(!child_in_history);
}

// ---- B2 controlled external Node engine (D-083 extension point) ----

#[tokio::test]
async fn js_repl_node_engine_runs_inside_the_sandbox() {
    let Some(program) = node_program() else {
        eprintln!("skipping: no node binary on PATH");
        return;
    };
    let workspace = tempfile::tempdir().unwrap();
    fs::write(workspace.path().join("seed.txt"), "NODE-SEED-7").unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let script = concat!(
        "const hit = await soyo.callTool('read_file', {path: 'seed.txt', start_line: 1, line_count: 10});",
        "const denied = await soyo.callTool('exec', {argv: ['/bin/id']});",
        "const via_soyo = (() => { try { return typeof soyo.constructor.constructor('return process')(); }",
        " catch (error) { return 'blocked:' + error.name; } })();",
        "const via_bridge = (() => { try { return typeof __soyo_bridge.constructor('return process')(); }",
        " catch (error) { return 'blocked:' + error.name; } })();",
        "return {content: hit.data.content, denied: denied.error.code,",
        " has_require: typeof require, has_process: typeof process,",
        " via_soyo: via_soyo, via_bridge: via_bridge};",
    );
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", script)],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let store = Arc::new(RecordingStore::default());
    let (read_files, read_roots) = node_exposure(&program);
    let runtime = recording_runtime(workspace.path(), run, model, store.clone())
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig {
            limits: soyo_core::js_repl::ReplLimits::default(),
            engine: soyo_core::js_repl::ReplEngine::Node {
                program: program.clone(),
            },
            read_files: read_files.clone(),
            read_roots: read_roots.clone(),
        }))
        .unwrap();
    let mut policy = soyo_core::execution::SandboxPolicy::workspace(workspace.path().into());
    policy.read_files.extend(read_files);
    policy.read_roots.extend(read_roots);
    let mut runtime = native_environment_with(runtime, policy);
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(result.ok, "repl result: {result:?}");
    let data = result.data.as_ref().unwrap();
    assert_eq!(data["content"], "NODE-SEED-7");
    assert_eq!(data["denied"], "permission_denied");
    assert_eq!(data["has_require"], "undefined");
    assert_eq!(data["has_process"], "undefined");
    assert_eq!(data["via_soyo"], "blocked:ReferenceError");
    assert_eq!(data["via_bridge"], "blocked:TypeError");
    assert_eq!(child_intents(&store, "repl-1:repl:").len(), 2);
    assert!(!session.history.iter().any(
        |item| matches!(item, HistoryItem::ToolResult { call_id, .. } if call_id.contains(":repl:"))
    ));
}

#[tokio::test]
async fn js_repl_node_engine_reports_script_failures() {
    let Some(program) = node_program() else {
        eprintln!("skipping: no node binary on PATH");
        return;
    };
    let workspace = tempfile::tempdir().unwrap();
    let (mut session, run) = fixture(workspace.path(), PermissionMode::Bypass);
    let model = Arc::new(FakeModel {
        turns: Mutex::new(
            vec![
                vec![script_call("repl-1", "throw new Error('node-boom');")],
                vec![HistoryItem::AssistantText("done".into())],
            ]
            .into(),
        ),
    });
    let store = Arc::new(RecordingStore::default());
    let (read_files, read_roots) = node_exposure(&program);
    let runtime = recording_runtime(workspace.path(), run, model, store)
        .unwrap()
        .with_js_repl(Some(soyo_core::js_repl::JsReplConfig {
            limits: soyo_core::js_repl::ReplLimits::default(),
            engine: soyo_core::js_repl::ReplEngine::Node {
                program: program.clone(),
            },
            read_files: read_files.clone(),
            read_roots: read_roots.clone(),
        }))
        .unwrap();
    let mut policy = soyo_core::execution::SandboxPolicy::workspace(workspace.path().into());
    policy.read_files.extend(read_files);
    policy.read_roots.extend(read_roots);
    let mut runtime = native_environment_with(runtime, policy);
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    let result = tool_result(&session, "repl-1").expect("tool result recorded");
    assert!(!result.ok);
    assert!(
        result.error.as_ref().unwrap().message.contains("node-boom"),
        "{result:?}"
    );
}
