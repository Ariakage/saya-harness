//! Managed Skill tools share the runtime's current approvals and durable flow.
use super::{AgentRuntime, CallProgress, error, failed, project_context_is_current};
use crate::{
    approvals::{Authorization, AuthorizationInput, OneShotPermit},
    boundary::{StoreChange, ToolContext, ToolDefinition},
    contract::{
        ApprovalSource, BoundaryError, EventPayload, HistoryItem, Session, ToolCall, ToolResult,
    },
    dispatch::DispatchReview,
    permissions::Operation,
    plugin_store::PluginLease,
    skills::{
        ActiveSkillSet, ResourceCursor, SkillAccessTarget, SkillCatalogCursor, SkillCatalogPage,
        SkillManager, SkillSelection,
    },
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub(super) struct SkillState {
    manager: Mutex<SkillManager>,
    plugin_leases: Mutex<Vec<Arc<PluginLease>>>,
}
impl SkillState {
    pub(super) fn clear_plugin_leases(&self) -> Result<(), BoundaryError> {
        self.manager
            .lock()
            .map_err(|_| error("skill_state", "Skill manager lock failed"))?
            .retain_plugin_leases(&BTreeSet::new())?;
        self.plugin_leases
            .lock()
            .map_err(|_| error("skill_state", "Plugin Skill lease lock failed"))?
            .clear();
        Ok(())
    }
    pub(super) fn context_version(&self) -> Result<String, BoundaryError> {
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| error("skill_state", "Skill manager lock failed"))?;
        manager
            .revalidate(&AtomicBool::new(false))
            .map(|state| state.version_sha256)
    }
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let identity = json!({"name":{"type":"string","minLength":1,"maxLength":193},"source_sha256":{"type":"string","pattern":"^[a-f0-9]{64}$"}});
        let mut definitions = Vec::new();
        for (name, description) in [
            (
                "skill_activate",
                "Activate a discovered Skill by its catalog name and exact source_sha256. Its selected document becomes lower-trust context on the next model request. This grants no tools or execution permission.",
            ),
            (
                "skill_deactivate",
                "Stop using the selected Skill as context. Supply its catalog name and exact source_sha256. No cleanup scripts are executed.",
            ),
        ] {
            definitions.push(ToolDefinition {name:name.into(),description:description.into(),input_schema:json!({"type":"object","properties":identity,"required":["name","source_sha256"],"additionalProperties":false})});
        }
        let mut properties = identity;
        properties["resource"] = json!({"type":"string","minLength":1,"maxLength":1024});
        properties["expected_resource_sha256"] =
            json!({"type":"string","pattern":"^[a-f0-9]{64}$"});
        properties["cursor"] = json!({"type":"object","properties":{"byte_offset":{"type":"integer","minimum":0},"view_sha256":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"required":["byte_offset","view_sha256"],"additionalProperties":false});
        properties["max_bytes"] =
            json!({"type":"integer","minimum":1,"maximum":65536,"default":4096});
        definitions.push(ToolDefinition {name:"skill_read_resource".into(),description:"Read a bounded UTF-8 text page relative to an active Skill. Paths stay inside that Skill's catalog root. This never executes scripts. Preserve the returned source hash and cursor for subsequent pages.".into(),input_schema:json!({"type":"object","properties":properties,"required":["name","source_sha256","resource"],"additionalProperties":false})});
        definitions.push(ToolDefinition {name:"skill_catalog".into(),description:"List a bounded page of discovered Skill metadata and source hashes. This does not load inactive Skill bodies. Follow next_cursor to discover further entries.".into(),input_schema:json!({"type":"object","properties":{
            "cursor":{"type":"object","properties":{"offset":{"type":"integer","minimum":0},"view_sha256":{"type":"string","pattern":"^[a-f0-9]{64}$"}},"required":["offset","view_sha256"],"additionalProperties":false},
            "max_items":{"type":"integer","minimum":1,"maximum":64,"default":32},"max_bytes":{"type":"integer","minimum":1024,"maximum":16384,"default":16384}},"additionalProperties":false})});
        definitions
    }
    fn prepare(
        &self,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<PreparedSkill, BoundaryError> {
        if call.arguments.len() > 16 * 1024 {
            return Err(error(
                "invalid_args",
                "Skill arguments exceed their byte limit",
            ));
        }
        let manager = self
            .manager
            .lock()
            .map_err(|_| error("skill_state", "Skill manager lock failed"))?;
        manager.validate_workspace(&context.workspace)?;
        let action = match call.name.as_str() {
            "skill_activate" | "skill_deactivate" => {
                let args: IdentityArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| error("invalid_args", "Skill identity arguments are invalid"))?;
                let selection = manager.resolve(&args.name, &args.source_sha256, None)?;
                if call.name == "skill_activate" {
                    SkillAction::Activate(selection)
                } else {
                    SkillAction::Deactivate(selection)
                }
            }
            "skill_read_resource" => {
                let args: ResourceArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| error("invalid_args", "Skill resource arguments are invalid"))?;
                if args
                    .max_bytes
                    .is_some_and(|bytes| bytes == 0 || bytes > 64 * 1024)
                {
                    return Err(error(
                        "invalid_args",
                        "Skill resource page limit is invalid",
                    ));
                }
                let selection =
                    manager.resolve(&args.name, &args.source_sha256, Some(&args.resource))?;
                SkillAction::Resource { selection, args }
            }
            "skill_catalog" => {
                let args: CatalogArgs = serde_json::from_str(&call.arguments)
                    .map_err(|_| error("invalid_args", "Skill catalog arguments are invalid"))?;
                let page = manager.catalog_page(
                    args.cursor.as_ref(),
                    args.max_items.unwrap_or(32),
                    args.max_bytes.unwrap_or(16 * 1024),
                )?;
                let mut targets = Vec::new();
                for entry in &page.entries {
                    for target in manager
                        .resolve(&entry.name, &entry.source_sha256, None)?
                        .targets()
                    {
                        if !targets.contains(target) {
                            targets.push(target.clone());
                        }
                    }
                }
                SkillAction::Catalog { page, targets }
            }
            _ => return Err(error("unknown_tool", "Unknown Skill operation")),
        };
        let targets = action.targets();
        let leases = self
            .plugin_leases
            .lock()
            .map_err(|_| error("skill_state", "Plugin Skill lease lock failed"))?;
        let mut paths = Vec::new();
        for target in &targets {
            let path = match target {
                SkillAccessTarget::ProjectFile { path, .. } => path.clone(),
                SkillAccessTarget::PluginData {
                    package_digest,
                    lease_id,
                    path,
                    ..
                } => {
                    let lease = leases
                        .iter()
                        .find(|lease| {
                            lease.record().lease_id == *lease_id
                                && lease.record().digest == *package_digest
                        })
                        .ok_or_else(|| {
                            error(
                                "skill_source_changed",
                                "Plugin Skill lease is no longer held by this Run",
                            )
                        })?;
                    lease.check_active()?;
                    if lease.record().session_id != context.run.session_id
                        || lease.record().run_id != context.run.id
                    {
                        return Err(error("skill_scope", "Plugin Skill belongs to another Run"));
                    }
                    crate::plugin_manifest::validate_relative_path(path)?;
                    lease.package_path().join(path)
                }
            };
            let file = Operation::file(&context.workspace, "read_file", &path)?;
            let path = file
                .file_target()
                .ok_or_else(|| error("skill_scope", "Skill path could not be normalized"))?
                .to_path_buf();
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        let metadata = match &action {
            SkillAction::Catalog { page, .. } => {
                json!({"kind":"skill_catalog","view_sha256":page.view_sha256,"offset":page.offset,"count":page.entries.len(),"targets":targets})
            }
            _ => {
                json!({"kind":call.name,"selection_sha256":action.selection().map(SkillSelection::binding_sha256),"targets":targets})
            }
        };
        let encoded = serde_json::to_vec(&metadata)
            .map_err(|_| error("skill_state", "Skill review binding cannot be encoded"))?;
        let binding = Sha256::digest(encoded)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(PreparedSkill {
            action,
            paths,
            binding,
            metadata,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityArgs {
    name: String,
    source_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceArgs {
    name: String,
    source_sha256: String,
    resource: String,
    #[serde(default)]
    expected_resource_sha256: Option<String>,
    #[serde(default)]
    cursor: Option<ResourceCursor>,
    #[serde(default)]
    max_bytes: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogArgs {
    #[serde(default)]
    cursor: Option<SkillCatalogCursor>,
    #[serde(default)]
    max_items: Option<usize>,
    #[serde(default)]
    max_bytes: Option<usize>,
}
enum SkillAction {
    Activate(SkillSelection),
    Deactivate(SkillSelection),
    Resource {
        selection: SkillSelection,
        args: ResourceArgs,
    },
    Catalog {
        page: SkillCatalogPage,
        targets: Vec<SkillAccessTarget>,
    },
}
impl SkillAction {
    fn selection(&self) -> Option<&SkillSelection> {
        match self {
            Self::Activate(selection)
            | Self::Deactivate(selection)
            | Self::Resource { selection, .. } => Some(selection),
            Self::Catalog { .. } => None,
        }
    }
    fn targets(&self) -> Vec<SkillAccessTarget> {
        match self {
            Self::Catalog { targets, .. } => targets.clone(),
            _ => self
                .selection()
                .map(|selection| selection.targets().to_vec())
                .unwrap_or_default(),
        }
    }
}
struct PreparedSkill {
    action: SkillAction,
    paths: Vec<PathBuf>,
    binding: String,
    metadata: Value,
}
struct PermitLease(Arc<OneShotPermit>);
impl Drop for PermitLease {
    fn drop(&mut self) {
        let _ = self.0.finish_managed();
    }
}

impl AgentRuntime {
    /// Attach a host-selected manager. Model arguments never choose its roots.
    pub fn with_skill_manager(mut self, manager: SkillManager) -> Result<Self, BoundaryError> {
        if self.project_context.is_none() || self.context_state.is_none() {
            return Err(error(
                "context_config",
                "Skills require project discovery and the host ContextBuilder",
            ));
        }
        self.skill_state = Some(Arc::new(SkillState {
            manager: Mutex::new(manager),
            plugin_leases: Mutex::new(Vec::new()),
        }));
        Ok(self)
    }
    /// Replace the host-acquired plugin catalog and promptly release removed
    /// package references before process/view cleanup tries to finish leases.
    pub fn set_plugin_skill_leases(
        &self,
        leases: Vec<Arc<PluginLease>>,
    ) -> Result<(), BoundaryError> {
        let Some(state) = &self.skill_state else {
            if leases.is_empty() {
                return Ok(());
            }
            return Err(error("skill_state", "Skill manager is unavailable"));
        };
        if leases.len() > 128
            || leases.iter().any(|lease| {
                lease.record().session_id != self.run.session_id
                    || lease.record().run_id != self.run.id
            })
        {
            return Err(error(
                "skill_scope",
                "Plugin Skill leases do not match this Run",
            ));
        }
        let ids = leases
            .iter()
            .map(|lease| lease.record().lease_id.clone())
            .collect::<BTreeSet<_>>();
        let mut manager = state
            .manager
            .lock()
            .map_err(|_| error("skill_state", "Skill manager lock failed"))?;
        manager.retain_plugin_leases(&ids)?;
        *state
            .plugin_leases
            .lock()
            .map_err(|_| error("skill_state", "Plugin Skill lease lock failed"))? = leases;
        Ok(())
    }
    pub(super) async fn skill_frame(
        &self,
        session: &Session,
    ) -> Result<Option<ActiveSkillSet>, BoundaryError> {
        let Some(state) = self.skill_state.clone() else {
            return Ok(None);
        };
        if session.id != self.run.session_id {
            return Err(error(
                "skill_scope",
                "Skill context session does not match this Run",
            ));
        }
        let project = self
            .project_context
            .clone()
            .ok_or_else(|| error("context_config", "Project discovery is unavailable"))?;
        let workspace = session.workspace.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            let snapshot = {
                let project = project
                    .lock()
                    .map_err(|_| error("project_context_error", "Project discovery lock failed"))?;
                project.loader.snapshot(project.target.as_deref())?
            };
            let mut manager = state
                .manager
                .lock()
                .map_err(|_| error("skill_state", "Skill manager lock failed"))?;
            manager.validate_workspace(&workspace)?;
            let leases = state
                .plugin_leases
                .lock()
                .map_err(|_| error("skill_state", "Plugin Skill lease lock failed"))?;
            manager.refresh(&snapshot, &leases, &worker_cancel)?;
            manager.revalidate(&worker_cancel).map(Some)
        });
        tokio::select! {
            result=&mut worker=>result,
            ()=self.cancel.cancelled()=>{cancel.store(true,Ordering::Release);worker.await},
            ()=tokio::time::sleep(self.limits.tool_timeout.min(self.budget.remaining()?))=>{cancel.store(true,Ordering::Release);worker.await},
        }.map_err(|_|error("skill_io","Skill context worker failed"))?
    }
    pub(super) async fn execute_skill(
        &self,
        session: &mut Session,
        call: &ToolCall,
        context: &ToolContext,
        progress: &mut CallProgress,
    ) -> Result<ToolResult, BoundaryError> {
        let Some(state) = self.skill_state.clone() else {
            return self
                .save_local(
                    session,
                    call,
                    failed("unknown_tool", "Skills are unavailable"),
                )
                .await;
        };
        let saved = self
            .store
            .load_session(session.id)
            .await?
            .ok_or_else(|| error("storage_error", "Skill evidence session is unavailable"))?;
        if &saved != session {
            return Err(error(
                "session_conflict",
                "Skill evidence differs from persisted history",
            ));
        }
        let prepare_state = state.clone();
        let call_copy = call.clone();
        let context_copy = context.clone();
        let prepared =
            tokio::task::spawn_blocking(move || prepare_state.prepare(&call_copy, &context_copy))
                .await
                .map_err(|_| error("skill_io", "Skill preparation worker failed"))?;
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => {
                return self
                    .save_local(session, call, failed(&failure.0.code, &failure.0.message))
                    .await;
            }
        };
        self.check()?;
        let operation = Operation::managed(
            &call.name,
            prepared.paths.clone(),
            prepared.binding.clone(),
            true,
            prepared.metadata.clone(),
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
                            failed("needs_confirmation", "Skill confirmation is unavailable"),
                        )
                        .await;
                }
            },
            other => other,
        };
        let permit = match authorization {
            Authorization::Permit(permit) => permit,
            Authorization::Denied(_) => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("permission_denied", "Skill operation was denied"),
                    )
                    .await;
            }
            Authorization::Stale => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("stale_approval", "Skill authorization changed"),
                    )
                    .await;
            }
            _ => {
                return self
                    .save_local(
                        session,
                        call,
                        failed("needs_confirmation", "Skill confirmation is required"),
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
            .ok_or_else(|| error("storage_error", "Skill dispatch session is unavailable"))?;
        if &saved != session {
            return Err(error(
                "session_conflict",
                "Skill dispatch differs from persisted history",
            ));
        }
        let project = self.project_context.clone();
        let call_copy = call.clone();
        let context_copy = context.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            let _lease = PermitLease(permit.clone());
            let current = state.prepare(&call_copy, &context_copy)?;
            if current.binding != prepared.binding || current.paths != prepared.paths {
                return Err(error(
                    "stale_approval",
                    "Skill source or approval targets changed",
                ));
            }
            if let Some(project) = project
                && !project_context_is_current(&project, &call_copy)?
            {
                return Err(error(
                    "project_context_changed",
                    "Project discovery changed before Skill dispatch",
                ));
            }
            for _ in 0..2 {
                if !permit.authorize_managed(&call_copy, &context_copy, &operation)? {
                    return Err(error("stale_approval", "Skill dispatch grant changed"));
                }
            }
            let mut manager = state
                .manager
                .lock()
                .map_err(|_| error("skill_state", "Skill manager lock failed"))?;
            manager.validate_workspace(&context_copy.workspace)?;
            if worker_cancel.load(Ordering::Acquire) {
                return Err(error("cancelled", "Skill operation was cancelled"));
            }
            let data = match prepared.action {
                SkillAction::Activate(selection) => {
                    let active = manager.activate(&selection, &worker_cancel)?;
                    json!({"skill":active.metadata,"active":true,"view_sha256":active.view_sha256,"redacted":active.redacted,"context_injection":"next_model_request"})
                }
                SkillAction::Deactivate(selection) => {
                    json!({"name":selection.name(),"active":false,"removed":manager.deactivate_prepared(&selection,&worker_cancel)?})
                }
                SkillAction::Resource { selection, args } => {
                    serde_json::to_value(manager.read_resource(
                        &selection,
                        args.expected_resource_sha256.as_deref(),
                        args.cursor.as_ref(),
                        args.max_bytes.unwrap_or(4096),
                        &worker_cancel,
                    )?)
                    .map_err(|_| error("skill_state", "Skill resource result cannot be encoded"))?
                }
                SkillAction::Catalog { page, .. } => serde_json::to_value(page)
                    .map_err(|_| error("skill_state", "Skill catalog result cannot be encoded"))?,
            };
            Ok::<_, BoundaryError>(ToolResult {
                ok: true,
                data: Some(data),
                error: None,
                truncated: false,
            })
        });
        let outcome=tokio::select! {
            result=&mut worker=>result,
            ()=self.cancel.cancelled()=>{cancel.store(true,Ordering::Release);worker.await},
            ()=tokio::time::sleep(self.limits.tool_timeout.min(self.budget.remaining()?))=>{cancel.store(true,Ordering::Release);worker.await},
        }.map_err(|_|error("skill_io","Skill dispatch worker failed"))?;
        let result = match outcome {
            Ok(result) => result,
            Err(failure) => failed(&failure.0.code, &failure.0.message),
        };
        let result = super::secrets::redact(result, &self.secret_source)?;
        let item = HistoryItem::ToolResult {
            call_id: call.call_id.clone(),
            result: result.clone(),
        };
        if let Err(mut failure) = self
            .store
            .commit(&[
                StoreChange::ToolFinished {
                    run_id: self.run.id,
                    call_id: call.call_id.clone(),
                    result: result.clone(),
                },
                StoreChange::History {
                    session_id: session.id,
                    item: item.clone(),
                },
            ])
            .await
        {
            failure.2 = Some(Box::new(result));
            return Err(failure);
        }
        session.history.push(item);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        approvals::Preauthorizations,
        boundary::{
            Cancellation, Model, ModelRequest, ModelStream, Operation as FutureOperation, Store,
        },
        context_builder::ContextBudget,
        contract::{ModelProtocol, PermissionMode, Run, RunId, SessionId, WorkMode},
        memory::MemoryStore,
        permissions::{PermissionConfig, Policy},
        project_context::ProjectContext,
        review::ReviewConfig,
        runtime::{ApprovalHandler, RuntimeLimits, RuntimeSetup},
        skills::SkillLimits,
        tools::{
            exec::ExecConfig,
            read::{ReadLimits, ReadTools},
        },
    };
    use std::{fs, path::Path, sync::atomic::AtomicUsize};
    struct Never;
    impl Cancellation for Never {
        fn is_cancelled(&self) -> bool {
            false
        }
        fn cancelled(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }
    struct NoModel;
    impl Model for NoModel {
        fn start<'a>(
            &'a self,
            _: &'a ModelRequest,
            _: &'a dyn Cancellation,
        ) -> FutureOperation<'a, Box<dyn ModelStream>> {
            Box::pin(async {
                Err(error(
                    "fixture_model",
                    "Skill dispatch must not call a model",
                ))
            })
        }
    }
    struct Confirmation {
        answer: bool,
        calls: AtomicUsize,
        change: Option<PathBuf>,
    }
    impl ApprovalHandler for Confirmation {
        fn confirm<'a>(
            &'a self,
            _: &'a crate::contract::ApprovalRequest,
            _: &'a dyn Cancellation,
        ) -> FutureOperation<'a, Option<bool>> {
            Box::pin(async {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if let Some(path) = &self.change {
                    fs::write(
                        path,
                        "---\nname: guide\ndescription: Changed\n---\nreplacement",
                    )
                    .unwrap();
                }
                Ok(Some(self.answer))
            })
        }
    }
    struct Fixture {
        root: tempfile::TempDir,
        workspace: PathBuf,
        directory: PathBuf,
        runtime: AgentRuntime,
        session: Session,
        store: Arc<MemoryStore>,
    }
    impl Fixture {
        async fn new(mode: WorkMode, permissions: PermissionMode, rules: Option<&str>) -> Self {
            let root = tempfile::tempdir().unwrap();
            let workspace = fs::canonicalize(root.path()).unwrap();
            let directory = workspace.join(".saya/skills/guide");
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("SKILL.md"),
                "---\nname: guide\ndescription: Fixture metadata\n---\nSelected source document.",
            )
            .unwrap();
            fs::write(directory.join("notes.txt"), "Resource text.").unwrap();
            let store = Arc::new(MemoryStore::default());
            let session = Session {
                id: SessionId::new(),
                workspace: workspace.clone(),
                work_mode: mode,
                profile: "fixture".into(),
                protocol: ModelProtocol::ChatCompletions,
                model: "fixture".into(),
                history: vec![],
            };
            let run = Run {
                id: RunId::new(),
                session_id: session.id,
                work_mode: mode,
                permissions,
            };
            let config = rules
                .map(|text| {
                    PermissionConfig::from_toml(&text.replace("ROOT", workspace.to_str().unwrap()))
                        .unwrap()
                })
                .unwrap_or_default();
            let secrets = Arc::new(crate::credentials::SecretRegistry::default());
            let runtime = AgentRuntime::new(RuntimeSetup {
                run: run.clone(),
                model: Arc::new(NoModel),
                store: store.clone(),
                policy: Arc::new(Policy::new(&workspace, config).unwrap()),
                files: Arc::new(
                    ReadTools::new(&workspace, &[], false, ReadLimits::default()).unwrap(),
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
            .unwrap()
            .with_context_budget(ContextBudget::new(128_000, 4096, 1024))
            .unwrap()
            .with_secret_registry(secrets.clone())
            .unwrap()
            .with_project_context(ProjectContext::new(&workspace, None).unwrap())
            .unwrap()
            .with_skill_manager(
                SkillManager::new(&workspace, None, secrets, SkillLimits::default()).unwrap(),
            )
            .unwrap();
            store
                .commit(&[
                    StoreChange::Session(session.clone()),
                    StoreChange::RunStarted(run),
                ])
                .await
                .unwrap();
            let mut fixture = Self {
                root,
                workspace,
                directory,
                runtime,
                session,
                store,
            };
            fixture.frame().await;
            fixture
        }
        async fn frame(&mut self) -> ActiveSkillSet {
            let frame = self
                .runtime
                .skill_frame(&self.session)
                .await
                .unwrap()
                .unwrap();
            let project = self.runtime.project_context.as_ref().unwrap();
            let mut project = project.lock().unwrap();
            let record = project
                .loader
                .snapshot(None)
                .unwrap()
                .record("fixture-request".into())
                .unwrap();
            project.provided = Some((None, record));
            frame
        }
        fn identity(&self) -> Value {
            let state = self.runtime.skill_state.as_ref().unwrap();
            let metadata = state.manager.lock().unwrap().catalog().unwrap().remove(0);
            json!({"name":metadata.name,"source_sha256":metadata.source_sha256})
        }
        async fn execute(&mut self, name: &str, args: Value) -> ToolResult {
            let call = ToolCall {
                call_id: format!("call-{}", self.session.history.len()),
                name: name.into(),
                arguments: args.to_string(),
            };
            let item = HistoryItem::ToolCall(call.clone());
            self.store
                .commit(&[StoreChange::History {
                    session_id: self.session.id,
                    item: item.clone(),
                }])
                .await
                .unwrap();
            self.session.history.push(item);
            let context = self.runtime.context(&self.session, self.session.work_mode);
            let mut progress = CallProgress::NotDispatched;
            self.runtime
                .execute_skill(&mut self.session, &call, &context, &mut progress)
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn plan_can_activate_page_resource_and_deactivate_with_durable_pairing() {
        let mut fixture = Fixture::new(WorkMode::Plan, PermissionMode::Auto, None).await;
        let frame = fixture.frame().await;
        assert!(frame.skills.is_empty());
        assert_eq!(frame.catalog.entries.len(), 1);
        assert!(
            !frame
                .context_data()
                .to_string()
                .contains("Selected source document.")
        );
        assert!(
            fixture
                .execute("skill_activate", fixture.identity())
                .await
                .ok
        );
        let frame = fixture.frame().await;
        assert_eq!(frame.skills.len(), 1);
        assert!(
            frame
                .context_data()
                .to_string()
                .contains("Selected source document.")
        );
        let mut args = fixture.identity();
        args["resource"] = json!("notes.txt");
        assert!(fixture.execute("skill_read_resource", args).await.ok);
        assert!(fixture.execute("skill_catalog", json!({})).await.ok);
        assert!(
            fixture
                .execute("skill_deactivate", fixture.identity())
                .await
                .ok
        );
        assert!(fixture.frame().await.skills.is_empty());
        assert_eq!(
            fixture
                .store
                .load_session(fixture.session.id)
                .await
                .unwrap()
                .unwrap(),
            fixture.session
        );
        assert_eq!(
            fs::read_to_string(fixture.directory.join("notes.txt")).unwrap(),
            "Resource text."
        );
        assert_eq!(
            fixture
                .session
                .history
                .iter()
                .filter(|item| matches!(item, HistoryItem::ToolResult { .. }))
                .count(),
            4
        );
    }

    #[tokio::test]
    async fn every_manual_skill_operation_requires_its_own_confirmation() {
        let mut fixture = Fixture::new(WorkMode::Work, PermissionMode::Manual, None).await;
        let handler = Arc::new(Confirmation {
            answer: true,
            calls: AtomicUsize::new(0),
            change: None,
        });
        fixture.runtime.confirmation = Some(handler.clone());
        assert!(
            fixture
                .execute("skill_activate", fixture.identity())
                .await
                .ok
        );
        assert!(fixture.execute("skill_catalog", json!({})).await.ok);
        let mut args = fixture.identity();
        args["resource"] = json!("notes.txt");
        assert!(fixture.execute("skill_read_resource", args).await.ok);
        assert!(
            fixture
                .execute("skill_deactivate", fixture.identity())
                .await
                .ok
        );
        assert_eq!(handler.calls.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn manual_denial_or_missing_confirmation_never_activates_a_document() {
        let mut fixture = Fixture::new(WorkMode::Work, PermissionMode::Manual, None).await;
        let result = fixture.execute("skill_activate", fixture.identity()).await;
        assert_eq!(result.error.unwrap().code, "needs_confirmation");
        assert!(fixture.frame().await.skills.is_empty());
        fixture.runtime.confirmation = Some(Arc::new(Confirmation {
            answer: false,
            calls: AtomicUsize::new(0),
            change: None,
        }));
        let result = fixture.execute("skill_activate", fixture.identity()).await;
        assert_eq!(result.error.unwrap().code, "permission_denied");
        assert!(fixture.frame().await.skills.is_empty());
    }

    #[tokio::test]
    async fn explicit_path_deny_applies_to_resource_and_document_even_under_bypass() {
        for permissions in [PermissionMode::Auto, PermissionMode::Bypass] {
            let mut fixture=Fixture::new(WorkMode::Code,permissions,Some("[[rules]]\neffect='deny'\ntool='skill_read_resource'\npath_glob='ROOT/.saya/skills/guide/notes.txt'\n")).await;
            assert!(
                fixture
                    .execute("skill_activate", fixture.identity())
                    .await
                    .ok
            );
            let mut args = fixture.identity();
            args["resource"] = json!("notes.txt");
            let result = fixture.execute("skill_read_resource", args).await;
            assert_eq!(result.error.unwrap().code, "permission_denied");
            assert!(fixture.frame().await.skills[0].resources.is_empty());
            let mut fixture = Fixture::new(
                WorkMode::Code,
                permissions,
                Some("[[rules]]\neffect='deny'\npath_glob='ROOT/.saya/skills/guide/SKILL.md'\n"),
            )
            .await;
            let result = fixture.execute("skill_activate", fixture.identity()).await;
            assert_eq!(result.error.unwrap().code, "permission_denied");
            assert!(fixture.frame().await.skills.is_empty());
        }
    }

    #[tokio::test]
    async fn source_changed_while_waiting_for_manual_approval_cannot_activate() {
        let mut fixture = Fixture::new(WorkMode::Work, PermissionMode::Manual, None).await;
        fixture.runtime.confirmation = Some(Arc::new(Confirmation {
            answer: true,
            calls: AtomicUsize::new(0),
            change: Some(fixture.directory.join("SKILL.md")),
        }));
        let result = fixture.execute("skill_activate", fixture.identity()).await;
        assert!(!result.ok);
        assert!(matches!(
            result.error.unwrap().code.as_str(),
            "project_context_changed" | "skill_source_changed"
        ));
        assert!(fixture.frame().await.skills.is_empty());
    }

    #[tokio::test]
    async fn skill_arguments_cannot_select_an_arbitrary_root_or_other_workspace() {
        let mut fixture = Fixture::new(WorkMode::Work, PermissionMode::Auto, None).await;
        let mut args = fixture.identity();
        args["root"] = json!("/tmp");
        assert_eq!(
            fixture
                .execute("skill_activate", args)
                .await
                .error
                .unwrap()
                .code,
            "invalid_args"
        );
        let state = fixture.runtime.skill_state.as_ref().unwrap();
        assert!(
            state
                .manager
                .lock()
                .unwrap()
                .validate_workspace(Path::new("/"))
                .is_err()
        );
        assert_eq!(state.definitions().len(), 4);
        assert!(fixture.root.path().exists());
        assert_eq!(fixture.workspace, fixture.session.workspace);
    }
}
