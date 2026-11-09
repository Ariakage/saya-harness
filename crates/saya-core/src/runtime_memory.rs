//! Native managed-memory dispatch with the same approvals and durable tool flow.
#[path = "runtime_memory/retired_outputs.rs"]
mod retired_outputs;
use super::{AgentRuntime, CallProgress, error, failed};
use crate::{
    approvals::{Authorization, AuthorizationInput, OneShotPermit, PermitSource},
    boundary::{StoreChange, ToolContext, ToolDefinition},
    config::MemorySettings,
    contract::{
        ApprovalSource, BoundaryError, EventPayload, HistoryItem, Session, ToolCall, ToolResult,
        WorkMode,
    },
    dispatch::DispatchReview,
    global_memory::{
        GlobalMemoryStore, MemoryProjection, MemoryProjectionOptions, MemoryQueryLimits,
    },
    permissions::Operation,
    tools::managed_memory::{
        ManagedMemoryLimits, ManagedMemoryTools, MemoryAuthorization, MemoryAuthorizationBinding,
        MemoryAuthorizationSource, MemoryGate,
    },
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

struct PreparationGate;
impl MemoryGate for PreparationGate {
    fn authorize(
        &self,
        _: &MemoryAuthorizationBinding,
        _: &ToolContext,
    ) -> Result<MemoryAuthorization, BoundaryError> {
        Err(error(
            "permission_denied",
            "Memory preparation has no dispatch grant",
        ))
    }
}
struct PermitGate {
    permit: Arc<OneShotPermit>,
    operation: Operation,
    call: ToolCall,
    binding: String,
    store: Arc<GlobalMemoryStore>,
    revision: String,
}
impl MemoryGate for PermitGate {
    fn authorize(
        &self,
        binding: &MemoryAuthorizationBinding,
        context: &ToolContext,
    ) -> Result<MemoryAuthorization, BoundaryError> {
        if self.store.snapshot()?.revision != self.revision
            || binding.fingerprint != self.binding
            || !self
                .permit
                .authorize_managed(&self.call, context, &self.operation)?
        {
            return Err(error("stale_approval", "Memory dispatch grant changed"));
        }
        let source = match self.permit.source() {
            PermitSource::IndependentModelReview => {
                MemoryAuthorizationSource::IndependentModelReview
            }
            PermitSource::User => MemoryAuthorizationSource::User,
            PermitSource::Bypass => MemoryAuthorizationSource::Bypass,
            PermitSource::Rule => MemoryAuthorizationSource::Rule,
            PermitSource::Preauthorization => {
                return Err(error(
                    "permission_denied",
                    "Command grants cannot authorize memory operations",
                ));
            }
        };
        Ok(MemoryAuthorization {
            binding_fingerprint: self.binding.clone(),
            source,
        })
    }
    fn finish(&self) -> Result<(), BoundaryError> {
        self.permit.finish_managed()
    }
}
pub(super) struct MemoryState {
    pub store: Arc<GlobalMemoryStore>,
    pub settings: MemorySettings,
    queries: MemoryQueryLimits,
    limits: ManagedMemoryLimits,
    pending: Mutex<Option<ModelExposureSeed>>,
    policy_revision: String,
}
#[derive(Clone)]
pub(super) struct MemoryFrame {
    pub text: Option<String>,
    pub revision: String,
    pub epoch: String,
    pub used: bool,
}
struct ModelExposureSeed {
    request_id_sha256: String,
    revision: String,
    epoch: String,
}
impl MemoryState {
    pub(super) fn check_frame(&self, frame: &MemoryFrame) -> Result<(), BoundaryError> {
        let snapshot = self.store.snapshot()?;
        if frame.revision != snapshot.revision
            || frame.epoch != self.epoch(&snapshot.forget_revision)
        {
            return Err(error(
                "memory_conflict",
                "Memory changed before model dispatch",
            ));
        }
        Ok(())
    }
    pub fn epoch(&self, forget_revision: &str) -> String {
        crate::memory_context::digest(
            format!(
                "saya-memory-context-v1\0{}\0{forget_revision}\0{}\0{}\0{}",
                self.store.root().display(),
                self.settings.read,
                self.settings.inject,
                self.policy_revision
            )
            .as_bytes(),
        )
    }
    fn tools(&self, gate: Arc<dyn MemoryGate>) -> Result<ManagedMemoryTools, BoundaryError> {
        ManagedMemoryTools::new(self.store.clone(), self.queries, self.limits, gate)
    }
    pub fn definitions(
        &self,
        mode: WorkMode,
        read_only: bool,
    ) -> Result<Vec<ToolDefinition>, BoundaryError> {
        Ok(self
            .tools(Arc::new(PreparationGate))?
            .definitions()
            .into_iter()
            .filter(|tool| {
                if tool.name == "memory_evidence" {
                    self.settings.read || self.settings.write
                } else if matches!(tool.name.as_str(), "memory_read" | "memory_search") {
                    self.settings.read
                } else {
                    self.settings.write && mode != WorkMode::Plan && !read_only
                }
            })
            .collect())
    }
    pub fn project(&self, session: &Session) -> Result<Option<MemoryProjection>, BoundaryError> {
        if !self.settings.read || !self.settings.inject {
            return Ok(None);
        }
        let query = session
            .history
            .iter()
            .rev()
            .find_map(|item| match item {
                HistoryItem::UserText(text) => Some(text),
                _ => None,
            })
            .cloned()
            .unwrap_or_default();
        let mut query: String = query
            .chars()
            .filter(|character| !character.is_control() || character.is_whitespace())
            .collect();
        while query.len() > self.queries.max_query_bytes {
            query.pop();
        }
        self.store
            .project(
                MemoryProjectionOptions {
                    query,
                    ..MemoryProjectionOptions::default()
                },
                self.queries,
            )
            .map(Some)
    }
}
impl AgentRuntime {
    /// Install a host-selected store. Opening and projecting a missing store
    /// performs no filesystem creation. Model arguments never select this root.
    pub fn with_global_memory(
        mut self,
        store: Arc<GlobalMemoryStore>,
        settings: MemorySettings,
    ) -> Result<Self, BoundaryError> {
        if self.context_state.is_none() {
            return Err(error(
                "context_config",
                "Managed memory requires the host ContextBuilder",
            ));
        }
        self.dispatcher.protect_host_paths(vec![
            store.root().join("MEMORY.md"),
            store.root().join("memory"),
            store.root().join(".saya-memory.lock"),
        ])?;
        let memory = Arc::new(MemoryState {
            store,
            settings,
            queries: MemoryQueryLimits::default(),
            limits: ManagedMemoryLimits::default(),
            pending: Mutex::new(None),
            policy_revision: self.dispatcher.permission_fingerprint(),
        });
        let guard_memory = memory.clone();
        let guard_store = self.store.clone();
        self.dispatcher
            .set_output_access_guard(Arc::new(move |call, context| {
                let memory = guard_memory.clone();
                let store = guard_store.clone();
                Box::pin(async move {
                    let session = store
                        .load_session(context.run.session_id)
                        .await?
                        .ok_or_else(|| {
                            error("output_provenance", "Output session is unavailable")
                        })?;
                    let records = store.memory_exposures(session.id).await?;
                    let registered = store.output_references(session.id).await?;
                    tokio::task::spawn_blocking(move || {
                        let retired = retirements(&memory, &session, &records, &[])?;
                        retired_outputs::RetiredOutputAcl::build(&session, &retired)?.output_result(
                            &session,
                            &call,
                            &registered,
                        )
                    })
                    .await
                    .map_err(|_| error("memory_io", "Output provenance worker failed"))?
                })
            }))?;
        self.managed_memory = Some(memory);
        Ok(self)
    }
    pub(super) async fn memory_frame(
        &self,
        session: &Session,
    ) -> Result<Option<MemoryFrame>, BoundaryError> {
        let Some(memory) = self.managed_memory.clone() else {
            return Ok(None);
        };
        let session_copy = session.clone();
        let mut frame = tokio::task::spawn_blocking(move || {
            let projection = memory.project(&session_copy)?;
            let snapshot = memory.store.snapshot()?;
            if projection.as_ref().is_some_and(|projection| {
                projection.revision != snapshot.revision
                    || projection.forget_revision != snapshot.forget_revision
            }) {
                return Err(error(
                    "memory_conflict",
                    "Memory changed while building context",
                ));
            }
            Ok(MemoryFrame {
                text: Some(projection.as_ref().filter(|projection| !projection.text.is_empty()).map(|projection| projection.text.clone()).unwrap_or_else(|| format!("# Memory context data\n{}",serde_json::json!({"schema_version":1,"revision":snapshot.revision,"routing":[],"selected":[]})))),
                revision: snapshot.revision,
                epoch: memory.epoch(&snapshot.forget_revision),
                used: projection.as_ref().is_some_and(|projection| !projection.routing.is_empty() || !projection.selected.is_empty()),
            })
        })
        .await
        .map_err(|_| error("memory_io", "Memory context worker failed"))??;
        if frame.used {
            let memory = self
                .managed_memory
                .as_ref()
                .ok_or_else(|| error("memory_context", "Memory capability is absent"))?;
            let (_, paths) = memory.store.authorization_paths()?;
            let operation = Operation::managed(
                "memory_read",
                paths,
                frame.epoch.clone(),
                true,
                serde_json::json!({"purpose":"automatic_context_injection"}),
            )?;
            let decision = self.dispatcher.managed_context_decision(
                session.work_mode,
                self.run.permissions,
                &operation,
            );
            if decision.decision == crate::permissions::Decision::Deny
                || decision.reason == "explicit_ask"
            {
                frame.text = Some(format!(
                    "# Memory context data\n{}",
                    serde_json::json!({"schema_version":1,"revision":frame.revision,"routing":[],"selected":[],"withheld":"current_permission_rules"})
                ));
                frame.used = false;
            }
        }
        let prior = self.store.memory_exposures(session.id).await?;
        frame.used = frame.used
            || prior
                .iter()
                .any(|record| record.policy_epoch == frame.epoch);
        Ok(Some(frame))
    }
    pub(super) fn bind_memory_request(
        &self,
        request_id: &str,
        frame: Option<&MemoryFrame>,
    ) -> Result<(), BoundaryError> {
        let Some(memory) = &self.managed_memory else {
            return Ok(());
        };
        if let Some(frame) = frame {
            memory.check_frame(frame)?;
        }
        *memory
            .pending
            .lock()
            .map_err(|_| error("memory_context", "Memory context binding lock failed"))? = frame
            .filter(|frame| frame.used)
            .map(|frame| ModelExposureSeed {
                request_id_sha256: crate::memory_context::digest(request_id.as_bytes()),
                revision: frame.revision.clone(),
                epoch: frame.epoch.clone(),
            });
        Ok(())
    }
    pub(super) fn memory_response_change(
        &self,
        session: &Session,
        items: &[HistoryItem],
    ) -> Result<Option<StoreChange>, BoundaryError> {
        let Some(memory) = &self.managed_memory else {
            return Ok(None);
        };
        let Some(seed) = memory
            .pending
            .lock()
            .map_err(|_| error("memory_context", "Memory context binding lock failed"))?
            .take()
        else {
            return Ok(None);
        };
        if items.is_empty() {
            return Ok(None);
        }
        let record = crate::memory_context::MemoryExposure {
            session_id: session.id,
            run_id: self.run.id,
            origin: crate::memory_context::MemoryExposureOrigin::ModelRequest {
                request_id_sha256: seed.request_id_sha256,
            },
            memory_revision: seed.revision,
            policy_epoch: seed.epoch,
            range: crate::context_builder::ItemRange {
                start: session.history.len(),
                end: session.history.len() + items.len(),
            },
            source_sha256: crate::context_builder::history_digest(items)?,
        };
        Ok(Some(StoreChange::MemoryExposure(record)))
    }
    pub(super) async fn memory_retirements(
        &self,
        session: &Session,
    ) -> Result<Vec<crate::context_builder::GroupRetirement>, BoundaryError> {
        let Some(memory) = self.managed_memory.clone() else {
            return Ok(Vec::new());
        };
        let records = self.store.memory_exposures(session.id).await?;
        let definitions = self.definitions(WorkMode::Build)?;
        let session = session.clone();
        tokio::task::spawn_blocking(move || retirements(&memory, &session, &records, &definitions))
            .await
            .map_err(|_| error("memory_io", "Memory provenance worker failed"))?
    }
    pub(super) async fn project_background_memory(
        &self,
        session: &Session,
        call: &ToolCall,
        result: ToolResult,
    ) -> Result<ToolResult, BoundaryError> {
        if call.name != "background_status" || !result.ok || self.managed_memory.is_none() {
            return Ok(result);
        }
        let retired = self.memory_retirements(session).await?;
        let acl = retired_outputs::RetiredOutputAcl::build(session, &retired)?;
        if !acl.is_active() {
            return Ok(result);
        }
        let arguments: serde_json::Value = serde_json::from_str(&call.arguments)
            .map_err(|_| error("invalid_args", "Background status arguments are invalid"))?;
        let job_id = arguments
            .get("job_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| error("invalid_args", "Background status identity is absent"))?;
        let origin = self.jobs.origin(job_id)?;
        let status = self.jobs.status(job_id)?;
        Ok(acl
            .background_result(session, self.run.id, call, origin.as_ref(), status.as_ref())?
            .unwrap_or(result))
    }
    pub(super) async fn execute_memory(
        &self,
        session: &mut Session,
        call: &ToolCall,
        context: &ToolContext,
        progress: &mut CallProgress,
    ) -> Result<ToolResult, BoundaryError> {
        let Some(memory) = &self.managed_memory else {
            return self
                .save_local(
                    session,
                    call,
                    failed("unknown_tool", "Managed memory is unavailable"),
                )
                .await;
        };
        let mutation = matches!(call.name.as_str(), "memory_update" | "memory_forget");
        if (mutation
            && (!memory.settings.write
                || self.read_only
                || context.run.work_mode == WorkMode::Plan))
            || (!mutation && call.name != "memory_evidence" && !memory.settings.read)
            || (call.name == "memory_evidence" && !memory.settings.read && !memory.settings.write)
        {
            return self
                .save_local(
                    session,
                    call,
                    failed("mode_denied", "This memory operation is disabled"),
                )
                .await;
        }
        let saved = self
            .store
            .load_session(session.id)
            .await?
            .ok_or_else(|| error("storage_error", "Memory evidence session is unavailable"))?;
        if &saved != session {
            return Err(error(
                "session_conflict",
                "Memory evidence differs from persisted history",
            ));
        }
        let tools = Arc::new(memory.tools(Arc::new(PreparationGate))?);
        let call_copy = call.clone();
        let context_copy = context.clone();
        let prepare_tools = tools.clone();
        let prepare_cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = prepare_cancel.clone();
        let prepare_timeout = self.limits.tool_timeout.min(self.budget.remaining()?);
        let mut worker = tokio::task::spawn_blocking(move || {
            prepare_tools.prepare(&call_copy, &saved, &context_copy, &worker_cancel)
        });
        let prepared = tokio::select! {
            result = &mut worker => result,
            () = self.cancel.cancelled() => {
                prepare_cancel.store(true, Ordering::Release);
                worker.await
            },
            () = tokio::time::sleep(prepare_timeout) => {
                prepare_cancel.store(true, Ordering::Release);
                worker.await
            },
        }
        .map_err(|_| error("memory_io", "Memory preparation worker failed"))?;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => {
                return self
                    .save_local(session, call, failed(&failure.0.code, &failure.0.message))
                    .await;
            }
        };
        self.check()?;
        let metadata = tools.review_metadata(&prepared)?;
        if mutation {
            let review: crate::tools::managed_memory::MemoryReviewMetadata =
                serde_json::from_value(metadata.clone()).map_err(|_| {
                    error(
                        "memory_review_invalid",
                        "Memory evidence metadata is invalid",
                    )
                })?;
            let retired = self.memory_retirements(session).await?;
            let stale_evidence = review.evidence.iter().any(|evidence| {
                let crate::tools::managed_memory::EvidenceSelector::ToolResult { call_id } =
                    &evidence.selector else { return false; };
                session.history.iter().enumerate().any(|(index,item)| {
                    matches!(item, HistoryItem::ToolResult {call_id: producer,..} if producer == call_id)
                        && retired.iter().any(|group| index >= group.range.start && index < group.range.end)
                })
            });
            if stale_evidence {
                return self
                    .save_local(
                        session,
                        call,
                        failed(
                            "memory_evidence_retired",
                            "Retired memory-dependent outputs cannot provide independent evidence",
                        ),
                    )
                    .await;
            }
        }
        let (authorized_revision, paths) = memory.store.authorization_paths()?;
        let operation = Operation::managed(
            &call.name,
            paths,
            prepared.binding().fingerprint.clone(),
            !mutation,
            metadata,
        )?;
        let authorization = self
            .dispatcher
            .authorize_managed(
                AuthorizationInput {
                    operation: &operation,
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
            )
            .await?;
        let authorization = match authorization {
            Authorization::Pending(request) => match self.confirm(&request).await? {
                Some(answer) => {
                    self.store
                        .commit(&[StoreChange::Activity {
                            run_id: self.run.id,
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
                None => {
                    return self
                        .save_local(
                            session,
                            call,
                            failed("needs_confirmation", "Memory confirmation is unavailable"),
                        )
                        .await;
                }
            },
            other => other,
        };
        let permit = match authorization {
            Authorization::Permit(permit) => permit,
            Authorization::NeedsConfirmation(_) | Authorization::Pending(_) => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("needs_confirmation", "Memory confirmation is required"),
                    )
                    .await;
            }
            Authorization::Stale => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("stale_approval", "Memory authorization changed"),
                    )
                    .await;
            }
            Authorization::Denied(_) => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("permission_denied", "Memory operation was denied"),
                    )
                    .await;
            }
        };
        self.check()?;
        self.store
            .commit(&[StoreChange::ToolIntent {
                run_id: self.run.id,
                call: call.clone(),
            }])
            .await?;
        *progress = CallProgress::Dispatched;
        let saved = self
            .store
            .load_session(session.id)
            .await?
            .ok_or_else(|| error("storage_error", "Memory dispatch session is unavailable"))?;
        let fingerprint = prepared.binding().fingerprint.clone();
        let tools = memory.tools(Arc::new(PermitGate {
            permit,
            operation,
            call: call.clone(),
            binding: fingerprint,
            store: memory.store.clone(),
            revision: authorized_revision.clone(),
        }))?;
        let before = memory.store.snapshot()?;
        let epoch = memory.epoch(&before.forget_revision);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let context = context.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            tools.execute_prepared(prepared, &saved, &context, &worker_cancel)
        });
        let outcome = tokio::select! {
            result = &mut worker => result,
            () = self.cancel.cancelled() => { cancel.store(true, Ordering::Release); worker.await },
            () = tokio::time::sleep(self.limits.tool_timeout.min(self.budget.remaining()?)) => { cancel.store(true, Ordering::Release); worker.await },
        }.map_err(|_| error("memory_io", "Memory dispatch worker failed"))?;
        let result = match outcome {
            Ok(result) => result,
            Err(failure) if failure.2.is_some() => return Err(failure),
            Err(failure) => failed(&failure.0.code, &failure.0.message),
        };
        let result = if !mutation && memory.store.snapshot()?.revision != authorized_revision {
            failed(
                "stale_approval",
                "Memory changed while reading the approved view",
            )
        } else {
            result
        };
        let item = HistoryItem::ToolResult {
            call_id: call.call_id.clone(),
            result: result.clone(),
        };
        let mut changes = vec![
            StoreChange::ToolFinished {
                run_id: self.run.id,
                call_id: call.call_id.clone(),
                result: result.clone(),
            },
            StoreChange::History {
                session_id: session.id,
                item: item.clone(),
            },
        ];
        let call_index = session.history.iter().position(|item| matches!(item,HistoryItem::ToolCall(existing) if existing.call_id == call.call_id))
            .ok_or_else(|| error("memory_exposure", "Native memory producer is absent"))?;
        let mut source = session.history[call_index..].to_vec();
        source.push(item.clone());
        changes.push(StoreChange::MemoryExposure(
            crate::memory_context::MemoryExposure {
                session_id: session.id,
                run_id: self.run.id,
                origin: crate::memory_context::MemoryExposureOrigin::NativeMemory {
                    call_id: call.call_id.clone(),
                },
                memory_revision: before.revision,
                policy_epoch: epoch,
                range: crate::context_builder::ItemRange {
                    start: call_index,
                    end: session.history.len() + 1,
                },
                source_sha256: crate::context_builder::history_digest(&source)?,
            },
        ));
        if let Err(mut failure) = self.store.commit(&changes).await {
            failure.2 = Some(Box::new(result));
            return Err(failure);
        }
        session.history.push(item);
        Ok(result)
    }
}

fn retirements(
    memory: &MemoryState,
    session: &Session,
    records: &[crate::memory_context::MemoryExposure],
    definitions: &[ToolDefinition],
) -> Result<Vec<crate::context_builder::GroupRetirement>, BoundaryError> {
    use crate::context_builder::{self, GroupRetirement, ObservationBinding, RetirementOrigin};
    let snapshot = memory.store.snapshot()?;
    let epoch = memory.epoch(&snapshot.forget_revision);
    let groups = context_builder::group_history(session.protocol, &session.history)?;
    let mut retired = Vec::new();
    for group in groups.groups.into_iter().filter(|group| group.closed) {
        let mut origins = Vec::new();
        for record in records
            .iter()
            .filter(|record| record.policy_epoch != epoch && group.range.overlaps(record.range))
        {
            record.validate_source(session)?;
            origins.push(match &record.origin {
                crate::memory_context::MemoryExposureOrigin::ModelRequest { request_id_sha256 } => {
                    RetirementOrigin::InjectedMemoryTurn {
                        request_id_sha256: request_id_sha256.clone(),
                        memory_revision: record.memory_revision.clone(),
                    }
                }
                crate::memory_context::MemoryExposureOrigin::NativeMemory { .. } => {
                    RetirementOrigin::MemoryToolCall {
                        call_index: record.range.start,
                        call_sha256: context_builder::item_digest(
                            &session.history[record.range.start],
                        )?,
                        result_index: record.range.end - 1,
                        result_sha256: context_builder::item_digest(
                            &session.history[record.range.end - 1],
                        )?,
                        memory_revision: record.memory_revision.clone(),
                    }
                }
            });
        }
        if origins.is_empty() {
            continue;
        }
        let mut observations = Vec::new();
        for index in group.range.start..group.range.end {
            if let HistoryItem::ToolCall(call) = &session.history[index]
                && !matches!(
                    call.name.as_str(),
                    "memory_read"
                        | "memory_search"
                        | "memory_evidence"
                        | "memory_update"
                        | "memory_forget"
                )
                && definitions.iter().any(|tool| tool.name == call.name)
                && let Some((result_index, result)) = session.history[index + 1..group.range.end]
                    .iter()
                    .enumerate()
                    .find_map(|(offset, item)| match item {
                        HistoryItem::ToolResult { call_id, result } if call_id == &call.call_id => {
                            Some((index + 1 + offset, result))
                        }
                        _ => None,
                    })
            {
                observations.push(ObservationBinding {
                    call_index: index,
                    call_sha256: context_builder::item_digest(&session.history[index])?,
                    result_index,
                    result_sha256: context_builder::item_digest(&session.history[result_index])?,
                    registered_tool_name: call.name.clone(),
                    state: if result.ok {
                        crate::contract::ToolState::Succeeded
                    } else {
                        crate::contract::ToolState::Failed
                    },
                });
            }
        }
        retired.push(GroupRetirement {
            range: group.range,
            source_sha256: context_builder::history_digest(
                &session.history[group.range.start..group.range.end],
            )?,
            current_memory_revision: snapshot.revision.clone(),
            origins,
            observations,
        });
    }
    Ok(retired)
}
