use saya_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
    },
    config::Environment,
    contract::{
        ApprovalRequest, BoundaryError, ErrorInfo, EventPayload, HistoryItem, McpServerState,
        McpServerStatus, ModelProtocol, PermissionMode, Run, RunId, RunStatus, Session, SessionId,
        ToolCall, ToolState, WorkMode,
    },
    credentials::SecretRegistry,
    execution::{BackendKind, SandboxPolicy},
    mcp::config::{
        HttpMcpSettings, McpLimitSettings, McpServerSettings, McpSettings, McpTransportSettings,
        StdioMcpSettings,
    },
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    review::ReviewConfig,
    runtime::{AgentRuntime, ApprovalHandler, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const TOKEN: &str = "runtime-service-fixture-private-token";
#[derive(Default)]
struct StatusStore {
    inner: MemoryStore,
    states: Mutex<Vec<Vec<McpServerStatus>>>,
    fail_connected: AtomicBool,
    intent_delay: Mutex<Option<Duration>>,
    tool_states: Mutex<Vec<ToolState>>,
}
impl Store for StatusStore {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            let delay = if changes
                .iter()
                .any(|change| matches!(change, StoreChange::ToolIntent { .. }))
            {
                self.intent_delay.lock().unwrap().take()
            } else {
                None
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            let statuses = changes
                .iter()
                .filter_map(|change| match change {
                    StoreChange::Activity {
                        payload: EventPayload::McpStatus { servers },
                        ..
                    } => Some(servers.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if statuses
                .iter()
                .flatten()
                .any(|server| server.state == McpServerState::Connected)
                && self.fail_connected.swap(false, Ordering::AcqRel)
            {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "storage_error".into(),
                        message: "controlled lifecycle storage failure".into(),
                    },
                    None,
                    None,
                ));
            }
            self.inner.commit(changes).await?;
            self.tool_states
                .lock()
                .unwrap()
                .extend(changes.iter().filter_map(|change| match change {
                    StoreChange::ToolState { state, .. } => Some(*state),
                    _ => None,
                }));
            self.states.lock().unwrap().extend(statuses);
            Ok(())
        })
    }
}
struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct FixtureModel {
    requests: Mutex<Vec<ModelRequest>>,
    use_tool: bool,
}
struct Finished(Option<ModelUpdate>);
impl ModelStream for Finished {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move { Ok(self.0.take()) })
    }
}
impl Model for FixtureModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            let mut requests = self.requests.lock().unwrap();
            let first = requests.is_empty();
            requests.push(request.clone());
            let items = if first && self.use_tool {
                let tool = request
                    .tools
                    .iter()
                    .find(|tool| tool.name.starts_with("mcp__"))
                    .expect("discovered MCP tool");
                vec![HistoryItem::ToolCall(ToolCall {
                    call_id: "remote-call".into(),
                    name: tool.name.clone(),
                    arguments: "{\"message\":\"hello\"}".into(),
                })]
            } else {
                vec![HistoryItem::AssistantText("done".into())]
            };
            Ok(
                Box::new(Finished(Some(ModelUpdate::Finished { items, usage: None })))
                    as Box<dyn ModelStream>,
            )
        })
    }
}
struct Approve {
    count: AtomicUsize,
    answer: bool,
}
#[derive(Default)]
struct PendingApproval {
    entered: AtomicBool,
    dropped: AtomicBool,
}
struct ApprovalWait<'a>(&'a AtomicBool);
impl Drop for ApprovalWait<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl ApprovalHandler for PendingApproval {
    fn confirm<'a>(
        &'a self,
        _: &'a ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            let _wait = ApprovalWait(&self.dropped);
            self.entered.store(true, Ordering::Release);
            pending().await
        })
    }
}
impl ApprovalHandler for Approve {
    fn confirm<'a>(
        &'a self,
        _: &'a ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            self.count.fetch_add(1, Ordering::AcqRel);
            Ok(Some(self.answer))
        })
    }
}
const SERVER: &str = r#"import os,sys,json,time
open(sys.argv[1]+'.tmp', 'w').write(str(os.getpid()))
os.replace(sys.argv[1]+'.tmp',sys.argv[1])
for line in sys.stdin:
    request=json.loads(line)
    if 'id' not in request: continue
    method=request['method']
    if method=='server/discover':
        result={'resultType':'complete','supportedVersions':['2026-07-28'],'capabilities':{'tools':{}},'ttlMs':0,'cacheScope':'private'}
    elif method=='tools/list':
        result={'resultType':'complete','tools':[{'name':'echo','description':'Fixture echo','inputSchema':{'type':'object','properties':{'message':{'type':'string'}},'required':['message']}}],'ttlMs':0,'cacheScope':'private'}
    elif method=='tools/call':
        if os.environ.get('WAIT_FILE'):
            open(sys.argv[2], 'w').write('calling')
            while not os.path.exists(os.environ['WAIT_FILE']): time.sleep(.005)
        result={'resultType':'complete','content':[{'type':'text','text':request['params']['arguments']['message']+'|'+os.environ.get('SERVER_TOKEN','')}],'isError':False}
    else: continue
    print(json.dumps({'jsonrpc':'2.0','id':request['id'],'result':result}),flush=True)
"#;

fn session(root: &Path, mode: WorkMode) -> Session {
    Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: mode,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture".into(),
        history: vec![HistoryItem::UserText("fixture task".into())],
    }
}
fn runtime(
    root: &Path,
    session: &Session,
    permissions: PermissionMode,
    model: Arc<FixtureModel>,
    confirmation: Option<Arc<dyn ApprovalHandler>>,
    wait: bool,
) -> (AgentRuntime, Arc<StatusStore>) {
    runtime_with_options(
        root,
        session,
        permissions,
        model,
        confirmation,
        wait,
        ServiceOptions::default(),
    )
}
struct ServiceOptions {
    active_time: Duration,
    http: Option<HttpMcpSettings>,
}
impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            active_time: Duration::from_secs(1800),
            http: None,
        }
    }
}
fn runtime_with_options(
    root: &Path,
    session: &Session,
    permissions: PermissionMode,
    model: Arc<FixtureModel>,
    confirmation: Option<Arc<dyn ApprovalHandler>>,
    wait: bool,
    options: ServiceOptions,
) -> (AgentRuntime, Arc<StatusStore>) {
    let script = root.join("mcp-server.py");
    std::fs::write(&script, SERVER).unwrap();
    let mut extra = vec![("SAYA_MCP_TEST_TOKEN".into(), TOKEN.into())];
    let mut env = BTreeMap::from([("SERVER_TOKEN".into(), "SAYA_MCP_TEST_TOKEN".into())]);
    if wait {
        extra.push((
            "SAYA_MCP_TEST_WAIT".into(),
            root.join("release").into_os_string(),
        ));
        env.insert("WAIT_FILE".into(), "SAYA_MCP_TEST_WAIT".into());
    }
    let environment = Environment::from_pairs(std::env::vars_os().chain(extra));
    let store = Arc::new(StatusStore::default());
    let exec = ExecConfig {
        environment: environment.clone(),
        secret_env_names: BTreeSet::from([
            "SAYA_MCP_TEST_TOKEN".into(),
            "SAYA_MCP_TEST_WAIT".into(),
        ]),
        ..ExecConfig::default()
    };
    let setup = RuntimeSetup {
        run: Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: session.work_mode,
            permissions,
        },
        model,
        store: store.clone(),
        policy: Arc::new(Policy::new(root, PermissionConfig::default()).unwrap()),
        files: Arc::new(
            ReadTools::new(
                root,
                &[],
                permissions == PermissionMode::Bypass,
                ReadLimits::default(),
            )
            .unwrap(),
        ),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("fixture".into()),
        exec,
        preauthorizations: Preauthorizations::default(),
        known_secrets: Vec::new(),
        summary: "fixture".into(),
        confirmation,
        limits: RuntimeLimits {
            active_time: options.active_time,
            model_connect: Duration::from_secs(3),
            model_idle: Duration::from_secs(3),
            model_total: Duration::from_secs(3),
            ..RuntimeLimits::default()
        },
    };
    let settings = McpSettings {
        servers: BTreeMap::from([(
            "local".into(),
            McpServerSettings {
                enabled: true,
                transport: options
                    .http
                    .map(McpTransportSettings::Http)
                    .unwrap_or_else(|| {
                        McpTransportSettings::Stdio(StdioMcpSettings {
                            argv: vec![
                                "python3".into(),
                                "-u".into(),
                                script.to_string_lossy().into_owned(),
                                root.join("pid").to_string_lossy().into_owned(),
                                root.join("calling").to_string_lossy().into_owned(),
                            ],
                            cwd: None,
                            env,
                        })
                    }),
                read_only_tools: BTreeSet::from(["echo".into()]),
                limits: McpLimitSettings {
                    connect_timeout_seconds: 3,
                    request_timeout_seconds: 3,
                    ..McpLimitSettings::default()
                },
            },
        )]),
    };
    let mut sandbox = SandboxPolicy::workspace(root.into());
    sandbox.backend = BackendKind::Host;
    let runtime = AgentRuntime::new(setup)
        .unwrap()
        .with_execution_settings(sandbox, None)
        .unwrap()
        .with_mcp_settings(
            settings,
            environment,
            None,
            Arc::new(SecretRegistry::default()),
        )
        .unwrap();
    (runtime, store)
}
fn model(use_tool: bool) -> Arc<FixtureModel> {
    Arc::new(FixtureModel {
        requests: Mutex::new(Vec::new()),
        use_tool,
    })
}
fn process_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn normal_entry_starts_authorized_stdio_redacts_results_and_reaps_before_return() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), WorkMode::Work);
    let model = model(true);
    let approval = Arc::new(Approve {
        count: AtomicUsize::new(0),
        answer: true,
    });
    let (mut runtime, store) = runtime(
        root.path(),
        &session,
        PermissionMode::Manual,
        model.clone(),
        Some(approval.clone()),
        false,
    );
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    assert_eq!(approval.count.load(Ordering::Acquire), 2);
    let pid = std::fs::read_to_string(root.path().join("pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    assert!(!process_alive(pid));
    let history = serde_json::to_string(&session.history).unwrap();
    assert!(!history.contains(TOKEN));
    assert!(history.contains("[REDACTED]"));
    let saved = store.load_session(session.id).await.unwrap().unwrap();
    assert!(!serde_json::to_string(&saved).unwrap().contains(TOKEN));
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[0]
            .dynamic_instructions
            .as_ref()
            .unwrap()
            .contains("saya_execution_environment_v1")
    );
    assert!(
        requests[0]
            .dynamic_instructions
            .as_ref()
            .unwrap()
            .contains("unsupported")
    );
    let states = store.states.lock().unwrap();
    let status = states
        .iter()
        .map(|servers| servers[0].state)
        .collect::<Vec<_>>();
    assert_eq!(
        status,
        vec![
            McpServerState::Configured,
            McpServerState::Connecting,
            McpServerState::Connected,
            McpServerState::Stopped
        ]
    );
    assert_eq!(states[2][0].tools, 1);
    assert_eq!(states[3][0].tools, 0);
}

#[tokio::test]
async fn plan_and_noninteractive_manual_never_launch_the_stdio_server() {
    for (mode, permissions, status) in [
        (WorkMode::Plan, PermissionMode::Bypass, RunStatus::Completed),
        (
            WorkMode::Work,
            PermissionMode::Manual,
            RunStatus::NeedsConfirmation,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path(), mode);
        let model = model(false);
        let (mut runtime, _) = runtime(
            root.path(),
            &session,
            permissions,
            model.clone(),
            None,
            false,
        );
        let result = runtime.run(&mut session).await.unwrap();
        assert_eq!(result.status, status);
        assert!(!root.path().join("pid").exists());
        if mode == WorkMode::Work {
            assert!(model.requests.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn plan_stays_pending_until_the_current_mcp_call_and_stdio_cleanup_finish() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), WorkMode::Work);
    let model = model(true);
    let (mut runtime, _) = runtime(
        root.path(),
        &session,
        PermissionMode::Bypass,
        model,
        None,
        true,
    );
    let handle = runtime.handle();
    let task = tokio::spawn(async move {
        let result = runtime.run(&mut session).await;
        (session, result)
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !root.path().join("calling").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(root.path().join("pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    handle.request_mode(WorkMode::Plan).unwrap();
    handle.request_mode(WorkMode::Plan).unwrap();
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(handle.mode().unwrap().current, WorkMode::Work);
    assert_eq!(handle.mode().unwrap().pending, Some(WorkMode::Plan));
    assert!(process_alive(pid));
    std::fs::write(root.path().join("release"), b"release").unwrap();
    let (session, result) = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap().status, RunStatus::Completed);
    assert_eq!(session.work_mode, WorkMode::Plan);
    assert!(!process_alive(pid));
}

#[tokio::test]
async fn plan_cancels_pending_stdio_startup_approval_before_launch() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), WorkMode::Work);
    let model = model(false);
    let approval = Arc::new(PendingApproval::default());
    let (mut runtime, _) = runtime(
        root.path(),
        &session,
        PermissionMode::Manual,
        model.clone(),
        Some(approval.clone()),
        false,
    );
    let handle = runtime.handle();
    let task = tokio::spawn(async move {
        let result = runtime.run(&mut session).await;
        (session, result)
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !approval.entered.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    handle.request_mode(WorkMode::Plan).unwrap();
    handle.request_mode(WorkMode::Plan).unwrap();
    let (session, result) = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap().status, RunStatus::Completed);
    assert_eq!(session.work_mode, WorkMode::Plan);
    assert!(approval.dropped.load(Ordering::Acquire));
    assert!(!root.path().join("pid").exists());
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn plan_cancels_stdio_handshake_and_reaps_the_already_started_child() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), WorkMode::Work);
    let model = model(false);
    let (mut runtime, _) = runtime(
        root.path(),
        &session,
        PermissionMode::Bypass,
        model.clone(),
        None,
        false,
    );
    std::fs::write(
        root.path().join("mcp-server.py"),
        "import os,sys,time\nopen(sys.argv[1]+'.tmp', 'w').write(str(os.getpid()))\nos.replace(sys.argv[1]+'.tmp',sys.argv[1])\nwhile True: time.sleep(.01)\n",
    )
    .unwrap();
    let handle = runtime.handle();
    let task = tokio::spawn(async move {
        let result = runtime.run(&mut session).await;
        (session, result)
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while !root.path().join("pid").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let pid = std::fs::read_to_string(root.path().join("pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    handle.request_mode(WorkMode::Plan).unwrap();
    let (session, result) = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    let result = result.unwrap();
    assert_eq!(result.status, RunStatus::Failed);
    assert_eq!(result.stop_reason.as_deref(), Some("outcome_unknown"));
    assert!(!process_alive(pid));
    assert_eq!(session.work_mode, WorkMode::Plan);
    assert!(model.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn lifecycle_storage_failure_stops_before_the_model_and_still_reaps_the_server() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), WorkMode::Work);
    let model = model(true);
    let (mut runtime, store) = runtime(
        root.path(),
        &session,
        PermissionMode::Bypass,
        model.clone(),
        None,
        false,
    );
    store.fail_connected.store(true, Ordering::Release);
    assert_eq!(
        runtime.run(&mut session).await.unwrap_err().0.code,
        "storage_error"
    );
    assert!(model.requests.lock().unwrap().is_empty());
    let pid = std::fs::read_to_string(root.path().join("pid"))
        .unwrap()
        .parse()
        .unwrap();
    assert!(!process_alive(pid));
    let states = store.states.lock().unwrap();
    assert!(
        !states
            .iter()
            .flatten()
            .any(|status| status.state == McpServerState::Connected)
    );
    assert_eq!(states.last().unwrap()[0].state, McpServerState::Stopped);
}

struct SlowApproval(Duration);
impl ApprovalHandler for SlowApproval {
    fn confirm<'a>(
        &'a self,
        _: &'a ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            tokio::time::sleep(self.0).await;
            Ok(Some(true))
        })
    }
}
#[tokio::test]
async fn slow_stdio_handshake_and_catalogue_respect_the_active_budget() {
    for method in ["server/discover", "tools/list"] {
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path(), WorkMode::Work);
        let model = model(false);
        let (mut runtime, _) = runtime_with_options(
            root.path(),
            &session,
            PermissionMode::Bypass,
            model.clone(),
            None,
            false,
            ServiceOptions {
                active_time: Duration::from_millis(250),
                http: None,
            },
        );
        let server = SERVER.replace(
            "    method=request['method']",
            &format!("    method=request['method']\n    if method=={method:?}: time.sleep(1.5)"),
        );
        std::fs::write(root.path().join("mcp-server.py"), server).unwrap();
        let started = std::time::Instant::now();
        let result = runtime.run(&mut session).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{method} ignored the active budget for {:?}",
            started.elapsed()
        );
        assert_eq!(
            result.stop_reason.as_deref(),
            Some(if method == "server/discover" {
                "outcome_unknown"
            } else {
                "limit_reached"
            })
        );
        assert!(model.requests.lock().unwrap().is_empty());
        let pid = std::fs::read_to_string(root.path().join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(!process_alive(pid));
    }
}
#[tokio::test]
async fn human_startup_confirmation_pauses_the_active_budget() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), WorkMode::Work);
    let model = model(false);
    let (mut runtime, _) = runtime_with_options(
        root.path(),
        &session,
        PermissionMode::Manual,
        model.clone(),
        Some(Arc::new(SlowApproval(Duration::from_millis(500)))),
        false,
        ServiceOptions {
            active_time: Duration::from_millis(250),
            http: None,
        },
    );
    let started = std::time::Instant::now();
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed);
    assert!(started.elapsed() >= Duration::from_millis(500));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
}
struct SlowHttp {
    endpoint: String,
    stop: Arc<AtomicBool>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl Drop for SlowHttp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}
fn slow_http(delayed_method: &str) -> SlowHttp {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let delayed = delayed_method.to_owned();
    let task = std::thread::spawn(move || {
        while !stopped.load(Ordering::Acquire) {
            let Ok((mut socket, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(1));
                continue;
            };
            socket.set_nonblocking(false).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let body = loop {
                let Ok(count) = socket.read(&mut buffer) else {
                    break None;
                };
                if count == 0 {
                    break None;
                }
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(start) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..start]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .and_then(|value| value.trim().parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= start + 4 + length {
                        break Some(bytes[start + 4..start + 4 + length].to_vec());
                    }
                }
            };
            let Some(body) = body else {
                continue;
            };
            let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
                continue;
            };
            let method = request["method"].as_str().unwrap_or("");
            if method == delayed {
                let until = std::time::Instant::now() + Duration::from_millis(1500);
                while !stopped.load(Ordering::Acquire) && std::time::Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let result = match method {
                "server/discover" => {
                    serde_json::json!({"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"})
                }
                "tools/list" => {
                    serde_json::json!({"resultType":"complete","tools":[{"name":"echo","description":"Fixture","inputSchema":{"type":"object"}}],"ttlMs":0,"cacheScope":"private"})
                }
                _ => continue,
            };
            let body =
                serde_json::json!({"jsonrpc":"2.0","id":request["id"],"result":result}).to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes());
        }
    });
    SlowHttp {
        endpoint,
        stop,
        task: Some(task),
    }
}
#[tokio::test]
async fn slow_http_handshake_and_catalogue_respect_the_active_budget() {
    for method in ["server/discover", "tools/list"] {
        let fixture = slow_http(method);
        let root = tempfile::tempdir().unwrap();
        let mut session = session(root.path(), WorkMode::Work);
        let model = model(false);
        let http = HttpMcpSettings {
            endpoint: fixture.endpoint.clone(),
            allow_insecure_http: true,
            bearer_env: None,
            headers_env: BTreeMap::new(),
            credential_ref: None,
            scopes: BTreeSet::new(),
        };
        let (mut runtime, _) = runtime_with_options(
            root.path(),
            &session,
            PermissionMode::Bypass,
            model.clone(),
            None,
            false,
            ServiceOptions {
                active_time: Duration::from_millis(250),
                http: Some(http),
            },
        );
        let started = std::time::Instant::now();
        let result = runtime.run(&mut session).await.unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{method} ignored the active budget for {:?}",
            started.elapsed()
        );
        assert_eq!(result.status, RunStatus::LimitReached);
        assert_eq!(result.stop_reason.as_deref(), Some("limit_reached"));
        assert!(model.requests.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn expired_budget_after_durable_intent_is_known_failed_before_process_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let mut session = session(root.path(), WorkMode::Work);
    let model = model(false);
    let (mut runtime, store) = runtime_with_options(
        root.path(),
        &session,
        PermissionMode::Bypass,
        model.clone(),
        None,
        false,
        ServiceOptions {
            active_time: Duration::from_millis(250),
            http: None,
        },
    );
    *store.intent_delay.lock().unwrap() = Some(Duration::from_millis(350));
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::LimitReached);
    assert_eq!(result.stop_reason.as_deref(), Some("limit_reached"));
    assert!(!root.path().join("pid").exists());
    assert!(model.requests.lock().unwrap().is_empty());
    assert_eq!(*store.tool_states.lock().unwrap(), vec![ToolState::Failed]);
}
