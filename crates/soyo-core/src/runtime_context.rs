//! Request projection at the shared runtime entry, before provider dispatch.
use super::{AgentRuntime, error, secrets};
use crate::{
    boundary::ModelRequest,
    context_builder::{
        self, BuildInput, ContextBudget, ContextDecision, ContextView, CountingIdentity, ItemRange,
        ResultProjection,
    },
    contract::{BoundaryError, HistoryItem, Session, ToolResult},
    output_store::{OutputMetadata, OutputStream},
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

/// Host fields retained when a long Web/Browser result is projected to
/// OutputStore references. MCP and exec projections keep their existing shape;
/// the browser model must still receive its opaque page/document/element
/// references, and web results keep their origin and result metadata.
fn retained_host_fields(outputs: &[OutputMetadata]) -> &'static [&'static str] {
    let browser = outputs
        .iter()
        .any(|output| output.scope.stream == OutputStream::Browser);
    let web = outputs
        .iter()
        .any(|output| output.scope.stream == OutputStream::Web);
    if browser {
        &[
            "source",
            "page",
            "document",
            "document_revision",
            "url",
            "title",
            "text",
            "elements",
            "truncated",
        ]
    } else if web {
        &[
            "source",
            "final_url",
            "status",
            "content_type",
            "redirects",
            "body",
            "result_count",
            "truncated",
        ]
    } else {
        &[]
    }
}

fn project_tool_result(result: &ToolResult, outputs: &[OutputMetadata]) -> ToolResult {
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
        for key in retained_host_fields(outputs) {
            if let Some(value) = source.get(*key) {
                data[*key] = value.clone();
            }
        }
    }
    ToolResult {
        ok: result.ok,
        error: result.error.clone(),
        truncated: true,
        data: Some(data),
        // Keep host GC pins and eligible images across output projection.
        media_refs: result.media_refs.clone(),
    }
}

// Reuse one bounded media snapshot across counting and dispatch.
type MediaKey = Vec<(bool, Vec<String>)>;
struct ProjectedMediaCounter<'a> {
    runtime: &'a AgentRuntime,
    session_id: crate::contract::SessionId,
    inner: Option<&'a dyn context_builder::RequestCounter>,
    snapshot: Mutex<Option<(MediaKey, std::sync::Arc<crate::media::MediaAttachments>)>>,
}
impl ProjectedMediaCounter<'_> {
    fn attach(&self, mut request: ModelRequest) -> Result<ModelRequest, BoundaryError> {
        if request.purpose != crate::contract::ModelPurpose::Execution {
            return Ok(request);
        }
        let key: MediaKey = request
            .history
            .iter()
            .filter_map(|item| {
                let (user, references) = match item {
                    HistoryItem::UserText(text) => (
                        true,
                        crate::media::text_references(text)
                            .into_iter()
                            .collect::<Vec<_>>(),
                    ),
                    HistoryItem::ToolResult { result, .. } => (false, result.media_refs.clone()),
                    _ => return None,
                };
                (!references.is_empty()).then_some((user, references))
            })
            .collect();
        let mut slot = self
            .snapshot
            .lock()
            .map_err(|_| error("media_unavailable", "Media snapshot lock failed"))?;
        if slot.as_ref().is_none_or(|(cached, _)| cached != &key) {
            // Drop the previous bounded payload before loading a new projection.
            *slot = None;
            let media = crate::media::load_history(
                &request.history,
                self.runtime.media_store.as_ref(),
                self.session_id,
                self.runtime.media_capabilities,
            )?;
            *slot = Some((key, std::sync::Arc::new(media)));
        }
        if let Some((_, media)) = slot.as_ref() {
            request.media = media.clone();
        }
        Ok(request)
    }
}
impl context_builder::RequestCounter for ProjectedMediaCounter<'_> {
    fn count(
        &self,
        request: &ModelRequest,
        identity: &CountingIdentity,
        fingerprint: &str,
    ) -> Result<context_builder::CountEvidence, BoundaryError> {
        let hydrated = self.attach(request.clone())?;
        self.inner
            .ok_or_else(|| error("context_counter", "Request counter is unavailable"))?
            .count(&hydrated, identity, fingerprint)
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
        let media_counter = ProjectedMediaCounter {
            runtime: self,
            session_id: session.id,
            inner: self.model.request_counter(),
            snapshot: Mutex::new(None),
        };
        let counter = media_counter
            .inner
            .map(|_| &media_counter as &dyn context_builder::RequestCounter);
        let Some(state) = &self.context_state else {
            return media_counter.attach(request);
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
                let replacement = project_tool_result(result, outputs);
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
                header["soyo_runtime_context_v1"]["todos"] = serde_json::json!([]);
                header["soyo_runtime_context_v1"]["retired_memory_dependent_todos"] =
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
        let checkpoint_section = format!(
            "\n# Host-selected context checkpoint\n{}\n",
            serde_json::json!({"checkpoint_selected_by_host":view.is_some()})
        );
        if let Some(index) = dynamic.find("# Dynamic request context\n") {
            dynamic.insert_str(index, &checkpoint_section);
        } else {
            dynamic.push_str(&checkpoint_section);
        }
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
        let built = context_builder::build(make_input(view.as_ref()), counter)?;
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
                    counter,
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
            let rebuilt = context_builder::build(make_input(Some(&checkpoint.view)), counter)?;
            return media_counter.attach(dispatchable(rebuilt)?);
        }
        media_counter.attach(dispatchable(built)?)
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

#[cfg(test)]
mod projection_tests {
    use super::*;
    use crate::{
        boundary::ToolDefinition,
        context_builder::{BuildInput, ContextBudget, CountingIdentity, ResultProjection},
        contract::{ModelProtocol, ModelPurpose, RunId, SessionId, ToolCall},
        output_store::{CaptureCompletion, OutputCoverage, OutputRange, OutputScope},
    };
    use serde_json::json;

    fn metadata(call_id: &str, stream: OutputStream) -> OutputMetadata {
        OutputMetadata {
            output_ref: format!("{call_id}_output"),
            scope: OutputScope {
                session_id: SessionId::new(),
                run_id: RunId::new(),
                call_id: call_id.into(),
                stream,
            },
            capture_limit_bytes: 256 * 1024,
            total_bytes_seen: 32 * 1024,
            retained_bytes: 8 * 1024,
            retained_source_bytes: 32 * 1024,
            retained_ranges: vec![OutputRange {
                start: 0,
                end: 32 * 1024,
            }],
            gaps: Vec::new(),
            artifact_complete: false,
            completion: CaptureCompletion::LimitReached,
            coverage: OutputCoverage::RetainedRangesOnly,
            withheld_reason: None,
            preview_head: "preview".into(),
            preview_tail: "tail".into(),
            sha256: "digest".into(),
        }
    }

    #[test]
    fn long_browser_projection_retains_interactive_references() {
        let elements = (0..40)
            .map(|index| {
                json!({
                    "element_ref": format!("el_{index:064}"),
                    "role": "button",
                    "name": "x".repeat(600),
                })
            })
            .collect::<Vec<_>>();
        let result = ToolResult {
            ok: true,
            error: None,
            truncated: true,
            data: Some(json!({
                "source": "untrusted_browser",
                "page": "page_abc",
                "document": "doc_abc",
                "document_revision": 3,
                "url": "https://example.com/page",
                "title": "Example",
                "elements": elements,
            })),
            media_refs: vec!["media_fixture".into()],
        };
        assert!(
            serde_json::to_vec(&result).unwrap().len() > 16 * 1024,
            "fixture must exceed the projection threshold"
        );
        let call_id = "call-browser".to_owned();
        let item = HistoryItem::ToolResult {
            call_id: call_id.clone(),
            result: result.clone(),
        };
        let outputs = vec![metadata(&call_id, OutputStream::Browser)];
        let projection = ResultProjection {
            source_index: 1,
            source_item_sha256: context_builder::item_digest(&item).unwrap(),
            call_id: call_id.clone(),
            replacement: project_tool_result(&result, &outputs),
            outputs: outputs.clone(),
        };
        let request = ModelRequest {
            request_id: "request-browser".into(),
            purpose: ModelPurpose::Execution,
            profile: "fixture".into(),
            instructions: "fixture".into(),
            dynamic_instructions: None,
            max_output_tokens: Some(100),
            history: vec![
                HistoryItem::ToolCall(ToolCall {
                    call_id: call_id.clone(),
                    name: "browser_snapshot".into(),
                    arguments: "{}".into(),
                }),
                item,
            ],
            media: Default::default(),
            tools: vec![ToolDefinition {
                name: "browser_snapshot".into(),
                description: "fixture".into(),
                input_schema: json!({"type":"object","properties":{}}),
            }],
        };
        let identity = CountingIdentity {
            protocol: ModelProtocol::ChatCompletions,
            model: "fixture-model".into(),
            adapter_version: "fixture-v1".into(),
        };
        let built = context_builder::build(
            BuildInput {
                request: &request,
                identity: &identity,
                budget: ContextBudget::new(100_000, 100, 100),
                source_history: &request.history,
                history_offset: 0,
                revision: 1,
                committed_view: None,
                protected_ranges: &[],
                output_projections: std::slice::from_ref(&projection),
                group_retirements: &[],
            },
            None,
        )
        .unwrap();
        let HistoryItem::ToolResult { result, .. } = built
            .request
            .history
            .iter()
            .find(|item| matches!(item, HistoryItem::ToolResult { .. }))
            .expect("projection did not produce a tool result")
        else {
            panic!("projection did not produce a tool result");
        };
        let data = result.data.as_ref().unwrap();
        assert_eq!(data["source"], "untrusted_browser");
        assert_eq!(data["page"], "page_abc");
        assert_eq!(data["document"], "doc_abc");
        assert_eq!(data["document_revision"], 3);
        assert_eq!(data["url"], "https://example.com/page");
        assert_eq!(data["elements"].as_array().unwrap().len(), 40);
        assert_eq!(result.media_refs, vec!["media_fixture".to_owned()]);
        assert!(
            data["output_refs"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
    }

    #[test]
    fn withheld_capture_projection_retains_the_inline_fallback() {
        for (stream, field) in [(OutputStream::Web, "body"), (OutputStream::Browser, "text")] {
            let mut data = serde_json::Map::new();
            data.insert("source".into(), json!("untrusted_source"));
            data.insert(field.into(), json!("readable fallback"));
            let result = ToolResult {
                ok: true,
                error: None,
                truncated: true,
                data: Some(serde_json::Value::Object(data)),
                media_refs: Vec::new(),
            };
            let mut output = metadata("call-withheld", stream);
            output.total_bytes_seen = 32;
            output.retained_bytes = 0;
            output.coverage = OutputCoverage::Withheld;
            output.withheld_reason = Some(crate::output_store::WithheldReason::CaptureDisabled);
            let projected = project_tool_result(&result, &[output]);
            assert_eq!(projected.data.unwrap()[field], "readable fallback");
        }
    }

    #[test]
    fn exec_projection_does_not_gain_browser_or_web_fields() {
        let mut result = ToolResult {
            ok: true,
            error: None,
            truncated: true,
            data: Some(json!({
                "stdout": "x".repeat(20 * 1024),
                "page": "page_should_not_leak",
                "final_url": "https://example.com",
            })),
            media_refs: Vec::new(),
        };
        result.data.as_mut().unwrap()["stdout"] = json!("x".repeat(20 * 1024));
        let outputs = vec![metadata("call-exec", OutputStream::Stdout)];
        let projected = project_tool_result(&result, &outputs);
        let data = projected.data.unwrap();
        assert!(data.get("page").is_none());
        assert!(data.get("final_url").is_none());
        assert!(data.get("stdout").is_none());
    }
}
