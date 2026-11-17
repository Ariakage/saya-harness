use super::*;

struct ActiveWorkspace {
    pub(super) directories: BrowserSandboxDirectories,
    pub(super) lease: Option<Arc<dyn BrowserWorkspaceLease>>,
    pub(super) temp: Option<TempArtifacts>,
}

impl ActiveWorkspace {
    pub(super) fn temp(artifacts: TempArtifacts) -> Self {
        let directories = artifacts.directories();
        Self {
            directories,
            lease: None,
            temp: Some(artifacts),
        }
    }

    pub(super) fn prepared(lease: Arc<dyn BrowserWorkspaceLease>) -> Self {
        Self {
            directories: lease.directories().clone(),
            lease: Some(lease),
            temp: None,
        }
    }

    pub(super) fn execution(&self) -> Option<PreparedEnvironment> {
        self.lease.as_ref().and_then(|lease| lease.execution())
    }

    /// Release the run-private workspace. Callers must reap the worker and
    /// Chromium process group before this; the environment refuses cleanup
    /// while an owned process lease is outstanding.
    pub(super) async fn release(&mut self) -> Result<(), BrowserFailure> {
        if let Some(lease) = &self.lease {
            lease.release().await.map_err(|_| {
                BrowserFailure::uncertain("Browser sandbox cleanup could not be confirmed")
            })?;
            self.lease = None;
        }
        if let Some(temp) = &mut self.temp {
            temp.remove().map_err(|_| {
                BrowserFailure::uncertain("Browser worker temp cleanup could not be confirmed")
            })?;
            self.temp = None;
        }
        Ok(())
    }
}

struct ProviderInner {
    pub(super) connection: Option<Arc<dyn WorkerConnection>>,
    pub(super) generation: u64,
    pub(super) session: Option<String>,
    pub(super) page: Option<String>,
    /// Shared so a shutdown future that is dropped by an outer timeout never
    /// takes the only reference away. A later retry still sees the workspace.
    pub(super) workspace: Option<Arc<AsyncMutex<ActiveWorkspace>>>,
    pub(super) sandbox_evidence: Option<ChromiumSandboxEvidence>,
    pub(super) sandbox_unusable: bool,
}

/// Playwright MCP implementation of [`BrowserProvider`].
pub struct PlaywrightMcpProvider {
    pub(super) label: &'static str,
    pub(super) identity_verifier: Arc<dyn WorkerIdentityVerifier>,
    pub(super) launch_planner: Arc<dyn WorkerLaunchPlanner>,
    /// Only the host-local backend can fall back to host temp artifacts when
    /// no prepared environment factory is supplied. Offline tests use this.
    pub(super) temp_settings: Option<BrowserWorkerSettings>,
    pub(super) launcher: Arc<dyn WorkerLauncher>,
    pub(super) environment_factory: Option<Arc<dyn BrowserEnvironmentFactory>>,
    pub(super) sandbox_inspector: Arc<dyn ChromiumSandboxInspector>,
    /// Bounded host-side network observations for backends that use the
    /// browser broker. Native workers keep this unset.
    pub(super) network_log: Option<Arc<crate::browser::network::BrowserNetworkLog>>,
    inner: AsyncMutex<ProviderInner>,
    pub(super) call_lock: AsyncMutex<()>,
    pub(super) shutdown_lock: AsyncMutex<()>,
}

impl std::fmt::Debug for PlaywrightMcpProvider {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlaywrightMcpProvider")
            .field("backend", &self.label)
            .finish_non_exhaustive()
    }
}

impl PlaywrightMcpProvider {
    pub fn new(
        settings: BrowserWorkerSettings,
        launcher: Arc<dyn WorkerLauncher>,
    ) -> Result<Self, BoundaryError> {
        Self::build(settings, launcher, None)
    }

    /// Assemble the production provider. Every worker lifecycle receives a
    /// freshly prepared browser environment from `factory`; the launcher
    /// applies that environment through `PreparedEnvironment::prepare_command`.
    pub fn managed(
        settings: BrowserWorkerSettings,
        factory: Arc<dyn BrowserEnvironmentFactory>,
        config: ExecConfig,
    ) -> Result<Self, BoundaryError> {
        Self::build(
            settings,
            Arc::new(ManagedProcessLauncher::new(config)),
            Some(factory),
        )
    }

    /// Assemble a provider over a backend-supplied identity verifier, launch
    /// planner and launcher while sharing lifecycle, catalogue, tool mapping
    /// and cleanup behavior. The VM browser backend uses this entry point.
    pub fn with_worker_backend(
        label: &'static str,
        identity_verifier: Arc<dyn WorkerIdentityVerifier>,
        launch_planner: Arc<dyn WorkerLaunchPlanner>,
        launcher: Arc<dyn WorkerLauncher>,
        environment_factory: Arc<dyn BrowserEnvironmentFactory>,
        sandbox_inspector: Arc<dyn ChromiumSandboxInspector>,
    ) -> Result<Self, BoundaryError> {
        identity_verifier.preflight()?;
        Ok(Self::assemble(
            label,
            identity_verifier,
            launch_planner,
            None,
            launcher,
            Some(environment_factory),
            sandbox_inspector,
        ))
    }

    pub(super) fn build(
        settings: BrowserWorkerSettings,
        launcher: Arc<dyn WorkerLauncher>,
        environment_factory: Option<Arc<dyn BrowserEnvironmentFactory>>,
    ) -> Result<Self, BoundaryError> {
        settings.validate()?;
        if !settings.enabled {
            return Err(invalid_config("Browser worker is disabled"));
        }
        let verifier = Arc::new(LocalWorkerIdentityVerifier::new(settings.clone()));
        verifier.preflight()?;
        let planner = Arc::new(LocalWorkerLaunchPlanner::new(settings.clone()));
        Ok(Self::assemble(
            "native",
            verifier,
            planner,
            Some(settings),
            launcher,
            environment_factory,
            Arc::new(ProcfsChromiumSandboxInspector::default()),
        ))
    }

    pub(super) fn assemble(
        label: &'static str,
        identity_verifier: Arc<dyn WorkerIdentityVerifier>,
        launch_planner: Arc<dyn WorkerLaunchPlanner>,
        temp_settings: Option<BrowserWorkerSettings>,
        launcher: Arc<dyn WorkerLauncher>,
        environment_factory: Option<Arc<dyn BrowserEnvironmentFactory>>,
        sandbox_inspector: Arc<dyn ChromiumSandboxInspector>,
    ) -> Self {
        Self {
            label,
            identity_verifier,
            launch_planner,
            temp_settings,
            launcher,
            environment_factory,
            sandbox_inspector,
            network_log: None,
            inner: AsyncMutex::new(ProviderInner {
                connection: None,
                generation: 0,
                session: None,
                page: None,
                workspace: None,
                sandbox_evidence: None,
                sandbox_unusable: false,
            }),
            call_lock: AsyncMutex::new(()),
            shutdown_lock: AsyncMutex::new(()),
        }
    }

    /// Replace the sandbox verifier. Intended for host integration tests.
    pub fn with_sandbox_inspector(mut self, inspector: Arc<dyn ChromiumSandboxInspector>) -> Self {
        self.sandbox_inspector = inspector;
        self
    }

    /// Attach the bounded observation log used by the VM browser broker.
    pub fn with_network_log(
        mut self,
        log: Arc<crate::browser::network::BrowserNetworkLog>,
    ) -> Self {
        self.network_log = Some(log);
        self
    }

    /// Bounded, path-free network observations recorded for this Run.
    pub fn network_observations(&self) -> Vec<crate::browser::network::BrowserNetworkObservation> {
        self.network_log
            .as_ref()
            .map(|log| log.observations())
            .unwrap_or_default()
    }

    /// Actual evidence from the last completed Chromium sandbox verification.
    pub async fn sandbox_evidence(&self) -> Option<ChromiumSandboxEvidence> {
        self.inner.lock().await.sandbox_evidence.clone()
    }

    pub(super) async fn connected(
        &self,
    ) -> Result<(Arc<dyn WorkerConnection>, u64), BrowserFailure> {
        let inner = self.inner.lock().await;
        if inner.sandbox_unusable {
            return Err(BrowserFailure::uncertain(
                "Browser sandbox verification or cleanup is incomplete; close the browser before reuse",
            ));
        }
        match &inner.connection {
            Some(connection) => Ok((connection.clone(), inner.generation)),
            None => Err(BrowserFailure::new(
                "worker_closed",
                "Browser worker is not running",
            )),
        }
    }

    /// Re-discover the worker catalogue after the worker itself changes it.
    ///
    /// Playwright publishes a new catalogue when it starts the browser, so the
    /// generation approved at launch is stale as soon as `open` creates its
    /// first page. Only lifecycle steps that cause the change call this;
    /// ordinary tool calls keep failing closed on a stale catalogue.
    pub(super) async fn resync_catalog(
        &self,
        connection: &Arc<dyn WorkerConnection>,
        cancel: &dyn Cancellation,
    ) -> Result<u64, BrowserFailure> {
        let mut last = None;
        for _ in 0..CATALOG_RESYNC_ATTEMPTS {
            match connection.list_tools(cancel).await {
                Ok(catalog) => {
                    validate_catalog(&catalog)?;
                    self.inner.lock().await.generation = catalog.generation;
                    return Ok(catalog.generation);
                }
                Err(failure) if failure.code == "stale_catalog" => last = Some(failure),
                Err(failure) => return Err(failure),
            }
        }
        Err(last.unwrap_or_else(|| {
            BrowserFailure::new(
                "stale_catalog",
                "Browser worker catalogue changed; begin a new session",
            )
        }))
    }

    async fn ensure_launched(
        &self,
        inner: &mut ProviderInner,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        if inner.sandbox_unusable {
            return Err(BrowserFailure::uncertain(
                "Browser worker must finish shutdown before it can be relaunched",
            ));
        }
        if inner.connection.is_some() {
            return Ok(());
        }
        // A remote Run root whose cleanup was not confirmed must be retried
        // before starting another environment. The launcher keeps the handle
        // across launches, so a failure here is never a fresh success.
        self.launcher.retry_pending_cleanup().await.map_err(|_| {
            BrowserFailure::uncertain(
                "Browser worker pending remote cleanup could not be confirmed",
            )
        })?;
        // A previous cleanup failure must be resolved before starting a new
        // environment; a closed environment is never reused.
        if let Some(stale) = inner.workspace.clone() {
            let released = {
                let mut workspace = stale.lock().await;
                workspace.release().await.is_ok()
            };
            if !released {
                return Err(BrowserFailure::uncertain(
                    "Browser worker previous cleanup could not be confirmed",
                ));
            }
            inner.workspace = None;
        }
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker launch cancelled before dispatch",
            ));
        }
        // Recheck the named files immediately before launch so construction-time
        // identity cannot silently drift before the first process dispatch.
        let identity = self.identity_verifier.verify(cancel).await?;
        // Environment preparation recreates the private sandbox tree and the
        // launch planner re-hashes the pinned executables and writes the worker
        // config; keep that synchronous filesystem work off the executor.
        let prepared = {
            let environment_factory = self.environment_factory.clone();
            let temp_settings = self.temp_settings.clone();
            let launch_planner = self.launch_planner.clone();
            tokio::task::spawn_blocking(move || {
                let workspace = match &environment_factory {
                    Some(factory) => {
                        let lease = factory
                            .prepare()
                            .map_err(|failure| map_environment_failure(&failure))?;
                        ActiveWorkspace::prepared(lease)
                    }
                    None => {
                        let settings = temp_settings.as_ref().ok_or_else(|| {
                            BrowserFailure::new(
                                "worker_environment",
                                "Browser worker requires a prepared environment factory",
                            )
                        })?;
                        ActiveWorkspace::temp(TempArtifacts::create(settings).map_err(|_| {
                            BrowserFailure::new(
                                "worker_environment",
                                "Browser worker temp setup failed",
                            )
                        })?)
                    }
                };
                let request = launch_planner.build(&identity, &workspace.directories);
                Ok::<_, BrowserFailure>((workspace, request))
            })
            .await
            .map_err(|_| {
                BrowserFailure::new(
                    "worker_environment",
                    "Browser worker environment preparation worker failed",
                )
            })??
        };
        let (workspace, request) = prepared;
        let workspace = Arc::new(AsyncMutex::new(workspace));
        // Register the workspace before any await so cancelling the launch
        // future cannot drop the only reference to an unreleased environment.
        inner.workspace = Some(workspace.clone());
        let mut request = match request {
            Ok(request) => request,
            Err(_) => {
                return Self::discard_workspace(
                    inner,
                    workspace,
                    BrowserFailure::new(
                        "worker_environment",
                        "Browser worker launch input is invalid",
                    ),
                )
                .await;
            }
        };
        request.execution = workspace.lock().await.execution();
        if cancel.is_cancelled() {
            return Self::discard_workspace(
                inner,
                workspace,
                BrowserFailure::new(
                    "cancelled",
                    "Browser worker launch cancelled before dispatch",
                ),
            )
            .await;
        }
        let connection = match self.launcher.launch(request, cancel).await {
            Ok(connection) => connection,
            Err(failure) => {
                return Self::discard_workspace(inner, workspace, failure).await;
            }
        };
        // Catalogue awaits must retain cleanup ownership if this future is dropped.
        inner.connection = Some(connection.clone());
        inner.sandbox_evidence = None;
        inner.sandbox_unusable = true;
        if connection.protocol_version().as_deref() != Some(WORKER_PROTOCOL_VERSION) {
            return self
                .discard_launch(
                    inner,
                    connection,
                    workspace,
                    BrowserFailure::new(
                        "worker_protocol",
                        "Browser worker protocol version is not supported",
                    ),
                )
                .await;
        }
        let catalog = match connection.list_tools(cancel).await {
            Ok(catalog) => catalog,
            Err(failure) => {
                return self
                    .discard_launch(inner, connection, workspace, failure)
                    .await;
            }
        };
        if let Err(failure) = validate_catalog(&catalog) {
            return self
                .discard_launch(inner, connection, workspace, failure)
                .await;
        }
        inner.connection = Some(connection);
        inner.generation = catalog.generation;
        inner.workspace = Some(workspace);
        Ok(())
    }

    async fn discard_workspace(
        inner: &mut ProviderInner,
        workspace: Arc<AsyncMutex<ActiveWorkspace>>,
        failure: BrowserFailure,
    ) -> Result<(), BrowserFailure> {
        // No worker process was accepted, so there is nothing to reap; the
        // prepared environment still has to release its scratch directory.
        let released = {
            let mut workspace = workspace.lock().await;
            workspace.release().await.is_ok()
        };
        if released {
            if inner
                .workspace
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &workspace))
            {
                inner.workspace = None;
            }
            return Err(failure);
        }
        inner.workspace = Some(workspace);
        Err(BrowserFailure::uncertain(
            "Browser worker startup cleanup could not be confirmed",
        ))
    }

    async fn discard_launch(
        &self,
        inner: &mut ProviderInner,
        connection: Arc<dyn WorkerConnection>,
        workspace: Arc<AsyncMutex<ActiveWorkspace>>,
        failure: BrowserFailure,
    ) -> Result<(), BrowserFailure> {
        // Reap the worker and Chromium group before releasing the environment.
        if connection.shutdown().await.is_err() {
            return Err(BrowserFailure::uncertain(
                "Browser worker startup cleanup could not be confirmed",
            ));
        }
        inner.connection = None;
        inner.generation = 0;
        let released = {
            let mut workspace = workspace.lock().await;
            workspace.release().await.is_ok()
        };
        if !released {
            // Keep the workspace for a retry; the connection was already
            // reaped, so a later shutdown only has to finish cleanup.
            inner.workspace = Some(workspace);
            return Err(BrowserFailure::uncertain(
                "Browser worker startup cleanup could not be confirmed",
            ));
        }
        if inner
            .workspace
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, &workspace))
        {
            inner.workspace = None;
        }
        inner.sandbox_unusable = false;
        Err(failure)
    }

    pub(super) async fn check_page(&self, session: &str, page: &str) -> Result<(), BrowserFailure> {
        let inner = self.inner.lock().await;
        if inner.session.as_deref() != Some(session) || inner.page.as_deref() != Some(page) {
            return Err(BrowserFailure::new(
                "invalid_provider_result",
                "Browser provider session or page is unknown",
            ));
        }
        Ok(())
    }

    pub(super) async fn call(
        &self,
        name: &str,
        arguments: Value,
        cancel: &dyn Cancellation,
    ) -> Result<McpToolResult, BrowserFailure> {
        let _guard = self.call_lock.lock().await;
        let (connection, generation) = self.connected().await?;
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker call cancelled before dispatch",
            ));
        }
        let result = connection
            .call_tool(name, arguments, generation, cancel)
            .await?;
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker reported a tool error. The call was not replayed",
            ));
        }
        if let Err(failure) = self.verify_sandbox(&connection, cancel).await {
            if self.shutdown_resources().await.is_err() {
                return Err(BrowserFailure::uncertain(
                    "Browser sandbox verification failed and worker cleanup could not be confirmed",
                ));
            }
            return Err(failure);
        }
        Ok(result)
    }

    pub(super) async fn verify_sandbox(
        &self,
        connection: &Arc<dyn WorkerConnection>,
        cancel: &dyn Cancellation,
    ) -> Result<(), BrowserFailure> {
        {
            let mut inner = self.inner.lock().await;
            inner.sandbox_evidence = None;
            // Dropping this future must not make an unverified lifecycle reusable.
            inner.sandbox_unusable = true;
        }
        let evidence = self
            .sandbox_inspector
            .inspect_connection(connection.as_ref(), cancel)
            .await?;
        tracing::info!(
            target: "soyo::browser",
            event = "browser.sandbox_verified",
            renderer_pid = evidence.renderer_pid,
            no_new_privs = evidence.no_new_privs,
            seccomp_mode = evidence.seccomp_mode,
            sandbox_arguments_valid = evidence.sandbox_arguments_valid,
            stderr_initializes_sandbox = evidence.stderr_initializes_sandbox
        );
        let mut inner = self.inner.lock().await;
        inner.sandbox_evidence = Some(evidence);
        inner.sandbox_unusable = false;
        Ok(())
    }

    /// Serialized cleanup shared by `shutdown` and sandbox-verification
    /// failure handling. Callers must hold `call_lock`, or have otherwise
    /// stopped dispatch, before invoking this.
    pub(super) async fn shutdown_resources(&self) -> Result<(), BrowserFailure> {
        // Clone the shared handles instead of taking them. An outer timeout
        // may drop this future mid-await; the provider must still hold the
        // only connection/workspace so a retry can continue and cannot
        // report a second false success.
        let (connection, workspace) = {
            let mut inner = self.inner.lock().await;
            inner.sandbox_evidence = None;
            inner.sandbox_unusable = true;
            (inner.connection.clone(), inner.workspace.clone())
        };
        if connection.is_none() && workspace.is_none() {
            self.launcher.retry_pending_cleanup().await.map_err(|_| {
                BrowserFailure::uncertain(
                    "Browser worker pending remote cleanup could not be confirmed",
                )
            })?;
            let mut inner = self.inner.lock().await;
            inner.session = None;
            inner.page = None;
            inner.sandbox_evidence = None;
            inner.sandbox_unusable = false;
            return Ok(());
        }
        let mut process_failed = false;
        if let Some(connection) = &connection
            && connection.shutdown().await.is_err()
        {
            // Keep the private directories until process termination can be
            // confirmed on a retry.
            process_failed = true;
        }
        let mut cleanup_failed = false;
        if !process_failed && let Some(workspace) = &workspace {
            let mut workspace = workspace.lock().await;
            cleanup_failed = workspace.release().await.is_err();
        }
        if process_failed || cleanup_failed {
            return Err(BrowserFailure::uncertain(
                "Browser worker shutdown could not be confirmed",
            ));
        }
        {
            let mut inner = self.inner.lock().await;
            if let Some(connection) = &connection
                && inner
                    .connection
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, connection))
            {
                inner.connection = None;
            }
            if let Some(workspace) = &workspace
                && inner
                    .workspace
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, workspace))
            {
                inner.workspace = None;
            }
            inner.generation = 0;
            inner.session = None;
            inner.page = None;
            inner.sandbox_evidence = None;
            inner.sandbox_unusable = false;
        }
        self.launcher.retry_pending_cleanup().await.map_err(|_| {
            BrowserFailure::uncertain(
                "Browser worker pending remote cleanup could not be confirmed",
            )
        })?;
        Ok(())
    }
}

#[async_trait]
impl BrowserProvider for PlaywrightMcpProvider {
    fn facts(&self) -> BrowserFacts {
        BrowserFacts {
            provider: format!("playwright-mcp-{PLAYWRIGHT_MCP_VERSION}"),
            // Pre-capture masking is delegated through the private host-only
            // `saya/browser/captureScreenshot` channel; runtime capture still
            // fails closed unless the worker and native capture engine confirm masking.
            screenshots: true,
            screenshot_masking: true,
            downloads: true,
            uploads: true,
        }
    }

    async fn open(
        &self,
        session: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserOpened, BrowserFailure> {
        let _guard = self.call_lock.lock().await;
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker open cancelled before dispatch",
            ));
        }
        {
            let inner = self.inner.lock().await;
            if inner.session.is_some() || inner.page.is_some() {
                return Err(BrowserFailure::new(
                    "browser_limit",
                    "This browser worker supports one page at a time",
                ));
            }
            if session.is_some() {
                return Err(BrowserFailure::new(
                    "invalid_provider_result",
                    "Browser provider has no session to resume",
                ));
            }
        }
        let (connection, generation) = {
            let mut inner = self.inner.lock().await;
            self.ensure_launched(&mut inner, cancel).await?;
            match &inner.connection {
                Some(connection) => (connection.clone(), inner.generation),
                None => {
                    return Err(BrowserFailure::new(
                        "worker_closed",
                        "Browser worker is not running",
                    ));
                }
            }
        };
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker open cancelled before dispatch",
            ));
        }
        let result = match connection
            .call_tool(
                "browser_navigate",
                json!({"url": "about:blank"}),
                generation,
                cancel,
            )
            .await
        {
            Ok(result) => result,
            Err(failure) if failure.code == "stale_catalog" => {
                // The stale check runs before dispatch, so nothing was sent.
                let generation = self.resync_catalog(&connection, cancel).await?;
                connection
                    .call_tool(
                        "browser_navigate",
                        json!({"url": "about:blank"}),
                        generation,
                        cancel,
                    )
                    .await?
            }
            Err(failure) => return Err(failure),
        };
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker could not create its initial page",
            ));
        }
        // Starting the browser publishes the worker's post-launch catalogue;
        // capture that generation so the page lifecycle stays approved.
        if let Err(failure) = self.resync_catalog(&connection, cancel).await {
            if self.shutdown_resources().await.is_err() {
                return Err(BrowserFailure::uncertain(
                    "Browser catalogue verification failed and worker cleanup could not be confirmed",
                ));
            }
            return Err(failure);
        }
        let text = first_text(&result).unwrap_or_default();
        let url = page_fields(text).0.unwrap_or_else(|| "about:blank".into());
        if let Err(failure) = self.verify_sandbox(&connection, cancel).await {
            if self.shutdown_resources().await.is_err() {
                return Err(BrowserFailure::uncertain(
                    "Browser sandbox verification failed and worker cleanup could not be confirmed",
                ));
            }
            return Err(failure);
        }
        let mut inner = self.inner.lock().await;
        inner.session = Some(WORKER_SESSION.into());
        inner.page = Some(WORKER_PAGE.into());
        Ok(BrowserOpened {
            session: WORKER_SESSION.into(),
            page: WORKER_PAGE.into(),
            url: Some(url),
        })
    }

    async fn close(&self, session: &str, cancel: &dyn Cancellation) -> Result<(), BrowserFailure> {
        let _guard = self.call_lock.lock().await;
        let sandbox_unusable = {
            let inner = self.inner.lock().await;
            if inner.session.is_none() && inner.page.is_none() {
                if !inner.sandbox_unusable {
                    return Ok(());
                }
            } else if inner.session.as_deref() != Some(session) {
                return Err(BrowserFailure::new(
                    "invalid_provider_result",
                    "Browser provider session is unknown",
                ));
            }
            inner.sandbox_unusable
        };
        if cancel.is_cancelled() {
            return Err(BrowserFailure::new(
                "cancelled",
                "Browser worker close cancelled before dispatch",
            ));
        }
        if sandbox_unusable {
            return self.shutdown_resources().await;
        }
        let (connection, _) = self.connected().await?;
        // `browser_close` is the lifecycle counterpart of `open`'s initial
        // navigation: re-discover first so a catalogue update published by the
        // launch cannot leave the close call stale.
        let mut generation = self.resync_catalog(&connection, cancel).await?;
        let mut result = None;
        for attempt in 0..CATALOG_RESYNC_ATTEMPTS {
            match connection
                .call_tool("browser_close", json!({}), generation, cancel)
                .await
            {
                Ok(value) => {
                    result = Some(value);
                    break;
                }
                Err(failure)
                    if failure.code == "stale_catalog" && attempt + 1 < CATALOG_RESYNC_ATTEMPTS =>
                {
                    generation = self.resync_catalog(&connection, cancel).await?;
                }
                Err(failure) => return Err(failure),
            }
        }
        let result = result.ok_or_else(|| {
            BrowserFailure::new(
                "stale_catalog",
                "Browser worker catalogue changed; begin a new session",
            )
        })?;
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker could not close its page",
            ));
        }
        let mut inner = self.inner.lock().await;
        inner.session = None;
        inner.page = None;
        inner.sandbox_evidence = None;
        Ok(())
    }

    async fn navigate(
        &self,
        session: &str,
        page: &str,
        url: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self
            .call("browser_navigate", json!({"url": url}), cancel)
            .await?;
        let action = parse_action(first_text(&result)?, Some(url));
        Ok(action)
    }

    async fn snapshot(
        &self,
        session: &str,
        page: &str,
        _max_bytes: usize,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserSnapshot, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self.call("browser_snapshot", json!({}), cancel).await?;
        parse_snapshot(first_text(&result)?, result.truncated)
    }

    async fn click(
        &self,
        session: &str,
        page: &str,
        element: &str,
        button: Option<&str>,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let mut arguments = json!({"target": element});
        if let Some(button) = button {
            arguments["button"] = json!(button);
        }
        let result = self.call("browser_click", arguments, cancel).await?;
        Ok(parse_action(first_text(&result)?, None))
    }

    async fn fill(
        &self,
        session: &str,
        page: &str,
        element: &str,
        text: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self
            .call(
                "browser_type",
                json!({"target": element, "text": text}),
                cancel,
            )
            .await?;
        Ok(parse_action(first_text(&result)?, None))
    }

    async fn press(
        &self,
        session: &str,
        page: &str,
        key: &str,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        let result = self
            .call("browser_press_key", json!({"key": key}), cancel)
            .await?;
        Ok(parse_action(first_text(&result)?, None))
    }

    async fn screenshot(
        &self,
        session: &str,
        page: &str,
        cancel: &dyn Cancellation,
    ) -> Result<crate::browser::BrowserScreenshot, BrowserFailure> {
        self.check_page(session, page).await?;
        let _guard = self.call_lock.lock().await;
        let (connection, _) = self.connected().await?;
        let captured = connection.capture_screenshot(cancel).await;
        if let Err(failure) = self.verify_sandbox(&connection, cancel).await {
            if self.shutdown_resources().await.is_err() {
                return Err(BrowserFailure::uncertain(
                    "Browser sandbox shutdown could not be confirmed",
                ));
            }
            return Err(failure);
        }
        parse_captured_screenshot(captured?)
    }
    async fn download(
        &self,
        session: &str,
        page: &str,
        element: &str,
        cancel: &dyn Cancellation,
    ) -> Result<crate::browser::BrowserDownload, BrowserFailure> {
        self.check_page(session, page).await?;
        let (connection, _) = self.connected().await?;
        // The download directory is the structural proof of which file this
        // click produced. No generic "last download" lookup is allowed.
        let before = connection.list_downloads(cancel).await?;
        // The click is the only action allowed to produce an accepted
        // download. Nothing is looked up from a previous action.
        let result = self
            .call("browser_click", json!({"target": element}), cancel)
            .await?;
        let mut text = first_text(&result)?.to_owned();
        let mut event = download_path(&text)?;
        let mut selected: Option<(String, String)> = None;
        for attempt in 0..20 {
            let after = connection.list_downloads(cancel).await?;
            let created: Vec<String> = after.difference(&before).cloned().collect();
            if created.len() > 1 {
                return Err(BrowserFailure::new(
                    "invalid_provider_result",
                    "Browser action created more than one new download file",
                ));
            }
            if let Some(name) = created.first() {
                if let Some(event_path) = &event {
                    if event_path.contains("..") {
                        return Err(BrowserFailure::new(
                            "file_path_denied",
                            "Browser download path contains traversal components",
                        ));
                    }
                    let event_name = event_path
                        .rsplit(['/', '\\'])
                        .next()
                        .filter(|event_name| !event_name.is_empty())
                        .ok_or_else(|| {
                            BrowserFailure::new(
                                "invalid_provider_result",
                                "Browser download path is invalid",
                            )
                        })?;
                    if event_name != name {
                        return Err(BrowserFailure::new(
                            "invalid_provider_result",
                            "Browser download event does not match the new file",
                        ));
                    }
                }
                selected = Some((name.clone(), name.clone()));
                break;
            }
            if attempt == 19 {
                break;
            }
            // A download may finish just after the click result; poll both the
            // trusted event section and the provider's output directory.
            let waited = self
                .call("browser_wait_for", json!({"time": 0.1}), cancel)
                .await?;
            text.push('\n');
            text.push_str(first_text(&waited).unwrap_or_default());
            event = download_path(&text)?;
        }
        let Some((filename, target)) = selected else {
            return Err(BrowserFailure::new(
                "download_missing",
                "The element action did not create exactly one new download file",
            ));
        };
        let bytes = connection
            .read_download(&target, PROVIDER_MAX_DOWNLOAD_BYTES, cancel)
            .await?;
        Ok(crate::browser::BrowserDownload {
            filename,
            // The host re-detects the real type from the magic bytes.
            mime: "application/octet-stream".into(),
            bytes,
        })
    }

    async fn upload(
        &self,
        session: &str,
        page: &str,
        element: &str,
        source: &crate::browser::BrowserUploadSource,
        cancel: &dyn Cancellation,
    ) -> Result<crate::browser::BrowserUpload, BrowserFailure> {
        self.check_page(session, page).await?;
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        let mut file = options.open(&source.path).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser upload staging file is unavailable")
        })?;
        let metadata = file.metadata().map_err(|_| {
            BrowserFailure::new(
                "file_broker",
                "Browser upload staging file cannot be inspected",
            )
        })?;
        if !metadata.is_file() || metadata.len() != source.size {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser upload staging file changed",
            ));
        }
        let mut bytes = Vec::with_capacity(source.size as usize);
        std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|_| {
            BrowserFailure::new("file_broker", "Browser upload staging file cannot be read")
        })?;
        if bytes.len() as u64 != source.size || sha256_hex(&bytes) != source.sha256 {
            return Err(BrowserFailure::new(
                "file_broker",
                "Browser upload staging content changed",
            ));
        }
        // Open the page's file chooser with the explicit document element.
        self.call("browser_click", json!({"target": element}), cancel)
            .await?;
        let (connection, _) = self.connected().await?;
        let remote = connection
            .put_upload(&source.filename, &bytes, cancel)
            .await?;
        // If cancellation drops this future between staging and cleanup, the
        // guard still removes the Run-private staging directory.
        let mut guard = RemoteUploadGuard {
            connection: Some(connection.clone()),
            path: remote.clone(),
        };
        let result = self
            .call(
                "browser_file_upload",
                json!({"paths": [remote.clone()]}),
                cancel,
            )
            .await;
        let cleanup = connection.remove_upload(&remote, cancel).await;
        if cleanup.is_ok() {
            guard.connection = None;
        }
        let result = result?;
        // The file chooser already consumed the staged file. If cleanup cannot
        // be confirmed after a successful upload, the operation must not be
        // retried as an ordinary failure: the page may already hold the file.
        cleanup.map_err(|_| {
            BrowserFailure::uncertain(
                "Browser upload completed but staging cleanup could not be confirmed",
            )
        })?;
        if result.is_error {
            return Err(BrowserFailure::new(
                "worker_tool_error",
                "Browser worker could not upload the file",
            ));
        }
        Ok(crate::browser::BrowserUpload {
            filename: source.filename.clone(),
            bytes_sent: source.size,
        })
    }

    async fn wait(
        &self,
        session: &str,
        page: &str,
        milliseconds: u64,
        cancel: &dyn Cancellation,
    ) -> Result<BrowserAction, BrowserFailure> {
        self.check_page(session, page).await?;
        if milliseconds == 0 {
            return Ok(BrowserAction {
                url: None,
                detail: None,
            });
        }
        let mut remaining = milliseconds;
        let mut detail = None;
        loop {
            let chunk = remaining.min(30_000);
            let seconds = chunk as f64 / 1000.0;
            let result = self
                .call("browser_wait_for", json!({"time": seconds}), cancel)
                .await?;
            if let Some(line) = result_detail(first_text(&result).unwrap_or_default()) {
                detail = Some(line);
            }
            remaining -= chunk;
            if remaining == 0 {
                break;
            }
        }
        Ok(BrowserAction { url: None, detail })
    }

    async fn shutdown(&self, _cancel: &dyn Cancellation) -> Result<(), BrowserFailure> {
        let _serial = self.shutdown_lock.lock().await;
        let _call = self.call_lock.lock().await;
        self.shutdown_resources().await
    }
}
