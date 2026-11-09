use super::*;
use crate::{
    contract::{PermissionMode, RunId, SessionId, WorkMode},
    plugin_manifest::{MANIFEST_NAME, PackageSource, SourceKind, inspect_package},
    plugin_store::{EnableGrant, PluginScope, PluginStore, StoreLimits},
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::pending,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct SoonCancel(AtomicBool);
impl Cancellation for SoonCancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(1)).await;
            self.0.store(true, Ordering::SeqCst);
        })
    }
}

struct PrivateRoot(tempfile::TempDir);
impl Drop for PrivateRoot {
    fn drop(&mut self) {
        fn writable(path: &Path) {
            let Ok(metadata) = std::fs::symlink_metadata(path) else {
                return;
            };
            if metadata.is_dir() {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
                if let Ok(entries) = std::fs::read_dir(path) {
                    for entry in entries.flatten() {
                        writable(&entry.path());
                    }
                }
            }
        }
        writable(self.0.path());
    }
}
struct Fixture {
    hooks: Vec<EnabledHook>,
    run: Run,
    _root: PrivateRoot,
}
impl Fixture {
    fn new(mode: WorkMode, permissions: PermissionMode) -> Self {
        let root = PrivateRoot(tempfile::tempdir().unwrap());
        let source = tempfile::tempdir().unwrap();
        let mut manifest = r#"schema_version = 1
[plugin]
id = "io.saya.hook-test"
version = "1.0.0"
description = "Controlled hook fixture."
license = "MIT"
[compatibility]
requires_saya = ">=0.2.0, <1.0.0"
requires_features = ["plugin-manifest-v1", "hook-events-v1"]
[capabilities]
requested = ["process.exec", "hook.observe", "hook.check"]
"#
        .to_string();
        for (id, event, kind) in [
            ("started", "run.started", "observer"),
            ("finished", "run.finished", "observer"),
            ("before", "tool.before_dispatch", "observer"),
            ("after", "tool.finished", "observer"),
            ("check", "tool.before_dispatch", "check"),
        ] {
            manifest.push_str(&format!("\n[[hooks]]\nid = {id:?}\nevent = {event:?}\nkind = {kind:?}\nprogram = \"python3\"\nargs = [\"@plugin/hooks/hook.py\"]\ntimeout_ms = 5\n"));
        }
        std::fs::create_dir(source.path().join("hooks")).unwrap();
        std::fs::write(source.path().join("hooks/hook.py"), "print('{}')\n").unwrap();
        std::fs::write(source.path().join(MANIFEST_NAME), manifest).unwrap();
        let package = inspect_package(source.path()).unwrap();
        let store = PluginStore::open(
            root.0.path().join("store"),
            Arc::new(SecretRegistry::default()),
            StoreLimits::default(),
        )
        .unwrap();
        store
            .install(
                &package,
                PackageSource {
                    kind: SourceKind::LocalDirectory,
                    reference: "./fixture".into(),
                },
            )
            .unwrap();
        let features = BTreeSet::from(["plugin-manifest-v1".into(), "hook-events-v1".into()]);
        store
            .enable(
                package.digest(),
                PluginScope::User,
                EnableGrant::requested(package.manifest()),
                "0.2.0",
                &features,
            )
            .unwrap();
        let run = Run {
            id: RunId::new(),
            session_id: SessionId::new(),
            work_mode: mode,
            permissions,
        };
        let lease = Arc::new(
            store
                .acquire(
                    "io.saya.hook-test",
                    root.0.path(),
                    run.session_id,
                    run.id,
                    "0.2.0",
                    &features,
                )
                .unwrap(),
        );
        let hooks = ["started", "finished", "before", "after", "check"]
            .iter()
            .map(|id| EnabledHook::from_lease(lease.clone(), id).unwrap())
            .collect();
        Self {
            hooks,
            run,
            _root: root,
        }
    }
    fn context(&self, event: HookEvent) -> HookContext {
        HookContext {
            event_id: "event-1".into(),
            root_event_id: "event-1".into(),
            event,
            run: self.run.clone(),
            parent_call_id: matches!(
                event,
                HookEvent::ToolBeforeDispatch | HookEvent::ToolFinished
            )
            .then(|| "parent-1".into()),
            depth: 0,
            ancestry: Vec::new(),
            data: json!({"observation":"host supplied data"}),
        }
    }
    fn observer(&self, event: HookEvent) -> Vec<EnabledHook> {
        self.hooks
            .iter()
            .filter(|hook| hook.event() == event && hook.kind() == HookKind::Observer)
            .cloned()
            .collect()
    }
    fn check(&self) -> Vec<EnabledHook> {
        vec![self.hooks[4].clone()]
    }
}

#[derive(Clone)]
enum Row {
    Running(HookIntent),
    Done(HookRecord),
    Unknown(HookIntent),
}
#[derive(Default)]
struct Journal {
    rows: Mutex<BTreeMap<String, Row>>,
    notes: Mutex<Vec<HookDiagnostic>>,
    fail_claim: AtomicBool,
    fail_finish: AtomicBool,
}
impl HookJournal for Journal {
    fn claim<'a>(&'a self, intent: &'a HookIntent) -> Operation<'a, HookClaim> {
        Box::pin(async move {
            if self.fail_claim.load(Ordering::SeqCst) {
                return Err(error("raw_host_error", "private adapter details"));
            }
            let mut rows = self.rows.lock().unwrap();
            if let Some(row) = rows.get(&intent.invocation_id) {
                let stored = match row {
                    Row::Running(stored) | Row::Unknown(stored) => stored,
                    Row::Done(record) => &record.intent,
                };
                if stored != intent {
                    return Ok(HookClaim::Conflict);
                }
                return Ok(match row {
                    Row::Running(_) => HookClaim::Running,
                    Row::Unknown(_) => HookClaim::Unknown,
                    Row::Done(record) => HookClaim::Completed(Box::new(record.clone())),
                });
            }
            let count = rows
                .values()
                .filter(|row| {
                    let stored = match row {
                        Row::Running(stored) | Row::Unknown(stored) => stored,
                        Row::Done(record) => &record.intent,
                    };
                    stored.run.id == intent.run.id
                })
                .count();
            if count >= intent.run_invocation_limit {
                return Ok(HookClaim::BudgetExhausted);
            }
            rows.insert(intent.invocation_id.clone(), Row::Running(intent.clone()));
            Ok(HookClaim::Execute)
        })
    }
    fn finish<'a>(&'a self, record: &'a HookRecord) -> Operation<'a, ()> {
        Box::pin(async {
            if self.fail_finish.load(Ordering::SeqCst) {
                return Err(error("raw_host_error", "private adapter details"));
            }
            let mut rows = self.rows.lock().unwrap();
            assert!(
                matches!(rows.get(&record.intent.invocation_id), Some(Row::Running(intent)) if intent == &record.intent)
            );
            rows.insert(
                record.intent.invocation_id.clone(),
                Row::Done(record.clone()),
            );
            Ok(())
        })
    }
    fn diagnostic<'a>(&'a self, note: &'a HookDiagnostic) -> Operation<'a, ()> {
        Box::pin(async {
            self.notes.lock().unwrap().push(note.clone());
            Ok(())
        })
    }
}

#[derive(Clone)]
struct Behavior {
    stdout: Vec<u8>,
    exit: Option<i32>,
    truncated: bool,
    begin_delay: Duration,
    wait_delay: Option<Duration>,
    cleanup_delay: Duration,
    cleanup: CleanupState,
    begin_unknown: bool,
    wait_error: bool,
    cleanup_error: bool,
}
impl Default for Behavior {
    fn default() -> Self {
        Self {
            stdout: b"{\"annotations\":[]}".to_vec(),
            exit: Some(0),
            truncated: false,
            begin_delay: Duration::ZERO,
            wait_delay: Some(Duration::ZERO),
            cleanup_delay: Duration::ZERO,
            cleanup: CleanupState::Cleaned,
            begin_unknown: false,
            wait_error: false,
            cleanup_error: false,
        }
    }
}
struct Executor {
    journal: Arc<Journal>,
    behavior: Behavior,
    calls: AtomicUsize,
    started: AtomicUsize,
    manual_approvals: AtomicUsize,
    cleaned: Arc<AtomicBool>,
    payloads: Mutex<Vec<Value>>,
}
impl Executor {
    fn new(journal: Arc<Journal>, behavior: Behavior) -> Self {
        Self {
            journal,
            behavior,
            calls: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            manual_approvals: AtomicUsize::new(0),
            cleaned: Arc::new(AtomicBool::new(false)),
            payloads: Mutex::new(Vec::new()),
        }
    }
}
struct Execution {
    behavior: Behavior,
    cleaned: Arc<AtomicBool>,
}
impl HookExecution for Execution {
    fn wait<'a>(&'a mut self) -> Operation<'a, HookProcessOutput> {
        Box::pin(async {
            match self.behavior.wait_delay {
                Some(delay) => tokio::time::sleep(delay).await,
                None => pending().await,
            }
            if self.behavior.wait_error {
                return Err(error("raw_process", "private process details"));
            }
            Ok(HookProcessOutput {
                exit_code: self.behavior.exit,
                stdout: self.behavior.stdout.clone(),
                truncated: self.behavior.truncated,
            })
        })
    }
    fn stop<'a>(&'a mut self, _: StopReason) -> Operation<'a, CleanupState> {
        Box::pin(async {
            tokio::time::sleep(self.behavior.cleanup_delay).await;
            if self.behavior.cleanup_error {
                return Err(error("raw_process", "private cleanup details"));
            }
            if self.behavior.cleanup == CleanupState::Cleaned {
                self.cleaned.store(true, Ordering::SeqCst);
            }
            Ok(self.behavior.cleanup)
        })
    }
}
impl HookHostExecutor for Executor {
    fn begin<'a>(
        &'a self,
        request: &'a HookExecutionRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, HookBegin> {
        Box::pin(async {
            assert!(matches!(
                self.journal
                    .rows
                    .lock()
                    .unwrap()
                    .get(&request.intent.invocation_id),
                Some(Row::Running(_))
            ));
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.payloads.lock().unwrap().push(request.payload.clone());
            assert_eq!(request.next_depth, request.intent.depth + 1);
            assert_eq!(
                request.ancestry.last().unwrap().invocation_id,
                request.intent.invocation_id
            );
            request.hook.lease().check_active()?;
            request.hook.lease().verify_contents()?;
            if request.intent.run.work_mode == WorkMode::Plan {
                return Ok(HookBegin::NotStarted { denied: true });
            }
            if request.intent.run.permissions == PermissionMode::Manual {
                self.manual_approvals.fetch_add(1, Ordering::SeqCst);
            }
            tokio::select! {
                _ = cancel.cancelled() => return Ok(HookBegin::NotStarted { denied: false }),
                _ = tokio::time::sleep(self.behavior.begin_delay) => {}
            }
            if self.behavior.begin_unknown {
                return Ok(HookBegin::Unknown);
            }
            self.started.fetch_add(1, Ordering::SeqCst);
            Ok(HookBegin::Started(Box::new(Execution {
                behavior: self.behavior.clone(),
                cleaned: self.cleaned.clone(),
            })))
        })
    }
}
fn engine(
    fixture: &Fixture,
    behavior: Behavior,
    limits: HookLimits,
    secrets: Arc<SecretRegistry>,
    journal: Arc<Journal>,
) -> (HookEngine, Arc<Executor>) {
    let executor = Arc::new(Executor::new(journal.clone(), behavior));
    let engine = HookEngine::new(
        fixture.run.clone(),
        limits,
        secrets,
        executor.clone(),
        journal,
    )
    .unwrap();
    (engine, executor)
}
fn ordinary(fixture: &Fixture, behavior: Behavior) -> (HookEngine, Arc<Executor>, Arc<Journal>) {
    let journal = Arc::new(Journal::default());
    let (engine, executor) = engine(
        fixture,
        behavior,
        HookLimits::default(),
        Arc::new(SecretRegistry::default()),
        journal.clone(),
    );
    (engine, executor, journal)
}

#[tokio::test]
async fn all_four_events_persist_intent_and_cleanup_result_before_return() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let (engine, executor, journal) = ordinary(&fixture, Behavior::default());
    for event in [
        HookEvent::RunStarted,
        HookEvent::RunFinished,
        HookEvent::ToolBeforeDispatch,
        HookEvent::ToolFinished,
    ] {
        let outcome = engine
            .fire(
                &fixture.context(event),
                &fixture.observer(event),
                &NeverCancel,
            )
            .await
            .unwrap();
        assert!(!outcome.blocked);
        assert_eq!(outcome.records[0].status, HookStatus::Succeeded);
        assert!(executor.cleaned.load(Ordering::SeqCst));
        assert!(matches!(
            journal
                .rows
                .lock()
                .unwrap()
                .get(&outcome.records[0].intent.invocation_id),
            Some(Row::Done(_))
        ));
    }
    assert_eq!(executor.started.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn checks_only_veto_and_observers_cannot_supply_a_decision() {
    let fixture = Fixture::new(WorkMode::Build, PermissionMode::Bypass);
    for (decision, blocked) in [("pass", false), ("abstain", false), ("deny", true)] {
        let (engine, _, _) = ordinary(
            &fixture,
            Behavior {
                stdout: format!("{{\"decision\":\"{decision}\"}}").into_bytes(),
                ..Behavior::default()
            },
        );
        let outcome = engine
            .fire(
                &fixture.context(HookEvent::ToolBeforeDispatch),
                &fixture.check(),
                &NeverCancel,
            )
            .await
            .unwrap();
        assert_eq!(outcome.blocked, blocked);
        assert_eq!(
            outcome.records[0].decision,
            Some(match decision {
                "pass" => HookDecision::Pass,
                "abstain" => HookDecision::Abstain,
                _ => HookDecision::Deny,
            })
        );
    }
    let (engine, _, _) = ordinary(
        &fixture,
        Behavior {
            stdout: b"{\"decision\":\"pass\"}".to_vec(),
            ..Behavior::default()
        },
    );
    let result = engine
        .fire(
            &fixture.context(HookEvent::RunStarted),
            &fixture.observer(HookEvent::RunStarted),
            &NeverCancel,
        )
        .await
        .unwrap();
    assert!(!result.blocked);
    assert_eq!(result.records[0].status, HookStatus::Failed);
    assert_eq!(result.diagnostics.len(), 1);
}

#[tokio::test]
async fn malformed_truncated_nonzero_and_control_output_fail_closed_for_checks() {
    let fixture = Fixture::new(WorkMode::Code, PermissionMode::Auto);
    for behavior in [
        Behavior { stdout: b"{\"decision\":\"pass\",\"decision\":\"deny\"}".to_vec(), ..Behavior::default() },
        Behavior { stdout: b"{\"decision\":\"pass\",\"grant\":true}".to_vec(), ..Behavior::default() },
        Behavior { stdout: b"{\"decision\":\"pass\",\"annotations\":[{\"level\":\"info\",\"message\":\"a\\u001bb\"}]}".to_vec(), ..Behavior::default() },
        Behavior { stdout: b"{\"decision\":\"pass\"}".to_vec(), truncated: true, ..Behavior::default() },
        Behavior { stdout: b"{\"decision\":\"pass\"}".to_vec(), exit: Some(1), ..Behavior::default() },
        Behavior { stdout: vec![b'x'; 20 * 1024], ..Behavior::default() },
        Behavior { wait_error: true, ..Behavior::default() },
    ] {
        let (engine, _, _) = ordinary(&fixture, behavior);
        let outcome = engine.fire(&fixture.context(HookEvent::ToolBeforeDispatch), &fixture.check(), &NeverCancel).await.unwrap();
        assert!(outcome.blocked);
        assert_eq!(outcome.records[0].status, HookStatus::Failed);
        assert_eq!(outcome.records[0].cleanup, CleanupState::Cleaned);
        assert!(outcome.records[0].annotations.is_empty());
    }
}

#[tokio::test]
async fn current_plan_policy_and_each_manual_operation_reach_the_mandatory_host() {
    let plan = Fixture::new(WorkMode::Plan, PermissionMode::Bypass);
    let (engine, executor, _) = ordinary(&plan, Behavior::default());
    let outcome = engine
        .fire(
            &plan.context(HookEvent::ToolBeforeDispatch),
            &plan.check(),
            &NeverCancel,
        )
        .await
        .unwrap();
    assert!(outcome.blocked);
    assert_eq!(executor.started.load(Ordering::SeqCst), 0);
    assert_eq!(outcome.records[0].execution, HookExecutionState::NotStarted);
    let manual = Fixture::new(WorkMode::Work, PermissionMode::Manual);
    let (engine, executor, _) = ordinary(&manual, Behavior::default());
    for event in [HookEvent::RunStarted, HookEvent::RunFinished] {
        engine
            .fire(
                &manual.context(event),
                &manual.observer(event),
                &NeverCancel,
            )
            .await
            .unwrap();
    }
    assert_eq!(executor.manual_approvals.load(Ordering::SeqCst), 2);
}

#[tokio::test(start_paused = true)]
async fn execution_timeout_starts_after_begin_and_waits_for_real_cleanup() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let (engine, _, _) = ordinary(
        &fixture,
        Behavior {
            begin_delay: Duration::from_millis(20),
            ..Behavior::default()
        },
    );
    let outcome = engine
        .fire(
            &fixture.context(HookEvent::RunStarted),
            &fixture.observer(HookEvent::RunStarted),
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(outcome.records[0].status, HookStatus::Succeeded);
    let (engine, executor, journal) = ordinary(
        &fixture,
        Behavior {
            wait_delay: None,
            cleanup_delay: Duration::from_millis(10),
            ..Behavior::default()
        },
    );
    let start = tokio::time::Instant::now();
    let outcome = engine
        .fire(
            &fixture.context(HookEvent::ToolBeforeDispatch),
            &fixture.check(),
            &NeverCancel,
        )
        .await
        .unwrap();
    assert!(start.elapsed() >= Duration::from_millis(15));
    assert!(executor.cleaned.load(Ordering::SeqCst));
    assert_eq!(outcome.records[0].status, HookStatus::TimedOut);
    assert!(outcome.blocked);
    assert!(matches!(
        journal.rows.lock().unwrap().values().next(),
        Some(Row::Done(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn cancellation_during_begin_or_wait_never_claims_unconfirmed_cleanup() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let (engine, executor, _) = ordinary(
        &fixture,
        Behavior {
            begin_delay: Duration::from_secs(1),
            ..Behavior::default()
        },
    );
    let outcome = engine
        .fire(
            &fixture.context(HookEvent::RunStarted),
            &fixture.observer(HookEvent::RunStarted),
            &SoonCancel(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(outcome.cancelled);
    assert_eq!(outcome.records[0].execution, HookExecutionState::NotStarted);
    assert_eq!(executor.started.load(Ordering::SeqCst), 0);
    let (engine, executor, _) = ordinary(
        &fixture,
        Behavior {
            wait_delay: None,
            cleanup_delay: Duration::from_millis(10),
            ..Behavior::default()
        },
    );
    let start = tokio::time::Instant::now();
    let outcome = engine
        .fire(
            &fixture.context(HookEvent::RunStarted),
            &fixture.observer(HookEvent::RunStarted),
            &SoonCancel(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    assert!(outcome.cancelled);
    assert!(start.elapsed() >= Duration::from_millis(11));
    assert!(executor.cleaned.load(Ordering::SeqCst));
    assert_eq!(outcome.records[0].status, HookStatus::Cancelled);
}

#[tokio::test(start_paused = true)]
async fn unknown_start_or_cleanup_blocks_even_an_observer_and_remains_unknown() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    for behavior in [
        Behavior {
            begin_unknown: true,
            ..Behavior::default()
        },
        Behavior {
            cleanup: CleanupState::Unknown,
            ..Behavior::default()
        },
        Behavior {
            cleanup_error: true,
            ..Behavior::default()
        },
        Behavior {
            cleanup_delay: Duration::from_secs(10),
            ..Behavior::default()
        },
    ] {
        let (engine, executor, _) = ordinary(&fixture, behavior);
        let outcome = engine
            .fire(
                &fixture.context(HookEvent::RunStarted),
                &fixture.observer(HookEvent::RunStarted),
                &NeverCancel,
            )
            .await
            .unwrap();
        assert!(outcome.blocked && outcome.cleanup_pending);
        assert_eq!(outcome.records[0].status, HookStatus::Unknown);
        assert_eq!(outcome.records[0].cleanup, CleanupState::Unknown);
        assert!(!executor.cleaned.load(Ordering::SeqCst));
        let repeated = engine
            .fire(
                &fixture.context(HookEvent::RunStarted),
                &fixture.observer(HookEvent::RunStarted),
                &NeverCancel,
            )
            .await
            .unwrap();
        assert!(repeated.blocked && repeated.cleanup_pending);
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn duplicate_and_restarted_events_only_reuse_matching_durable_results() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let (first, executor, journal) = ordinary(&fixture, Behavior::default());
    let context = fixture.context(HookEvent::RunStarted);
    let hooks = fixture.observer(HookEvent::RunStarted);
    let result = first.fire(&context, &hooks, &NeverCancel).await.unwrap();
    let repeated = first.fire(&context, &hooks, &NeverCancel).await.unwrap();
    assert_eq!(result.records, repeated.records);
    assert_eq!(executor.started.load(Ordering::SeqCst), 1);
    let (restarted, executor) = engine(
        &fixture,
        Behavior::default(),
        HookLimits::default(),
        Arc::new(SecretRegistry::default()),
        journal.clone(),
    );
    assert_eq!(
        restarted
            .fire(&context, &hooks, &NeverCancel)
            .await
            .unwrap()
            .records,
        result.records
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    let mut changed = context.clone();
    changed.data = json!({"changed":true});
    assert!(
        restarted
            .fire(&changed, &hooks, &NeverCancel)
            .await
            .unwrap()
            .blocked
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    for unknown in [false, true] {
        let intent = result.records[0].intent.clone();
        journal.rows.lock().unwrap().insert(
            intent.invocation_id.clone(),
            if unknown {
                Row::Unknown(intent)
            } else {
                Row::Running(intent)
            },
        );
        assert!(
            restarted
                .fire(&context, &hooks, &NeverCancel)
                .await
                .unwrap()
                .cleanup_pending
        );
        assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn failures_to_save_intent_prevent_start_and_result_failure_retains_the_outcome() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let (engine, executor, journal) = ordinary(&fixture, Behavior::default());
    journal.fail_claim.store(true, Ordering::SeqCst);
    let context = fixture.context(HookEvent::RunStarted);
    let hooks = fixture.observer(HookEvent::RunStarted);
    assert!(engine.fire(&context, &hooks, &NeverCancel).await.is_err());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    journal.fail_claim.store(false, Ordering::SeqCst);
    journal.fail_finish.store(true, Ordering::SeqCst);
    let failure = engine
        .fire(&context, &hooks, &NeverCancel)
        .await
        .unwrap_err();
    assert!(executor.cleaned.load(Ordering::SeqCst));
    assert!(
        failure
            .2
            .as_ref()
            .unwrap()
            .data
            .as_ref()
            .unwrap()
            .get("hook_record")
            .is_some()
    );
    assert!(!format!("{failure:?}").contains("private adapter details"));
    journal.fail_finish.store(false, Ordering::SeqCst);
    let result = engine.fire(&context, &hooks, &NeverCancel).await.unwrap();
    assert!(result.blocked && result.cleanup_pending);
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn known_secrets_are_redacted_from_data_annotations_and_durable_results() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let secret = "fixture-credential-value";
    let secrets = Arc::new(SecretRegistry::default());
    secrets.register(&[secret.into()]).unwrap();
    let journal = Arc::new(Journal::default());
    let (engine, executor) = engine(
        &fixture,
        Behavior {
            stdout: format!(
                "{{\"annotations\":[{{\"level\":\"info\",\"message\":\"token={secret}\"}}]}}"
            )
            .into_bytes(),
            ..Behavior::default()
        },
        HookLimits::default(),
        secrets,
        journal.clone(),
    );
    let mut context = fixture.context(HookEvent::RunStarted);
    context.data = json!({"nested":format!("{{\"token\":\"{secret}\"}}")});
    let result = engine
        .fire(
            &context,
            &fixture.observer(HookEvent::RunStarted),
            &NeverCancel,
        )
        .await
        .unwrap();
    assert_eq!(result.records[0].status, HookStatus::Succeeded);
    assert!(
        !serde_json::to_string(&result.records)
            .unwrap()
            .contains(secret)
    );
    assert!(
        !serde_json::to_string(&*executor.payloads.lock().unwrap())
            .unwrap()
            .contains(secret)
    );
    assert!(!format!("{:?}", journal.notes.lock().unwrap()).contains(secret));
    let mut invalid = context.clone();
    invalid.event_id = secret.into();
    invalid.root_event_id = secret.into();
    assert!(
        engine
            .fire(
                &invalid,
                &fixture.observer(HookEvent::RunStarted),
                &NeverCancel
            )
            .await
            .is_err()
    );
    assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn durable_run_budget_survives_engine_recreation_and_recursion_is_suppressed() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let limits = HookLimits {
        max_invocations_per_run: 1,
        ..HookLimits::default()
    };
    let journal = Arc::new(Journal::default());
    let (first, _) = engine(
        &fixture,
        Behavior::default(),
        limits,
        Arc::new(SecretRegistry::default()),
        journal.clone(),
    );
    let context = fixture.context(HookEvent::RunStarted);
    let hooks = fixture.observer(HookEvent::RunStarted);
    first.fire(&context, &hooks, &NeverCancel).await.unwrap();
    let (restarted, executor) = engine(
        &fixture,
        Behavior::default(),
        limits,
        Arc::new(SecretRegistry::default()),
        journal,
    );
    let mut next = context.clone();
    next.event_id = "event-2".into();
    next.root_event_id = "event-2".into();
    let result = restarted.fire(&next, &hooks, &NeverCancel).await.unwrap();
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.diagnostics[0].code, "hook_budget");
    let mut recursive = context;
    recursive.depth = 1;
    recursive.event_id = "nested-event".into();
    recursive.ancestry.push(HookAncestor {
        hook_key: hooks[0].key(),
        invocation_id: "a".repeat(64),
    });
    let result = restarted
        .fire(&recursive, &hooks, &NeverCancel)
        .await
        .unwrap();
    assert_eq!(result.diagnostics[0].code, "hook_recursion");
    assert!(result.records.is_empty());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn host_identity_limits_and_duplicate_entries_are_validated_before_journaling() {
    let fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto);
    let (engine, executor, journal) = ordinary(&fixture, Behavior::default());
    let mut context = fixture.context(HookEvent::RunStarted);
    let hooks = fixture.observer(HookEvent::RunStarted);
    assert!(
        engine
            .fire(
                &context,
                &[hooks[0].clone(), hooks[0].clone()],
                &NeverCancel
            )
            .await
            .is_err()
    );
    context.parent_call_id = Some("illegal-parent".into());
    assert!(engine.fire(&context, &hooks, &NeverCancel).await.is_err());
    context = fixture.context(HookEvent::RunStarted);
    context.run.id = RunId::new();
    assert!(engine.fire(&context, &hooks, &NeverCancel).await.is_err());
    context = fixture.context(HookEvent::RunStarted);
    context.data = json!("a".repeat(64 * 1024));
    assert!(engine.fire(&context, &hooks, &NeverCancel).await.is_err());
    context.data = json!(0);
    for _ in 0..26 {
        context.data = json!([context.data]);
    }
    assert!(engine.fire(&context, &hooks, &NeverCancel).await.is_err());
    assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
    assert!(journal.rows.lock().unwrap().is_empty());
    assert!(EnabledHook::from_lease(fixture.hooks[0].lease.clone(), "absent").is_err());
    assert!(
        HookEngine::new(
            fixture.run.clone(),
            HookLimits {
                max_depth: 0,
                ..HookLimits::default()
            },
            Arc::new(SecretRegistry::default()),
            executor,
            journal
        )
        .is_err()
    );
}
