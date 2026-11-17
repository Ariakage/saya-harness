use super::*;
use crate::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation as AdapterOperation,
        Store,
    },
    checkpoint::{
        CheckpointLimits, FileOutcome, FileOutcomeStatus, FileState, FileVersion, RestoreOperation,
        WorkspaceIdentity,
    },
    contract::{
        ApprovalRequest, ModelProtocol, ModelPurpose, PermissionMode, Run, RunId, RunStatus,
        SessionId,
    },
    credentials::SecretRegistry,
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    review::ReviewConfig,
    runtime::{ApprovalHandler, RuntimeLimits, RuntimeSetup},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    collections::VecDeque,
    future::{Future, pending},
    pin::Pin,
    sync::{Mutex, atomic::AtomicUsize},
};

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct Stream(Option<Vec<HistoryItem>>);
impl ModelStream for Stream {
    fn next<'a>(
        &'a mut self,
        _: &'a dyn Cancellation,
    ) -> AdapterOperation<'a, Option<ModelUpdate>> {
        Box::pin(async move {
            Ok(self
                .0
                .take()
                .map(|items| ModelUpdate::Finished { items, usage: None }))
        })
    }
}
struct Script(Mutex<VecDeque<Vec<HistoryItem>>>);
impl Model for Script {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> AdapterOperation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            assert_eq!(request.purpose, ModelPurpose::Execution);
            let items = self
                .0
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| vec![HistoryItem::AssistantText("done".into())]);
            Ok(Box::new(Stream(Some(items))) as Box<dyn ModelStream>)
        })
    }
}
struct Confirm {
    count: AtomicUsize,
    answer: bool,
    external_edit: Option<PathBuf>,
}
impl ApprovalHandler for Confirm {
    fn confirm<'a>(
        &'a self,
        request: &'a ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> AdapterOperation<'a, Option<bool>> {
        Box::pin(async move {
            self.count.fetch_add(1, Ordering::SeqCst);
            assert!(request.tool.starts_with("checkpoint_"));
            if let Some(path) = &self.external_edit {
                std::fs::write(path, "external modification").unwrap();
            }
            Ok(Some(self.answer))
        })
    }
}
struct FailFinishedStore {
    inner: Arc<dyn Store>,
}
impl Store for FailFinishedStore {
    fn load_session<'a>(&'a self, id: SessionId) -> AdapterOperation<'a, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> AdapterOperation<'a, ()> {
        Box::pin(async move {
            if changes
                .iter()
                .any(|change| matches!(change, StoreChange::ToolFinished { .. }))
            {
                return Err(error("storage_error", "Controlled result storage failure"));
            }
            self.inner.commit(changes).await
        })
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    store: Arc<CheckpointStore>,
    session: Session,
    run: Run,
}
impl Fixture {
    fn new(mode: WorkMode, permissions: PermissionMode) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        std::fs::write(workspace.join("file.txt"), "original\n").unwrap();
        let store = Arc::new(
            CheckpointStore::open(
                root.path().join("checkpoints"),
                Arc::new(SecretRegistry::default()),
                CheckpointLimits::default(),
            )
            .unwrap(),
        );
        let session = Session {
            id: SessionId::new(),
            workspace: workspace.clone(),
            work_mode: mode,
            profile: "fixture".into(),
            model: "fixture".into(),
            protocol: ModelProtocol::ChatCompletions,
            history: vec![HistoryItem::UserText(
                "Use checkpoints for the selected file".into(),
            )],
        };
        let run = Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: mode,
            permissions,
        };
        Self {
            _root: root,
            workspace,
            store,
            session,
            run,
        }
    }
    fn baseline(&self) -> CheckpointToken {
        self.store
            .bind_workspace(&self.workspace, self.session.id, &[])
            .unwrap()
            .capture(
                &["file.txt".into()],
                CheckpointSource {
                    session_id: self.session.id,
                    run_id: self.run.id,
                    revision: self.session.history.len() as u64,
                    label: "baseline".into(),
                },
                &AtomicBool::new(false),
            )
            .unwrap()
            .token
    }
    fn runtime(
        &self,
        call: ToolCall,
        handler: Option<Arc<dyn ApprovalHandler>>,
        permissions: PermissionConfig,
        write_roots: Vec<PathBuf>,
        protected: Vec<PathBuf>,
    ) -> AgentRuntime {
        let model = Arc::new(Script(Mutex::new(VecDeque::from([vec![
            HistoryItem::ToolCall(call),
        ]]))));
        AgentRuntime::new(RuntimeSetup {
            run: self.run.clone(),
            model,
            store: Arc::new(MemoryStore::default()),
            policy: Arc::new(Policy::new(&self.workspace, permissions).unwrap()),
            files: Arc::new(
                ReadTools::new(
                    &self.workspace,
                    &[],
                    self.run.permissions == PermissionMode::Bypass,
                    ReadLimits::default(),
                )
                .unwrap(),
            ),
            cancellation: Arc::new(Never),
            review: ReviewConfig::new("fixture".into()),
            exec: ExecConfig::default(),
            preauthorizations: Preauthorizations::default(),
            known_secrets: Vec::new(),
            summary: "checkpoint fixture".into(),
            confirmation: handler,
            limits: RuntimeLimits::default(),
        })
        .unwrap()
        .with_checkpoints(self.store.clone(), protected, write_roots)
        .unwrap()
    }
}
fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        call_id: "checkpoint-call".into(),
        name: name.into(),
        arguments: arguments.to_string(),
    }
}
fn last_tool(session: &Session) -> &ToolResult {
    session
        .history
        .iter()
        .rev()
        .find_map(|item| match item {
            HistoryItem::ToolResult { result, .. } => Some(result),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn preparation_deadline_blocks_late_success_before_human_approval() {
    use std::os::fd::AsRawFd;
    use std::time::Duration;
    let mut fixture = Fixture::new(WorkMode::Code, PermissionMode::Manual);
    fixture.baseline();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.store.protected_path().join("index.lock"))
        .unwrap();
    // SAFETY: this owned regular-file descriptor is held until the helper
    // releases it; the worker uses its independent descriptor for flock.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(lock);
    });
    let handler = Arc::new(Confirm {
        count: AtomicUsize::new(0),
        answer: true,
        external_edit: None,
    });
    let mut runtime = fixture.runtime(
        call("checkpoint_list", json!({})),
        Some(handler.clone()),
        PermissionConfig::default(),
        vec![fixture.workspace.clone()],
        Vec::new(),
    );
    runtime.limits.tool_timeout = Duration::from_millis(10);
    let outcome = runtime.run(&mut fixture.session).await.unwrap();
    release.join().unwrap();
    assert_eq!(outcome.status, RunStatus::Completed);
    assert_eq!(
        last_tool(&fixture.session).error.as_ref().unwrap().code,
        "tool_timeout"
    );
    assert_eq!(handler.count.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .store
            .bind_workspace(&fixture.workspace, fixture.session.id, &[])
            .unwrap()
            .list()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn normal_runtime_create_list_diff_accept_and_restore_use_the_checkpoint_store() {
    for name in [
        "checkpoint_create",
        "checkpoint_list",
        "checkpoint_diff",
        "checkpoint_accept",
        "checkpoint_restore",
    ] {
        let mut fixture = Fixture::new(WorkMode::Code, PermissionMode::Bypass);
        let token = fixture.baseline();
        std::fs::write(fixture.workspace.join("file.txt"), "changed\n").unwrap();
        let arguments = match name {
            "checkpoint_create" => json!({"paths":["file.txt"]}),
            "checkpoint_list" => json!({"max_items":1}),
            "checkpoint_diff" => json!({"checkpoint":token}),
            _ => json!({"checkpoint":token,"paths":["file.txt"]}),
        };
        let mut runtime = fixture.runtime(
            call(name, arguments),
            None,
            PermissionConfig::default(),
            vec![fixture.workspace.clone()],
            Vec::new(),
        );
        let outcome = runtime.run(&mut fixture.session).await.unwrap();
        assert_eq!(
            outcome.status,
            RunStatus::Completed,
            "{name}: {:?}",
            outcome.error
        );
        let result = last_tool(&fixture.session);
        assert!(result.ok, "{name}: {:?}", result.error);
        assert!(serde_json::to_vec(result).unwrap().len() <= MAX_RESULT_BYTES);
        if name == "checkpoint_restore" {
            assert_eq!(
                std::fs::read_to_string(fixture.workspace.join("file.txt")).unwrap(),
                "original\n"
            );
        }
        let count = fixture
            .store
            .bind_workspace(&fixture.workspace, fixture.session.id, &[])
            .unwrap()
            .list()
            .unwrap()
            .len();
        assert_eq!(
            count,
            if matches!(name, "checkpoint_create" | "checkpoint_accept") {
                2
            } else {
                1
            }
        );
    }
}

#[tokio::test]
async fn plan_bypass_and_host_write_roots_never_restore_unapproved_files() {
    for (mode, write_roots) in [(WorkMode::Plan, true), (WorkMode::Work, false)] {
        let mut fixture = Fixture::new(mode, PermissionMode::Bypass);
        let token = fixture.baseline();
        std::fs::write(fixture.workspace.join("file.txt"), "current content").unwrap();
        let roots = if write_roots {
            vec![fixture.workspace.clone()]
        } else {
            Vec::new()
        };
        let mut runtime = fixture.runtime(
            call(
                "checkpoint_restore",
                json!({"checkpoint":token,"paths":["file.txt"]}),
            ),
            None,
            PermissionConfig::default(),
            roots,
            Vec::new(),
        );
        runtime.run(&mut fixture.session).await.unwrap();
        assert!(!last_tool(&fixture.session).ok);
        assert_eq!(
            std::fs::read_to_string(fixture.workspace.join("file.txt")).unwrap(),
            "current content"
        );
    }
}

#[tokio::test]
async fn manual_denial_and_explicit_deny_are_enforced_on_exact_checkpoint_targets() {
    let mut fixture = Fixture::new(WorkMode::Work, PermissionMode::Manual);
    let token = fixture.baseline();
    std::fs::write(fixture.workspace.join("file.txt"), "current content").unwrap();
    let handler = Arc::new(Confirm {
        count: AtomicUsize::new(0),
        answer: false,
        external_edit: None,
    });
    let mut runtime = fixture.runtime(
        call(
            "checkpoint_restore",
            json!({"checkpoint":token,"paths":["file.txt"]}),
        ),
        Some(handler.clone()),
        PermissionConfig::default(),
        vec![fixture.workspace.clone()],
        Vec::new(),
    );
    runtime.run(&mut fixture.session).await.unwrap();
    assert_eq!(handler.count.load(Ordering::SeqCst), 1);
    assert!(!last_tool(&fixture.session).ok);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("file.txt")).unwrap(),
        "current content"
    );
    let mut fixture = Fixture::new(WorkMode::Work, PermissionMode::Bypass);
    let token = fixture.baseline();
    std::fs::write(fixture.workspace.join("file.txt"), "current content").unwrap();
    let permissions: PermissionConfig = serde_json::from_value(json!({"rules":[{"effect":"deny","tool":"checkpoint_restore","path_glob":fixture.workspace.join("file.txt").to_str().unwrap()}]})).unwrap();
    let mut runtime = fixture.runtime(
        call(
            "checkpoint_restore",
            json!({"checkpoint":token,"paths":["file.txt"]}),
        ),
        None,
        permissions,
        vec![fixture.workspace.clone()],
        Vec::new(),
    );
    runtime.run(&mut fixture.session).await.unwrap();
    assert!(!last_tool(&fixture.session).ok);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("file.txt")).unwrap(),
        "current content"
    );
}

#[tokio::test]
async fn approved_capture_rechecks_external_edits_and_manual_reads_ask_once() {
    let mut fixture = Fixture::new(WorkMode::Plan, PermissionMode::Manual);
    let handler = Arc::new(Confirm {
        count: AtomicUsize::new(0),
        answer: true,
        external_edit: Some(fixture.workspace.join("file.txt")),
    });
    let mut runtime = fixture.runtime(
        call("checkpoint_create", json!({"paths":["file.txt"]})),
        Some(handler.clone()),
        PermissionConfig::default(),
        vec![fixture.workspace.clone()],
        Vec::new(),
    );
    runtime.run(&mut fixture.session).await.unwrap();
    assert_eq!(handler.count.load(Ordering::SeqCst), 1);
    assert_eq!(
        last_tool(&fixture.session).error.as_ref().unwrap().code,
        "checkpoint_conflict"
    );
    assert!(
        fixture
            .store
            .bind_workspace(&fixture.workspace, fixture.session.id, &[])
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
    let mut fixture = Fixture::new(WorkMode::Plan, PermissionMode::Manual);
    let handler = Arc::new(Confirm {
        count: AtomicUsize::new(0),
        answer: true,
        external_edit: None,
    });
    let mut runtime = fixture.runtime(
        call("checkpoint_list", json!({})),
        Some(handler.clone()),
        PermissionConfig::default(),
        Vec::new(),
        Vec::new(),
    );
    runtime.run(&mut fixture.session).await.unwrap();
    assert_eq!(handler.count.load(Ordering::SeqCst), 1);
    assert!(last_tool(&fixture.session).ok);
}

#[tokio::test]
async fn result_storage_failure_retains_actual_restore_outcome_and_stops_before_another_model_call()
{
    let mut fixture = Fixture::new(WorkMode::Code, PermissionMode::Bypass);
    let token = fixture.baseline();
    std::fs::write(fixture.workspace.join("file.txt"), "changed\n").unwrap();
    let mut runtime = fixture.runtime(
        call(
            "checkpoint_restore",
            json!({"checkpoint":token,"paths":["file.txt"]}),
        ),
        None,
        PermissionConfig::default(),
        vec![fixture.workspace.clone()],
        Vec::new(),
    );
    runtime.store = Arc::new(FailFinishedStore {
        inner: runtime.store.clone(),
    });
    let failure = runtime.run(&mut fixture.session).await.unwrap_err();
    assert_eq!(failure.0.code, "storage_error");
    let actual = failure.2.as_ref().unwrap();
    assert_eq!(actual.data.as_ref().unwrap()["status"], "completed");
    assert_eq!(actual.data.as_ref().unwrap()["applied_files"], 1);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("file.txt")).unwrap(),
        "original\n"
    );
    assert_eq!(runtime.budget.used().unwrap(), 1);
    let saved = runtime
        .store
        .load_session(fixture.session.id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        saved
            .history
            .iter()
            .any(|item| matches!(item, HistoryItem::ToolCall(_)))
    );
    assert!(
        !saved
            .history
            .iter()
            .any(|item| matches!(item, HistoryItem::ToolResult { .. }))
    );
}

#[tokio::test]
async fn large_journal_failure_keeps_bounded_actual_counts_and_stops_without_replay() {
    let mut fixture = Fixture::new(WorkMode::Code, PermissionMode::Bypass);
    let paths = (0..128)
        .map(|index| format!("{index:03}-{}.txt", "x".repeat(240)))
        .collect::<Vec<_>>();
    for path in &paths {
        std::fs::write(fixture.workspace.join(path), "original").unwrap();
    }
    let workspace = fixture
        .store
        .bind_workspace(&fixture.workspace, fixture.session.id, &[])
        .unwrap();
    let source = CheckpointSource {
        session_id: fixture.session.id,
        run_id: fixture.run.id,
        revision: 1,
        label: "multi-file baseline".into(),
    };
    let token = workspace
        .capture(&paths, source.clone(), &AtomicBool::new(false))
        .unwrap()
        .token;
    for path in &paths {
        std::fs::write(fixture.workspace.join(path), "changed").unwrap();
    }
    let probe = workspace
        .prepare_restore(&token, &paths, source, &AtomicBool::new(false))
        .unwrap();
    assert!(serde_json::to_vec(probe.changes()).unwrap().len() > MAX_RESULT_BYTES);
    fixture.store.fail_next_journal_for_test();
    let mut runtime = fixture.runtime(
        call(
            "checkpoint_restore",
            json!({"checkpoint":token,"paths":paths}),
        ),
        None,
        PermissionConfig::default(),
        vec![fixture.workspace.clone()],
        Vec::new(),
    );
    let failure = runtime.run(&mut fixture.session).await.unwrap_err();
    assert_eq!(failure.0.code, "checkpoint_storage");
    let actual = failure.2.unwrap();
    assert!(actual.truncated && !actual.ok);
    assert!(serde_json::to_vec(&actual).unwrap().len() <= MAX_RESULT_BYTES);
    let data = actual.data.unwrap();
    assert_eq!(data["total_files"], 128);
    assert_eq!(data["applied_files"], 0);
    assert!(data["files_omitted"].as_u64().unwrap() > 0);
    assert!(data["operation_id"].as_str().is_some());
    assert_eq!(runtime.budget.used().unwrap(), 1);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join(&paths[0])).unwrap(),
        "changed"
    );
}

#[test]
fn strict_arguments_and_pages_reject_unknown_fields_duplicate_keys_and_unbound_offsets() {
    let token = CheckpointToken {
        id: uuid::Uuid::now_v7().to_string(),
        sha256: "a".repeat(64),
    };
    for arguments in [
        json!({"checkpoint":token,"extra":true}),
        json!({"checkpoint":token,"offset":1}),
        json!({"checkpoint":token,"max_items":33}),
    ] {
        assert!(parse(&call("checkpoint_diff", arguments), 128).is_err());
    }
    let duplicate = ToolCall {
        call_id: "id".into(),
        name: "checkpoint_create".into(),
        arguments: "{\"paths\":[\"a\"],\"paths\":[\"b\"]}".into(),
    };
    assert!(parse(&duplicate, 128).is_err());
    for paths in [
        json!(["../outside"]),
        json!(["a", "a"]),
        json!(["/absolute"]),
        json!(["a\u{1b}"]),
    ] {
        assert!(parse(&call("checkpoint_create", json!({"paths":paths})), 128).is_err());
    }
    let first = Page {
        offset: None,
        max_items: Some(1),
        max_bytes: Some(1024),
        catalog_sha256: None,
    };
    let result = page_result(vec!["one", "two"], &first, "items", false, json!({})).unwrap();
    assert!(result.truncated);
    let data = result.data.unwrap();
    assert_eq!(data["items"], json!(["one"]));
    let next = Page {
        offset: Some(1),
        catalog_sha256: Some(data["catalog_sha256"].as_str().unwrap().into()),
        ..first
    };
    assert_eq!(
        page_result(vec!["one", "two"], &next, "items", false, json!({}))
            .unwrap()
            .data
            .unwrap()["items"],
        json!(["two"])
    );
    assert!(page_result(vec!["one", "changed"], &next, "items", false, json!({})).is_err());
}

#[test]
fn partial_and_unknown_restore_results_keep_actual_counts_with_bounded_file_details() {
    let source = CheckpointSource {
        session_id: SessionId::new(),
        run_id: RunId::new(),
        revision: 1,
        label: "restore".into(),
    };
    let current = FileVersion {
        state: FileState::Absent,
        observation: "a".repeat(64),
    };
    let files = (0..128)
        .map(|index| FileOutcome {
            operation: RestoreOperation {
                path: format!("{}-{index}", "f".repeat(1000)),
                expected: current.clone(),
                desired: FileState::Absent,
            },
            status: if index == 0 {
                FileOutcomeStatus::Applied
            } else {
                FileOutcomeStatus::Pending
            },
            publication_observed: index == 0,
            observed: None,
            error: None,
        })
        .collect();
    let report = ChangedFiles {
        schema_version: 1,
        operation_id: uuid::Uuid::now_v7().to_string(),
        checkpoint: CheckpointToken {
            id: uuid::Uuid::now_v7().to_string(),
            sha256: "b".repeat(64),
        },
        workspace: WorkspaceIdentity {
            path: PathBuf::from("/workspace"),
            device: 1,
            inode: 1,
        },
        source,
        status: RestoreStatus::Partial,
        files,
        error: None,
    };
    let result = restore_result(report.clone()).unwrap();
    assert!(!result.ok && result.truncated);
    assert_eq!(result.data.as_ref().unwrap()["applied_files"], 1);
    assert_eq!(result.data.as_ref().unwrap()["total_files"], 128);
    assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_RESULT_BYTES);
    let mut unknown = report;
    unknown.status = RestoreStatus::Unknown;
    assert_eq!(
        restore_result(unknown).unwrap().error.unwrap().code,
        "outcome_unknown"
    );
}
