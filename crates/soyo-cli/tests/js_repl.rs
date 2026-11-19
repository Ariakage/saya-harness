//! `__soyo-js-worker` and `js_repl` end-to-end coverage.
//!
//! The stdio tests exercise the private worker entry point inside the real
//! `soyo` binary without configuration or app state, mirroring the
//! `__soyo-file-worker` precedent. The runtime tests drive a full
//! `AgentRuntime` run with the native prepared environment on this host.

use serde_json::{Value, json};
use soyo_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store},
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, ModelProtocol, PermissionMode, Run, RunId,
        RunStatus, Session, SessionId, ToolCall, ToolResult, WorkMode,
    },
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    review::ReviewConfig,
    runtime::{AgentRuntime, RuntimeLimits, RuntimeSetup},
    tools::{exec::ExecConfig, read::ReadLimits, read::ReadTools},
};
use std::{
    collections::VecDeque,
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
        .with_js_repl(Some(soyo_core::js_repl::ReplLimits::default()))
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
        .with_js_repl(Some(soyo_core::js_repl::ReplLimits::default()))
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
        .with_js_repl(Some(soyo_core::js_repl::ReplLimits::default()))
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
        .with_js_repl(Some(soyo_core::js_repl::ReplLimits::default()))
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
        .with_js_repl(Some(soyo_core::js_repl::ReplLimits::default()))
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
