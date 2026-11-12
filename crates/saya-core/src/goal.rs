//! Durable Goal records, acceptance evidence, budgets and state validation.
//!
//! Goal orchestration composes bounded Runs but owns its own durable lifecycle.
//! Model text is data: only host-recorded evidence can complete an acceptance
//! criterion, and an uncertain tool outcome is never replayed automatically.

mod store;

pub use store::{GoalLease, GoalStore, StoreSettings};

use crate::{
    contract::{
        Artifact, BoundaryError, ErrorInfo, PermissionMode, RunId, RunResult, RunStatus, SessionId,
        Usage, WorkMode,
    },
    text,
};
use cap_std::fs::OpenOptionsExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

pub const GOAL_SCHEMA_VERSION: u32 = 1;
pub const MAX_GOAL_OBJECTIVE_BYTES: usize = 16 * 1024;
pub const MAX_GOAL_ACCEPTANCE: usize = 32;
pub const MAX_GOAL_STEPS: usize = 64;
pub const MAX_GOAL_TITLE_BYTES: usize = 256;
pub const MAX_GOAL_INSTRUCTION_BYTES: usize = 32 * 1024;
pub const MAX_GOAL_EVIDENCE_BYTES: usize = 16 * 1024;
const MAX_GOAL_EVIDENCE_FILE_BYTES: u64 = 32 * 1024 * 1024;

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

fn valid_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && !value.chars().any(text::is_terminal_control)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.is_relative()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::str::FromStr for $name {
            type Err = BoundaryError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value
                    .parse::<Uuid>()
                    .map(Self)
                    .map_err(|_| failure("invalid_goal_id", "Goal identifier is invalid"))
            }
        }
    };
}

id_type!(GoalId);
id_type!(StepId);
id_type!(EvidenceId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalStatus {
    Draft,
    Ready,
    Running,
    Waiting,
    Paused,
    Blocked,
    Completed,
    Failed,
    Cancelled,
    LimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPhase {
    Planning,
    Executing,
    Verifying,
    Replanning,
    Finished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchState {
    Intent,
    Running,
    Completed,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GoalWaitReason {
    PlanConfirmation,
    Verification,
    UnknownSideEffect { step_id: StepId },
    ExternalInput,
    Authorization,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[serde(deny_unknown_fields)]
pub enum VerificationSpec {
    Manual,
    FileExists { path: PathBuf },
    FileSha256 { path: PathBuf, sha256: String },
}

impl VerificationSpec {
    fn validate(&self) -> Result<(), BoundaryError> {
        match self {
            Self::Manual => Ok(()),
            Self::FileExists { path } if valid_relative_path(path) => Ok(()),
            Self::FileSha256 { path, sha256 }
                if valid_relative_path(path) && valid_digest(sha256) =>
            {
                Ok(())
            }
            _ => Err(failure(
                "invalid_goal",
                "file acceptance checks require a relative workspace path and a lowercase SHA-256",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptanceCriterion {
    pub id: String,
    pub text: String,
    #[serde(default = "manual_verification")]
    pub verification: VerificationSpec,
}

fn manual_verification() -> VerificationSpec {
    VerificationSpec::Manual
}

impl AcceptanceCriterion {
    fn validate(&self) -> Result<(), BoundaryError> {
        if !valid_identifier(&self.id) || !valid_text(&self.text, MAX_GOAL_INSTRUCTION_BYTES) {
            return Err(failure(
                "invalid_goal",
                "acceptance criterion identity or text is invalid",
            ));
        }
        self.verification.validate()
    }
}

fn bounded_file_digest(reader: impl Read) -> Result<String, BoundaryError> {
    let mut limited = reader.take(MAX_GOAL_EVIDENCE_FILE_BYTES + 1);
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = limited
            .read(&mut buffer)
            .map_err(|_| failure("goal_verification_failed", "acceptance file cannot be read"))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > MAX_GOAL_EVIDENCE_FILE_BYTES {
            return Err(failure(
                "goal_verification_failed",
                "acceptance file exceeded its size limit while being read",
            ));
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Recheck a bounded workspace-relative file criterion without following links.
/// Manual criteria return `None` and must be verified by an explicit user action.
pub fn verify_file_criterion(
    workspace: &Path,
    criterion: &AcceptanceCriterion,
    observed_at: String,
) -> Result<Option<GoalEvidence>, BoundaryError> {
    criterion.validate()?;
    let (path, expected) = match &criterion.verification {
        VerificationSpec::Manual => return Ok(None),
        VerificationSpec::FileExists { path } => (path, None),
        VerificationSpec::FileSha256 { path, sha256 } => (path, Some(sha256.as_str())),
    };
    let mut components = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned),
            Component::CurDir => None,
            _ => Some(String::new()),
        })
        .collect::<Vec<_>>();
    let name = components.pop().unwrap_or_default();
    if name.is_empty() || components.iter().any(String::is_empty) {
        return Err(failure(
            "goal_verification_failed",
            "acceptance path is invalid",
        ));
    }
    let mut directory = cap_std::fs::Dir::open_ambient_dir(workspace, cap_std::ambient_authority())
        .map_err(|_| failure("goal_verification_failed", "workspace is unavailable"))?;
    for component in components {
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        );
        let file = directory
            .open_with(component, &options)
            .map_err(|_| failure("goal_verification_failed", "acceptance directory is unsafe"))?;
        directory = cap_std::fs::Dir::from_std_file(file.into_std());
    }
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = directory
        .open_with(name, &options)
        .map_err(|_| failure("goal_verification_failed", "acceptance file is unavailable"))?
        .into_std();
    let metadata = file
        .metadata()
        .map_err(|_| failure("goal_verification_failed", "acceptance file is unavailable"))?;
    if !metadata.is_file() || metadata.len() > MAX_GOAL_EVIDENCE_FILE_BYTES {
        return Err(failure(
            "goal_verification_failed",
            "acceptance file is not a bounded regular file",
        ));
    }
    let digest = if expected.is_some() {
        Some(bounded_file_digest(file)?)
    } else {
        None
    };
    if let (Some(expected), Some(actual)) = (expected, digest.as_deref())
        && expected != actual
    {
        return Err(failure(
            "goal_verification_failed",
            "acceptance file SHA-256 differs from the declared digest",
        ));
    }
    Ok(Some(GoalEvidence {
        id: EvidenceId::new(),
        criterion_id: criterion.id.clone(),
        step_id: None,
        run_id: None,
        kind: if expected.is_some() {
            EvidenceKind::FileDigest
        } else {
            EvidenceKind::FileExists
        },
        summary: if expected.is_some() {
            "Host rechecked the declared file SHA-256".into()
        } else {
            "Host confirmed the declared regular file exists".into()
        },
        path: Some(path.clone()),
        sha256: digest,
        observed_at,
    }))
}

fn report_text(value: &str, limit: usize) -> String {
    value
        .chars()
        .take(limit)
        .map(|character| {
            if character == '\n' || character == '\t' {
                character
            } else if text::is_terminal_control(character) {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect::<String>()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn goal_report_json(record: &GoalRecord) -> Result<Vec<u8>, BoundaryError> {
    let value = serde_json::json!({
        "schema_version": 1,
        "export_type": "saya.goal",
        "goal": record,
    });
    let mut bytes = serde_json::to_vec_pretty(&value)
        .map_err(|_| failure("goal_report", "Goal report cannot be encoded"))?;
    bytes.push(b'\n');
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(failure(
            "goal_report_limit",
            "Goal report exceeds its limit",
        ));
    }
    Ok(bytes)
}

pub fn goal_report_markdown(record: &GoalRecord) -> Result<Vec<u8>, BoundaryError> {
    let mut report = String::new();
    report.push_str("# Saya Goal report\n\n");
    report.push_str(&format!(
        "- Goal: `{}`\n- Status: `{:?}`\n- Phase: `{:?}`\n- Workspace: `{}`\n- Profile: `{}`\n- Mode: `{:?}`\n- Permissions: `{:?}`\n\n",
        record.id.0,
        record.status,
        record.phase,
        report_text(&record.workspace.to_string_lossy(), 4096),
        report_text(&record.profile, 256),
        record.mode,
        record.permissions
    ));
    report.push_str("## Objective\n\n");
    report.push_str(&report_text(&record.objective, MAX_GOAL_OBJECTIVE_BYTES));
    report.push_str("\n\n## Acceptance criteria\n\n");
    for criterion in &record.acceptance {
        let verified = record.criterion_verified(&criterion.id);
        report.push_str(&format!(
            "- [{}] `{}` {}\n",
            if verified { "x" } else { " " },
            report_text(&criterion.id, 64),
            report_text(&criterion.text, MAX_GOAL_INSTRUCTION_BYTES)
        ));
    }
    report.push_str("\n## Steps\n\n");
    for step in &record.steps {
        report.push_str(&format!(
            "### {}. {}\n\n- Status: `{:?}`\n- Mode: `{:?}`\n- Attempts: {}\n- Run: `{}`\n\n{}\n\n",
            step.ordinal,
            report_text(&step.title, MAX_GOAL_TITLE_BYTES),
            step.status,
            step.mode,
            step.attempts,
            step.run_id
                .map(|run| run.0.to_string())
                .unwrap_or_else(|| "—".into()),
            report_text(&step.instruction, MAX_GOAL_INSTRUCTION_BYTES)
        ));
        if let Some(result) = &step.last_result {
            report.push_str(&format!(
                "- Run status: `{:?}`\n- Persistence saved: `{}`\n\n",
                result.status, result.persistence_saved
            ));
        }
    }
    report.push_str("## Evidence\n\n");
    for evidence in &record.evidence {
        report.push_str(&format!(
            "- `{}` for `{}`: {} (`{:?}`, observed `{}`)\n",
            evidence.id.0,
            report_text(&evidence.criterion_id, 64),
            report_text(&evidence.summary, MAX_GOAL_EVIDENCE_BYTES),
            evidence.kind,
            report_text(&evidence.observed_at, 128)
        ));
    }
    report.push_str("\n## Budget and usage\n\n");
    report.push_str("```json\n");
    report.push_str(
        &serde_json::to_string_pretty(&serde_json::json!({
            "budget": record.budget,
            "usage": record.usage,
            "revision": record.revision,
            "updated_at": record.updated_at,
        }))
        .map_err(|_| failure("goal_report", "Goal report cannot be encoded"))?,
    );
    report.push_str("\n```\n");
    let bytes = report.into_bytes();
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(failure(
            "goal_report_limit",
            "Goal report exceeds its limit",
        ));
    }
    Ok(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalRunSummary {
    pub run_id: RunId,
    pub status: RunStatus,
    pub stop_reason: Option<String>,
    pub text: Option<String>,
    pub error_code: Option<String>,
    pub usage: Option<Usage>,
    pub artifacts: Vec<Artifact>,
    pub persistence_saved: bool,
    pub finished_at: String,
}

impl GoalRunSummary {
    pub fn from_result(result: &RunResult, finished_at: String) -> Result<Self, BoundaryError> {
        let run_id = result
            .run_id
            .ok_or_else(|| failure("invalid_goal_run", "Goal Run result has no Run ID"))?;
        Ok(Self {
            run_id,
            status: result.status,
            stop_reason: result.stop_reason.as_ref().map(|value| {
                let mut value = value.clone();
                value.truncate(MAX_GOAL_TITLE_BYTES);
                while !value.is_char_boundary(value.len()) {
                    value.pop();
                }
                value
            }),
            text: result.text.as_ref().map(|value| {
                let mut value = value.clone();
                value.truncate(MAX_GOAL_EVIDENCE_BYTES);
                while !value.is_char_boundary(value.len()) {
                    value.pop();
                }
                value
            }),
            error_code: result.error.as_ref().map(|error| error.code.clone()),
            usage: result.usage.clone(),
            artifacts: result.artifacts.clone(),
            persistence_saved: result
                .persistence
                .as_ref()
                .is_some_and(|persistence| persistence.saved),
            finished_at,
        })
    }

    fn validate(&self) -> Result<(), BoundaryError> {
        if self
            .text
            .as_ref()
            .is_some_and(|value| value.len() > MAX_GOAL_EVIDENCE_BYTES)
            || self
                .stop_reason
                .as_ref()
                .is_some_and(|value| value.len() > MAX_GOAL_TITLE_BYTES)
        {
            return Err(failure(
                "invalid_goal",
                "Goal Run summary exceeds its storage limit",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalStep {
    pub id: StepId,
    pub ordinal: u32,
    pub title: String,
    pub instruction: String,
    pub mode: WorkMode,
    #[serde(default)]
    pub depends_on: Vec<StepId>,
    pub status: StepStatus,
    pub attempts: u32,
    pub run_id: Option<RunId>,
    pub last_result: Option<GoalRunSummary>,
}

impl GoalStep {
    fn validate(&self, step_ids: &HashSet<StepId>) -> Result<(), BoundaryError> {
        if self.ordinal == 0
            || !valid_text(&self.title, MAX_GOAL_TITLE_BYTES)
            || !valid_text(&self.instruction, MAX_GOAL_INSTRUCTION_BYTES)
            || self.depends_on.len() > MAX_GOAL_STEPS
            || self
                .depends_on
                .iter()
                .any(|id| id == &self.id || !step_ids.contains(id))
        {
            return Err(failure(
                "invalid_goal",
                "Goal step identity, text, ordering or dependency is invalid",
            ));
        }
        if self.status == StepStatus::Running && self.run_id.is_none() {
            return Err(failure(
                "invalid_goal",
                "a running Goal step requires its Run ID",
            ));
        }
        if let Some(result) = &self.last_result {
            result.validate()?;
            if self.run_id != Some(result.run_id) {
                return Err(failure(
                    "invalid_goal",
                    "Goal step Run and result identity differ",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    Manual,
    FileExists,
    FileDigest,
    RunArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalEvidence {
    pub id: EvidenceId,
    pub criterion_id: String,
    pub step_id: Option<StepId>,
    pub run_id: Option<RunId>,
    pub kind: EvidenceKind,
    pub summary: String,
    pub path: Option<PathBuf>,
    pub sha256: Option<String>,
    pub observed_at: String,
}

impl GoalEvidence {
    fn validate(&self, criteria: &HashSet<&str>) -> Result<(), BoundaryError> {
        if !criteria.contains(self.criterion_id.as_str())
            || !valid_text(&self.summary, MAX_GOAL_EVIDENCE_BYTES)
            || self
                .path
                .as_ref()
                .is_some_and(|path| !valid_relative_path(path))
            || self
                .sha256
                .as_deref()
                .is_some_and(|digest| !valid_digest(digest))
        {
            return Err(failure(
                "invalid_goal",
                "Goal evidence identity, text, path or digest is invalid",
            ));
        }
        Ok(())
    }

    fn matches_criterion(&self, criterion: &AcceptanceCriterion) -> bool {
        match (&criterion.verification, &self.kind) {
            (VerificationSpec::Manual, EvidenceKind::Manual) => {
                self.path.is_none() && self.sha256.is_none()
            }
            (VerificationSpec::FileExists { path }, EvidenceKind::FileExists) => {
                self.path.as_ref() == Some(path) && self.sha256.is_none()
            }
            (VerificationSpec::FileSha256 { path, sha256 }, EvidenceKind::FileDigest) => {
                self.path.as_ref() == Some(path) && self.sha256.as_deref() == Some(sha256.as_str())
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalBudget {
    pub max_runs: u32,
    pub max_model_requests: u64,
    pub max_tool_calls: u64,
    pub max_active_seconds: u64,
    pub max_tokens: u64,
    pub max_planning_attempts: u32,
    pub max_verification_attempts: u32,
    pub max_replans: u32,
}

impl Default for GoalBudget {
    fn default() -> Self {
        Self {
            max_runs: 32,
            max_model_requests: 512,
            max_tool_calls: 4096,
            max_active_seconds: 24 * 60 * 60,
            max_tokens: 8_000_000,
            max_planning_attempts: 3,
            max_verification_attempts: 8,
            max_replans: 2,
        }
    }
}

impl GoalBudget {
    fn validate(self) -> Result<(), BoundaryError> {
        if self.max_runs == 0
            || self.max_model_requests == 0
            || self.max_tool_calls == 0
            || self.max_active_seconds == 0
            || self.max_tokens == 0
            || self.max_planning_attempts == 0
            || self.max_verification_attempts == 0
        {
            return Err(failure(
                "invalid_goal",
                "Goal budget limits must be positive",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalUsage {
    pub runs: u32,
    pub model_requests: u64,
    pub tool_calls: u64,
    pub active_seconds: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Successful model calls without complete token reporting stop new dispatch.
    #[serde(default)]
    pub token_usage_unknown: bool,
    pub planning_attempts: u32,
    pub verification_attempts: u32,
    pub replans: u32,
}

impl GoalUsage {
    fn tokens(self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    pub fn add_usage(&mut self, usage: Option<&Usage>) {
        self.add_usage_inner(usage, true);
    }

    /// Preserve known usage from a failed Run; bounded replanning owns its retry.
    pub fn add_incomplete_usage(&mut self, usage: Option<&Usage>) {
        self.add_usage_inner(usage, false);
    }

    fn add_usage_inner(&mut self, usage: Option<&Usage>, require_complete: bool) {
        let Some(usage) = usage else {
            if require_complete {
                self.token_usage_unknown = true;
            }
            return;
        };
        if let Some(input) = usage.input_tokens {
            self.input_tokens = self.input_tokens.saturating_add(input);
        } else if require_complete {
            self.token_usage_unknown = true;
        }
        if let Some(output) = usage.output_tokens {
            self.output_tokens = self.output_tokens.saturating_add(output);
        } else if require_complete {
            self.token_usage_unknown = true;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAuthorization {
    pub auto: bool,
    pub mode: WorkMode,
    pub permissions: PermissionMode,
    pub config_fingerprint: String,
    pub authorized_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalRecord {
    pub schema_version: u32,
    pub id: GoalId,
    pub session_id: SessionId,
    pub objective: String,
    pub workspace: PathBuf,
    pub profile: String,
    pub mode: WorkMode,
    pub permissions: PermissionMode,
    pub status: GoalStatus,
    pub phase: GoalPhase,
    pub acceptance: Vec<AcceptanceCriterion>,
    pub steps: Vec<GoalStep>,
    pub evidence: Vec<GoalEvidence>,
    pub budget: GoalBudget,
    pub usage: GoalUsage,
    pub current_step: Option<StepId>,
    pub wait_reason: Option<GoalWaitReason>,
    pub failure: Option<ErrorInfo>,
    pub authorization: Option<GoalAuthorization>,
    pub created_at: String,
    pub updated_at: String,
    pub revision: u64,
}

pub struct GoalSeed {
    pub session_id: SessionId,
    pub objective: String,
    pub workspace: PathBuf,
    pub profile: String,
    pub mode: WorkMode,
    pub permissions: PermissionMode,
    pub budget: GoalBudget,
    pub now: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannedStep {
    pub id: StepId,
    pub title: String,
    pub instruction: String,
    #[serde(default)]
    pub depends_on: Vec<StepId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalPlan {
    pub acceptance: Vec<AcceptanceCriterion>,
    pub steps: Vec<PlannedStep>,
}

impl GoalPlan {
    /// Parse the strict model plan shape. A single surrounding Markdown JSON
    /// fence is accepted because providers commonly decorate JSON responses.
    pub fn parse(raw: &str) -> Result<Self, BoundaryError> {
        let trimmed = raw.trim();
        let json = if let Some(rest) = trimmed.strip_prefix("```json")
            && let Some(json) = rest.strip_suffix("```")
        {
            json.trim()
        } else if let Some(rest) = trimmed.strip_prefix("```")
            && let Some(json) = rest.strip_suffix("```")
        {
            json.trim()
        } else {
            trimmed
        };
        let plan: Self = serde_json::from_str(json)
            .map_err(|_| failure("invalid_goal_plan", "Goal plan JSON is invalid"))?;
        plan.validate()?;
        Ok(plan)
    }

    pub fn validate(&self) -> Result<(), BoundaryError> {
        let mut probe = GoalRecord::new(GoalSeed {
            session_id: SessionId::default(),
            objective: "Goal plan validation".into(),
            workspace: PathBuf::from("/validation"),
            profile: "validation".into(),
            mode: WorkMode::Work,
            permissions: PermissionMode::Auto,
            budget: GoalBudget::default(),
            now: "validation".into(),
        })?;
        probe.apply_plan(self.clone(), "validation".into())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GoalDispatch {
    pub goal: GoalRecord,
    pub step_id: StepId,
    pub attempt: u32,
    pub run_id: RunId,
}

impl GoalRecord {
    pub fn new(seed: GoalSeed) -> Result<Self, BoundaryError> {
        let record = Self {
            schema_version: GOAL_SCHEMA_VERSION,
            id: GoalId::new(),
            session_id: seed.session_id,
            objective: seed.objective,
            workspace: seed.workspace,
            profile: seed.profile,
            mode: seed.mode,
            permissions: seed.permissions,
            status: GoalStatus::Draft,
            phase: GoalPhase::Planning,
            acceptance: Vec::new(),
            steps: Vec::new(),
            evidence: Vec::new(),
            budget: seed.budget,
            usage: GoalUsage::default(),
            current_step: None,
            wait_reason: None,
            failure: None,
            authorization: None,
            created_at: seed.now.clone(),
            updated_at: seed.now,
            revision: 0,
        };
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.schema_version != GOAL_SCHEMA_VERSION
            || !valid_text(&self.objective, MAX_GOAL_OBJECTIVE_BYTES)
            || !self.workspace.is_absolute()
            || !valid_identifier(&self.profile)
            || self.acceptance.len() > MAX_GOAL_ACCEPTANCE
            || self.steps.len() > MAX_GOAL_STEPS
            || self.evidence.len() > MAX_GOAL_ACCEPTANCE.saturating_mul(MAX_GOAL_STEPS)
        {
            return Err(failure(
                "invalid_goal",
                "Goal schema, objective, workspace, profile or collection limit is invalid",
            ));
        }
        self.budget.validate()?;
        let criteria = self
            .acceptance
            .iter()
            .map(|criterion| criterion.id.as_str())
            .collect::<HashSet<_>>();
        if criteria.len() != self.acceptance.len()
            || self.acceptance.iter().any(|item| item.validate().is_err())
        {
            return Err(failure(
                "invalid_goal",
                "Goal acceptance criteria are invalid or repeated",
            ));
        }
        let step_ids = self
            .steps
            .iter()
            .map(|step| step.id)
            .collect::<HashSet<_>>();
        if step_ids.len() != self.steps.len()
            || self
                .steps
                .iter()
                .any(|step| step.validate(&step_ids).is_err())
        {
            return Err(failure(
                "invalid_goal",
                "Goal steps are invalid or repeated",
            ));
        }
        validate_acyclic(&self.steps)?;
        if self.evidence.iter().any(|evidence| {
            evidence.validate(&criteria).is_err()
                || self
                    .acceptance
                    .iter()
                    .find(|criterion| criterion.id == evidence.criterion_id)
                    .is_none_or(|criterion| !evidence.matches_criterion(criterion))
        }) {
            return Err(failure(
                "invalid_goal",
                "Goal evidence does not match its acceptance criterion",
            ));
        }
        if self.current_step.is_some_and(|id| !step_ids.contains(&id)) {
            return Err(failure(
                "invalid_goal",
                "Goal current step is not part of this Goal",
            ));
        }
        if self.status == GoalStatus::Completed {
            let verified = self
                .evidence
                .iter()
                .map(|evidence| evidence.criterion_id.as_str())
                .collect::<HashSet<_>>();
            if self.acceptance.is_empty()
                || self.steps.is_empty()
                || self
                    .acceptance
                    .iter()
                    .any(|item| !verified.contains(item.id.as_str()))
                || self
                    .steps
                    .iter()
                    .any(|step| step.status != StepStatus::Completed)
            {
                return Err(failure(
                    "goal_not_verified",
                    "Goal completion requires every step and acceptance criterion to be verified",
                ));
            }
        }
        if self.status == GoalStatus::Waiting && self.wait_reason.is_none() {
            return Err(failure(
                "invalid_goal",
                "a waiting Goal requires a wait reason",
            ));
        }
        Ok(())
    }

    pub fn apply_plan(&mut self, plan: GoalPlan, now: String) -> Result<(), BoundaryError> {
        if !matches!(
            self.status,
            GoalStatus::Draft | GoalStatus::Paused | GoalStatus::Blocked
        ) || self.current_step.is_some()
        {
            return Err(failure(
                "invalid_goal_state",
                "Goal plan can be replaced only while no step is active",
            ));
        }
        if plan.steps.is_empty() || plan.acceptance.is_empty() {
            return Err(failure(
                "invalid_goal_plan",
                "Goal plan requires acceptance criteria and executable steps",
            ));
        }
        let mut steps = Vec::with_capacity(plan.steps.len());
        for (index, planned) in plan.steps.into_iter().enumerate() {
            if planned.id.0.get_version_num() != 7 {
                return Err(failure(
                    "invalid_goal_plan",
                    "Goal plan step IDs must be UUIDv7 values",
                ));
            }
            steps.push(GoalStep {
                id: planned.id,
                ordinal: u32::try_from(index + 1).unwrap_or(u32::MAX),
                title: planned.title,
                instruction: planned.instruction,
                mode: self.mode,
                depends_on: planned.depends_on,
                status: StepStatus::Pending,
                attempts: 0,
                run_id: None,
                last_result: None,
            });
        }
        self.acceptance = plan.acceptance;
        self.steps = steps;
        self.evidence.clear();
        self.status = GoalStatus::Waiting;
        self.phase = GoalPhase::Planning;
        self.wait_reason = Some(GoalWaitReason::PlanConfirmation);
        self.failure = None;
        self.updated_at = now;
        self.validate()
    }

    pub fn confirm_plan(&mut self, now: String) -> Result<(), BoundaryError> {
        if self.status != GoalStatus::Waiting
            || self.wait_reason != Some(GoalWaitReason::PlanConfirmation)
            || self.steps.is_empty()
        {
            return Err(failure(
                "invalid_goal_state",
                "Goal has no plan awaiting confirmation",
            ));
        }
        self.status = GoalStatus::Ready;
        self.phase = GoalPhase::Executing;
        self.wait_reason = None;
        self.updated_at = now;
        self.validate()
    }

    pub fn next_pending_step(&self) -> Option<&GoalStep> {
        let completed = self
            .steps
            .iter()
            .filter(|step| step.status == StepStatus::Completed)
            .map(|step| step.id)
            .collect::<HashSet<_>>();
        self.steps.iter().find(|step| {
            step.status == StepStatus::Pending
                && step.depends_on.iter().all(|id| completed.contains(id))
        })
    }

    pub fn add_evidence(&mut self, evidence: GoalEvidence) -> Result<(), BoundaryError> {
        if !matches!(self.status, GoalStatus::Ready | GoalStatus::Waiting)
            || self.current_step.is_some()
            || self.steps.is_empty()
            || (self.status == GoalStatus::Waiting
                && self.wait_reason != Some(GoalWaitReason::Verification))
        {
            return Err(failure(
                "invalid_goal_state",
                "Goal evidence requires an active plan with no running step",
            ));
        }
        let criterion = self
            .acceptance
            .iter()
            .find(|criterion| criterion.id == evidence.criterion_id)
            .ok_or_else(|| failure("invalid_goal_evidence", "acceptance criterion is unknown"))?;
        if !evidence.matches_criterion(criterion) {
            return Err(failure(
                "invalid_goal_evidence",
                "evidence does not match the acceptance verification method",
            ));
        }
        if evidence.kind == EvidenceKind::Manual
            && (self.status != GoalStatus::Waiting
                || self.wait_reason != Some(GoalWaitReason::Verification)
                || self
                    .steps
                    .iter()
                    .any(|step| step.status != StepStatus::Completed))
        {
            return Err(failure(
                "invalid_goal_state",
                "manual evidence requires a completed plan awaiting verification",
            ));
        }
        if let Some(existing) = self
            .evidence
            .iter_mut()
            .find(|item| item.criterion_id == evidence.criterion_id)
        {
            *existing = evidence;
        } else {
            self.evidence.push(evidence);
        }
        if self
            .acceptance
            .iter()
            .all(|criterion| self.criterion_verified(&criterion.id))
            && self
                .steps
                .iter()
                .all(|step| step.status == StepStatus::Completed)
        {
            self.status = GoalStatus::Completed;
            self.phase = GoalPhase::Finished;
            self.wait_reason = None;
            self.current_step = None;
            self.failure = None;
        }
        self.validate()
    }

    pub fn criterion_verified(&self, criterion_id: &str) -> bool {
        self.evidence
            .iter()
            .any(|evidence| evidence.criterion_id == criterion_id)
    }

    pub fn budget_exhausted(&self) -> bool {
        self.usage.token_usage_unknown
            || self.usage.runs >= self.budget.max_runs
            || self.usage.model_requests >= self.budget.max_model_requests
            || self.usage.tool_calls >= self.budget.max_tool_calls
            || self.usage.active_seconds >= self.budget.max_active_seconds
            || self.usage.tokens() >= self.budget.max_tokens
    }

    pub fn reserve_run(&mut self) -> Result<(), BoundaryError> {
        if self.usage.runs >= self.budget.max_runs {
            self.status = GoalStatus::LimitReached;
            self.phase = GoalPhase::Finished;
            return Err(failure("goal_budget", "Goal Run budget is exhausted"));
        }
        self.usage.runs = self.usage.runs.saturating_add(1);
        Ok(())
    }
}

fn validate_acyclic(steps: &[GoalStep]) -> Result<(), BoundaryError> {
    fn visit(
        id: StepId,
        graph: &HashMap<StepId, &GoalStep>,
        visiting: &mut HashSet<StepId>,
        visited: &mut HashSet<StepId>,
    ) -> Result<(), BoundaryError> {
        if visited.contains(&id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(failure(
                "invalid_goal",
                "Goal step dependencies contain a cycle",
            ));
        }
        for dependency in &graph[&id].depends_on {
            visit(*dependency, graph, visiting, visited)?;
        }
        visiting.remove(&id);
        visited.insert(id);
        Ok(())
    }

    let graph = steps
        .iter()
        .map(|step| (step.id, step))
        .collect::<HashMap<_, _>>();
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for step in steps {
        visit(step.id, &graph, &mut visiting, &mut visited)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
