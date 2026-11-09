//! Owned hook processes remain discoverable when a wait or cleanup future is
//! cancelled. Only awaited native cleanup releases the entry's view evidence.
use super::*;
use crate::hooks::{
    CleanupState, HookBegin, HookExecution, HookExecutionRequest, HookHostExecutor,
    HookProcessOutput, StopReason,
};
use tokio::io::AsyncWriteExt;

#[derive(Default, Clone)]
pub(in crate::runtime::services) struct HookProcesses(
    Arc<Mutex<BTreeMap<String, Arc<AsyncMutex<RunningHook>>>>>,
);
impl HookProcesses {
    async fn stop(&self, entries: Option<&BTreeSet<String>>) -> Result<(), BoundaryError> {
        let processes = self
            .0
            .lock()
            .map_err(|_| error("hook_execution", "Hook process registry lock failed"))?
            .iter()
            .map(|(id, process)| (id.clone(), process.clone()))
            .collect::<Vec<_>>();
        for (id, process) in processes {
            let mut running = process.lock().await;
            if entries.is_some_and(|entries| !entries.contains(&running.entry)) {
                continue;
            }
            running.stop(StopReason::Cancelled).await?;
            drop(running);
            self.0
                .lock()
                .map_err(|_| error("hook_execution", "Hook process registry lock failed"))?
                .remove(&id);
        }
        Ok(())
    }
    pub(in crate::runtime::services) async fn stop_entries(
        &self,
        entries: &BTreeSet<String>,
    ) -> Result<(), BoundaryError> {
        self.stop(Some(entries)).await
    }
    pub(in crate::runtime::services) async fn stop_all(&self) -> Result<(), BoundaryError> {
        self.stop(None).await
    }
}

pub(super) struct NativeHookExecutor {
    plugins: Weak<PluginRun>,
    services: Weak<RuntimeServices>,
    dispatcher: Arc<ToolDispatcher>,
    model: Arc<dyn Model>,
    store: Arc<dyn Store>,
    modes: crate::mode::ModeController,
    budget: SharedBudget,
    review: ReviewConfig,
    confirmation: Option<Arc<dyn super::super::super::ApprovalHandler>>,
    root: Arc<dyn Cancellation>,
    secrets: crate::runtime::secrets::SecretSource,
    context: ToolContext,
    summary: String,
    limit: usize,
}
impl NativeHookExecutor {
    pub(super) fn new(
        runtime: &AgentRuntime,
        session: &Session,
        services: Weak<RuntimeServices>,
        plugins: Arc<PluginRun>,
    ) -> Self {
        Self {
            plugins: Arc::downgrade(&plugins),
            services,
            dispatcher: runtime.dispatcher.clone(),
            model: runtime.model.clone(),
            store: runtime.store.clone(),
            modes: runtime.modes.clone(),
            budget: runtime.budget.clone(),
            review: runtime.review.clone(),
            confirmation: runtime.confirmation.clone(),
            root: runtime.cancel.clone(),
            secrets: runtime.secret_source.clone(),
            context: runtime.context(session, runtime.run.work_mode),
            summary: runtime.summary.clone(),
            limit: runtime.limits.tool_calls,
        }
    }
    fn safe_input(&self, request: &HookExecutionRequest) -> Result<Option<Vec<u8>>, BoundaryError> {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(&request.payload)
            .map_err(|_| error("hook_payload", "Hook payload encoding failed"))?;
        if bytes.len() > HookLimits::default().max_payload_bytes {
            return Ok(None);
        }
        let digest: String = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        if digest != request.intent.payload_sha256 {
            return Ok(None);
        }
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| error("hook_payload", "Hook payload encoding failed"))?;
        if crate::redaction::contains_secret(
            text,
            &crate::runtime::secrets::snapshot(&self.secrets)?,
        ) {
            return Ok(None);
        }
        Ok(Some(bytes))
    }
    async fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        operation: &crate::permissions::Operation,
        cancel: &StartupCancel,
    ) -> Result<Option<Arc<crate::approvals::OneShotPermit>>, BoundaryError> {
        let remaining = self.budget.remaining()?;
        if remaining.is_zero() {
            return Err(error(
                "limit_reached",
                "Hook authorization exhausted the active Run budget",
            ));
        }
        let authorize = self.dispatcher.authorize_mcp_launch(
            AuthorizationInput {
                operation,
                call,
                context,
                summary: &self.summary,
                interactive: self.confirmation.is_some(),
            },
            DispatchReview {
                model: self.model.as_ref(),
                budget: &self.budget,
                config: &self.review,
            },
        );
        let authorization = tokio::select! {biased;_ =cancel.cancelled()=>return Ok(None),_ =tokio::time::sleep(remaining)=>return Err(error("limit_reached", "Hook authorization exceeded the active Run budget")),authorization=authorize=>authorization?};
        let authorization = match authorization {
            Authorization::Pending(request) => {
                let Some(handler) = &self.confirmation else {
                    return Ok(None);
                };
                self.budget.pause()?;
                let answer = tokio::select! {biased;_ =cancel.cancelled()=>Ok(None),answer=handler.confirm(&request,cancel)=>answer};
                self.budget.resume()?;
                let Some(answer) = answer? else {
                    return Ok(None);
                };
                self.store
                    .commit(&[StoreChange::Activity {
                        run_id: self.context.run.id,
                        payload: EventPayload::PermissionResolved {
                            approval_id: request.approval_id.clone(),
                            call_id: call.call_id.clone(),
                            decision: if answer { "allow" } else { "deny" }.into(),
                            source: ApprovalSource::User,
                        },
                    }])
                    .await?;
                self.dispatcher.resolve(&request.approval_id, answer)?
            }
            other => other,
        };
        match authorization {
            Authorization::Permit(permit) => Ok(Some(permit)),
            _ => Ok(None),
        }
    }
    async fn begin_inner(
        &self,
        request: &HookExecutionRequest,
        cancel: &dyn Cancellation,
    ) -> Result<HookBegin, BoundaryError> {
        let plugins = self.plugins.upgrade().ok_or_else(|| {
            error(
                "hook_execution",
                "Plugin lifecycle supervisor is unavailable",
            )
        })?;
        let descriptor = plugins
            .hooks
            .values()
            .find(|hook| {
                hook.identity.plugin_id == request.intent.plugin_id
                    && hook.identity.package_digest == request.intent.package_digest
                    && hook.identity.entry_id == request.intent.entry_id
                    && hook.lease_id == request.intent.lease_id
            })
            .ok_or_else(|| {
                error(
                    "hook_identity",
                    "Hook intent differs from its prepared descriptor",
                )
            })?;
        let snapshot = self.modes.snapshot()?;
        if cancel.is_cancelled()
            || snapshot.current == WorkMode::Plan
            || snapshot.pending.is_some()
            || self.root.is_cancelled()
        {
            return Ok(HookBegin::NotStarted { denied: true });
        }
        if request.intent.run.id != self.context.run.id
            || request.intent.run.session_id != self.context.run.session_id
            || request.intent.enable_revision != descriptor.ceiling.enable_revision
        {
            return Ok(HookBegin::NotStarted { denied: true });
        }
        plugins.check(&descriptor.hook_id)?;
        let services = self
            .services
            .upgrade()
            .ok_or_else(|| error("hook_execution", "Hook lifecycle supervisor is unavailable"))?;
        services.reserve_tool(self.limit)?;
        services
            .startup_calls
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.limit).then_some(count + 1)
            })
            .map_err(|_| error("limit_reached", "Hook startup exhausted the tool budget"))?;
        let start_cancel = StartupCancel {
            root: self.root.clone(),
            modes: self.modes.clone(),
            revision: snapshot.revision,
            current: snapshot.current,
        };
        let environment = descriptor
            .process
            .environment_references
            .iter()
            .map(|(name, reference)| {
                (
                    name.clone(),
                    json!({"source":"reference","value":reference}),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        let call = ToolCall {
            call_id: format!("hook-exec-{}", request.intent.invocation_id),
            name: "exec".into(),
            arguments: json!({"argv":descriptor.argv,"cwd":descriptor.cwd,"env":environment})
                .to_string(),
        };
        let mut context = self.context.clone();
        context.run.work_mode = snapshot.current;
        let native = plugins.environment(&descriptor.hook_id).await?;
        if self.safe_input(request)?.is_none() {
            return Ok(HookBegin::NotStarted { denied: true });
        }
        let mut launch = plugins.scoped_plan(
            &descriptor.hook_id,
            &context,
            &call,
            &native,
            self.dispatcher.effective_exec_config()?,
        )?;
        launch
            .operation
            .bind_plugin_input(&request.intent.payload_sha256, request.payload.clone())?;
        let permit = match self
            .authorize(&call, &context, &launch.operation, &start_cancel)
            .await
        {
            Ok(Some(permit)) => permit,
            Ok(None) => return Ok(HookBegin::NotStarted { denied: true }),
            Err(failure)
                if matches!(
                    failure.0.code.as_str(),
                    "permission_denied" | "stale_approval" | "needs_confirmation"
                ) =>
            {
                return Ok(HookBegin::NotStarted { denied: true });
            }
            Err(failure) => return Err(failure),
        };
        let _permit = StartupPermit(permit.clone());
        if cancel.is_cancelled()
            || start_cancel.is_cancelled()
            || !permit.authorize_managed(&call, &context, &launch.operation)?
        {
            return Ok(HookBegin::NotStarted { denied: true });
        }
        if self.safe_input(request)?.is_none() {
            return Ok(HookBegin::NotStarted { denied: true });
        }
        if self.budget.remaining()?.is_zero() {
            return Ok(HookBegin::NotStarted { denied: false });
        }
        self.store
            .commit(&[StoreChange::ToolIntent {
                run_id: context.run.id,
                call: call.clone(),
            }])
            .await?;
        let mut launch = plugins.scoped_plan(
            &descriptor.hook_id,
            &context,
            &call,
            &native,
            self.dispatcher.effective_exec_config()?,
        )?;
        launch
            .operation
            .bind_plugin_input(&request.intent.payload_sha256, request.payload.clone())?;
        if cancel.is_cancelled()
            || start_cancel.is_cancelled()
            || !permit.authorize_managed(&call, &context, &launch.operation)?
        {
            save_not_started(self.store.as_ref(), &context, &call).await?;
            return Ok(HookBegin::NotStarted { denied: true });
        }
        let Some(input) = self.safe_input(request)? else {
            save_not_started(self.store.as_ref(), &context, &call).await?;
            return Ok(HookBegin::NotStarted { denied: true });
        };
        if self.budget.remaining()?.is_zero() {
            save_not_started(self.store.as_ref(), &context, &call).await?;
            return Ok(HookBegin::NotStarted { denied: false });
        }
        let process = ManagedProcess::spawn(
            &launch.plan,
            launch.cwd,
            launch.environment,
            &launch.config,
            true,
        );
        let mut process = match process {
            Ok(process) => process,
            Err(_) => {
                save_not_started(self.store.as_ref(), &context, &call).await?;
                return Ok(HookBegin::NotStarted { denied: false });
            }
        };
        plugins.record(&descriptor.hook_id, StopEvidence::Unknown)?;
        services.active.fetch_add(1, Ordering::AcqRel);
        let secret_source = self.secrets.clone();
        let writer = process.take_stdin().map(|mut stdin| {
            tokio::spawn(async move {
                let clean =
                    crate::runtime::secrets::snapshot(&secret_source).is_ok_and(|secrets| {
                        std::str::from_utf8(&input)
                            .is_ok_and(|text| !crate::redaction::contains_secret(text, &secrets))
                    });
                if !clean {
                    return Err(std::io::Error::other(
                        "Hook input credential boundary changed",
                    ));
                }
                stdin.write_all(&input).await?;
                stdin.shutdown().await
            })
        });
        let max = request.intent.output_max_bytes;
        let stdout = process.take_stdout().map(|mut output| {
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut truncated = false;
                let mut buffer = [0u8; 4096];
                loop {
                    let count = output
                        .read(&mut buffer)
                        .await
                        .map_err(|_| error("hook_execution", "Hook stdout read failed"))?;
                    if count == 0 {
                        break;
                    }
                    let keep = count.min(max.saturating_sub(bytes.len()));
                    bytes.extend_from_slice(&buffer[..keep]);
                    truncated |= keep < count;
                }
                Ok((bytes, truncated))
            })
        });
        let stderr = process.take_stderr().map(|mut output| {
            tokio::spawn(async move {
                let mut buffer = [0u8; 4096];
                while let Ok(count) = output.read(&mut buffer).await {
                    if count == 0 {
                        break;
                    }
                }
            })
        });
        let process = Arc::new(AsyncMutex::new(RunningHook {
            entry: descriptor.hook_id.clone(),
            process,
            writer,
            stdout,
            stderr,
            output: None,
            exit_code: None,
            store: self.store.clone(),
            context,
            call,
            plugins: Arc::downgrade(&plugins),
            active: Some(ActiveCall(services.active.clone())),
            finished: false,
        }));
        let id = request.intent.invocation_id.clone();
        plugins
            .processes
            .0
            .lock()
            .map_err(|_| error("hook_execution", "Hook process registry lock failed"))?
            .insert(id.clone(), process.clone());
        permit.finish_managed()?;
        Ok(HookBegin::Started(Box::new(NativeHookExecution {
            id,
            process,
            owner: plugins.processes.clone(),
        })))
    }
}
impl HookHostExecutor for NativeHookExecutor {
    fn begin<'a>(
        &'a self,
        request: &'a HookExecutionRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, HookBegin> {
        Box::pin(self.begin_inner(request, cancel))
    }
}
async fn save_not_started(
    store: &dyn Store,
    context: &ToolContext,
    call: &ToolCall,
) -> Result<(), BoundaryError> {
    store
        .commit(&[
            StoreChange::ToolFinished {
                run_id: context.run.id,
                call_id: call.call_id.clone(),
                result: ToolResult {
                    ok: false,
                    data: None,
                    error: Some(crate::contract::ErrorInfo {
                        code: "hook_not_started".into(),
                        message: "Hook process was not started".into(),
                    }),
                    truncated: false,
                },
            },
            StoreChange::ToolState {
                run_id: context.run.id,
                call_id: call.call_id.clone(),
                state: ToolState::Failed,
            },
        ])
        .await
}

struct NativeHookExecution {
    id: String,
    process: Arc<AsyncMutex<RunningHook>>,
    owner: HookProcesses,
}
impl HookExecution for NativeHookExecution {
    fn wait<'a>(&'a mut self) -> Operation<'a, HookProcessOutput> {
        Box::pin(async move { self.process.lock().await.wait().await })
    }
    fn stop<'a>(&'a mut self, reason: StopReason) -> Operation<'a, CleanupState> {
        Box::pin(async move {
            self.process.lock().await.stop(reason).await?;
            self.owner
                .0
                .lock()
                .map_err(|_| error("hook_execution", "Hook process registry lock failed"))?
                .remove(&self.id);
            Ok(CleanupState::Cleaned)
        })
    }
}
struct NeverCancelled;
impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
type OutputReader = JoinHandle<Result<(Vec<u8>, bool), BoundaryError>>;
struct RunningHook {
    entry: String,
    process: ManagedProcess,
    writer: Option<JoinHandle<std::io::Result<()>>>,
    stdout: Option<OutputReader>,
    stderr: Option<JoinHandle<()>>,
    output: Option<(Vec<u8>, bool)>,
    exit_code: Option<i32>,
    store: Arc<dyn Store>,
    context: ToolContext,
    call: ToolCall,
    plugins: Weak<PluginRun>,
    active: Option<ActiveCall>,
    finished: bool,
}
impl RunningHook {
    async fn wait(&mut self) -> Result<HookProcessOutput, BoundaryError> {
        let end = self
            .process
            .wait(&NeverCancelled, Duration::from_secs(24 * 60 * 60))
            .await?;
        self.exit_code = end.status.code();
        if let Some(writer) = &mut self.writer {
            writer
                .await
                .map_err(|_| error("hook_execution", "Hook input worker failed"))?
                .map_err(|_| error("hook_execution", "Hook input was not delivered safely"))?;
            self.writer = None;
        }
        if let Some(stdout) = &mut self.stdout {
            self.output = Some(
                stdout
                    .await
                    .map_err(|_| error("hook_execution", "Hook stdout worker failed"))??,
            );
            self.stdout = None;
        }
        let (stdout, truncated) = self
            .output
            .clone()
            .ok_or_else(|| error("hook_execution", "Hook stdout pipe is unavailable"))?;
        Ok(HookProcessOutput {
            exit_code: self.exit_code,
            stdout,
            truncated,
        })
    }
    async fn stop(&mut self, reason: StopReason) -> Result<(), BoundaryError> {
        if self.finished {
            return Ok(());
        }
        let status = self.process.terminate().await?;
        self.exit_code = status.code();
        if let Some(writer) = self.writer.take() {
            writer.abort();
            let _ = writer.await;
        }
        if let Some(stdout) = self.stdout.take() {
            stdout.abort();
            let _ = stdout.await;
        }
        if let Some(stderr) = self.stderr.take() {
            stderr.abort();
            let _ = stderr.await;
        }
        let ok = matches!(reason, StopReason::Completed) && status.success();
        self.store.commit(&[StoreChange::ToolFinished {run_id:self.context.run.id,call_id:self.call.call_id.clone(),result:ToolResult {ok,data:Some(json!({"hook":self.entry,"exit_code":self.exit_code,"cancelled":matches!(reason,StopReason::Cancelled),"timed_out":matches!(reason,StopReason::TimedOut)})),error:None,truncated:false}},StoreChange::ToolState {run_id:self.context.run.id,call_id:self.call.call_id.clone(),state:if ok {ToolState::Succeeded}else {ToolState::Failed}}]).await?;
        if let Some(plugins) = self.plugins.upgrade() {
            plugins.record(&self.entry, StopEvidence::Stopped)?;
        }
        self.finished = true;
        self.active.take();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{ModelProtocol, PermissionMode, Run, Session, SessionId};

    #[tokio::test]
    async fn aborting_hook_wait_keeps_process_owned_until_explicit_cleanup() {
        let directory = tempfile::tempdir().unwrap();
        let context = ToolContext {
            workspace: directory.path().into(),
            run: Run {
                id: RunId::new(),
                session_id: SessionId::new(),
                work_mode: WorkMode::Work,
                permissions: PermissionMode::Bypass,
            },
        };
        let call = ToolCall {
            call_id: "controlled-hook-owner-fixture".into(),
            name: "exec".into(),
            arguments: "{}".into(),
        };
        let store = Arc::new(crate::memory::MemoryStore::default());
        let session = Session {
            id: context.run.session_id,
            workspace: context.workspace.clone(),
            work_mode: context.run.work_mode,
            profile: "fixture".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fixture".into(),
            history: Vec::new(),
        };
        store
            .commit(&[
                StoreChange::Session(session),
                StoreChange::RunStarted(context.run.clone()),
                StoreChange::ToolIntent {
                    run_id: context.run.id,
                    call: call.clone(),
                },
            ])
            .await
            .unwrap();
        let plan = crate::tools::exec::ExecPlan {
            argv: vec!["/bin/sh".into(), "-c".into(), "exec /bin/sleep 60".into()],
            cwd: directory.path().into(),
            timeout: Duration::from_secs(120),
            environment_digest: "fixture".into(),
        };
        let cwd =
            cap_std::fs::Dir::open_ambient_dir(directory.path(), cap_std::ambient_authority())
                .unwrap();
        let process =
            ManagedProcess::spawn(&plan, cwd, BTreeMap::new(), &ExecConfig::default(), true)
                .unwrap();
        let count = Arc::new(AtomicUsize::new(1));
        let running = Arc::new(AsyncMutex::new(RunningHook {
            entry: "fixture-entry".into(),
            process,
            writer: None,
            stdout: None,
            stderr: None,
            output: None,
            exit_code: None,
            store,
            context,
            call,
            plugins: Weak::new(),
            active: Some(ActiveCall(count.clone())),
            finished: false,
        }));
        let owner = HookProcesses::default();
        owner
            .0
            .lock()
            .unwrap()
            .insert("fixture-invocation".into(), running.clone());
        let mut execution = NativeHookExecution {
            id: "fixture-invocation".into(),
            process: running,
            owner: owner.clone(),
        };
        let wait = tokio::spawn(async move { execution.wait().await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        wait.abort();
        let _ = wait.await;
        assert_eq!(owner.0.lock().unwrap().len(), 1);
        assert_eq!(count.load(Ordering::Acquire), 1);
        owner.stop_all().await.unwrap();
        assert!(owner.0.lock().unwrap().is_empty());
        assert_eq!(count.load(Ordering::Acquire), 0);
    }
}
