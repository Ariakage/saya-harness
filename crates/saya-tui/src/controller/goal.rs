//! Goal selector, lifecycle controls and bounded execution jobs.

use super::*;
use crate::{
    i18n::{picker as picker_text, services as text},
    view::{Details, PickerItem, PickerKind, PickerPanel},
};
use saya_app::{
    goal::{
        GoalRunContext, account_planning, configured_auto_goal, continue_once,
        goal_profile_fingerprint, now_ms, plan_goal,
    },
    session,
};
use saya_core::{
    config::Config,
    contract::PermissionMode,
    goal::{
        GoalAuthorization, GoalBudget, GoalId, GoalPhase, GoalRecord, GoalSeed, GoalStatus,
        GoalStore, StepStatus, goal_report_json, goal_report_markdown,
    },
};
use std::sync::Arc;

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        saya_core::contract::ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

fn secrets(config: &Config) -> std::result::Result<Vec<String>, BoundaryError> {
    config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| failure("goal_storage", "Goal credentials are unavailable"))
}

fn store(config: &Config) -> std::result::Result<GoalStore, BoundaryError> {
    GoalStore::open(&config.data_dir, secrets(config)?)
}

fn goal_lines(goal: &GoalRecord) -> Vec<String> {
    let mut lines = vec![
        format!("Goal {}", goal.id.0),
        format!("Status: {:?} · Phase: {:?}", goal.status, goal.phase),
        format!("Objective: {}", goal.objective),
        format!("Workspace: {}", goal.workspace.display()),
        format!("Profile: {} · Mode: {:?}", goal.profile, goal.mode),
        format!(
            "Budget: {}/{} runs · {}/{} requests · {}/{} tools · {}/{} tokens",
            goal.usage.runs,
            goal.budget.max_runs,
            goal.usage.model_requests,
            goal.budget.max_model_requests,
            goal.usage.tool_calls,
            goal.budget.max_tool_calls,
            goal.usage
                .input_tokens
                .saturating_add(goal.usage.output_tokens),
            goal.budget.max_tokens
        ),
    ];
    if let Some(reason) = &goal.wait_reason {
        lines.push(format!("Waiting: {reason:?}"));
    }
    if let Some(error) = &goal.failure {
        lines.push(format!("Failure: {} — {}", error.code, error.message));
    }
    lines.push(String::new());
    lines.push("Acceptance".into());
    for criterion in &goal.acceptance {
        lines.push(format!(
            "[{}] {} · {}",
            if goal.criterion_verified(&criterion.id) {
                "x"
            } else {
                " "
            },
            criterion.id,
            criterion.text
        ));
    }
    lines.push(String::new());
    lines.push("Steps".into());
    for step in &goal.steps {
        lines.push(format!(
            "{}. {:?} · {} · attempts {} · {}",
            step.ordinal,
            step.status,
            step.title,
            step.attempts,
            step.run_id
                .map(|run| run.0.to_string())
                .unwrap_or_else(|| "no Run".into())
        ));
        lines.push(format!("   {}", step.instruction));
        if let Some(result) = &step.last_result {
            lines.push(format!(
                "   Result {:?} · persisted {} · {} artifacts",
                result.status,
                result.persistence_saved,
                result.artifacts.len()
            ));
        }
    }
    lines.push(String::new());
    lines.push("Evidence".into());
    if goal.evidence.is_empty() {
        lines.push("No verified acceptance evidence".into());
    } else {
        for evidence in &goal.evidence {
            lines.push(format!(
                "{} · {} · {} · {:?}",
                evidence.criterion_id, evidence.summary, evidence.observed_at, evidence.kind
            ));
        }
    }
    lines
}

fn details(goal: &GoalRecord) -> Details {
    Details {
        title: format!("Goal · {:?}", goal.status),
        lines: goal_lines(goal),
    }
}

fn panel(goals: Vec<GoalRecord>) -> PickerPanel {
    let items = goals
        .into_iter()
        .map(|goal| {
            let completed = goal
                .steps
                .iter()
                .filter(|step| step.status == StepStatus::Completed)
                .count();
            PickerItem {
                id: goal.id.0.to_string(),
                label: goal.objective.chars().take(80).collect(),
                description: format!(
                    "{:?} · {completed}/{} steps · {}",
                    goal.status,
                    goal.steps.len(),
                    goal.id.0
                ),
                details: goal_lines(&goal),
                active: matches!(goal.status, GoalStatus::Running | GoalStatus::Ready),
            }
        })
        .collect();
    PickerPanel {
        kind: PickerKind::Goals,
        title: picker_text::GOALS_TITLE.into(),
        items,
        selected: 0,
        scroll: 0,
        page_rows: 0,
        hint: picker_text::NO_GOALS.into(),
    }
}

fn parse_id(raw: &str) -> std::result::Result<GoalId, BoundaryError> {
    raw.parse()
}

async fn finish_job(
    goal: GoalRecord,
    cancelled: bool,
) -> std::result::Result<services::UiResult, BoundaryError> {
    if cancelled {
        return Err(failure("cancelled", text::GOAL_CANCELLED));
    }
    Ok(services::UiResult::Details(details(&goal)))
}

impl App {
    pub(super) async fn open_goals(&mut self) -> Result<()> {
        if self.ui_job.is_some() {
            return Err(text::UI_ACTIVE.into());
        }
        session::check_roots(&self.config, &self.config.cwd, self.current_mode()?)?;
        let config = self.config.clone();
        let cancel = Arc::new(RunCancellation::new());
        let checking = cancel.clone();
        let join = tokio::spawn(async move {
            let goals = store(&config)?.list().await?;
            if checking.is_cancelled() {
                return Err(failure("cancelled", text::UI_QUERY_CANCELLED));
            }
            Ok(services::UiResult::Picker(panel(goals)))
        });
        self.begin_ui(cancel, join);
        Ok(())
    }

    pub(super) async fn goal(&mut self, argument: &str) -> Result<()> {
        if argument.is_empty() {
            return self.open_goals().await;
        }
        if self.ui_job.is_some() {
            return Err(text::UI_ACTIVE.into());
        }
        if argument == "create" || argument.starts_with("create ") {
            let objective = argument
                .strip_prefix("create")
                .map(str::trim)
                .filter(|objective| !objective.is_empty())
                .ok_or(text::GOAL_CREATE_USAGE)?;
            self.require_idle()?;
            let config = self.config.clone();
            let objective = objective.to_owned();
            let cancel = Arc::new(RunCancellation::new());
            let checking = cancel.clone();
            let join = tokio::spawn(async move {
                let profile = config
                    .profile
                    .clone()
                    .ok_or_else(|| failure("invalid_goal", text::GOAL_PROFILE_REQUIRED))?;
                let mut session = saya_app::new_session(&config)
                    .map_err(|_| failure("invalid_goal", text::GOAL_PROFILE_REQUIRED))?;
                session.work_mode = config.mode;
                let prepared = session::create(&config, session)
                    .await
                    .map_err(|_| failure("goal_session", text::GOAL_SESSION_FAILED))?;
                let session_id = prepared.session.id;
                let workspace = prepared.session.workspace.clone();
                drop(prepared);
                let goal = GoalRecord::new(GoalSeed {
                    session_id,
                    objective,
                    workspace,
                    profile,
                    mode: config.mode,
                    permissions: config.permissions_mode,
                    budget: GoalBudget::default(),
                    now: saya_app::events::timestamp()?,
                })?;
                let goal = store(&config)?.create(goal).await?;
                finish_job(goal, checking.is_cancelled()).await
            });
            self.begin_ui(cancel, join);
            return Ok(());
        }
        let (action, rest) = argument.split_once(' ').unwrap_or((argument, ""));
        let (id, tail) = rest.split_once(' ').unwrap_or((rest, ""));
        match action {
            "plan" if !id.is_empty() && tail.is_empty() => {
                let config = self.config.clone();
                let goal_id = parse_id(id)?;
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let context_cancel: Arc<dyn Cancellation> = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal_store = store(&config)?;
                    let lease = goal_store
                        .acquire_lease(
                            goal_id,
                            format!("tui-plan-{}", now_ms()),
                            now_ms(),
                            Some(300_000),
                        )
                        .await?;
                    let result = async {
                        let mut goal = goal_store.load(goal_id).await?;
                        if goal.usage.planning_attempts >= goal.budget.max_planning_attempts {
                            goal.status = GoalStatus::LimitReached;
                            goal.phase = GoalPhase::Finished;
                            goal.updated_at = saya_app::events::timestamp()?;
                            let revision = goal.revision;
                            goal_store.save(goal, revision).await?;
                            return Err(failure("goal_budget", text::GOAL_PLAN_BUDGET));
                        }
                        goal.usage.planning_attempts =
                            goal.usage.planning_attempts.saturating_add(1);
                        if goal.usage.model_requests >= goal.budget.max_model_requests {
                            goal.status = GoalStatus::LimitReached;
                            goal.phase = GoalPhase::Finished;
                            goal.updated_at = saya_app::events::timestamp()?;
                            let revision = goal.revision;
                            goal_store.save(goal, revision).await?;
                            return Err(failure("goal_budget", text::GOAL_PLAN_BUDGET));
                        }
                        goal.usage.model_requests = goal.usage.model_requests.saturating_add(1);
                        goal.updated_at = saya_app::events::timestamp()?;
                        let revision = goal.revision;
                        goal = goal_store.save(goal, revision).await?;
                        let existing = goal.acceptance.clone();
                        let outcome =
                            plan_goal(&config, &goal, existing, context_cancel.as_ref()).await?;
                        account_planning(&mut goal, &outcome, saya_app::events::timestamp()?)?;
                        let revision = goal.revision;
                        goal = goal_store.save(goal, revision).await?;
                        goal.apply_plan(outcome.plan, saya_app::events::timestamp()?)?;
                        let revision = goal.revision;
                        goal_store.save(goal, revision).await
                    }
                    .await;
                    let _ = goal_store.release_lease(&lease).await;
                    let goal = result?;
                    finish_job(goal, checking.is_cancelled()).await
                });
                self.begin_ui(cancel, join);
            }
            "confirm" if !id.is_empty() && tail.is_empty() => {
                let config = self.config.clone();
                let goal_id = parse_id(id)?;
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal_store = store(&config)?;
                    let mut goal = goal_store.load(goal_id).await?;
                    goal.confirm_plan(saya_app::events::timestamp()?)?;
                    let revision = goal.revision;
                    let goal = goal_store.save(goal, revision).await?;
                    finish_job(goal, checking.is_cancelled()).await
                });
                self.begin_ui(cancel, join);
            }
            "status" if !id.is_empty() && tail.is_empty() => {
                let config = self.config.clone();
                let goal_id = parse_id(id)?;
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal = store(&config)?.load(goal_id).await?;
                    if checking.is_cancelled() {
                        return Err(failure("cancelled", text::UI_QUERY_CANCELLED));
                    }
                    Ok(services::UiResult::Details(details(&goal)))
                });
                self.begin_ui(cancel, join);
            }
            "report" if !id.is_empty() => {
                let config = self.config.clone();
                let goal_id = parse_id(id)?;
                let target = (!tail.is_empty()).then(|| std::path::PathBuf::from(tail));
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let context_cancel: Arc<dyn Cancellation> = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal = store(&config)?.load(goal_id).await?;
                    let Some(target) = target else {
                        return finish_job(goal, checking.is_cancelled()).await;
                    };
                    let markdown = target
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| matches!(extension, "md" | "markdown"));
                    let bytes = if markdown {
                        goal_report_markdown(&goal)?
                    } else {
                        goal_report_json(&goal)?
                    };
                    let target = if target.is_absolute() {
                        target
                    } else {
                        config.cwd.join(target)
                    };
                    let receipt = saya_app::export::publish(
                        &target,
                        &bytes,
                        &goal.workspace,
                        config.mode,
                        context_cancel.as_ref(),
                    )?;
                    if checking.is_cancelled() {
                        return Err(failure("cancelled", text::GOAL_CANCELLED));
                    }
                    Ok(services::UiResult::Details(Details {
                        title: text::GOAL_REPORT_EXPORTED.into(),
                        lines: vec![serde_json::to_string_pretty(&receipt).map_err(|_| {
                            failure("goal_report", text::GOAL_REPORT_ENCODE_FAILED)
                        })?],
                    }))
                });
                self.begin_ui(cancel, join);
            }
            "pause" | "resume" | "cancel" if !id.is_empty() && tail.is_empty() => {
                let config = self.config.clone();
                let goal_id = parse_id(id)?;
                let transition = match action {
                    "pause" => (GoalStatus::Paused, GoalPhase::Executing, None),
                    "resume" => (GoalStatus::Ready, GoalPhase::Executing, None),
                    _ => (GoalStatus::Cancelled, GoalPhase::Finished, None),
                };
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal = store(&config)?
                        .transition(
                            goal_id,
                            transition.0,
                            transition.1,
                            transition.2,
                            saya_app::events::timestamp()?,
                        )
                        .await?;
                    if checking.is_cancelled() {
                        return Err(failure("cancelled", text::UI_QUERY_CANCELLED));
                    }
                    Ok(services::UiResult::Details(details(&goal)))
                });
                self.begin_ui(cancel, join);
            }
            "continue" if !id.is_empty() && tail.is_empty() => {
                self.require_idle()?;
                if self.config.permissions_mode == PermissionMode::Manual {
                    return Err(text::GOAL_MANUAL_UNAVAILABLE.into());
                }
                let config = self.config.clone();
                let options = self.options.personal_context.clone();
                let goal_id = parse_id(id)?;
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let context_cancel: Arc<dyn Cancellation> = cancel.clone();
                let run = GoalRunContext {
                    permissions: config.permissions_mode,
                    confirmation: None,
                    input: None,
                    approvals: None,
                    personal_context: options,
                    anthropic_prompt_cache: self.options.anthropic_prompt_cache,
                    accept_config_change: false,
                    cancellation: context_cancel,
                };
                let join = tokio::spawn(async move {
                    let goal_store = store(&config)?;
                    let outcome = continue_once(&config, &goal_store, goal_id, run).await?;
                    finish_job(outcome.goal, checking.is_cancelled()).await
                });
                self.begin_ui(cancel, join);
            }
            "start" if !id.is_empty() && tail.is_empty() => {
                self.require_idle()?;
                if self.config.permissions_mode == PermissionMode::Manual {
                    return Err(text::GOAL_MANUAL_UNAVAILABLE.into());
                }
                let config = self.config.clone();
                let options = self.options.personal_context.clone();
                let anthropic_prompt_cache = self.options.anthropic_prompt_cache;
                let goal_id = parse_id(id)?;
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let worker_cancel = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal_store = store(&config)?;
                    let mut goal = goal_store.load(goal_id).await?;
                    if goal.status != GoalStatus::Ready {
                        return Err(failure("invalid_goal_state", text::GOAL_NOT_READY));
                    }
                    goal.authorization = Some(GoalAuthorization {
                        auto: true,
                        mode: goal.mode,
                        permissions: config.permissions_mode,
                        config_fingerprint: goal_profile_fingerprint(&config, &goal.profile)?,
                        authorized_at: saya_app::events::timestamp()?,
                    });
                    goal.updated_at = saya_app::events::timestamp()?;
                    let revision = goal.revision;
                    goal = goal_store.save(goal, revision).await?;
                    loop {
                        configured_auto_goal(&config, &goal)?;
                        if goal.status != GoalStatus::Ready || checking.is_cancelled() {
                            break;
                        }
                        let context_cancel: Arc<dyn Cancellation> = worker_cancel.clone();
                        let context = GoalRunContext {
                            permissions: config.permissions_mode,
                            confirmation: None,
                            input: None,
                            approvals: None,
                            personal_context: options.clone(),
                            anthropic_prompt_cache,
                            accept_config_change: false,
                            cancellation: context_cancel,
                        };
                        goal = match continue_once(&config, &goal_store, goal.id, context).await {
                            Ok(outcome) => outcome.goal,
                            Err(error) if error.0.code == "goal_budget" => {
                                goal = goal_store.load(goal.id).await?;
                                break;
                            }
                            Err(error) => return Err(error),
                        };
                        if checking.is_cancelled() {
                            break;
                        }
                        if goal.status == GoalStatus::Blocked {
                            goal = goal_store
                                .replan_failed_step(goal.id, saya_app::events::timestamp()?)
                                .await?;
                        }
                    }
                    finish_job(goal, checking.is_cancelled()).await
                });
                self.begin_ui(cancel, join);
            }
            "verify" if !id.is_empty() => {
                let Some((criterion, summary)) = tail.split_once(' ') else {
                    return Err(text::GOAL_VERIFY_USAGE.into());
                };
                if criterion.is_empty() || summary.trim().is_empty() {
                    return Err(text::GOAL_VERIFY_USAGE.into());
                }
                let config = self.config.clone();
                let goal_id = parse_id(id)?;
                let criterion = criterion.to_owned();
                let summary = summary.to_owned();
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let context_cancel: Arc<dyn Cancellation> = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal = saya_app::goal::verify_manual_criterion(
                        &config,
                        &store(&config)?,
                        goal_id,
                        &criterion,
                        summary,
                        context_cancel.as_ref(),
                    )
                    .await?;
                    finish_job(goal, checking.is_cancelled()).await
                });
                self.begin_ui(cancel, join);
            }
            "replan" if !id.is_empty() && tail.is_empty() => {
                let config = self.config.clone();
                let goal_id = parse_id(id)?;
                let cancel = Arc::new(RunCancellation::new());
                let checking = cancel.clone();
                let join = tokio::spawn(async move {
                    let goal = store(&config)?
                        .replan_failed_step(goal_id, saya_app::events::timestamp()?)
                        .await?;
                    finish_job(goal, checking.is_cancelled()).await
                });
                self.begin_ui(cancel, join);
            }
            _ => return Err(text::GOAL_USAGE.into()),
        }
        Ok(())
    }
}
