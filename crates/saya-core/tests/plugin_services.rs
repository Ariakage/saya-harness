//! Explicit native acceptance. Run with a verified candidate CLI pinned through
//! SAYA_TEST_EXECUTABLE. Ordinary cargo test never builds a hidden CLI image.
use saya_core::{
    approvals::Preauthorizations,
    boundary::{
        Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation, Store, StoreChange,
    },
    config::Environment,
    context_builder::ContextBudget,
    contract::{
        ApprovalRequest, HistoryItem, ModelProtocol, PermissionMode, Run, RunId, RunStatus,
        Session, SessionId, ToolCall, ToolState, WorkMode,
    },
    credentials::SecretRegistry,
    execution::{self, SandboxPolicy},
    hook_store::{HookStore, HookStoreLimits},
    hooks::{CleanupState, HookStatus},
    mcp::config::McpSettings,
    memory::MemoryStore,
    permissions::{PermissionConfig, Policy},
    plugin_manifest::{MANIFEST_NAME, PackageSource, SourceKind, inspect_package},
    plugin_runtime::{ApprovedRuntime, PluginHostBindings, ViewLimits, ViewPool},
    plugin_store::{EnableGrant, InstanceState, PluginScope, PluginStore, StoreLimits},
    project_context::ProjectContext,
    review::ReviewConfig,
    runtime::{AgentRuntime, ApprovalHandler, RuntimeLimits, RuntimeSetup},
    skills::{SkillLimits, SkillManager},
    tools::{
        exec::ExecConfig,
        read::{ReadLimits, ReadTools},
    },
};
use std::{
    collections::BTreeSet,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

const PLUGIN: &str = "io.saya.native-services";
const SERVER: &str = r#"#!/bin/sh
printf '%s' "$$" > "$HOME/pid"
case "$UNDECLARED_VALUE" in '') ;; *) exit 86 ;; esac
/bin/cat "$1" >/dev/null 2>&1 && exit 87
printf 'escaped' > "$2" 2>/dev/null && exit 88
while IFS= read -r line; do
    id=$(printf '%s\n' "$line" | /usr/bin/sed -n 's/.*"id":[ ]*\([^,}]*\).*/\1/p')
    [ -n "$id" ] || continue
    case "$line" in
      *'server/discover'*) result='{"resultType":"complete","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}},"ttlMs":0,"cacheScope":"private"}' ;;
      *'tools/list'*) result='{"resultType":"complete","tools":[{"name":"echo","description":"Native plugin echo","inputSchema":{"type":"object","properties":{}}}],"ttlMs":0,"cacheScope":"private"}' ;;
      *'tools/call'*) printf 'called' > "$HOME/called"; result='{"resultType":"complete","content":[{"type":"text","text":"native plugin response"}],"isError":false}' ;;
      *) continue ;;
    esac
    printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$result"
done
"#;
const OBSERVER: &str = r#"#!/bin/sh
IFS= read -r event || :
printf 'observed' > "$HOME/hook-ran"
printf '{"annotations":[{"level":"info","message":"native hook observed"}]}'
"#;
const CHECK_DENY: &str = r#"#!/bin/sh
IFS= read -r event || :
printf '{"decision":"deny","annotations":[{"level":"warning","message":"fixture veto"}]}'
"#;

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
#[derive(Default)]
struct RecordedStore {
    inner: MemoryStore,
    changes: Mutex<Vec<StoreChange>>,
}
impl Store for RecordedStore {
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
struct Finished(Option<ModelUpdate>);
impl ModelStream for Finished {
    fn next<'a>(&'a mut self, _: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>> {
        Box::pin(async move { Ok(self.0.take()) })
    }
}
struct FixtureModel {
    requests: Mutex<Vec<ModelRequest>>,
    calls: AtomicUsize,
    use_tool: bool,
    disable: Option<PluginStore>,
    pool: Arc<ViewPool>,
    observed_pids: Mutex<Vec<i32>>,
    observed_remote_call: Mutex<bool>,
}
impl Model for FixtureModel {
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
            for view in self.pool.inventory()? {
                if let Ok(pid) = std::fs::read_to_string(view.identity.scratch.join("pid"))
                    && let Ok(pid) = pid.parse()
                {
                    self.observed_pids.lock().unwrap().push(pid);
                }
                if view.identity.scratch.join("called").exists() {
                    *self.observed_remote_call.lock().unwrap() = true;
                }
            }
            let item = if first && self.use_tool {
                if let Some(store) = &self.disable {
                    store.disable(PLUGIN, None)?;
                }
                let tool = request
                    .tools
                    .iter()
                    .find(|tool| tool.name.starts_with("mcp__"));
                match tool {
                    Some(tool) => HistoryItem::ToolCall(ToolCall {
                        call_id: "native-remote-call".into(),
                        name: tool.name.clone(),
                        arguments: "{}".into(),
                    }),
                    None => HistoryItem::AssistantText("No remote tool is available".into()),
                }
            } else {
                HistoryItem::AssistantText("native acceptance completed".into())
            };
            Ok(Box::new(Finished(Some(ModelUpdate::Finished {
                items: vec![item],
                usage: None,
            }))) as Box<dyn ModelStream>)
        })
    }
}
struct Approve(AtomicUsize);
impl ApprovalHandler for Approve {
    fn confirm<'a>(
        &'a self,
        _: &'a ApprovalRequest,
        _: &'a dyn Cancellation,
    ) -> Operation<'a, Option<bool>> {
        Box::pin(async move {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Some(true))
        })
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    workspace: PathBuf,
    store: PluginStore,
    pool: Arc<ViewPool>,
    journal: Arc<HookStore>,
    secrets: Arc<SecretRegistry>,
    bindings: PluginHostBindings,
}
impl Fixture {
    fn new(check: bool, skill_only: bool) -> Self {
        let image = std::env::var_os("SAYA_TEST_EXECUTABLE")
            .expect("explicit native acceptance requires SAYA_TEST_EXECUTABLE");
        execution::pin_launcher_image_from(Path::new(&image))
            .expect("pin the exact verified candidate CLI");
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = std::fs::canonicalize(workspace).unwrap();
        std::fs::write(
            workspace.join("secret.txt"),
            "workspace read must be denied to this plugin",
        )
        .unwrap();
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
        std::fs::create_dir(&source).unwrap();
        let mut text = format!(
            r#"schema_version=1
[plugin]
id="{PLUGIN}"
version="1.0.0"
description="Explicit native acceptance fixture."
license="MIT"
[compatibility]
requires_saya=">=0.2.0, <1.0.0"
requires_features=["plugin-manifest-v1",{}]
[capabilities]
requested=[{}]
"#,
            if skill_only {
                "\"skill-directory-v1\""
            } else {
                "\"mcp-stdio-v1\",\"hook-events-v1\""
            },
            if skill_only {
                "\"workspace.read\""
            } else if check {
                "\"mcp.stdio\",\"process.exec\",\"hook.observe\",\"hook.check\""
            } else {
                "\"mcp.stdio\",\"process.exec\",\"hook.observe\""
            }
        );
        if skill_only {
            std::fs::create_dir(source.join("guide")).unwrap();
            std::fs::write(source.join("guide/SKILL.md"),"---\nname: guide\ndescription: A native plugin catalog fixture.\n---\nInactive private Skill body must not be injected.\n").unwrap();
            text.push_str("[[skills]]\nid=\"guide\"\npath=\"guide\"\n");
        } else {
            std::fs::write(source.join("server.sh"), SERVER).unwrap();
            std::fs::write(source.join("observer.sh"), OBSERVER).unwrap();
            std::fs::write(source.join("check.sh"), CHECK_DENY).unwrap();
            let arguments = vec![
                "@plugin/server.sh".to_string(),
                workspace.join("secret.txt").to_string_lossy().into_owned(),
                workspace.join("escaped.txt").to_string_lossy().into_owned(),
            ];
            text.push_str(&format!(
                "[[mcp]]\nid=\"local\"\ntransport=\"stdio\"\nprogram=\"shell\"\nargs={}\n",
                serde_json::to_string(&arguments).unwrap()
            ));
            for (id, event) in [("started", "run.started"), ("finished", "run.finished")] {
                text.push_str(&format!("[[hooks]]\nid=\"{id}\"\nevent=\"{event}\"\nkind=\"observer\"\nprogram=\"shell\"\nargs=[\"@plugin/observer.sh\"]\ntimeout_ms=5000\n"));
            }
            if check {
                text.push_str("[[hooks]]\nid=\"veto\"\nevent=\"tool.before_dispatch\"\nkind=\"check\"\nprogram=\"shell\"\nargs=[\"@plugin/check.sh\"]\ntimeout_ms=5000\n");
            }
        }
        std::fs::write(source.join(MANIFEST_NAME), text).unwrap();
        let package = inspect_package(&source).unwrap();
        store
            .install(
                &package,
                PackageSource {
                    kind: SourceKind::LocalDirectory,
                    reference: "./native-fixture".into(),
                },
            )
            .unwrap();
        let features = BTreeSet::from([
            "plugin-manifest-v1".into(),
            "mcp-stdio-v1".into(),
            "hook-events-v1".into(),
            "skill-directory-v1".into(),
        ]);
        store
            .enable(
                package.digest(),
                PluginScope::workspace(&workspace).unwrap(),
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
            workspace,
            store,
            pool,
            journal,
            secrets,
            bindings,
        }
    }
    fn runtime(
        &self,
        mode: WorkMode,
        permissions: PermissionMode,
        check: bool,
        disable: bool,
    ) -> (
        AgentRuntime,
        Session,
        Arc<FixtureModel>,
        Arc<RecordedStore>,
        Arc<Approve>,
    ) {
        let session = Session {
            id: SessionId::new(),
            workspace: self.workspace.clone(),
            work_mode: mode,
            profile: "fixture".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fixture".into(),
            history: vec![HistoryItem::UserText(
                "Exercise the installed fixture".into(),
            )],
        };
        let model = Arc::new(FixtureModel {
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            use_tool: check,
            disable: disable.then(|| self.store.clone()),
            pool: self.pool.clone(),
            observed_pids: Mutex::new(Vec::new()),
            observed_remote_call: Mutex::new(false),
        });
        let store = Arc::new(RecordedStore::default());
        let approval = Arc::new(Approve(AtomicUsize::new(0)));
        let environment = Environment::from_pairs([(
            "UNDECLARED_VALUE".into(),
            "must-never-be-inherited".into(),
        )]);
        let runtime = AgentRuntime::new(RuntimeSetup {
            run: Run {
                id: RunId::new(),
                session_id: session.id,
                work_mode: mode,
                permissions,
            },
            model: model.clone(),
            store: store.clone(),
            policy: Arc::new(Policy::new(&self.workspace, PermissionConfig::default()).unwrap()),
            files: Arc::new(
                ReadTools::new(&self.workspace, &[], false, ReadLimits::default()).unwrap(),
            ),
            cancellation: Arc::new(Never),
            review: ReviewConfig::new("fixture".into()),
            exec: ExecConfig {
                environment: environment.clone(),
                ..ExecConfig::default()
            },
            preauthorizations: Preauthorizations::default(),
            known_secrets: Vec::new(),
            summary: "native plugin fixture".into(),
            confirmation: Some(approval.clone()),
            limits: RuntimeLimits::default(),
        })
        .unwrap()
        .with_execution_settings(SandboxPolicy::workspace(self.workspace.clone()), None)
        .unwrap()
        .with_mcp_settings(
            McpSettings::default(),
            environment,
            None,
            self.secrets.clone(),
        )
        .unwrap()
        .with_context_budget(ContextBudget::new(128_000, 4096, 4096))
        .unwrap()
        .with_project_context(ProjectContext::new(&self.workspace, None).unwrap())
        .unwrap()
        .with_skill_manager(
            SkillManager::new(
                &self.workspace,
                None,
                self.secrets.clone(),
                SkillLimits::default(),
            )
            .unwrap(),
        )
        .unwrap()
        .with_plugins(
            self.store.clone(),
            self.pool.clone(),
            self.bindings.clone(),
            self.journal.clone(),
        )
        .unwrap();
        (runtime, session, model, store, approval)
    }
    fn assert_clean(&self) {
        assert!(!self.workspace.join("escaped.txt").exists());
        assert!(self.pool.inventory().unwrap().is_empty());
        assert!(self.store.recover().unwrap().is_empty());
        assert!(
            self.store
                .list()
                .unwrap()
                .iter()
                .flat_map(|package| &package.instances)
                .all(|lease| lease.state == InstanceState::Cleaned)
        );
    }
}

#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn native_run_calls_plugin_mcp_and_hooks_and_releases_all_views() {
    let fixture = Fixture::new(false, false);
    let (mut runtime, mut session, model, store, _) =
        fixture.runtime(WorkMode::Work, PermissionMode::Bypass, true, false);
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    assert!(*model.observed_remote_call.lock().unwrap());
    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult {call_id,result} if call_id=="native-remote-call" && result.ok)));
    let hooks = store
        .changes
        .lock()
        .unwrap()
        .iter()
        .filter_map(|change| match change {
            StoreChange::ToolIntent { call, .. } => {
                call.call_id.strip_prefix("hook-exec-").map(str::to_owned)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(hooks.len(), 2);
    for id in hooks {
        let record = fixture
            .journal
            .inspect(&id)
            .await
            .unwrap()
            .unwrap()
            .record
            .unwrap();
        assert_eq!(record.status, HookStatus::Succeeded);
        assert_eq!(record.cleanup, CleanupState::Cleaned);
    }
    assert!(!model.observed_pids.lock().unwrap().is_empty());
    for pid in model.observed_pids.lock().unwrap().iter() {
        assert_eq!(unsafe { libc::kill(*pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
    fixture.assert_clean();
}
#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn manual_approval_covers_plugin_startup_remote_call_and_each_hook() {
    let fixture = Fixture::new(false, false);
    let (mut runtime, mut session, _, _, approvals) =
        fixture.runtime(WorkMode::Work, PermissionMode::Manual, true, false);
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    assert_eq!(approvals.0.load(Ordering::SeqCst), 4);
    fixture.assert_clean();
}
#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn plan_never_starts_plugin_processes_even_with_bypass_permissions() {
    let fixture = Fixture::new(false, false);
    let (mut runtime, mut session, model, store, _) =
        fixture.runtime(WorkMode::Plan, PermissionMode::Bypass, false, false);
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    assert!(model.observed_pids.lock().unwrap().is_empty());
    assert!(
        !store
            .changes
            .lock()
            .unwrap()
            .iter()
            .any(|change| matches!(change, StoreChange::ToolIntent { .. }))
    );
    fixture.assert_clean();
}
#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn hook_check_veto_does_not_dispatch_the_parent_remote_operation() {
    let fixture = Fixture::new(true, false);
    let (mut runtime, mut session, model, _, _) =
        fixture.runtime(WorkMode::Work, PermissionMode::Bypass, true, false);
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    assert!(!*model.observed_remote_call.lock().unwrap());
    assert!(session.history.iter().any(|item|matches!(item,HistoryItem::ToolResult {call_id,result} if call_id=="native-remote-call" && !result.ok)));
    fixture.assert_clean();
}
#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn disable_after_model_catalog_prevents_call_and_cleans_real_process() {
    let fixture = Fixture::new(false, false);
    let (mut runtime, mut session, model, store, _) =
        fixture.runtime(WorkMode::Work, PermissionMode::Bypass, true, true);
    let _ = runtime.run(&mut session).await.unwrap();
    assert!(!*model.observed_remote_call.lock().unwrap());
    assert!(!store.changes.lock().unwrap().iter().any(|change|matches!(change,StoreChange::ToolState {call_id,state:ToolState::Succeeded,..} if call_id=="native-remote-call")));
    fixture.assert_clean();
}
#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn skill_only_plugin_is_discovered_without_injecting_its_inactive_body() {
    let fixture = Fixture::new(false, true);
    let (mut runtime, mut session, model, _, _) =
        fixture.runtime(WorkMode::Work, PermissionMode::Bypass, false, false);
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Completed, "{result:?}");
    let requests = model.requests.lock().unwrap();
    let first = requests.first().unwrap();
    let encoded = format!(
        "{}{}",
        first.combined_instructions(),
        serde_json::to_string(&first.history).unwrap()
    );
    assert!(encoded.contains(PLUGIN));
    assert!(!encoded.contains("Inactive private Skill body must not be injected"));
    fixture.assert_clean();
}

#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn aggregate_resource_policy_fails_before_plugin_intent_or_process_dispatch() {
    let fixture = Fixture::new(false, false);
    let (runtime, mut session, model, store, _) =
        fixture.runtime(WorkMode::Work, PermissionMode::Bypass, true, false);
    let mut policy = SandboxPolicy::workspace(fixture.workspace.clone());
    policy.resources.total_processes = Some(8);
    policy.cgroup_root = std::env::var_os("SAYA_TEST_CGROUP_ROOT").map(PathBuf::from);
    let mut runtime = runtime.with_execution_settings(policy, None).unwrap();
    let result = runtime.run(&mut session).await.unwrap();
    assert_eq!(result.status, RunStatus::Failed, "{result:?}");
    assert!(matches!(
        result.error.as_ref().unwrap().code.as_str(),
        "unsupported_resource_policy" | "sandbox_capability_unavailable"
    ));
    assert!(model.requests.lock().unwrap().is_empty());
    assert!(
        !store
            .changes
            .lock()
            .unwrap()
            .iter()
            .any(|change| matches!(change, StoreChange::ToolIntent { .. }))
    );
    fixture.assert_clean();
}

#[tokio::test]
#[ignore = "requires a verified SAYA_TEST_EXECUTABLE and mandatory native backend"]
async fn aborted_initial_preparation_is_joined_before_shutdown_can_report_clean_resources() {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};
    use std::time::Duration;

    let fixture = Fixture::new(false, false);
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(fixture.pool.path().join(".lock"))
        .unwrap();
    // SAFETY: flock only operates on this owned private regular-file FD.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let (mut runtime, mut session, model, store, _) =
        fixture.runtime(WorkMode::Work, PermissionMode::Bypass, false, false);
    let session_id = session.id;
    let handle = runtime.handle();
    let running = tokio::spawn(async move { runtime.run(&mut session).await });
    // The lease is acquired before package materialization blocks on our lock.
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if fixture.store.list().unwrap().iter().any(|package| {
                package.instances.iter().any(|lease| {
                    lease.session_id == session_id && lease.state == InstanceState::Running
                })
            }) {
                break;
            }
            assert!(
                !running.is_finished(),
                "Runtime stopped before reaching the controlled package-view barrier"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Initial preparation must reach the controlled view lock before its deadline");
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    let cleanup = tokio::spawn(async move { handle.shutdown_background().await });
    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !cleanup.is_finished(),
        "Shutdown reported completion while initial preparation was still blocked"
    );
    // SAFETY: release our own advisory lock. No package or process is modified.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) }, 0);
    drop(lock);
    tokio::time::timeout(Duration::from_secs(15), cleanup)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(model.requests.lock().unwrap().is_empty());
    assert!(
        !store
            .changes
            .lock()
            .unwrap()
            .iter()
            .any(|change| matches!(change, StoreChange::ToolIntent { .. }))
    );
    fixture.assert_clean();
    // A detached prepare worker used to publish views after cleanup returned.
    tokio::time::sleep(Duration::from_millis(60)).await;
    fixture.assert_clean();
}
