//! CLI adapter for durable Goal planning, bounded continuation and reports.

use super::{Cancel, Cli, Terminal, personal_context, signals};
use clap::{Args, Subcommand, ValueEnum};
use saya_app::{
    goal::{
        GoalRunContext, account_planning, configured_auto_goal, continue_once,
        goal_profile_fingerprint, now_ms, plan_goal, verify_manual_criterion,
    },
    new_session, session,
};
use saya_core::{
    approvals::Preauthorizations,
    boundary::Cancellation,
    config::{Config, Output},
    contract::{BoundaryError, ErrorInfo, PermissionMode},
    goal::{
        AcceptanceCriterion, GoalAuthorization, GoalBudget, GoalId, GoalPhase, GoalPlan,
        GoalRecord, GoalSeed, GoalStatus, GoalStore, StepId, VerificationSpec, goal_report_json,
        goal_report_markdown,
    },
};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{self, IsTerminal, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

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

fn start_signals(cancel: Arc<Cancel>) -> Result<signals::CancellationListener, BoundaryError> {
    signals::CancellationListener::start(cancel)
        .map_err(|_| failure("signal_error", "Goal signal handling is unavailable"))
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum GoalReportFormat {
    Json,
    Markdown,
}

#[derive(Debug, Clone, Args)]
pub struct GoalBudgetArgs {
    #[arg(long)]
    pub max_runs: Option<u32>,
    #[arg(long)]
    pub max_model_requests: Option<u64>,
    #[arg(long)]
    pub max_tool_calls: Option<u64>,
    #[arg(long)]
    pub max_active_seconds: Option<u64>,
    #[arg(long)]
    pub max_tokens: Option<u64>,
    #[arg(long)]
    pub max_planning_attempts: Option<u32>,
    #[arg(long)]
    pub max_verification_attempts: Option<u32>,
    #[arg(long)]
    pub max_replans: Option<u32>,
}

impl GoalBudgetArgs {
    fn resolve(&self) -> GoalBudget {
        let defaults = GoalBudget::default();
        GoalBudget {
            max_runs: self.max_runs.unwrap_or(defaults.max_runs),
            max_model_requests: self
                .max_model_requests
                .unwrap_or(defaults.max_model_requests),
            max_tool_calls: self.max_tool_calls.unwrap_or(defaults.max_tool_calls),
            max_active_seconds: self
                .max_active_seconds
                .unwrap_or(defaults.max_active_seconds),
            max_tokens: self.max_tokens.unwrap_or(defaults.max_tokens),
            max_planning_attempts: self
                .max_planning_attempts
                .unwrap_or(defaults.max_planning_attempts),
            max_verification_attempts: self
                .max_verification_attempts
                .unwrap_or(defaults.max_verification_attempts),
            max_replans: self.max_replans.unwrap_or(defaults.max_replans),
        }
    }
}

#[derive(Debug, Clone, Subcommand)]
pub enum GoalAction {
    /// Save a new Goal and its dedicated session.
    Create {
        objective: String,
        /// Repeat to seed immutable acceptance criteria.
        #[arg(long = "accept")]
        acceptance: Vec<String>,
        #[command(flatten)]
        budget: GoalBudgetArgs,
    },
    /// Generate a structured plan and save it for explicit confirmation.
    Plan {
        goal_id: String,
        /// Use a caller-reviewed GoalPlan JSON file instead of a model call.
        #[arg(long)]
        plan_file: Option<PathBuf>,
    },
    /// Confirm the current plan and make its first dependency-ready step runnable.
    Confirm { goal_id: String },
    /// List saved Goals.
    List,
    /// Show one complete Goal record.
    Status { goal_id: String },
    /// Execute exactly one dependency-ready step.
    Continue {
        goal_id: String,
        #[arg(long)]
        approval_file: Option<PathBuf>,
        #[arg(long)]
        accept_config_change: bool,
    },
    /// Explicitly start finite automatic continuation for one Goal.
    Start {
        goal_id: String,
        #[arg(long)]
        auto: bool,
        #[arg(long)]
        approval_file: Option<PathBuf>,
        #[arg(long)]
        accept_config_change: bool,
    },
    /// Stop scheduling new steps after the current local Run has ended.
    Pause { goal_id: String },
    /// Return a paused or blocked Goal to its ready state.
    Resume { goal_id: String },
    /// Cancel a Goal and stop automatic scheduling.
    Cancel { goal_id: String },
    /// Verify one manual acceptance criterion with explicit user evidence.
    Verify {
        goal_id: String,
        criterion: String,
        #[arg(long)]
        summary: String,
    },
    /// Resolve an unknown step outcome. Retry is never implicit.
    Resolve {
        goal_id: String,
        step_id: String,
        #[arg(long)]
        retry: bool,
    },
    /// Apply one bounded replan to a known failed step.
    Replan { goal_id: String },
    /// Render a standalone Goal report.
    Report {
        goal_id: String,
        #[arg(long, value_enum, default_value_t = GoalReportFormat::Json)]
        format: GoalReportFormat,
        #[arg(long)]
        file: Option<PathBuf>,
    },
    /// Resume explicitly authorized automatic Goals from durable state.
    Worker {
        /// Perform one recovery/scheduling pass and exit.
        #[arg(long)]
        once: bool,
    },
}

fn parse_goal(raw: &str) -> Result<GoalId, BoundaryError> {
    raw.parse()
}

fn parse_step(raw: &str) -> Result<StepId, BoundaryError> {
    raw.parse()
}

fn secrets(config: &Config) -> Result<Vec<String>, BoundaryError> {
    config
        .secret_env_names()
        .iter()
        .map(|name| config.secret(name).map(str::to_owned))
        .collect()
}

fn store(config: &Config) -> Result<GoalStore, BoundaryError> {
    GoalStore::open(&config.data_dir, secrets(config)?)
}

fn print_value(config: &Config, value: &Value, text: impl FnOnce()) {
    match config.output {
        Output::Text => text(),
        Output::Json => println!("{value}"),
        Output::Jsonl => println!("{value}"),
    }
}

fn goal_text(goal: &GoalRecord) {
    println!("Goal {}", goal.id.0);
    println!("Status: {:?} · Phase: {:?}", goal.status, goal.phase);
    println!("{}", goal.objective);
    if let Some(reason) = &goal.wait_reason {
        println!("Waiting: {reason:?}");
    }
    println!(
        "Budget: {}/{} runs · {}/{} requests · {}/{} tools",
        goal.usage.runs,
        goal.budget.max_runs,
        goal.usage.model_requests,
        goal.budget.max_model_requests,
        goal.usage.tool_calls,
        goal.budget.max_tool_calls
    );
    for criterion in &goal.acceptance {
        println!(
            "[{}] {}: {}",
            if goal.criterion_verified(&criterion.id) {
                "x"
            } else {
                " "
            },
            criterion.id,
            criterion.text
        );
    }
    for step in &goal.steps {
        println!(
            "{}. {:?} · {} · {} attempt(s) · {}",
            step.ordinal,
            step.status,
            step.title,
            step.attempts,
            step.run_id
                .map(|run| run.0.to_string())
                .unwrap_or_else(|| "no Run".into())
        );
    }
}

fn bounded_file(path: &PathBuf) -> Result<String, BoundaryError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| failure("invalid_goal_plan", "Goal plan file is unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("invalid_goal_plan", "Goal plan file is unavailable"))?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(failure(
            "invalid_goal_plan",
            "Goal plan file is not a bounded regular file",
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|_| failure("invalid_goal_plan", "Goal plan file is not UTF-8"))?;
    Ok(text)
}

async fn create_goal(
    action: &GoalAction,
    _cli: &Cli,
    config: &Config,
) -> Result<Value, BoundaryError> {
    let GoalAction::Create {
        objective,
        acceptance,
        budget,
    } = action
    else {
        unreachable!()
    };
    if objective.trim().is_empty() {
        return Err(failure("invalid_goal", "Goal objective cannot be empty"));
    }
    let mut selected = config.clone();
    selected.mode = config.mode;
    let mut new = new_session(&selected)
        .map_err(|_| failure("invalid_goal", "Goal session configuration is invalid"))?;
    new.work_mode = config.mode;
    let prepared = session::create(&selected, new)
        .await
        .map_err(|_| failure("goal_session", "Goal session cannot be created"))?;
    let session_id = prepared.session.id;
    let workspace = prepared.session.workspace.clone();
    drop(prepared);
    let profile = selected
        .profile
        .clone()
        .ok_or_else(|| failure("invalid_goal", "Goal profile is not selected"))?;
    let mut goal = GoalRecord::new(GoalSeed {
        session_id,
        objective: objective.clone(),
        workspace,
        profile,
        mode: selected.mode,
        permissions: selected.permissions_mode,
        budget: budget.resolve(),
        now: saya_app::events::timestamp()?,
    })?;
    goal.acceptance = acceptance
        .iter()
        .enumerate()
        .map(|(index, text)| AcceptanceCriterion {
            id: format!("criterion-{}", index + 1),
            text: text.clone(),
            verification: VerificationSpec::Manual,
        })
        .collect();
    goal.validate()?;
    let goal = store(config)?.create(goal).await?;
    Ok(json!({"schema_version":1,"data":{"goal":goal}}))
}

async fn plan(
    goal_id: GoalId,
    plan_file: Option<&PathBuf>,
    config: &Config,
) -> Result<Value, BoundaryError> {
    let store = store(config)?;
    let mut goal = store.load(goal_id).await?;
    let lease = store
        .acquire_lease(
            goal.id,
            format!("plan-{}", now_ms()),
            now_ms(),
            Some(300_000),
        )
        .await?;
    let result = async {
        if goal.usage.planning_attempts >= goal.budget.max_planning_attempts {
            goal.status = GoalStatus::LimitReached;
            goal.phase = GoalPhase::Finished;
            goal.updated_at = saya_app::events::timestamp()?;
            let revision = goal.revision;
            store.save(goal, revision).await?;
            return Err(failure("goal_budget", "Goal planning budget is exhausted"));
        }
        goal.usage.planning_attempts = goal.usage.planning_attempts.saturating_add(1);
        if plan_file.is_none() {
            if goal.usage.model_requests >= goal.budget.max_model_requests {
                goal.status = GoalStatus::LimitReached;
                goal.phase = GoalPhase::Finished;
                goal.updated_at = saya_app::events::timestamp()?;
                let revision = goal.revision;
                store.save(goal, revision).await?;
                return Err(failure(
                    "goal_budget",
                    "Goal model-request budget is exhausted",
                ));
            }
            goal.usage.model_requests = goal.usage.model_requests.saturating_add(1);
        }
        goal.updated_at = saya_app::events::timestamp()?;
        let revision = goal.revision;
        goal = store.save(goal, revision).await?;
        let plan = if let Some(path) = plan_file {
            GoalPlan::parse(&bounded_file(path)?)?
        } else {
            let cancel = Arc::new(Cancel::new());
            let _signals = start_signals(cancel.clone())?;
            let existing = goal.acceptance.clone();
            let outcome = plan_goal(config, &goal, existing, cancel.as_ref()).await?;
            account_planning(&mut goal, &outcome, saya_app::events::timestamp()?)?;
            let revision = goal.revision;
            goal = store.save(goal, revision).await?;
            outcome.plan
        };
        if !goal.acceptance.is_empty() && plan.acceptance != goal.acceptance {
            return Err(failure(
                "invalid_goal_plan",
                "plan changed caller-supplied acceptance criteria",
            ));
        }
        goal.apply_plan(plan, saya_app::events::timestamp()?)?;
        let revision = goal.revision;
        goal = store.save(goal, revision).await?;
        Ok(json!({"schema_version":1,"data":{"goal":goal,"confirmation_required":true}}))
    }
    .await;
    let _ = store.release_lease(&lease).await;
    result
}

async fn confirm(goal_id: GoalId, config: &Config) -> Result<Value, BoundaryError> {
    let store = store(config)?;
    let mut goal = store.load(goal_id).await?;
    goal.confirm_plan(saya_app::events::timestamp()?)?;
    let revision = goal.revision;
    let goal = store.save(goal, revision).await?;
    Ok(json!({"schema_version":1,"data":{"goal":goal}}))
}

fn terminal(config: &Config) -> Result<Option<Arc<Terminal>>, BoundaryError> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Terminal::open(secrets(config)?)
            .map(Some)
            .map(|terminal| terminal.map(Arc::new))
            .map_err(|_| failure("terminal_error", "Goal terminal is unavailable"))
    } else {
        Ok(None)
    }
}

fn run_context(
    cli: &Cli,
    config: &Config,
    approval_file: Option<&PathBuf>,
    cancellation: Arc<dyn Cancellation>,
    interactive: bool,
    accept_config_change: bool,
) -> Result<GoalRunContext, BoundaryError> {
    let terminal = if interactive { terminal(config)? } else { None };
    let approvals = approval_file
        .map(|path| {
            Preauthorizations::from_file(path, &config.cwd)
                .map_err(|_| failure("invalid_approval_file", "Goal approval file is invalid"))
        })
        .transpose()?;
    Ok(GoalRunContext {
        permissions: config.permissions_mode,
        confirmation: terminal
            .as_ref()
            .map(|terminal| terminal.clone() as Arc<dyn saya_core::runtime::ApprovalHandler>),
        input: terminal.map(|terminal| terminal as Arc<dyn saya_core::input::InputHandler>),
        approvals,
        personal_context: personal_context(cli, config)
            .map_err(|_| failure("personal_context", "Goal personal context is invalid"))?,
        anthropic_prompt_cache: cli.anthropic_prompt_cache,
        accept_config_change,
        cancellation,
    })
}

async fn continue_goal(
    goal_id: GoalId,
    cli: &Cli,
    config: &Config,
    approval_file: Option<&PathBuf>,
    accept_config_change: bool,
) -> Result<Value, BoundaryError> {
    let store = store(config)?;
    if cli.mode.is_some() {
        let mut goal = store.load(goal_id).await?;
        if goal.mode != config.mode {
            goal.mode = config.mode;
            for step in &mut goal.steps {
                if step.status == saya_core::goal::StepStatus::Pending {
                    step.mode = config.mode;
                }
            }
            let revision = goal.revision;
            store.save(goal, revision).await?;
        }
    }
    let cancel = Arc::new(Cancel::new());
    let _signals = start_signals(cancel.clone())?;
    let context = run_context(
        cli,
        config,
        approval_file,
        cancel,
        true,
        accept_config_change,
    )?;
    let outcome = continue_once(config, &store, goal_id, context).await?;
    Ok(json!({
        "schema_version":1,
        "data":{"goal":outcome.goal,"run":outcome.result}
    }))
}

async fn auto_loop(
    mut goal: GoalRecord,
    cli: &Cli,
    config: &Config,
    approval_file: Option<&PathBuf>,
    interactive: bool,
    accept_config_change: bool,
    cancel: Arc<Cancel>,
) -> Result<(GoalRecord, Vec<Value>), BoundaryError> {
    let store = store(config)?;
    let mut runs = Vec::new();
    loop {
        configured_auto_goal(config, &goal)?;
        if goal.status != GoalStatus::Ready {
            break;
        }
        if goal.budget_exhausted() {
            goal.status = GoalStatus::LimitReached;
            goal.phase = GoalPhase::Finished;
            goal.updated_at = saya_app::events::timestamp()?;
            let revision = goal.revision;
            goal = store.save(goal, revision).await?;
            break;
        }
        let context = run_context(
            cli,
            config,
            approval_file,
            cancel.clone(),
            interactive,
            accept_config_change,
        )?;
        let outcome = match continue_once(config, &store, goal.id, context).await {
            Ok(outcome) => outcome,
            Err(error) if error.0.code == "goal_budget" => {
                goal = store.load(goal.id).await?;
                break;
            }
            Err(error) => return Err(error),
        };
        runs.push(serde_json::to_value(&outcome.result).unwrap_or(Value::Null));
        goal = outcome.goal;
        if cancel.is_cancelled() {
            break;
        }
        if goal.status == GoalStatus::Blocked {
            goal = store
                .replan_failed_step(goal.id, saya_app::events::timestamp()?)
                .await?;
        }
        if !matches!(goal.status, GoalStatus::Ready) {
            break;
        }
    }
    Ok((goal, runs))
}

async fn start(
    goal_id: GoalId,
    auto: bool,
    cli: &Cli,
    config: &Config,
    approval_file: Option<&PathBuf>,
    accept_config_change: bool,
) -> Result<Value, BoundaryError> {
    if !auto {
        return Err(failure(
            "invalid_goal_start",
            "Use --auto to explicitly authorize finite automatic continuation",
        ));
    }
    if config.permissions_mode == PermissionMode::Manual {
        return Err(failure(
            "goal_authorization",
            "finite automatic continuation requires auto or bypass permissions",
        ));
    }
    let store = store(config)?;
    let mut goal = store.load(goal_id).await?;
    if goal.status != GoalStatus::Ready {
        return Err(failure(
            "invalid_goal_state",
            "Goal must be confirmed and ready before automatic start",
        ));
    }
    goal.mode = config.mode;
    for step in &mut goal.steps {
        if step.status == saya_core::goal::StepStatus::Pending {
            step.mode = config.mode;
        }
    }
    goal.authorization = Some(GoalAuthorization {
        auto: true,
        mode: config.mode,
        permissions: config.permissions_mode,
        config_fingerprint: goal_profile_fingerprint(config, &goal.profile)?,
        authorized_at: saya_app::events::timestamp()?,
    });
    goal.updated_at = saya_app::events::timestamp()?;
    let revision = goal.revision;
    goal = store.save(goal, revision).await?;
    let cancel = Arc::new(Cancel::new());
    let _signals = start_signals(cancel.clone())?;
    let (goal, runs) = auto_loop(
        goal,
        cli,
        config,
        approval_file,
        true,
        accept_config_change,
        cancel,
    )
    .await?;
    Ok(json!({"schema_version":1,"data":{"goal":goal,"runs":runs}}))
}

async fn worker_once(
    cli: &Cli,
    config: &Config,
    cancel: Arc<Cancel>,
) -> Result<Value, BoundaryError> {
    let store = store(config)?;
    let recovered = store
        .recover_unknown(saya_app::events::timestamp()?, now_ms())
        .await?;
    let goals = store.list().await?;
    let mut continued = Vec::new();
    for goal in goals {
        if goal.status != GoalStatus::Ready || goal.authorization.is_none() {
            continue;
        }
        if config.permissions_mode == PermissionMode::Manual
            || configured_auto_goal(config, &goal).is_err()
        {
            let blocked = store
                .transition(
                    goal.id,
                    GoalStatus::Blocked,
                    GoalPhase::Executing,
                    Some(saya_core::goal::GoalWaitReason::Authorization),
                    saya_app::events::timestamp()?,
                )
                .await?;
            continued.push(json!({"goal":blocked,"runs":[]}));
            continue;
        }
        let (goal, runs) = auto_loop(goal, cli, config, None, false, false, cancel.clone()).await?;
        continued.push(json!({"goal":goal,"runs":runs}));
    }
    Ok(json!({
        "schema_version":1,
        "data":{"recovered_unknown":recovered,"continued":continued}
    }))
}

async fn report(
    goal_id: GoalId,
    format: GoalReportFormat,
    file: Option<&PathBuf>,
    config: &Config,
) -> Result<Value, BoundaryError> {
    let goal = store(config)?.load(goal_id).await?;
    let bytes = match format {
        GoalReportFormat::Json => goal_report_json(&goal)?,
        GoalReportFormat::Markdown => goal_report_markdown(&goal)?,
    };
    if let Some(target) = file {
        let cancel = Arc::new(Cancel::new());
        let receipt = saya_app::export::publish(
            target,
            &bytes,
            &goal.workspace,
            config.mode,
            cancel.as_ref(),
        )?;
        Ok(json!({"schema_version":1,"data":{"goal_id":goal.id,"receipt":receipt}}))
    } else {
        if io::stdout().lock().write_all(&bytes).is_err() {
            return Err(failure("goal_report", "Goal report cannot be written"));
        }
        Ok(Value::Null)
    }
}

async fn control(
    action: GoalAction,
    config: &Config,
    transition: GoalStatus,
    phase: GoalPhase,
    reason: Option<saya_core::goal::GoalWaitReason>,
) -> Result<Value, BoundaryError> {
    let goal_id = match action {
        GoalAction::Pause { goal_id }
        | GoalAction::Resume { goal_id }
        | GoalAction::Cancel { goal_id } => parse_goal(&goal_id)?,
        _ => {
            return Err(failure(
                "invalid_goal_action",
                "Goal control action is invalid",
            ));
        }
    };
    let store = store(config)?;
    let goal = store
        .transition(
            goal_id,
            transition,
            phase,
            reason,
            saya_app::events::timestamp()?,
        )
        .await?;
    Ok(json!({"schema_version":1,"data":{"goal":goal}}))
}

pub async fn run(action: &GoalAction, cli: &Cli, config: &Config) -> Result<u8, BoundaryError> {
    let value = match action {
        GoalAction::Create { .. } => create_goal(action, cli, config).await?,
        GoalAction::Plan { goal_id, plan_file } => {
            plan(parse_goal(goal_id)?, plan_file.as_ref(), config).await?
        }
        GoalAction::Confirm { goal_id } => confirm(parse_goal(goal_id)?, config).await?,
        GoalAction::List => {
            let goals = store(config)?.list().await?;
            let value = json!({"schema_version":1,"data":{"goals":goals.clone()}});
            print_value(config, &value, || {
                for goal in goals {
                    println!(
                        "{} {:?} {}",
                        goal.id.0,
                        goal.status,
                        goal.objective.chars().take(80).collect::<String>()
                    );
                }
            });
            return Ok(0);
        }
        GoalAction::Status { goal_id } => {
            let goal = store(config)?.load(parse_goal(goal_id)?).await?;
            let value = json!({"schema_version":1,"data":{"goal":goal.clone()}});
            print_value(config, &value, || goal_text(&goal));
            return Ok(0);
        }
        GoalAction::Continue {
            goal_id,
            approval_file,
            accept_config_change,
        } => {
            continue_goal(
                parse_goal(goal_id)?,
                cli,
                config,
                approval_file.as_ref(),
                *accept_config_change,
            )
            .await?
        }
        GoalAction::Start {
            goal_id,
            auto,
            approval_file,
            accept_config_change,
        } => {
            start(
                parse_goal(goal_id)?,
                *auto,
                cli,
                config,
                approval_file.as_ref(),
                *accept_config_change,
            )
            .await?
        }
        GoalAction::Pause { .. } => {
            control(
                action.clone(),
                config,
                GoalStatus::Paused,
                GoalPhase::Executing,
                None,
            )
            .await?
        }
        GoalAction::Resume { .. } => {
            control(
                action.clone(),
                config,
                GoalStatus::Ready,
                GoalPhase::Executing,
                None,
            )
            .await?
        }
        GoalAction::Cancel { .. } => {
            control(
                action.clone(),
                config,
                GoalStatus::Cancelled,
                GoalPhase::Finished,
                None,
            )
            .await?
        }
        GoalAction::Verify {
            goal_id,
            criterion,
            summary,
        } => {
            let cancel = Arc::new(Cancel::new());
            let _signals = start_signals(cancel.clone())?;
            let goal = verify_manual_criterion(
                config,
                &store(config)?,
                parse_goal(goal_id)?,
                criterion,
                summary.clone(),
                cancel.as_ref(),
            )
            .await?;
            json!({"schema_version":1,"data":{"goal":goal}})
        }
        GoalAction::Resolve {
            goal_id,
            step_id,
            retry,
        } => {
            let goal = store(config)?
                .resolve_unknown_step(
                    parse_goal(goal_id)?,
                    parse_step(step_id)?,
                    *retry,
                    saya_app::events::timestamp()?,
                )
                .await?;
            json!({"schema_version":1,"data":{"goal":goal}})
        }
        GoalAction::Replan { goal_id } => {
            let goal = store(config)?
                .replan_failed_step(parse_goal(goal_id)?, saya_app::events::timestamp()?)
                .await?;
            json!({"schema_version":1,"data":{"goal":goal}})
        }
        GoalAction::Report {
            goal_id,
            format,
            file,
        } => {
            let value = report(parse_goal(goal_id)?, *format, file.as_ref(), config).await?;
            if value.is_null() {
                return Ok(0);
            }
            value
        }
        GoalAction::Worker { once } => {
            let cancel = Arc::new(Cancel::new());
            let _signals = start_signals(cancel.clone())?;
            if *once {
                worker_once(cli, config, cancel).await?
            } else {
                let mut latest = json!({"schema_version":1,"data":{"goals":[]}});
                loop {
                    latest = worker_once(cli, config, cancel.clone()).await?;
                    if cancel.is_cancelled() {
                        break latest;
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    };
    let value = value;
    print_value(config, &value, || {
        if let Some(goal) = value
            .get("data")
            .and_then(|data| data.get("goal"))
            .and_then(|goal| serde_json::from_value::<GoalRecord>(goal.clone()).ok())
        {
            goal_text(&goal);
        }
        if let Some(text) = value
            .get("data")
            .and_then(|data| data.get("run"))
            .and_then(|run| run.get("text"))
            .and_then(Value::as_str)
        {
            println!("\n{text}");
        }
    });
    let run_failed = value
        .get("data")
        .and_then(|data| data.get("run"))
        .and_then(|run| run.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status != "completed");
    let goal_status = value
        .get("data")
        .and_then(|data| data.get("goal"))
        .and_then(|goal| goal.get("status"))
        .and_then(Value::as_str);
    Ok(match goal_status {
        Some("cancelled") if !matches!(action, GoalAction::Cancel { .. }) => 130,
        Some("cancelled") => 0,
        Some("limit_reached") => 5,
        Some("failed" | "blocked") => 1,
        _ => u8::from(run_failed),
    })
}
