use super::*;
use crate::{
    approvals::Preauthorizations,
    boundary::{ModelRequest, ModelStream},
    contract::{HistoryItem, ModelProtocol, PermissionMode, Run, RunStatus, SessionId},
    hook_store::HookStoreLimits,
    permissions::{PermissionConfig, Policy},
    plugin_manifest::{MANIFEST_NAME, PackageSource, SourceKind, inspect_package},
    plugin_runtime::{ViewLimits, WorkspaceAccess},
    plugin_store::{EnableGrant, PluginScope, StoreLimits},
    runtime::{RuntimeLimits, RuntimeSetup},
};

fn ceiling(origins: &[&str]) -> EffectiveCeiling {
    EffectiveCeiling {
        capabilities: BTreeSet::from(["network.outbound".into(), "mcp.http".into()]),
        network_origins: origins.iter().map(|origin| (*origin).into()).collect(),
        credential_slots: BTreeSet::new(),
        workspace: PathBuf::from("/fixture"),
        workspace_access: WorkspaceAccess::None,
        scope: PluginScope::User,
        enable_revision: 1,
    }
}
#[test]
fn network_intersection_never_reuses_the_broader_host_listener() {
    let mut host = BrokerPolicy {
        allowed_origins: vec![
            "https://one.example.test".into(),
            "https://two.example.test:443".into(),
        ],
        ..BrokerPolicy::default()
    };
    host.unix_socket = Some("/host/private.sock".into());
    let selected = intersect_broker(
        &ceiling(&["https://two.example.test", "https://three.example.test"]),
        Some(&host),
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.allowed_origins, vec!["https://two.example.test"]);
    assert!(selected.unix_socket.is_none());
    assert!(selected.localhost_development_origin.is_none());
    assert_eq!(selected.limits, host.limits);
}
#[test]
fn no_network_capability_or_host_allowlist_preserves_deny() {
    let host = BrokerPolicy {
        allowed_origins: vec!["https://one.example.test".into()],
        ..BrokerPolicy::default()
    };
    let mut requested = ceiling(&["https://one.example.test"]);
    requested.capabilities.clear();
    assert!(intersect_broker(&requested, Some(&host)).unwrap().is_none());
    assert!(
        intersect_broker(&ceiling(&["https://one.example.test"]), None)
            .unwrap()
            .is_none()
    );
    assert!(
        intersect_broker(&ceiling(&["https://two.example.test"]), Some(&host))
            .unwrap()
            .is_none()
    );
}

#[test]
fn localhost_development_exception_requires_both_exact_host_and_plugin_origins() {
    let host = BrokerPolicy {
        allowed_origins: vec![
            "https://localhost:9443".into(),
            "https://one.example.test".into(),
        ],
        localhost_development_origin: Some("https://localhost:9443".into()),
        ..BrokerPolicy::default()
    };
    let selected = intersect_broker(&ceiling(&["https://localhost:9443"]), Some(&host))
        .unwrap()
        .unwrap();
    assert_eq!(
        selected.localhost_development_origin,
        Some("https://localhost:9443".into())
    );
    let selected = intersect_broker(
        &ceiling(&["https://localhost:9444", "https://one.example.test"]),
        Some(&host),
    )
    .unwrap()
    .unwrap();
    assert_eq!(selected.allowed_origins, vec!["https://one.example.test"]);
    assert!(selected.localhost_development_origin.is_none());
    let host = BrokerPolicy {
        localhost_development_origin: None,
        ..host
    };
    let selected = intersect_broker(&ceiling(&["https://localhost:9443"]), Some(&host))
        .unwrap()
        .unwrap();
    assert!(selected.localhost_development_origin.is_none());
}

#[test]
fn host_startups_and_parent_tools_reserve_from_one_atomic_run_budget() {
    let services = RuntimeServices::default();
    services.reserve_tool(3).unwrap();
    services.startup_calls.fetch_add(1, Ordering::AcqRel);
    services.reserve_tool(3).unwrap();
    services.reserve_tool(3).unwrap();
    assert_eq!(
        services.reserve_tool(3).unwrap_err().0.code,
        "limit_reached"
    );
    assert_eq!(services.startup_calls(), 1);
    assert_eq!(services.used_tools.load(Ordering::Acquire), 3);
    assert_eq!(
        RuntimeServices::default()
            .reserve_tool(0)
            .unwrap_err()
            .0
            .code,
        "limit_reached"
    );
}

#[test]
fn concurrent_host_and_parent_reservations_never_exceed_the_limit() {
    let services = Arc::new(RuntimeServices::default());
    let accepted = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    for index in 0..8 {
        let services = services.clone();
        let accepted = accepted.clone();
        workers.push(std::thread::spawn(move || {
            for _ in 0..20 {
                if services.reserve_tool(37).is_ok() {
                    accepted.fetch_add(1, Ordering::AcqRel);
                    if index % 2 == 0 {
                        services.startup_calls.fetch_add(1, Ordering::AcqRel);
                    }
                }
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(accepted.load(Ordering::Acquire), 37);
    assert_eq!(services.used_tools.load(Ordering::Acquire), 37);
    assert!(services.startup_calls() <= 37);
    assert_eq!(
        services.reserve_tool(37).unwrap_err().0.code,
        "limit_reached"
    );
}

#[test]
fn aggregate_run_resources_are_rejected_for_process_entries() {
    let mut base = SandboxPolicy::workspace(PathBuf::from("/fixture"));
    validate_aggregate_resources(&base, true).unwrap();
    for resource in [0, 1, 2] {
        base.resources = crate::execution::ResourceLimits::default();
        match resource {
            0 => base.resources.total_memory_bytes = Some(64 * 1024 * 1024),
            1 => base.resources.total_processes = Some(8),
            _ => {
                base.resources.cpu_quota = Some(crate::execution::CpuQuota {
                    quota_micros: 50_000,
                    period_micros: 100_000,
                })
            }
        }
        assert_eq!(
            validate_aggregate_resources(&base, true)
                .unwrap_err()
                .0
                .code,
            "unsupported_resource_policy"
        );
        validate_aggregate_resources(&base, false).unwrap();
    }
}

struct Fixture {
    root: tempfile::TempDir,
    workspace: PathBuf,
    store: PluginStore,
    pool: Arc<ViewPool>,
    journal: Arc<HookStore>,
    secrets: Arc<SecretRegistry>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let workspace = std::fs::canonicalize(workspace).unwrap();
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
        Self {
            root,
            workspace,
            store,
            pool,
            journal,
            secrets,
        }
    }
    fn context(&self) -> PluginRunContext {
        PluginRunContext {
            workspace: self.workspace.clone(),
            session_id: SessionId::new(),
            run_id: RunId::new(),
            host_version: "0.2.0".into(),
            features: BTreeSet::from(["plugin-manifest-v1".into(), "mcp-http-v1".into()]),
            native_isolation_available: false,
            protected_roots: vec![
                self.store.protected_path().into(),
                self.journal.protected_path().into(),
            ],
            environment: Environment::default(),
            secrets: self.secrets.clone(),
            host_write_roots: vec![self.workspace.clone()],
        }
    }
    fn http(&self) -> ActivePlugins {
        let source = self.root.path().join("package");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(
            source.join(MANIFEST_NAME),
            r#"schema_version=1
[plugin]
id="io.saya.http-fixture"
version="1.0.0"
description="An offline HTTP ceiling fixture."
license="MIT"
[compatibility]
requires_saya=">=0.2.0, <1.0.0"
requires_features=["plugin-manifest-v1","mcp-http-v1"]
[capabilities]
requested=["mcp.http","network.outbound"]
network_origins=["https://one.example.test"]
[[mcp]]
id="remote"
transport="streamable-http"
url="https://one.example.test/api"
credential_slots=[]
"#,
        )
        .unwrap();
        let package = inspect_package(&source).unwrap();
        self.store
            .install(
                &package,
                PackageSource {
                    kind: SourceKind::LocalDirectory,
                    reference: "./fixture".into(),
                },
            )
            .unwrap();
        let context = self.context();
        self.store
            .enable(
                package.digest(),
                PluginScope::workspace(&self.workspace).unwrap(),
                EnableGrant::requested(package.manifest()),
                "0.2.0",
                &context.features,
            )
            .unwrap();
        ActivePlugins::prepare(
            &self.store,
            &context,
            &PluginHostBindings::default(),
            self.pool.clone(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }
}
#[test]
fn http_endpoint_must_belong_to_both_enablement_and_current_host_policy() {
    let fixture = Fixture::new();
    let mut active = fixture.http();
    let server = active.servers().values().next().unwrap();
    let allowed = BrokerPolicy {
        allowed_origins: vec!["https://one.example.test".into()],
        ..BrokerPolicy::default()
    };
    validate_http(server, Some(&allowed)).unwrap();
    assert_eq!(
        validate_http(server, None).unwrap_err().0.code,
        "plugin_network_scope"
    );
    let denied = BrokerPolicy {
        allowed_origins: vec!["https://elsewhere.example.test".into()],
        ..BrokerPolicy::default()
    };
    assert_eq!(
        validate_http(server, Some(&denied)).unwrap_err().0.code,
        "plugin_network_scope"
    );
    let evidence = active
        .servers()
        .keys()
        .map(|id| (id.clone(), StopEvidence::NeverStarted))
        .collect();
    assert!(
        active
            .cleanup(&evidence, &AtomicBool::new(false))
            .unwrap()
            .cleanup_pending
            .is_empty()
    );
}
#[test]
fn stored_http_authorizer_cannot_expand_a_plugin_origin_ceiling() {
    let fixture = Fixture::new();
    let mut active = fixture.http();
    let mut server = active.servers().values().next().unwrap().clone();
    let McpTransportSettings::Http(http) = &mut server.settings.transport else {
        panic!("HTTP fixture");
    };
    http.credential_ref = Some("11111111-1111-4111-8111-111111111111".parse().unwrap());
    let host = BrokerPolicy {
        allowed_origins: vec!["https://one.example.test".into()],
        ..BrokerPolicy::default()
    };
    assert_eq!(
        validate_http(&server, Some(&host)).unwrap_err().0.code,
        "plugin_credential_scope_unsupported"
    );
    let evidence = active
        .servers()
        .keys()
        .map(|id| (id.clone(), StopEvidence::NeverStarted))
        .collect();
    assert!(
        active
            .cleanup(&evidence, &AtomicBool::new(false))
            .unwrap()
            .cleanup_pending
            .is_empty()
    );
}

struct Never;
impl Cancellation for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
struct NoModel;
impl Model for NoModel {
    fn start<'a>(
        &'a self,
        _: &'a ModelRequest,
        _: &'a dyn Cancellation,
    ) -> crate::boundary::Operation<'a, Box<dyn ModelStream>> {
        Box::pin(async {
            Err(error(
                "model_called",
                "No model request should precede mandatory plugin isolation",
            ))
        })
    }
}
#[tokio::test]
async fn normal_runtime_fails_closed_on_host_backend_before_model_or_plugin_start() {
    let fixture = Fixture::new();
    let mut session = Session {
        id: SessionId::new(),
        workspace: fixture.workspace.clone(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fixture".into(),
        history: vec![HistoryItem::UserText("fixture".into())],
    };
    let store = Arc::new(crate::memory::MemoryStore::default());
    let mut runtime = AgentRuntime::new(RuntimeSetup {
        run: Run {
            id: RunId::new(),
            session_id: session.id,
            work_mode: session.work_mode,
            permissions: PermissionMode::Bypass,
        },
        model: Arc::new(NoModel),
        store,
        policy: Arc::new(Policy::new(&fixture.workspace, PermissionConfig::default()).unwrap()),
        files: Arc::new(
            ReadTools::new(&fixture.workspace, &[], false, ReadLimits::default()).unwrap(),
        ),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "fixture".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
    .unwrap();
    let mut policy = SandboxPolicy::workspace(fixture.workspace.clone());
    policy.backend = BackendKind::Host;
    runtime.services.configure_execution(policy, None).unwrap();
    runtime
        .services
        .configure_mcp(
            McpSettings::default(),
            Environment::default(),
            None,
            fixture.secrets.clone(),
        )
        .unwrap();
    runtime
        .services
        .configure_plugins(
            fixture.store.clone(),
            fixture.pool.clone(),
            PluginHostBindings::default(),
            fixture.journal.clone(),
        )
        .unwrap();
    let result = runtime.run(&mut session).await.unwrap();
    assert_ne!(result.status, RunStatus::Completed);
    assert!(
        result
            .error
            .as_ref()
            .is_none_or(|failure| failure.code != "model_called")
    );
    assert!(fixture.store.recover().unwrap().is_empty());
}

#[tokio::test]
async fn aborted_preparation_wait_retains_ready_resources_until_owned_cleanup() {
    let fixture = Fixture::new();
    let active = fixture.http();
    let services = Arc::new(RuntimeServices::default());
    let (release, ready) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    services.state.lock().await.plugin_preparation = Some(PluginPreparation {
        cancelled: cancelled.clone(),
        worker: Some(tokio::spawn(async move {
            ready.await.unwrap();
            PluginPreparationOutcome::Ready(active)
        })),
        unknown: false,
    });
    let owner = services.clone();
    let waiter = tokio::spawn(async move {
        let mut state = owner.state.lock().await;
        owner.finish_plugin_preparation(&mut state, false).await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    waiter.abort();
    let _ = waiter.await;
    let mut state = services.state.lock().await;
    let preparation = state.plugin_preparation.as_ref().unwrap();
    assert!(preparation.worker.is_some());
    preparation.cancel();
    assert!(cancelled.load(Ordering::Acquire));
    release.send(()).unwrap();
    services
        .finish_plugin_preparation(&mut state, true)
        .await
        .unwrap();
    assert!(state.plugin_preparation.is_none());
    assert!(state.plugins.is_some());
    services.cleanup_plugins(&mut state).await.unwrap();
    assert!(state.plugins.is_none());
    assert!(fixture.store.list().unwrap().iter().all(|package| {
        package
            .instances
            .iter()
            .all(|lease| lease.state == crate::plugin_store::InstanceState::Cleaned)
    }));
    assert!(fixture.pool.inventory().unwrap().is_empty());
}

#[tokio::test]
async fn preparation_failure_requires_empty_durable_inventories_before_clean_shutdown() {
    let fixture = Fixture::new();
    let services = RuntimeServices::default();
    let run_id = RunId::new();
    let outcome = failed_preparation(
        &fixture.store,
        &fixture.pool,
        run_id,
        error("cancelled", "Fixture cancelled before dispatch"),
    );
    let mut state = State {
        plugin_preparation: Some(PluginPreparation {
            cancelled: Arc::new(AtomicBool::new(true)),
            worker: Some(tokio::spawn(async move { outcome })),
            unknown: false,
        }),
        ..State::default()
    };
    services
        .finish_plugin_preparation(&mut state, true)
        .await
        .unwrap();
    assert!(state.plugin_preparation.is_none());
    assert!(state.plugins.is_none());

    let active = fixture.http();
    let run_id = fixture.store.list().unwrap()[0].instances[0].run_id;
    drop(active);
    let outcome = failed_preparation(
        &fixture.store,
        &fixture.pool,
        run_id,
        error("cancelled", "Fixture stopped before adoption"),
    );
    state.plugin_preparation = Some(PluginPreparation {
        cancelled: Arc::new(AtomicBool::new(true)),
        worker: Some(tokio::spawn(async move { outcome })),
        unknown: false,
    });
    assert_eq!(
        services
            .finish_plugin_preparation(&mut state, true)
            .await
            .unwrap_err()
            .0
            .code,
        "outcome_unknown"
    );
    assert!(state.plugin_preparation.as_ref().unwrap().unknown);
    assert_eq!(
        services
            .finish_plugin_preparation(&mut state, true)
            .await
            .unwrap_err()
            .0
            .code,
        "outcome_unknown"
    );
    assert!(fixture.store.list().unwrap().iter().any(|package| {
        package
            .instances
            .iter()
            .any(|lease| lease.state != crate::plugin_store::InstanceState::Cleaned)
    }));
}

#[tokio::test]
async fn panicked_preparation_is_retained_as_unknown_on_repeated_cleanup() {
    let services = RuntimeServices::default();
    let mut state = State {
        plugin_preparation: Some(PluginPreparation {
            cancelled: Arc::new(AtomicBool::new(false)),
            worker: Some(tokio::spawn(async { panic!("controlled worker failure") })),
            unknown: false,
        }),
        ..State::default()
    };
    for _ in 0..2 {
        assert_eq!(
            services
                .finish_plugin_preparation(&mut state, true)
                .await
                .unwrap_err()
                .0
                .code,
            "outcome_unknown"
        );
        assert!(state.plugin_preparation.as_ref().unwrap().unknown);
    }
}

#[tokio::test]
async fn last_preparation_owner_drop_signals_cancel_without_resolving_unknown_cleanup() {
    let cancelled = Arc::new(AtomicBool::new(false));
    let observed = cancelled.clone();
    let (signal, received) = tokio::sync::oneshot::channel();
    let owner = Arc::new(PluginPreparation {
        cancelled: cancelled.clone(),
        worker: Some(tokio::spawn(async move {
            while !observed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
            let _ = signal.send(());
            PluginPreparationOutcome::Failed {
                failure: error("cancelled", "Controlled worker observed owner cancellation"),
                cleanup_pending: false,
            }
        })),
        unknown: false,
    });
    let remaining = owner.clone();
    drop(owner);
    assert!(!cancelled.load(Ordering::Acquire));
    drop(remaining);
    assert!(cancelled.load(Ordering::Acquire));
    tokio::time::timeout(Duration::from_secs(1), received)
        .await
        .unwrap()
        .unwrap();

    let unknown_cancelled = Arc::new(AtomicBool::new(false));
    let mut state = State {
        plugin_preparation: Some(PluginPreparation {
            cancelled: unknown_cancelled.clone(),
            worker: None,
            unknown: true,
        }),
        ..State::default()
    };
    let services = RuntimeServices::default();
    assert_eq!(
        services
            .finish_plugin_preparation(&mut state, true)
            .await
            .unwrap_err()
            .0
            .code,
        "outcome_unknown"
    );
    assert!(state.plugin_preparation.as_ref().unwrap().unknown);
    assert!(!unknown_cancelled.load(Ordering::Acquire));
    drop(state);
    assert!(unknown_cancelled.load(Ordering::Acquire));
}

#[tokio::test]
async fn orphaned_service_activity_fails_bounded_without_claiming_plan_became_active() {
    let fixture = Fixture::new();
    let run = Run {
        id: RunId::new(),
        session_id: SessionId::new(),
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Bypass,
    };
    let runtime = AgentRuntime::new(RuntimeSetup {
        run,
        model: Arc::new(NoModel),
        store: Arc::new(crate::memory::MemoryStore::default()),
        policy: Arc::new(Policy::new(&fixture.workspace, PermissionConfig::default()).unwrap()),
        files: Arc::new(
            ReadTools::new(&fixture.workspace, &[], false, ReadLimits::default()).unwrap(),
        ),
        cancellation: Arc::new(Never),
        review: ReviewConfig::new("fixture".into()),
        exec: ExecConfig::default(),
        preauthorizations: Preauthorizations::default(),
        known_secrets: vec![],
        summary: "fixture".into(),
        confirmation: None,
        limits: RuntimeLimits::default(),
    })
    .unwrap();
    runtime.modes.hold_transition().unwrap();
    runtime.jobs.request_mode(WorkMode::Plan).unwrap();
    runtime.services.active.store(1, Ordering::Release);
    let failure = tokio::time::timeout(
        Duration::from_millis(200),
        runtime.services.cleanup_for_plan(
            &runtime.modes,
            &runtime.dispatcher,
            Duration::from_millis(1),
        ),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(failure.0.code, "outcome_unknown");
    let mode = runtime.modes.snapshot().unwrap();
    assert_eq!(mode.current, WorkMode::Work);
    assert_eq!(mode.pending, Some(WorkMode::Plan));
    assert_eq!(runtime.modes.active_invocations().unwrap(), 1);
    runtime.services.active.store(0, Ordering::Release);
    runtime.modes.finish().unwrap();
}
