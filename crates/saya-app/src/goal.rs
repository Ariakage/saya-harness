//! Shared Goal planning and bounded Run orchestration.

use crate::{
    Launch, build_runtime,
    events::timestamp,
    session::{self, ResumeOptions},
};
use saya_core::{
    approvals::Preauthorizations,
    boundary::{Cancellation, Model, ModelRequest, ModelUpdate},
    config::Config,
    contract::{
        BoundaryError, ErrorInfo, HistoryItem, RunId, RunResult, RunStatus, SCHEMA_VERSION, Usage,
    },
    goal::{
        AcceptanceCriterion, GoalId, GoalLease, GoalPlan, GoalRecord, GoalRunSummary, GoalStatus,
        GoalStep, GoalStore, VerificationSpec, verify_file_criterion,
    },
    input::InputHandler,
    runtime::{ApprovalHandler, PersonalContext},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const PLAN_RESPONSE_LIMIT: usize = 256 * 1024;
const VERIFY_RESPONSE_LIMIT: usize = 16 * 1024;

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

async fn bounded<T>(
    future: impl std::future::Future<Output = Result<T, BoundaryError>>,
    cancel: &dyn Cancellation,
    timeout: Duration,
) -> Result<T, BoundaryError> {
    tokio::select! {biased;
        () = cancel.cancelled() => Err(failure("cancelled", "Goal model request cancelled")),
        result = tokio::time::timeout(timeout, future) => result.map_err(|_| failure("model_timeout", "Goal model request timed out"))?,
    }
}

async fn complete_text(
    model: Arc<dyn Model>,
    request: ModelRequest,
    config: &Config,
    cancel: &dyn Cancellation,
    limit: usize,
) -> Result<(String, Option<Usage>), BoundaryError> {
    model.check_start(&request)?;
    let connect = Duration::from_secs(config.limits.model_connect_timeout_seconds);
    let idle = Duration::from_secs(config.limits.model_idle_timeout_seconds);
    let total = Duration::from_secs(config.limits.model_timeout_seconds);
    let started = Instant::now();
    let mut stream = bounded(model.start(&request, cancel), cancel, connect).await?;
    loop {
        let remaining = if total.is_zero() {
            idle
        } else {
            idle.min(total.saturating_sub(started.elapsed()))
        };
        match bounded(stream.next(cancel), cancel, remaining).await? {
            Some(ModelUpdate::Finished { items, usage }) => {
                let mut text = String::new();
                for item in items {
                    match item {
                        HistoryItem::AssistantText(value) => {
                            if text.len().saturating_add(value.len()) > limit {
                                return Err(failure(
                                    "goal_model_limit",
                                    "Goal model response exceeded its byte limit",
                                ));
                            }
                            text.push_str(&value);
                        }
                        HistoryItem::ProviderItem { .. } => {}
                        _ => {
                            return Err(failure(
                                "goal_model_output",
                                "Goal model returned non-text content",
                            ));
                        }
                    }
                }
                return Ok((text, usage));
            }
            Some(
                ModelUpdate::TextDelta(_)
                | ModelUpdate::ThinkingDelta { .. }
                | ModelUpdate::Progress,
            ) => {}
            None => {
                return Err(failure(
                    "goal_model_output",
                    "Goal model stream ended without a completed response",
                ));
            }
        }
    }
}

fn planning_payload(record: &GoalRecord, existing: &[AcceptanceCriterion]) -> serde_json::Value {
    serde_json::json!({
        "objective": record.objective,
        "workspace": record.workspace,
        "mode": record.mode,
        "existing_acceptance": existing,
    })
}

fn selected_model(config: &Config, profile_name: &str) -> Result<Arc<dyn Model>, BoundaryError> {
    let profile = config.profiles.get(profile_name).ok_or_else(|| {
        failure(
            "invalid_goal",
            "Goal profile is not present in current configuration",
        )
    })?;
    super::model(config, profile_name, profile, false)
}

#[derive(Debug, Clone)]
pub struct GoalPlanOutcome {
    pub plan: GoalPlan,
    pub usage: Option<Usage>,
    pub request_count: u64,
}

/// Ask the execution profile for a structured plan. The result is parsed and
/// validated here but is not applied or confirmed by this operation.
pub async fn plan_goal(
    config: &Config,
    record: &GoalRecord,
    existing: Vec<AcceptanceCriterion>,
    cancel: &dyn Cancellation,
) -> Result<GoalPlanOutcome, BoundaryError> {
    let model = selected_model(config, &record.profile)?;
    let request = ModelRequest {
        request_id: RunId::new().0.to_string(),
        purpose: saya_core::contract::ModelPurpose::Review,
        profile: record.profile.clone(),
        instructions: saya_core::prompt::goal_planning_instructions().into(),
        dynamic_instructions: None,
        max_output_tokens: None,
        history: vec![HistoryItem::UserText(
            planning_payload(record, &existing).to_string(),
        )],
        media: Default::default(),
        tools: vec![],
    };
    let (text, usage) = complete_text(model, request, config, cancel, PLAN_RESPONSE_LIMIT).await?;
    let plan = GoalPlan::parse(&text)?;
    if !existing.is_empty() && plan.acceptance != existing {
        return Err(failure(
            "invalid_goal_plan",
            "planner changed caller-supplied acceptance criteria",
        ));
    }
    Ok(GoalPlanOutcome {
        plan,
        usage,
        request_count: 1,
    })
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GoalVerificationDecision {
    pub satisfied: bool,
    pub reason: String,
}

pub async fn verify_manual_evidence(
    config: &Config,
    record: &GoalRecord,
    criterion: &AcceptanceCriterion,
    evidence: &saya_core::goal::GoalEvidence,
    cancel: &dyn Cancellation,
) -> Result<(GoalVerificationDecision, Option<Usage>), BoundaryError> {
    let model = selected_model(config, &record.profile)?;
    let request = ModelRequest {
        request_id: RunId::new().0.to_string(),
        purpose: saya_core::contract::ModelPurpose::Review,
        profile: record.profile.clone(),
        instructions: saya_core::prompt::goal_verification_instructions().into(),
        dynamic_instructions: None,
        max_output_tokens: Some(1024),
        history: vec![HistoryItem::UserText(
            serde_json::json!({
                "criterion": criterion,
                "evidence": evidence,
                "objective": record.objective,
            })
            .to_string(),
        )],
        media: Default::default(),
        tools: vec![],
    };
    let (text, usage) =
        complete_text(model, request, config, cancel, VERIFY_RESPONSE_LIMIT).await?;
    let decision: GoalVerificationDecision = serde_json::from_str(text.trim())
        .map_err(|_| failure("invalid_goal_verification", "verification JSON is invalid"))?;
    if decision.reason.trim().is_empty() || decision.reason.len() > 4096 {
        return Err(failure(
            "invalid_goal_verification",
            "verification reason is invalid",
        ));
    }
    Ok((decision, usage))
}

pub struct GoalRunContext {
    pub permissions: saya_core::contract::PermissionMode,
    pub confirmation: Option<Arc<dyn ApprovalHandler>>,
    pub input: Option<Arc<dyn InputHandler>>,
    pub approvals: Option<Preauthorizations>,
    pub personal_context: PersonalContext,
    pub anthropic_prompt_cache: bool,
    pub accept_config_change: bool,
    pub cancellation: Arc<crate::interaction::RunCancellation>,
}

pub struct GoalRunOutcome {
    pub goal: GoalRecord,
    pub result: RunResult,
    pub streamed: bool,
}

fn failed_result(
    session_id: saya_core::contract::SessionId,
    run_id: RunId,
    error: BoundaryError,
) -> RunResult {
    RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: Some(session_id),
        run_id: Some(run_id),
        status: RunStatus::Failed,
        stop_reason: Some("goal_run_failed".into()),
        text: None,
        error: Some(error.0),
        usage: None,
        artifacts: vec![],
        persistence: None,
    }
}

async fn run_step(
    config: &Config,
    store: &GoalStore,
    lease: &GoalLease,
    goal: &GoalRecord,
    step: &GoalStep,
    context: &mut GoalRunContext,
) -> Result<GoalRunOutcome, BoundaryError> {
    let mut prepared = session::resume(
        config,
        &goal.session_id.0.to_string(),
        ResumeOptions {
            accept_config_change: context.accept_config_change,
            cwd_explicit: false,
            profile_explicit: false,
            mode_override: Some(goal.mode),
        },
    )
    .await
    .map_err(|_| failure("goal_session", "Goal session cannot be resumed"))?;
    let run_id = RunId::new();
    let now = timestamp()?;
    let dispatch = store.begin_dispatch(lease, run_id, now_ms(), now).await?;
    store.mark_running(&dispatch, lease, now_ms()).await?;
    if let Err(error) = session::append(&mut prepared, step.instruction.clone()).await {
        return Err(failure(
            "goal_session",
            if error.to_string().contains("sensitive") {
                "Goal step cannot be persisted"
            } else {
                "Goal step input cannot be persisted"
            },
        ));
    }
    let summary = format!(
        "Durable Goal: {}\nCurrent step: {}\n{}",
        goal.objective, step.title, step.instruction
    );
    let (mut runtime, streamed) = build_runtime(
        config,
        &prepared.session,
        &summary,
        Launch {
            permissions: context.permissions,
            store: prepared.store.clone(),
            cancellation: context.cancellation.clone(),
            confirmation: context.confirmation.clone(),
            input: context.input.clone(),
            approvals: context.approvals.take().unwrap_or_default(),
            personal_context: context.personal_context.clone(),
            anthropic_prompt_cache: context.anthropic_prompt_cache,
            run_id: Some(run_id),
            secret_registry: None,
        },
        |model, _| model,
    )
    .map_err(|_| failure("goal_runtime", "Goal runtime cannot be assembled"))?;
    let mut session = prepared.session.clone();
    let heartbeat_store = store.clone();
    let mut heartbeat_lease = lease.clone();
    let heartbeat_ttl = lease.expires_at_ms.saturating_sub(now_ms()).max(1);
    let heartbeat_wait = Duration::from_millis((heartbeat_ttl / 3).clamp(1_000, 30_000));
    let heartbeat_error = Arc::new(Mutex::new(None::<BoundaryError>));
    let heartbeat_failure = heartbeat_error.clone();
    let heartbeat_cancellation = context.cancellation.clone();
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::time::sleep(heartbeat_wait).await;
            if let Err(error) = heartbeat_store
                .renew_lease(&mut heartbeat_lease, now_ms(), Some(heartbeat_ttl))
                .await
            {
                heartbeat_cancellation.cancel();
                if let Ok(mut failure) = heartbeat_failure.lock() {
                    *failure = Some(error);
                }
                break;
            }
        }
    });
    let control_store = store.clone();
    let control_cancellation = context.cancellation.clone();
    let control_goal_id = goal.id;
    let control = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            match control_store.load(control_goal_id).await {
                Ok(current)
                    if matches!(current.status, GoalStatus::Paused | GoalStatus::Cancelled) =>
                {
                    control_cancellation.cancel();
                    break;
                }
                Ok(_) => {}
                Err(_) => {
                    control_cancellation.cancel();
                    break;
                }
            }
        }
    });
    let result = runtime
        .run(&mut session)
        .await
        .unwrap_or_else(|error| failed_result(session.id, run_id, error));
    heartbeat.abort();
    control.abort();
    let _ = heartbeat.await;
    let _ = control.await;
    if let Some(error) = heartbeat_error
        .lock()
        .ok()
        .and_then(|mut failure| failure.take())
    {
        return Err(error);
    }
    let metrics = prepared
        .store
        .run_metrics(session.id)
        .await?
        .into_iter()
        .rev()
        .find(|record| record.run_id == run_id)
        .map(|record| record.metrics)
        .unwrap_or_default();
    let goal = store
        .finish_dispatch(
            lease,
            &dispatch,
            GoalRunSummary::from_result(&result, timestamp()?)?,
            &metrics,
            timestamp()?,
        )
        .await?;
    let mut goal = goal;
    if matches!(goal.status, GoalStatus::Ready | GoalStatus::Waiting) {
        let final_verification = !goal.steps.is_empty()
            && goal
                .steps
                .iter()
                .all(|step| step.status == saya_core::goal::StepStatus::Completed);
        let acceptance = goal.acceptance.clone();
        for criterion in &acceptance {
            if matches!(
                criterion.verification,
                saya_core::goal::VerificationSpec::Manual
            ) || (goal.criterion_verified(&criterion.id) && !final_verification)
            {
                continue;
            }
            match verify_file_criterion(&goal.workspace, criterion, timestamp()?) {
                Ok(Some(evidence)) => {
                    goal = store
                        .record_evidence(goal.id, evidence, timestamp()?)
                        .await?;
                }
                Ok(None) => {}
                Err(_) if final_verification && goal.criterion_verified(&criterion.id) => {
                    goal = store
                        .invalidate_file_evidence(goal.id, &criterion.id, timestamp()?)
                        .await?;
                }
                Err(_) => {}
            }
        }
    }
    Ok(GoalRunOutcome {
        goal,
        result,
        streamed,
    })
}

/// Execute exactly one ready Goal step under a durable lease and session lock.
/// Crashes after the dispatch intent are recovered as unknown, never replayed.
pub async fn continue_once(
    config: &Config,
    store: &GoalStore,
    goal_id: GoalId,
    mut context: GoalRunContext,
) -> Result<GoalRunOutcome, BoundaryError> {
    let now = now_ms();
    let owner = RunId::new().0.to_string();
    let ttl = config
        .limits
        .active_run_timeout_seconds
        .saturating_add(60)
        .max(60)
        .saturating_mul(1000);
    let lease = store.acquire_lease(goal_id, owner, now, Some(ttl)).await?;
    let result = async {
        let goal = store.load(goal_id).await?;
        if goal.status != GoalStatus::Ready {
            return Err(failure(
                "invalid_goal_state",
                "Goal must be ready before continuing a step",
            ));
        }
        let step = goal
            .next_pending_step()
            .cloned()
            .ok_or_else(|| failure("goal_no_step", "Goal has no dependency-ready pending step"))?;
        run_step(config, store, &lease, &goal, &step, &mut context).await
    }
    .await;
    let release = store.release_lease(&lease).await;
    match (result, release) {
        (Ok(outcome), Ok(())) => Ok(outcome),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Verify one criterion after all steps complete. Manual evidence receives a
/// separate no-tools review; file criteria are always rechecked by the host.
pub async fn verify_criterion(
    config: &Config,
    store: &GoalStore,
    goal_id: GoalId,
    criterion_id: &str,
    summary: Option<String>,
    cancel: &dyn Cancellation,
) -> Result<GoalRecord, BoundaryError> {
    let mut goal = store
        .reserve_verification(goal_id, criterion_id, timestamp()?)
        .await?;
    let criterion = goal
        .acceptance
        .iter()
        .find(|criterion| criterion.id == criterion_id)
        .ok_or_else(|| failure("goal_not_found", "acceptance criterion was not found"))?
        .clone();
    if !matches!(&criterion.verification, VerificationSpec::Manual) {
        if summary.is_some() {
            return Err(failure(
                "invalid_goal_evidence",
                "file acceptance verification does not take a manual summary",
            ));
        }
        let evidence = match verify_file_criterion(&goal.workspace, &criterion, timestamp()?) {
            Ok(Some(evidence)) => evidence,
            Ok(None) => {
                return Err(failure(
                    "goal_verification_failed",
                    "file criterion did not produce host evidence",
                ));
            }
            Err(error) => {
                if goal.criterion_verified(&criterion.id) {
                    store
                        .invalidate_file_evidence(goal.id, &criterion.id, timestamp()?)
                        .await?;
                }
                return Err(error);
            }
        };
        return store.record_evidence(goal.id, evidence, timestamp()?).await;
    }
    let summary = summary
        .filter(|summary| {
            !summary.trim().is_empty() && summary.len() <= saya_core::goal::MAX_GOAL_EVIDENCE_BYTES
        })
        .ok_or_else(|| {
            failure(
                "invalid_goal_evidence",
                "manual evidence summary is invalid",
            )
        })?;
    let evidence = saya_core::goal::GoalEvidence {
        id: saya_core::goal::EvidenceId::new(),
        criterion_id: criterion_id.into(),
        step_id: goal
            .steps
            .iter()
            .rev()
            .find_map(|step| step.run_id.map(|_| step.id)),
        run_id: goal.steps.iter().rev().find_map(|step| step.run_id),
        kind: saya_core::goal::EvidenceKind::Manual,
        summary,
        path: None,
        sha256: None,
        observed_at: timestamp()?,
    };
    let (decision, usage) =
        verify_manual_evidence(config, &goal, &criterion, &evidence, cancel).await?;
    if !decision.satisfied {
        return Err(failure(
            "goal_verification_failed",
            "provided evidence does not satisfy the acceptance criterion",
        ));
    }
    goal.usage.add_usage(usage.as_ref());
    goal.updated_at = timestamp()?;
    let revision = goal.revision;
    goal = store.save(goal, revision).await?;
    store.record_evidence(goal.id, evidence, timestamp()?).await
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

pub fn account_planning(
    goal: &mut GoalRecord,
    outcome: &GoalPlanOutcome,
    now: String,
) -> Result<(), BoundaryError> {
    goal.usage.add_usage(outcome.usage.as_ref());
    goal.updated_at = now;
    Ok(())
}

pub fn configured_auto_goal(config: &Config, goal: &GoalRecord) -> Result<(), BoundaryError> {
    let Some(auth) = &goal.authorization else {
        return Err(failure(
            "goal_authorization",
            "automatic Goal continuation is not authorized",
        ));
    };
    if !auth.auto
        || auth.mode != goal.mode
        || auth.permissions != config.permissions_mode
        || auth.config_fingerprint != goal_profile_fingerprint(config, &goal.profile)?
    {
        return Err(failure(
            "goal_authorization",
            "automatic Goal authorization does not match current configuration",
        ));
    }
    Ok(())
}

pub fn goal_profile_fingerprint(config: &Config, profile: &str) -> Result<String, BoundaryError> {
    let profile_fingerprint = config
        .profiles
        .get(profile)
        .ok_or_else(|| failure("invalid_goal", "Goal profile is unavailable"))?
        .fingerprint()?;
    let mut hash = Sha256::new();
    hash.update(b"saya-goal-authorization-v1\0");
    hash.update(profile_fingerprint.as_bytes());
    hash.update(format!("{:?}", config.mode).as_bytes());
    hash.update(format!("{:?}", config.permissions_mode).as_bytes());
    hash.update(format!("{:?}", config.permissions).as_bytes());
    hash.update(serde_json::to_vec(&config.sandbox).map_err(|_| {
        failure(
            "invalid_goal",
            "Goal sandbox configuration cannot be fingerprinted",
        )
    })?);
    hash.update(serde_json::to_vec(&config.mcp).map_err(|_| {
        failure(
            "invalid_goal",
            "Goal MCP configuration cannot be fingerprinted",
        )
    })?);
    hash.update(serde_json::to_vec(&config.plugins).map_err(|_| {
        failure(
            "invalid_goal",
            "Goal Plugin configuration cannot be fingerprinted",
        )
    })?);
    hash.update(
        serde_json::to_vec(&serde_json::json!({
            "review_profile": config.review.profile.as_deref(),
            "review_max_output_tokens": config.review.max_output_tokens,
            "context": {
                "auto_compact": config.context.auto_compact,
                "trigger_percent": config.context.trigger_percent,
                "trigger_tokens": config.context.trigger_tokens,
                "target_percent": config.context.target_percent,
                "extra_safety_tokens": config.context.extra_safety_tokens,
                "summary_max_output_tokens": config.context.summary_max_output_tokens,
                "max_compaction_calls": config.context.max_compaction_calls,
                "compaction_timeout_seconds": config.context.compaction_timeout_seconds,
            },
            "memory": {
                "read": config.memory.read,
                "inject": config.memory.inject,
                "write": config.memory.write,
                "root": config.memory.root.as_deref(),
            },
            "output_capture_enabled": config.output_capture.enabled,
        }))
        .map_err(|_| {
            failure(
                "invalid_goal",
                "Goal context configuration cannot be fingerprinted",
            )
        })?,
    );
    hash.update(format!("{:?}", config.commands.inherit_env).as_bytes());
    let limits = &config.limits;
    hash.update(
        serde_json::to_vec(&serde_json::json!({
            "max_model_requests": limits.max_model_requests,
            "max_tool_calls": limits.max_tool_calls,
            "max_background_running": limits.max_background_running,
            "max_background_jobs": limits.max_background_jobs,
            "active_run_timeout_seconds": limits.active_run_timeout_seconds,
            "model_connect_timeout_seconds": limits.model_connect_timeout_seconds,
            "model_idle_timeout_seconds": limits.model_idle_timeout_seconds,
            "model_timeout_seconds": limits.model_timeout_seconds,
            "read_return_max_bytes": limits.read_return_max_bytes,
            "read_file_max_bytes": limits.read_file_max_bytes,
            "search_max_results": limits.search_max_results,
            "search_return_max_bytes": limits.search_return_max_bytes,
            "list_max_entries": limits.list_max_entries,
            "list_return_max_bytes": limits.list_return_max_bytes,
            "write_max_bytes": limits.write_max_bytes,
            "command_timeout_seconds": limits.command_timeout_seconds,
            "max_command_timeout_seconds": limits.max_command_timeout_seconds,
            "command_stdout_max_bytes": limits.command_stdout_max_bytes,
            "command_stderr_max_bytes": limits.command_stderr_max_bytes,
        }))
        .map_err(|_| failure("invalid_goal", "Goal limits cannot be fingerprinted"))?,
    );
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
