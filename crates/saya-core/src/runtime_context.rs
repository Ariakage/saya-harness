//! Request projection at the shared runtime entry, before provider dispatch.
use super::{AgentRuntime, error, secrets};
use crate::{
    boundary::ModelRequest,
    context_builder::{
        self, BuildInput, ContextBudget, ContextDecision, ContextView, CountingIdentity, ItemRange,
        ResultProjection,
    },
    contract::{BoundaryError, HistoryItem, Session, ToolResult},
};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct ContextState {
    pub budget: ContextBudget,
    pub automatic: bool,
    pub view: Mutex<Option<ContextView>>,
    pub coordinator: crate::compaction::CompactionCoordinator,
    pub requested: AtomicBool,
    pub manual_only: AtomicBool,
    pub published: AtomicBool,
    pub rebuild: AtomicBool,
}
impl ContextState {
    pub fn new(
        budget: ContextBudget,
        automatic: bool,
        config: crate::compaction::CompactionConfig,
    ) -> Result<Self, BoundaryError> {
        Ok(Self {
            budget,
            automatic,
            view: Mutex::new(None),
            coordinator: crate::compaction::CompactionCoordinator::new(config)?,
            requested: AtomicBool::new(false),
            manual_only: AtomicBool::new(false),
            published: AtomicBool::new(false),
            rebuild: AtomicBool::new(false),
        })
    }
}

impl AgentRuntime {
    pub(super) async fn context_request(
        &self,
        session: &Session,
        mut request: ModelRequest,
        memory_frame: Option<&super::managed_memory::MemoryFrame>,
        skill_frame: Option<&crate::skills::ActiveSkillSet>,
    ) -> Result<ModelRequest, BoundaryError> {
        let Some(state) = &self.context_state else {
            return Ok(request);
        };
        let model = ModeBoundCompactionModel {
            inner: self.model.clone(),
            modes: self.modes.clone(),
            memory: self.managed_memory.clone().zip(memory_frame.cloned()),
            skills: self
                .skill_state
                .clone()
                .zip(skill_frame.map(|frame| frame.version_sha256.clone())),
        };
        model.check_sources()?;
        let identity = CountingIdentity {
            protocol: session.protocol,
            model: session.model.clone(),
            adapter_version: crate::provider::COUNTING_ADAPTER_VERSION.into(),
        };
        let records = secrets::redact(
            self.store.output_references(session.id).await?,
            &self.secret_source,
        )?;
        let mut by_call: BTreeMap<String, Vec<_>> = BTreeMap::new();
        for record in records {
            if record.scope.session_id != session.id {
                return Err(error(
                    "output_registration",
                    "Host output registry has a different session",
                ));
            }
            by_call
                .entry(record.scope.call_id.clone())
                .or_default()
                .push(record);
        }
        let mut projections = Vec::new();
        for (source_index, item) in session.history.iter().enumerate() {
            if let HistoryItem::ToolResult { call_id, result } = item
                && let Some(outputs) = by_call.get(call_id)
                && serde_json::to_vec(result)
                    .map_err(|_| error("context_projection", "Tool result cannot be measured"))?
                    .len()
                    > 16 * 1024
            {
                let mut data = serde_json::json!({"output_refs":outputs,"projection":"Retrieve required retained evidence using output_search and output_read. Inspect completeness and gaps."});
                if let Some(source) = result.data.as_ref().and_then(serde_json::Value::as_object) {
                    for key in [
                        "exit_code",
                        "timed_out",
                        "cancelled",
                        "stdout_truncated",
                        "stderr_truncated",
                        "is_error",
                        "output_capture_errors",
                        "output_capture_format",
                    ] {
                        if let Some(value) = source.get(key) {
                            data[key] = value.clone();
                        }
                    }
                }
                let replacement = ToolResult {
                    ok: result.ok,
                    error: result.error.clone(),
                    truncated: true,
                    data: Some(data),
                };
                projections.push(ResultProjection {
                    source_index,
                    source_item_sha256: context_builder::item_digest(item)?,
                    call_id: call_id.clone(),
                    replacement,
                    outputs: outputs.clone(),
                });
            }
        }
        let retirements = self.memory_retirements(session).await?;
        if !retirements.is_empty() && self.jobs.running_count()? != 0 {
            self.jobs.cancel_and_drain().await?;
            self.check()?;
        }
        if !retirements.is_empty() {
            let latest_todo = session.history.iter().enumerate().rev().find_map(|(index,item)| match item {
                HistoryItem::ToolResult {call_id,result} if crate::tools::todo::result_todos(result).is_some()
                    && session.history[..index].iter().any(|item| matches!(item,HistoryItem::ToolCall(call) if call.call_id == *call_id && call.name == "todo_write")) => Some(index),
                _ => None,
            });
            if latest_todo.is_some_and(|index| {
                retirements.iter().any(|retirement| {
                    index >= retirement.range.start && index < retirement.range.end
                })
            }) && let Some(HistoryItem::UserText(text)) = request.history.first_mut()
            {
                let mut header: serde_json::Value = serde_json::from_str(text)
                    .map_err(|_| error("context_retirement", "Runtime task metadata is invalid"))?;
                header["saya_runtime_context_v1"]["todos"] = serde_json::json!([]);
                header["saya_runtime_context_v1"]["retired_memory_dependent_todos"] =
                    serde_json::json!(true);
                *text = header.to_string();
            }
        }
        let versions = self.context_versions()?;
        model.check_sources()?;
        let stored = self.store.context_checkpoint(session.id).await?;
        let current_view = stored
            .as_ref()
            .filter(|checkpoint| {
                checkpoint.validate().is_ok()
                    && checkpoint.record.context_versions == versions
                    && checkpoint.view.identity == identity
                    && checkpoint.view.profile == session.profile
            })
            .map(|checkpoint| checkpoint.view.clone());
        let view = current_view;
        *state
            .view
            .lock()
            .map_err(|_| error("context_view", "Context view lock failed"))? = view.clone();
        let protected = if self.jobs.running_count()? == 0 || session.history.is_empty() {
            Vec::new()
        } else {
            vec![ItemRange {
                start: view.as_ref().map_or(0, |view| view.covered_prefix),
                end: session.history.len(),
            }]
        };
        request
            .instructions
            .push_str("\n\n# Context continuation\n");
        if view.is_some() {
            request
                .instructions
                .push_str(crate::compaction::CONTINUATION_INSTRUCTIONS);
        } else {
            request.instructions.push_str(
                "No historical context checkpoint is selected for this request. Continue from the current original user text, retained protocol groups, and effective runtime context.",
            );
        }
        let dynamic = request.dynamic_instructions.get_or_insert_with(String::new);
        dynamic.push_str("\n# Host-selected context checkpoint\n");
        dynamic.push_str(
            &serde_json::json!({"checkpoint_selected_by_host":view.is_some()}).to_string(),
        );
        dynamic.push('\n');
        let make_input = |view| BuildInput {
            history_offset: request.history.len().saturating_sub(session.history.len()),
            request: &request,
            identity: &identity,
            budget: state.budget,
            source_history: &session.history,
            revision: session.history.len() as u64,
            committed_view: view,
            protected_ranges: &protected,
            output_projections: &projections,
            group_retirements: &retirements,
        };
        let built =
            context_builder::build(make_input(view.as_ref()), self.model.request_counter())?;
        let forced = state.requested.swap(false, Ordering::AcqRel);
        let prefix = if forced {
            built
                .grouping
                .safe_prefix_ends(&protected)
                .into_iter()
                .filter(|end| *end > view.as_ref().map_or(0, |view| view.covered_prefix))
                .max()
        } else if state.automatic {
            match built.decision {
                ContextDecision::NeedsCompact { max_prefix_end, .. } => Some(max_prefix_end),
                _ => None,
            }
        } else {
            None
        };
        if let Some(prefix_end) = prefix {
            let checkpoint_id = format!("compact_{}", uuid::Uuid::now_v7().simple());
            // Source coverage is checked by the summary schema. Historical user
            // wording may be corrected later and is not all permanently active.
            let required = Vec::new();
            let mut references = Vec::new();
            for (index, item) in session.history.iter().enumerate().take(prefix_end) {
                if let HistoryItem::ToolResult { call_id, .. } = item
                    && !retirements.iter().any(|retirement| {
                        index >= retirement.range.start && index < retirement.range.end
                    })
                    && let Some(outputs) = by_call.get(call_id)
                {
                    for output in outputs {
                        references.push(crate::compaction::ContextReference {
                            id:output.output_ref.clone(),source_ids:vec![format!("history:{index}")],
                            metadata:serde_json::json!({"scope":output.scope,"sha256":output.sha256,"artifact_complete":output.artifact_complete,
                                "coverage":output.coverage,"retained_ranges":output.retained_ranges,"gaps":output.gaps,"total_bytes_seen":output.total_bytes_seen}),
                        });
                    }
                }
            }
            let known_secrets = secrets::snapshot(&self.secret_source)?;
            let prepared = state
                .coordinator
                .prepare(
                    crate::compaction::CompactionInput {
                        context: make_input(view.as_ref()),
                        session_id: session.id,
                        checkpoint_id: &checkpoint_id,
                        prefix_end,
                        work_mode: self.modes.snapshot()?.current,
                        permissions: self.run.permissions,
                        required_items: &required,
                        references: &references,
                        context_versions: &versions,
                        known_secrets: &known_secrets,
                    },
                    &model,
                    &self.budget,
                    self.model.request_counter(),
                    self.cancel.as_ref(),
                )
                .await;
            let mut prepared = match prepared {
                Ok(prepared) => prepared,
                Err(error) if error.0.code == "compaction_mode_changed" => {
                    state.requested.store(forced, Ordering::Release);
                    state.rebuild.store(true, Ordering::Release);
                    return Ok(request);
                }
                Err(error) => return Err(error),
            };
            self.check()?;
            let current_versions = self.context_versions()?;
            prepared.verify_current(
                session.id,
                &session.history,
                session.history.len() as u64,
                &session.profile,
                &identity,
                &current_versions,
            )?;
            secrets::model_request(&prepared.projected_request, &self.secret_source)?;
            prepared.record.previous_checkpoint = stored
                .as_ref()
                .map(|checkpoint| checkpoint.view.checkpoint_id.clone());
            let checkpoint = crate::context_store::ContextCheckpoint {
                view: prepared.view,
                record: prepared.record,
            };
            self.store
                .commit(&[crate::boundary::StoreChange::ContextCheckpoint {
                    run_id: self.run.id,
                    checkpoint: Box::new(checkpoint.clone()),
                }])
                .await?;
            *state
                .view
                .lock()
                .map_err(|_| error("context_view", "Context view lock failed"))? =
                Some(checkpoint.view.clone());
            state.published.store(true, Ordering::Release);
            state.rebuild.store(true, Ordering::Release);
            // Rebuild from the committed host record. Model-returned control
            // state never replaces the current runtime policy.
            let rebuilt = context_builder::build(
                make_input(Some(&checkpoint.view)),
                self.model.request_counter(),
            )?;
            return dispatchable(rebuilt);
        }
        dispatchable(built)
    }

    fn context_versions(&self) -> Result<Vec<crate::compaction::ContextVersion>, BoundaryError> {
        use sha2::{Digest, Sha256};
        let mut versions = Vec::new();
        if let Some(project) = &self.project_context {
            let project = project
                .lock()
                .map_err(|_| error("project_context_error", "Project context lock failed"))?;
            let snapshot = project.loader.snapshot(project.target.as_deref())?;
            versions.push(crate::compaction::ContextVersion {
                namespace: "project".into(),
                id: "workspace".into(),
                revision: 0,
                sha256: snapshot.fingerprint()?,
            });
        }
        for (id, text) in [
            ("preferences", &self.personal_context.preferences),
            ("memory", &self.personal_context.memory),
        ] {
            if let Some(text) = text {
                versions.push(crate::compaction::ContextVersion {
                    namespace: "personal".into(),
                    id: id.into(),
                    revision: 0,
                    sha256: Sha256::digest(text.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect(),
                });
            }
        }
        if let Some(skills) = &self.skill_state {
            versions.push(crate::compaction::ContextVersion {
                namespace: "skills".into(),
                id: "active".into(),
                revision: 0,
                sha256: skills.context_version()?,
            });
        }
        if let Some(memory) = &self.managed_memory {
            let snapshot = memory.store.snapshot()?;
            versions.push(crate::compaction::ContextVersion {
                namespace: "managed_memory".into(),
                id: "current".into(),
                revision: 0,
                sha256: crate::memory_context::digest(
                    format!(
                        "{}:{}",
                        snapshot.revision,
                        memory.epoch(&snapshot.forget_revision)
                    )
                    .as_bytes(),
                ),
            });
        }
        if let Some(state) = &self.context_state {
            let bytes = serde_json::to_vec(&state.budget)
                .map_err(|_| error("context_budget", "Context budget cannot be fingerprinted"))?;
            versions.push(crate::compaction::ContextVersion {
                namespace: "budget".into(),
                id: "execution".into(),
                revision: 0,
                sha256: Sha256::digest(&bytes)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect(),
            });
        }
        let prompt = format!(
            "{}\n{}\n{}",
            crate::prompt::VERSION,
            crate::compaction::COMPACT_INSTRUCTIONS,
            crate::compaction::CONTINUATION_INSTRUCTIONS
        );
        versions.push(crate::compaction::ContextVersion {
            namespace: "prompt".into(),
            id: "compaction".into(),
            revision: 0,
            sha256: Sha256::digest(prompt.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        });
        Ok(versions)
    }
}

fn dispatchable(built: context_builder::BuiltContext) -> Result<ModelRequest, BoundaryError> {
    match built.decision {
        ContextDecision::Ready
        | ContextDecision::NeedsCompact {
            exceeds_input_capacity: false,
            ..
        } => Ok(built.request),
        ContextDecision::NeedsCompact { .. } | ContextDecision::ContextLimit { .. } => Err(error(
            "context_limit",
            "Required context exceeds the configured input capacity",
        )),
        ContextDecision::AwaitingToolResults { .. } => Err(error(
            "invalid_history",
            "Context contains an unfinished tool group",
        )),
    }
}

struct ModeBoundCompactionModel {
    inner: std::sync::Arc<dyn crate::boundary::Model>,
    modes: crate::mode::ModeController,
    memory: Option<(
        std::sync::Arc<super::managed_memory::MemoryState>,
        super::managed_memory::MemoryFrame,
    )>,
    skills: Option<(std::sync::Arc<super::skills::SkillState>, String)>,
}
impl ModeBoundCompactionModel {
    fn check_sources(&self) -> Result<(), BoundaryError> {
        if let Some((memory, frame)) = &self.memory {
            memory.check_frame(frame)?;
        }
        if let Some((skills, version)) = &self.skills
            && skills.context_version()? != *version
        {
            return Err(error(
                "skill_context_changed",
                "Skill context changed before compaction dispatch",
            ));
        }
        Ok(())
    }
}
impl crate::boundary::Model for ModeBoundCompactionModel {
    fn check_start(&self, request: &ModelRequest) -> Result<(), BoundaryError> {
        if self.modes.snapshot()?.pending.is_some() {
            return Err(error(
                "compaction_mode_changed",
                "A queued work-mode change must be applied before another model call",
            ));
        }
        self.check_sources()?;
        self.inner.check_start(request)
    }
    fn request_counter(&self) -> Option<&dyn context_builder::RequestCounter> {
        self.inner.request_counter()
    }
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn crate::boundary::Cancellation,
    ) -> crate::boundary::Operation<'a, Box<dyn crate::boundary::ModelStream>> {
        Box::pin(async move {
            self.check_sources()?;
            self.inner.start(request, cancel).await
        })
    }
}
