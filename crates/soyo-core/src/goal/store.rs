//! Independent versioned SQLite storage for Goal lifecycle and dispatch intents.

use super::{
    DispatchState, GoalDispatch, GoalId, GoalRecord, GoalStatus, GoalWaitReason, StepId,
    StepStatus, StepVerificationResult, StepVerificationStatus, StepVerifier, VerificationSpec,
    failure,
};
use crate::{
    contract::{BoundaryError, ErrorInfo, RunId},
    metrics::RunMetrics,
    redaction,
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const DATABASE_NAME: &str = "goals.sqlite3";
const SCHEMA_VERSION: i64 = 2;
const APPLICATION_ID: i64 = 0x5347_4f31;
const MAX_GOAL_ROWS: usize = 512;
const V1_TO_V2_MIGRATION: &str = r#"
CREATE TABLE IF NOT EXISTS step_verifications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    goal_id TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    step_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    status TEXT NOT NULL,
    exit_code INTEGER,
    record_json TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS step_verifications_lookup
    ON step_verifications(goal_id, step_id, attempt DESC, id DESC);
"#;
const SCHEMA: &str = r#"
CREATE TABLE goals (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    workspace TEXT NOT NULL,
    profile TEXT NOT NULL,
    status TEXT NOT NULL,
    revision INTEGER NOT NULL,
    record TEXT NOT NULL
);
CREATE INDEX goals_status ON goals(status, revision);
CREATE TABLE leases (
    goal_id TEXT PRIMARY KEY REFERENCES goals(id) ON DELETE CASCADE,
    owner TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL
);
CREATE TABLE workspace_leases (
    workspace TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    owner TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL
);
CREATE TABLE dispatches (
    goal_id TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    step_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    state TEXT NOT NULL,
    PRIMARY KEY(goal_id, step_id, attempt)
);
CREATE INDEX dispatches_state ON dispatches(state);
CREATE TABLE step_verifications (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    goal_id TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    step_id TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    run_id TEXT NOT NULL,
    status TEXT NOT NULL,
    exit_code INTEGER,
    record_json TEXT NOT NULL,
    recorded_at TEXT NOT NULL
);
CREATE INDEX step_verifications_lookup
    ON step_verifications(goal_id, step_id, attempt DESC, id DESC);
"#;

fn storage(code: &str, message: &str) -> BoundaryError {
    failure(code, message)
}

fn sql_error(_error: rusqlite::Error) -> BoundaryError {
    storage("goal_storage", "Goal database operation failed")
}

fn io_error() -> BoundaryError {
    storage("goal_storage", "Goal storage I/O failed")
}

#[derive(Debug, Clone)]
pub struct StoreSettings {
    pub busy_timeout: Duration,
    pub default_lease_ttl_ms: u64,
}

impl Default for StoreSettings {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_secs(5),
            default_lease_ttl_ms: 60_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalLease {
    pub goal_id: GoalId,
    pub owner: String,
    pub expires_at_ms: u64,
}

#[derive(Clone)]
pub struct GoalStore {
    connection: Arc<Mutex<Connection>>,
    path: PathBuf,
    secrets: Arc<Vec<String>>,
    settings: StoreSettings,
}

fn id(value: impl std::fmt::Display) -> String {
    value.to_string()
}

fn redact_verification(
    mut verification: StepVerificationResult,
    secrets: &[String],
    limit: usize,
) -> Result<StepVerificationResult, BoundaryError> {
    let mut value = serde_json::to_value(&verification)
        .map_err(|_| storage("goal_storage", "Step verification cannot be encoded"))?;
    redaction::redact(&mut value, secrets).map_err(|_| {
        storage(
            "storage_sensitive_data",
            "Step verification contains protected data",
        )
    })?;
    verification = serde_json::from_value(value)
        .map_err(|_| storage("goal_storage", "Step verification cannot be decoded"))?;
    verification.bound_outputs(limit);
    verification.validate()?;
    Ok(verification)
}

fn verification_status_label(status: StepVerificationStatus) -> &'static str {
    match status {
        StepVerificationStatus::Passed => "passed",
        StepVerificationStatus::Failed => "failed",
        StepVerificationStatus::TimedOut => "timed_out",
        StepVerificationStatus::Cancelled => "cancelled",
        StepVerificationStatus::Denied => "denied",
        StepVerificationStatus::SkippedPlan => "skipped_plan",
        StepVerificationStatus::Unknown => "unknown",
    }
}

fn encode(record: &GoalRecord, secrets: &[String]) -> Result<String, BoundaryError> {
    record.validate()?;
    let mut value = serde_json::to_value(record)
        .map_err(|_| storage("goal_storage", "Goal record cannot be encoded"))?;
    let original = value.clone();
    redaction::redact(&mut value, secrets)
        .map_err(|_| storage("storage_sensitive_data", "Goal contains protected data"))?;
    if value != original {
        return Err(storage(
            "storage_sensitive_data",
            "Goal contains a configured credential",
        ));
    }
    let encoded = serde_json::to_string(&value)
        .map_err(|_| storage("goal_storage", "Goal record cannot be encoded"))?;
    if redaction::contains_secret(&encoded, secrets) {
        return Err(storage(
            "storage_sensitive_data",
            "Goal contains a configured credential",
        ));
    }
    Ok(encoded)
}

fn decode(raw: &str) -> Result<GoalRecord, BoundaryError> {
    let record: GoalRecord =
        serde_json::from_str(raw).map_err(|_| storage("goal_corrupt", "Goal record is invalid"))?;
    record
        .validate()
        .map_err(|_| storage("goal_corrupt", "Goal record violates its invariants"))?;
    Ok(record)
}

fn load_connection(connection: &Connection, goal_id: GoalId) -> Result<GoalRecord, BoundaryError> {
    let raw: String = connection
        .query_row(
            "SELECT record FROM goals WHERE id=?1",
            [id(goal_id.0)],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| storage("goal_not_found", "Goal was not found"))?;
    decode(&raw)
}

fn write_connection(
    connection: &Connection,
    record: &GoalRecord,
    secrets: &[String],
    expected_revision: Option<u64>,
) -> Result<(), BoundaryError> {
    let raw = encode(record, secrets)?;
    let status = serde_json::to_value(record.status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| storage("goal_storage", "Goal status cannot be encoded"))?;
    let changed = connection
        .execute(
            "UPDATE goals SET status=?2,revision=?3,record=?4 WHERE id=?1 AND (?5 IS NULL OR revision=?5)",
            params![
                id(record.id.0),
                status,
                i64::try_from(record.revision).unwrap_or(i64::MAX),
                raw,
                expected_revision.map(|value| i64::try_from(value).unwrap_or(i64::MAX))
            ],
        )
        .map_err(sql_error)?;
    if changed != 1 {
        return Err(storage("goal_not_found", "Goal was not found"));
    }
    Ok(())
}

fn active_lease(
    connection: &Connection,
    goal_id: GoalId,
    owner: &str,
    now_ms: u64,
) -> Result<(), BoundaryError> {
    let lease: Option<(String, u64)> = connection
        .query_row(
            "SELECT owner,expires_at_ms FROM leases WHERE goal_id=?1",
            [id(goal_id.0)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    match lease {
        Some((current, until)) if current == owner && until > now_ms => Ok(()),
        Some(_) => Err(storage("goal_leased", "Goal is owned by another worker")),
        None => Err(storage("goal_lease_lost", "Goal worker lease is missing")),
    }
}

fn resumed_state(
    record: &GoalRecord,
) -> Result<(GoalStatus, crate::goal::GoalPhase, Option<GoalWaitReason>), BoundaryError> {
    use crate::goal::GoalPhase;
    if record.current_step.is_some() {
        return Err(storage(
            "goal_running",
            "Goal step is still running or requires recovery",
        ));
    }
    match &record.wait_reason {
        Some(GoalWaitReason::PlanConfirmation) => {
            return Ok((
                GoalStatus::Waiting,
                GoalPhase::Planning,
                Some(GoalWaitReason::PlanConfirmation),
            ));
        }
        Some(reason @ GoalWaitReason::UnknownSideEffect { .. }) => {
            return Ok((
                GoalStatus::Waiting,
                GoalPhase::Verifying,
                Some(reason.clone()),
            ));
        }
        _ => {}
    }
    let all_steps_completed = !record.steps.is_empty()
        && record
            .steps
            .iter()
            .all(|step| step.status == StepStatus::Completed);
    if record.budget_exhausted() {
        return Ok((GoalStatus::LimitReached, GoalPhase::Finished, None));
    }
    if let Some(step) = record
        .steps
        .iter()
        .find(|step| step.status == StepStatus::Failed)
        && step
            .last_result
            .as_ref()
            .is_some_and(|result| result.status == crate::contract::RunStatus::Cancelled)
    {
        return Ok((
            GoalStatus::Waiting,
            GoalPhase::Verifying,
            Some(GoalWaitReason::UnknownSideEffect { step_id: step.id }),
        ));
    }
    if record.wait_reason == Some(GoalWaitReason::ExternalInput)
        || record
            .steps
            .iter()
            .any(|step| step.status == StepStatus::Failed)
    {
        return Ok((
            GoalStatus::Blocked,
            GoalPhase::Executing,
            Some(GoalWaitReason::ExternalInput),
        ));
    }
    if all_steps_completed {
        return Ok((
            GoalStatus::Waiting,
            GoalPhase::Verifying,
            Some(GoalWaitReason::Verification),
        ));
    }
    Ok((GoalStatus::Ready, GoalPhase::Executing, None))
}

impl GoalStore {
    /// Open the private Goal database. This performs only local schema work.
    pub fn open(data_dir: &Path, secrets: Vec<String>) -> Result<Self, BoundaryError> {
        Self::open_with_settings(data_dir, secrets, StoreSettings::default())
    }

    pub fn open_with_settings(
        data_dir: &Path,
        secrets: Vec<String>,
        settings: StoreSettings,
    ) -> Result<Self, BoundaryError> {
        if settings.busy_timeout.is_zero() || settings.default_lease_ttl_ms == 0 {
            return Err(storage(
                "invalid_goal_store",
                "Goal storage timeouts must be positive",
            ));
        }
        let root = data_dir.join("goals");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)
            .map_err(|_| io_error())?;
        let root = fs::canonicalize(&root).map_err(|_| io_error())?;
        let metadata = fs::symlink_metadata(&root).map_err(|_| io_error())?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(storage("goal_storage", "Goal directory is unsafe"));
        }
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(|_| io_error())?;
        let path = root.join(DATABASE_NAME);
        let created = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => {
                drop(file);
                true
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err(io_error()),
        };
        let metadata = fs::symlink_metadata(&path).map_err(|_| io_error())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(storage("goal_storage", "Goal database is unsafe"));
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|_| io_error())?;
        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sql_error)?;
        connection
            .busy_timeout(settings.busy_timeout)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(sql_error)?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_error)?;
        let application: i64 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(sql_error)?;
        if created {
            if version != 0 || application != 0 {
                return Err(storage("goal_corrupt", "new Goal database is not empty"));
            }
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            transaction.execute_batch(SCHEMA).map_err(sql_error)?;
            transaction
                .pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(sql_error)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
        } else if application == APPLICATION_ID && version == 1 {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            transaction
                .execute_batch(V1_TO_V2_MIGRATION)
                .map_err(sql_error)?;
            transaction
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
        } else if application != APPLICATION_ID || version != SCHEMA_VERSION {
            return Err(storage(
                if version > SCHEMA_VERSION {
                    "goal_storage_version"
                } else {
                    "goal_corrupt"
                },
                "Goal database version is unsupported",
            ));
        }
        let journal: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(sql_error)?;
        if !journal.eq_ignore_ascii_case("wal") {
            return Err(storage("goal_storage", "Goal storage WAL is unavailable"));
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path,
            secrets: Arc::new(secrets),
            settings,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn create(&self, record: GoalRecord) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let raw = encode(&record, &secrets)?;
            let status = serde_json::to_value(record.status)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(|| storage("goal_storage", "Goal status cannot be encoded"))?;
            connection
                .execute(
                    "INSERT INTO goals(id,session_id,workspace,profile,status,revision,record) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        id(record.id.0),
                        id(record.session_id.0),
                        record.workspace.to_string_lossy(),
                        record.profile,
                        status,
                        i64::try_from(record.revision).unwrap_or(i64::MAX),
                        raw
                    ],
                )
                .map_err(|_| storage("goal_exists", "Goal already exists"))?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn load(&self, goal_id: GoalId) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            load_connection(&connection, goal_id)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn list(&self) -> Result<Vec<GoalRecord>, BoundaryError> {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let mut statement = connection
                .prepare("SELECT record FROM goals ORDER BY revision DESC,id LIMIT ?1")
                .map_err(sql_error)?;
            let rows = statement
                .query_map([i64::try_from(MAX_GOAL_ROWS).unwrap_or(i64::MAX)], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(sql_error)?;
            let mut goals = Vec::new();
            for row in rows {
                goals.push(decode(&row.map_err(sql_error)?)?);
            }
            Ok(goals)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn save(
        &self,
        mut record: GoalRecord,
        expected_revision: u64,
    ) -> Result<GoalRecord, BoundaryError> {
        record.revision = expected_revision.saturating_add(1);
        record.validate()?;
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            write_connection(&connection, &record, &secrets, Some(expected_revision))
                .map_err(|_| storage("goal_conflict", "Goal revision changed before save"))?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn acquire_lease(
        &self,
        goal_id: GoalId,
        owner: String,
        now_ms: u64,
        ttl_ms: Option<u64>,
    ) -> Result<GoalLease, BoundaryError> {
        if owner.is_empty()
            || owner.len() > 128
            || owner.chars().any(crate::text::is_terminal_control)
        {
            return Err(storage("invalid_goal_lease", "Goal owner is invalid"));
        }
        let ttl = ttl_ms.unwrap_or(self.settings.default_lease_ttl_ms);
        if ttl == 0 {
            return Err(storage("invalid_goal_lease", "Goal lease must be positive"));
        }
        let expires = now_ms.saturating_add(ttl);
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let record = load_connection(&transaction, goal_id)?;
            let workspace = record.workspace.to_string_lossy().into_owned();
            let current: Option<(String, u64)> = transaction
                .query_row(
                    "SELECT owner,expires_at_ms FROM leases WHERE goal_id=?1",
                    [id(goal_id.0)],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(sql_error)?;
            if let Some((current, until)) = current
                && current != owner
                && until > now_ms
            {
                return Err(storage("goal_leased", "Goal is owned by another worker"));
            }
            let workspace_lease: Option<(String, String, u64)> = transaction
                .query_row(
                    "SELECT goal_id,owner,expires_at_ms FROM workspace_leases WHERE workspace=?1",
                    [&workspace],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(sql_error)?;
            if let Some((current_goal, current_owner, until)) = workspace_lease
                && until > now_ms
                && (current_goal != id(goal_id.0) || current_owner != owner)
            {
                return Err(storage(
                    "goal_workspace_busy",
                    "Workspace is owned by another active Goal worker",
                ));
            }
            transaction
                .execute(
                    "INSERT INTO leases(goal_id,owner,expires_at_ms) VALUES(?1,?2,?3) ON CONFLICT(goal_id) DO UPDATE SET owner=excluded.owner,expires_at_ms=excluded.expires_at_ms",
                    params![id(goal_id.0), owner.clone(), i64::try_from(expires).unwrap_or(i64::MAX)],
                )
                .map_err(sql_error)?;
            transaction
                .execute(
                    "INSERT INTO workspace_leases(workspace,goal_id,owner,expires_at_ms) VALUES(?1,?2,?3,?4) ON CONFLICT(workspace) DO UPDATE SET goal_id=excluded.goal_id,owner=excluded.owner,expires_at_ms=excluded.expires_at_ms",
                    params![workspace, id(goal_id.0), owner, i64::try_from(expires).unwrap_or(i64::MAX)],
                )
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
            Ok(GoalLease {
                goal_id,
                owner,
                expires_at_ms: expires,
            })
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn release_lease(&self, lease: &GoalLease) -> Result<(), BoundaryError> {
        let connection = self.connection.clone();
        let goal_id = lease.goal_id;
        let owner = lease.owner.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            connection
                .execute(
                    "DELETE FROM leases WHERE goal_id=?1 AND owner=?2",
                    params![id(goal_id.0), owner.clone()],
                )
                .map_err(sql_error)?;
            connection
                .execute(
                    "DELETE FROM workspace_leases WHERE goal_id=?1 AND owner=?2",
                    params![id(goal_id.0), owner],
                )
                .map_err(sql_error)?;
            Ok(())
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn renew_lease(
        &self,
        lease: &mut GoalLease,
        now_ms: u64,
        ttl_ms: Option<u64>,
    ) -> Result<(), BoundaryError> {
        let ttl = ttl_ms.unwrap_or(self.settings.default_lease_ttl_ms);
        if ttl == 0 {
            return Err(storage("invalid_goal_lease", "Goal lease must be positive"));
        }
        let expires = now_ms.saturating_add(ttl);
        let connection = self.connection.clone();
        let goal_id = lease.goal_id;
        let owner = lease.owner.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let changed = connection
                .execute(
                    "UPDATE leases SET expires_at_ms=?3 WHERE goal_id=?1 AND owner=?2",
                    params![
                        id(goal_id.0),
                        owner.clone(),
                        i64::try_from(expires).unwrap_or(i64::MAX)
                    ],
                )
                .map_err(sql_error)?;
            if changed != 1 {
                return Err(storage("goal_lease_lost", "Goal worker lease was lost"));
            }
            let changed = connection
                .execute(
                    "UPDATE workspace_leases SET expires_at_ms=?3 WHERE goal_id=?1 AND owner=?2",
                    params![
                        id(goal_id.0),
                        owner.clone(),
                        i64::try_from(expires).unwrap_or(i64::MAX)
                    ],
                )
                .map_err(sql_error)?;
            if changed != 1 {
                return Err(storage("goal_lease_lost", "Goal workspace lease was lost"));
            }
            Ok(())
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))??;
        lease.expires_at_ms = expires;
        Ok(())
    }

    pub async fn begin_dispatch(
        &self,
        lease: &GoalLease,
        run_id: RunId,
        now_ms: u64,
        now: String,
    ) -> Result<GoalDispatch, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        let goal_id = lease.goal_id;
        let owner = lease.owner.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            active_lease(&transaction, goal_id, &owner, now_ms)?;
            let mut record = load_connection(&transaction, goal_id)?;
            if record.status != GoalStatus::Ready {
                return Err(storage(
                    "invalid_goal_state",
                    "Goal is not ready for another step",
                ));
            }
            let step = record
                .next_pending_step()
                .cloned()
                .ok_or_else(|| storage("goal_no_step", "Goal has no runnable step"))?;
            if record.budget_exhausted() {
                record.status = GoalStatus::LimitReached;
                record.phase = crate::goal::GoalPhase::Finished;
                record.wait_reason = None;
                record.updated_at = now;
                record.revision = record.revision.saturating_add(1);
                write_connection(&transaction, &record, &secrets, None)?;
                transaction.commit().map_err(sql_error)?;
                return Err(storage("goal_budget", "Goal budget is exhausted"));
            }
            record.reserve_run()?;
            let attempt = step.attempts.saturating_add(1);
            let step_index = record
                .steps
                .iter()
                .position(|item| item.id == step.id)
                .ok_or_else(|| storage("goal_corrupt", "Goal step disappeared"))?;
            record.steps[step_index].status = StepStatus::Running;
            record.steps[step_index].attempts = attempt;
            record.steps[step_index].run_id = Some(run_id);
            record.current_step = Some(step.id);
            record.status = GoalStatus::Running;
            record.phase = crate::goal::GoalPhase::Executing;
            record.wait_reason = None;
            record.updated_at = now;
            record.revision = record.revision.saturating_add(1);
            record.validate()?;
            write_connection(&transaction, &record, &secrets, None)?;
            transaction
                .execute(
                    "INSERT INTO dispatches(goal_id,step_id,attempt,run_id,state) VALUES(?1,?2,?3,?4,'intent')",
                    params![id(goal_id.0), id(step.id.0), i64::from(attempt), id(run_id.0)],
                )
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
            Ok(GoalDispatch {
                goal: record,
                step_id: step.id,
                attempt,
                run_id,
            })
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn mark_running(
        &self,
        dispatch: &GoalDispatch,
        lease: &GoalLease,
        now_ms: u64,
    ) -> Result<(), BoundaryError> {
        let connection = self.connection.clone();
        let goal_id = lease.goal_id;
        let owner = lease.owner.clone();
        let step_id = dispatch.step_id;
        let attempt = dispatch.attempt;
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            active_lease(&transaction, goal_id, &owner, now_ms)?;
            let changed = transaction
                .execute(
                    "UPDATE dispatches SET state='running' WHERE goal_id=?1 AND step_id=?2 AND attempt=?3 AND state='intent'",
                    params![id(goal_id.0), id(step_id.0), i64::from(attempt)],
                )
                .map_err(sql_error)?;
            if changed != 1 {
                return Err(storage(
                    "goal_dispatch_conflict",
                    "Goal step dispatch changed before start",
                ));
            }
            transaction.commit().map_err(sql_error)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn finish_dispatch(
        &self,
        lease: &GoalLease,
        dispatch: &GoalDispatch,
        summary: super::GoalRunSummary,
        metrics: &RunMetrics,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        self.finish_dispatch_with_verification(lease, dispatch, summary, None, metrics, now)
            .await
    }

    pub async fn finish_dispatch_with_verification(
        &self,
        lease: &GoalLease,
        dispatch: &GoalDispatch,
        summary: super::GoalRunSummary,
        verification: Option<StepVerificationResult>,
        metrics: &RunMetrics,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        summary.validate()?;
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        let metrics = metrics.clone();
        let goal_id = lease.goal_id;
        let owner = lease.owner.clone();
        let step_id = dispatch.step_id;
        let attempt = dispatch.attempt;
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let current: Option<String> = transaction
                .query_row(
                    "SELECT owner FROM leases WHERE goal_id=?1",
                    [id(goal_id.0)],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            if current.as_deref() != Some(owner.as_str()) {
                return Err(storage("goal_lease_lost", "Goal worker lease was lost"));
            }
            let mut record = load_connection(&transaction, goal_id)?;
            if record.current_step != Some(step_id)
                || record
                    .steps
                    .iter()
                    .find(|step| step.id == step_id)
                    .and_then(|step| step.run_id)
                    != Some(summary.run_id)
            {
                return Err(storage(
                    "goal_dispatch_conflict",
                    "Goal step is not waiting for this Run result",
                ));
            }
            let observed_requests = metrics
                .execution_requests
                .saturating_add(metrics.review_requests)
                .saturating_add(metrics.compaction_requests);
            record.usage.model_requests = record
                .usage
                .model_requests
                .saturating_add(observed_requests.max(1));
            record.usage.tool_calls = record.usage.tool_calls.saturating_add(metrics.tool_calls);
            record.usage.active_seconds = record
                .usage
                .active_seconds
                .saturating_add(metrics.duration_ms.div_ceil(1000));
            let run_completed = summary.status == crate::contract::RunStatus::Completed
                && summary.persistence_saved;
            if run_completed {
                record.usage.add_usage(summary.usage.as_ref());
            } else {
                record.usage.add_incomplete_usage(summary.usage.as_ref());
            }
            let index = record
                .steps
                .iter()
                .position(|step| step.id == step_id)
                .ok_or_else(|| storage("goal_corrupt", "Goal step disappeared"))?;
            let verifier_limit = record.steps[index]
                .verifier
                .as_ref()
                .map_or(super::MAX_VERIFIER_OUTPUT_BYTES, |verifier| {
                    verifier.max_output_bytes
                });
            let verification = verification
                .map(|item| redact_verification(item, &secrets, verifier_limit))
                .transpose()?;
            if let Some(item) = &verification {
                if item.step_id != step_id
                    || item.attempt != attempt
                    || item.run_id != summary.run_id
                {
                    return Err(storage(
                        "goal_dispatch_conflict",
                        "Goal step verification identity differs from active dispatch",
                    ));
                }
                record.usage.active_seconds = record
                    .usage
                    .active_seconds
                    .saturating_add(item.duration_ms.div_ceil(1000));
                let encoded = serde_json::to_string(item)
                    .map_err(|_| storage("goal_storage", "Step verification cannot be encoded"))?;
                if redaction::contains_secret(&encoded, &secrets) {
                    return Err(storage(
                        "storage_sensitive_data",
                        "Step verification contains a configured credential",
                    ));
                }
                transaction
                    .execute(
                        "INSERT INTO step_verifications(goal_id,step_id,attempt,run_id,status,exit_code,record_json,recorded_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                        params![
                            id(goal_id.0),
                            id(step_id.0),
                            i64::from(attempt),
                            id(summary.run_id.0),
                            verification_status_label(item.status),
                            item.exit_code,
                            encoded,
                            item.recorded_at,
                        ],
                    )
                    .map_err(sql_error)?;
            }
            let has_verifier = record.steps[index].verifier.is_some();
            let verifier_passed = !has_verifier
                || verification.as_ref().is_some_and(|item| {
                    item.status == StepVerificationStatus::Passed && item.exit_code == Some(0)
                });
            let step_completed = run_completed && verifier_passed;
            let step_status = if step_completed {
                StepStatus::Completed
            } else {
                StepStatus::Failed
            };
            record.steps[index].status = step_status;
            record.steps[index].last_result = Some(summary.clone());
            record.steps[index].last_verification = verification.clone();
            record.current_step = None;
            record.updated_at = now;
            let mut dispatch_state = if step_completed {
                DispatchState::Completed
            } else {
                DispatchState::Failed
            };
            let requested_status = record.status;
            if step_completed
                && record
                    .steps
                    .iter()
                    .all(|step| step.status == StepStatus::Completed)
            {
                record.status = GoalStatus::Waiting;
                record.phase = crate::goal::GoalPhase::Verifying;
                record.wait_reason = Some(GoalWaitReason::Verification);
                record.failure = None;
            } else if step_completed {
                record.status = GoalStatus::Ready;
                record.phase = crate::goal::GoalPhase::Executing;
                record.wait_reason = None;
                record.failure = None;
            } else if !run_completed {
                record.status = GoalStatus::Blocked;
                record.phase = crate::goal::GoalPhase::Executing;
                record.failure = summary.stop_reason.clone().map(|message| ErrorInfo {
                    code: "goal_step_failed".into(),
                    message,
                });
                record.wait_reason = Some(GoalWaitReason::ExternalInput);
            } else {
                match verification.as_ref().map(|item| item.status) {
                    Some(StepVerificationStatus::Failed) => {
                        let code_suffix = verification
                            .as_ref()
                            .and_then(|item| item.exit_code)
                            .map(|code| format!("with exit code {code}"))
                            .unwrap_or_else(|| "without exit status".into());
                        record.status = GoalStatus::Blocked;
                        record.phase = crate::goal::GoalPhase::Executing;
                        record.wait_reason = Some(GoalWaitReason::ExternalInput);
                        record.failure = Some(ErrorInfo {
                            code: "goal_step_verifier_failed".into(),
                            message: verification
                                .as_ref()
                                .and_then(|item| item.error_message.clone())
                                .unwrap_or_else(|| format!("Step host verifier failed {code_suffix}")),
                        });
                    }
                    Some(StepVerificationStatus::Denied | StepVerificationStatus::SkippedPlan) => {
                        record.status = GoalStatus::Blocked;
                        record.phase = crate::goal::GoalPhase::Executing;
                        record.wait_reason = Some(GoalWaitReason::Authorization);
                        record.failure = Some(ErrorInfo {
                            code: verification
                                .as_ref()
                                .and_then(|item| item.error_code.clone())
                                .unwrap_or_else(|| "permission_denied".into()),
                            message: verification
                                .as_ref()
                                .and_then(|item| item.error_message.clone())
                                .unwrap_or_else(|| "Step verifier execution was denied".into()),
                        });
                    }
                    Some(StepVerificationStatus::TimedOut | StepVerificationStatus::Unknown) => {
                        dispatch_state = DispatchState::Unknown;
                        record.status = GoalStatus::Waiting;
                        record.phase = crate::goal::GoalPhase::Verifying;
                        record.wait_reason = Some(GoalWaitReason::UnknownSideEffect { step_id });
                        record.failure = Some(ErrorInfo {
                            code: verification
                                .as_ref()
                                .and_then(|item| item.error_code.clone())
                                .unwrap_or_else(|| "goal_verifier_unknown".into()),
                            message: verification
                                .as_ref()
                                .and_then(|item| item.error_message.clone())
                                .unwrap_or_else(|| {
                                    "Step verifier timed out or ended with an unknown outcome".into()
                                }),
                        });
                    }
                    Some(StepVerificationStatus::Cancelled) => {
                        record.status = GoalStatus::Waiting;
                        record.phase = crate::goal::GoalPhase::Verifying;
                        record.wait_reason = Some(GoalWaitReason::UnknownSideEffect { step_id });
                        record.failure = Some(ErrorInfo {
                            code: "cancelled".into(),
                            message: "Step verifier was cancelled".into(),
                        });
                    }
                    Some(StepVerificationStatus::Passed) | None => {
                        record.status = GoalStatus::Blocked;
                        record.phase = crate::goal::GoalPhase::Executing;
                        record.wait_reason = Some(GoalWaitReason::Authorization);
                        record.failure = Some(ErrorInfo {
                            code: "goal_not_verified".into(),
                            message: "Step verifier did not produce passing host evidence".into(),
                        });
                    }
                }
            }
            if record.budget_exhausted()
                && !matches!(record.wait_reason, Some(GoalWaitReason::UnknownSideEffect { .. }))
            {
                record.status = GoalStatus::LimitReached;
                record.phase = crate::goal::GoalPhase::Finished;
            }
            match requested_status {
                GoalStatus::Cancelled => {
                    record.status = GoalStatus::Cancelled;
                    record.phase = crate::goal::GoalPhase::Finished;
                    record.wait_reason = None;
                }
                GoalStatus::Paused => {
                    record.status = GoalStatus::Paused;
                    if record
                        .steps
                        .iter()
                        .all(|step| step.status == StepStatus::Completed)
                    {
                        record.phase = crate::goal::GoalPhase::Verifying;
                        record.wait_reason = Some(GoalWaitReason::Verification);
                    } else if matches!(
                        record.wait_reason,
                        Some(GoalWaitReason::UnknownSideEffect { .. })
                    ) {
                        record.phase = crate::goal::GoalPhase::Verifying;
                    } else if matches!(record.wait_reason, Some(GoalWaitReason::Authorization)) {
                        record.phase = crate::goal::GoalPhase::Executing;
                    } else if step_status == StepStatus::Failed {
                        record.phase = crate::goal::GoalPhase::Executing;
                        record.wait_reason = Some(GoalWaitReason::ExternalInput);
                    } else {
                        record.phase = crate::goal::GoalPhase::Executing;
                        record.wait_reason = None;
                    }
                }
                _ => {}
            }
            record.revision = record.revision.saturating_add(1);
            record.validate()?;
            write_connection(&transaction, &record, &secrets, None)?;
            transaction
                .execute(
                    "UPDATE dispatches SET state=?4 WHERE goal_id=?1 AND step_id=?2 AND attempt=?3",
                    params![
                        id(goal_id.0),
                        id(step_id.0),
                        i64::from(attempt),
                        match dispatch_state {
                            DispatchState::Intent => "intent",
                            DispatchState::Running => "running",
                            DispatchState::Completed => "completed",
                            DispatchState::Failed => "failed",
                            DispatchState::Unknown => "unknown",
                        }
                    ],
                )
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn set_step_verifier(
        &self,
        goal_id: GoalId,
        step_id: StepId,
        verifier: Option<StepVerifier>,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or_default();
            let leased: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM leases WHERE goal_id=?1 AND expires_at_ms>?2",
                    params![id(goal_id.0), i64::try_from(now_ms).unwrap_or(i64::MAX)],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if leased != 0 {
                return Err(storage(
                    "goal_running",
                    "Goal has an active worker; wait for it to finish before editing verifiers",
                ));
            }
            let mut record = load_connection(&transaction, goal_id)?;
            record.set_step_verifier(step_id, verifier, now)?;
            record.revision = record.revision.saturating_add(1);
            record.validate()?;
            write_connection(&transaction, &record, &secrets, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn step_verifications(
        &self,
        goal_id: GoalId,
    ) -> Result<Vec<StepVerificationResult>, BoundaryError> {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let mut statement = connection
                .prepare("SELECT record_json FROM step_verifications WHERE goal_id=?1 ORDER BY id")
                .map_err(sql_error)?;
            let rows = statement
                .query_map([id(goal_id.0)], |row| row.get::<_, String>(0))
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            rows.into_iter()
                .map(|raw| {
                    let parsed: StepVerificationResult =
                        serde_json::from_str(&raw).map_err(|_| {
                            storage("goal_corrupt", "Step verification record is invalid")
                        })?;
                    parsed.validate()?;
                    Ok(parsed)
                })
                .collect()
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn record_evidence(
        &self,
        goal_id: GoalId,
        evidence: super::GoalEvidence,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let mut record = load_connection(&transaction, goal_id)?;
            record.add_evidence(evidence)?;
            record.updated_at = now;
            record.revision = record.revision.saturating_add(1);
            record.validate()?;
            write_connection(&transaction, &record, &secrets, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn invalidate_file_evidence(
        &self,
        goal_id: GoalId,
        criterion_id: &str,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        let criterion_id = criterion_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let mut record = load_connection(&transaction, goal_id)?;
            let criterion = record
                .acceptance
                .iter()
                .find(|criterion| criterion.id == criterion_id)
                .ok_or_else(|| storage("goal_not_found", "acceptance criterion was not found"))?;
            if matches!(&criterion.verification, VerificationSpec::Manual)
                || record.status != GoalStatus::Waiting
                || record.wait_reason != Some(GoalWaitReason::Verification)
            {
                return Err(storage(
                    "invalid_goal_state",
                    "only stale file evidence awaiting verification can be invalidated",
                ));
            }
            let previous = record.evidence.len();
            record
                .evidence
                .retain(|evidence| evidence.criterion_id != criterion_id);
            if record.evidence.len() != previous {
                record.updated_at = now;
                record.revision = record.revision.saturating_add(1);
                write_connection(&transaction, &record, &secrets, None)?;
                transaction.commit().map_err(sql_error)?;
            }
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn recover_unknown(
        &self,
        now: String,
        now_ms: u64,
    ) -> Result<Vec<GoalRecord>, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let mut statement = transaction
                .prepare(
                    "SELECT d.goal_id,d.step_id,d.attempt,d.run_id FROM dispatches d LEFT JOIN leases l ON l.goal_id=d.goal_id WHERE d.state IN ('intent','running') AND (l.goal_id IS NULL OR l.expires_at_ms<=?1) ORDER BY d.goal_id",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map([i64::try_from(now_ms).unwrap_or(i64::MAX)], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(sql_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sql_error)?;
            drop(statement);
            let mut recovered = Vec::new();
            for (goal_id, step_id, attempt, run_id) in rows {
                transaction
                    .execute(
                        "UPDATE dispatches SET state='unknown' WHERE goal_id=?1 AND step_id=?2 AND attempt=?3",
                        params![goal_id, step_id, attempt],
                    )
                    .map_err(sql_error)?;
                let goal_id = goal_id
                    .parse::<uuid::Uuid>()
                    .map(GoalId)
                    .map_err(|_| storage("goal_corrupt", "Goal ID is invalid"))?;
                let mut record = load_connection(&transaction, goal_id)?;
                let step_id = step_id
                    .parse::<uuid::Uuid>()
                    .map(super::StepId)
                    .map_err(|_| storage("goal_corrupt", "Goal step ID is invalid"))?;
                let run_id = run_id
                    .parse::<uuid::Uuid>()
                    .map(RunId)
                    .map_err(|_| storage("goal_corrupt", "Goal Run ID is invalid"))?;
                if let Some(step) = record.steps.iter_mut().find(|step| step.id == step_id) {
                    step.status = StepStatus::Failed;
                    step.run_id = Some(run_id);
                }
                record.current_step = None;
                let requested_status = record.status;
                record.phase = crate::goal::GoalPhase::Verifying;
                record.wait_reason = Some(GoalWaitReason::UnknownSideEffect { step_id });
                record.failure = Some(ErrorInfo {
                    code: "goal_outcome_unknown".into(),
                    message: "worker stopped before the Run outcome was recorded".into(),
                });
                match requested_status {
                    GoalStatus::Cancelled => {
                        record.status = GoalStatus::Cancelled;
                        record.phase = crate::goal::GoalPhase::Finished;
                        record.wait_reason = None;
                        record.authorization = None;
                    }
                    GoalStatus::Paused => {
                        record.status = GoalStatus::Paused;
                    }
                    _ => {
                        record.status = GoalStatus::Waiting;
                    }
                }
                record.usage.model_requests =
                    record.usage.model_requests.saturating_add(1);
                record.usage.tool_calls = record.usage.tool_calls.saturating_add(1);
                record.updated_at = now.clone();
                record.revision = record.revision.saturating_add(1);
                record.validate()?;
                write_connection(&transaction, &record, &secrets, None)?;
                recovered.push(record);
            }
            transaction.commit().map_err(sql_error)?;
            Ok(recovered)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn resolve_unknown_step(
        &self,
        goal_id: GoalId,
        step_id: super::StepId,
        retry: bool,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let mut record = load_connection(&transaction, goal_id)?;
            if record.wait_reason != Some(GoalWaitReason::UnknownSideEffect { step_id }) {
                return Err(storage(
                    "invalid_goal_state",
                    "Goal is not waiting for this unknown step outcome",
                ));
            }
            if retry {
                let step = record
                    .steps
                    .iter_mut()
                    .find(|step| step.id == step_id)
                    .ok_or_else(|| storage("goal_corrupt", "Goal step disappeared"))?;
                step.status = StepStatus::Pending;
                step.run_id = None;
                step.last_result = None;
                step.last_verification = None;
                record.status = GoalStatus::Ready;
                record.phase = crate::goal::GoalPhase::Executing;
                record.wait_reason = None;
                record.failure = None;
            } else {
                record.status = GoalStatus::Blocked;
                record.phase = crate::goal::GoalPhase::Executing;
                record.wait_reason = Some(GoalWaitReason::ExternalInput);
            }
            record.updated_at = now;
            record.revision = record.revision.saturating_add(1);
            record.validate()?;
            write_connection(&transaction, &record, &secrets, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn replan_failed_step(
        &self,
        goal_id: GoalId,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let mut record = load_connection(&transaction, goal_id)?;
            if record.status != GoalStatus::Blocked
                || record.wait_reason != Some(GoalWaitReason::ExternalInput)
                || record.current_step.is_some()
            {
                return Err(storage(
                    "invalid_goal_state",
                    "Goal has no known failed step available for bounded replanning",
                ));
            }
            if record.usage.replans >= record.budget.max_replans {
                record.status = GoalStatus::LimitReached;
                record.phase = crate::goal::GoalPhase::Finished;
                record.updated_at = now;
                record.revision = record.revision.saturating_add(1);
                write_connection(&transaction, &record, &secrets, None)?;
                transaction.commit().map_err(sql_error)?;
                return Ok(record);
            }
            let step_index = record
                .steps
                .iter()
                .rposition(|step| step.status == StepStatus::Failed)
                .ok_or_else(|| storage("invalid_goal_state", "Goal has no failed step to replan"))?;
            let result = record.steps[step_index]
                .last_result
                .as_ref()
                .ok_or_else(|| storage("invalid_goal_state", "failed Goal step has no Run result"))?;
            if !result.persistence_saved
                || result.status == crate::contract::RunStatus::Cancelled
                || result
                    .error_code
                    .as_deref()
                    .is_some_and(|code| code.contains("unknown"))
                || record.steps[step_index]
                    .last_verification
                    .as_ref()
                    .is_some_and(|verification| {
                        matches!(
                            verification.status,
                            StepVerificationStatus::TimedOut
                                | StepVerificationStatus::Cancelled
                                | StepVerificationStatus::Unknown
                        )
                    })
            {
                record.status = GoalStatus::Waiting;
                record.phase = crate::goal::GoalPhase::Verifying;
                record.wait_reason = Some(GoalWaitReason::UnknownSideEffect {
                    step_id: record.steps[step_index].id,
                });
                record.updated_at = now;
                record.revision = record.revision.saturating_add(1);
                write_connection(&transaction, &record, &secrets, None)?;
                transaction.commit().map_err(sql_error)?;
                return Ok(record);
            }
            if record.steps[step_index]
                .last_verification
                .as_ref()
                .is_some_and(|verification| {
                    matches!(
                        verification.status,
                        StepVerificationStatus::Denied | StepVerificationStatus::SkippedPlan
                    )
                })
            {
                return Err(storage(
                    "invalid_goal_state",
                    "Denied or PLAN-skipped step verifiers cannot be automatically replanned",
                ));
            }
            let sanitize_inline = |raw: &str| -> String {
                raw.chars()
                    .map(|ch| {
                        if crate::text::is_terminal_control(ch) {
                            ' '
                        } else {
                            ch
                        }
                    })
                    .collect()
            };
            let reason = if let Some(verification) = &record.steps[step_index].last_verification {
                let mut detail = format!(
                    "Host step verifier `{}` failed (exit {})",
                    verification.argv.join(" "),
                    verification
                        .exit_code
                        .map(|code| code.to_string())
                        .unwrap_or_else(|| "none".into()),
                );
                if let Some(msg) = &verification.error_message {
                    detail.push_str(&format!(": {}", sanitize_inline(msg)));
                }
                let excerpt = |label: &str, text: &str| -> String {
                    let clean = sanitize_inline(text);
                    let trimmed = clean.trim();
                    if trimmed.is_empty() {
                        return String::new();
                    }
                    let mut out = trimmed.to_owned();
                    if out.len() > 1024 {
                        out.truncate(1024);
                        while !out.is_char_boundary(out.len()) {
                            out.pop();
                        }
                        out.push('…');
                    }
                    format!(" [{label}: {out}]")
                };
                detail.push_str(&excerpt("stdout", &verification.stdout_excerpt));
                detail.push_str(&excerpt("stderr", &verification.stderr_excerpt));
                detail
            } else {
                sanitize_inline(
                    &result
                        .stop_reason
                        .clone()
                        .or_else(|| result.error_code.clone())
                        .unwrap_or_else(|| "the previous attempt failed".into()),
                )
            };
            let step = &mut record.steps[step_index];
            let original = step
                .instruction
                .split_once(" Original task: ")
                .map(|(_, core)| core.to_owned())
                .unwrap_or_else(|| step.instruction.clone());
            step.instruction = format!(
                "Previous attempt failed: {reason}. Re-inspect current state before acting, preserve completed work, and do not replay an unconfirmed side effect. Original task: {original}"
            );
            if step.instruction.len() > super::MAX_GOAL_INSTRUCTION_BYTES {
                step.instruction.truncate(super::MAX_GOAL_INSTRUCTION_BYTES);
                while !step.instruction.is_char_boundary(step.instruction.len()) {
                    step.instruction.pop();
                }
            }
            step.status = StepStatus::Pending;
            step.run_id = None;
            step.last_result = None;
            step.last_verification = None;
            record.usage.replans = record.usage.replans.saturating_add(1);
            record.status = GoalStatus::Ready;
            record.phase = crate::goal::GoalPhase::Replanning;
            record.wait_reason = None;
            record.failure = None;
            record.updated_at = now;
            record.revision = record.revision.saturating_add(1);
            record.validate()?;
            write_connection(&transaction, &record, &secrets, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn reserve_verification(
        &self,
        goal_id: GoalId,
        criterion_id: &str,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        let criterion_id = criterion_id.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let mut record = load_connection(&transaction, goal_id)?;
            let criterion = record
                .acceptance
                .iter()
                .find(|criterion| criterion.id == criterion_id)
                .ok_or_else(|| storage("goal_not_found", "acceptance criterion was not found"))?;
            if record.status != GoalStatus::Waiting
                || record.wait_reason != Some(GoalWaitReason::Verification)
                || record.steps.is_empty()
                || record
                    .steps
                    .iter()
                    .any(|step| step.status != StepStatus::Completed)
            {
                return Err(storage(
                    "invalid_goal_state",
                    "Goal is not awaiting acceptance verification",
                ));
            }
            let manual = matches!(&criterion.verification, VerificationSpec::Manual);
            if manual && record.usage.model_requests >= record.budget.max_model_requests {
                record.status = GoalStatus::LimitReached;
                record.phase = crate::goal::GoalPhase::Finished;
                record.updated_at = now;
                record.revision = record.revision.saturating_add(1);
                write_connection(&transaction, &record, &secrets, None)?;
                transaction.commit().map_err(sql_error)?;
                return Err(storage(
                    "goal_budget",
                    "Goal model-request budget is exhausted",
                ));
            }
            if record.usage.verification_attempts >= record.budget.max_verification_attempts {
                record.status = GoalStatus::LimitReached;
                record.phase = crate::goal::GoalPhase::Finished;
                record.updated_at = now;
                record.revision = record.revision.saturating_add(1);
                write_connection(&transaction, &record, &secrets, None)?;
                transaction.commit().map_err(sql_error)?;
                return Err(storage(
                    "goal_budget",
                    "Goal verification budget is exhausted",
                ));
            }
            record.usage.verification_attempts =
                record.usage.verification_attempts.saturating_add(1);
            if manual {
                record.usage.model_requests = record.usage.model_requests.saturating_add(1);
            }
            record.updated_at = now;
            record.revision = record.revision.saturating_add(1);
            write_connection(&transaction, &record, &secrets, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }

    pub async fn transition(
        &self,
        goal_id: GoalId,
        status: GoalStatus,
        phase: crate::goal::GoalPhase,
        wait_reason: Option<GoalWaitReason>,
        now: String,
    ) -> Result<GoalRecord, BoundaryError> {
        let connection = self.connection.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| storage("goal_storage", "Goal database lock poisoned"))?;
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let mut record = load_connection(&transaction, goal_id)?;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or_default();
            let leased: i64 = transaction
                .query_row(
                    "SELECT COUNT(*) FROM leases WHERE goal_id=?1 AND expires_at_ms>?2",
                    params![id(goal_id.0), i64::try_from(now_ms).unwrap_or(i64::MAX)],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            if leased != 0 && !matches!(status, GoalStatus::Paused | GoalStatus::Cancelled) {
                return Err(storage(
                    "goal_running",
                    "Goal has an active worker; wait for its current Run to finish",
                ));
            }
            let allowed = match status {
                GoalStatus::Paused => matches!(
                    record.status,
                    GoalStatus::Ready
                        | GoalStatus::Running
                        | GoalStatus::Waiting
                        | GoalStatus::Blocked
                ),
                GoalStatus::Ready => {
                    record.status == GoalStatus::Paused
                        || (record.status == GoalStatus::Blocked
                            && !record
                                .steps
                                .iter()
                                .any(|step| step.status == StepStatus::Failed)
                            && record.wait_reason != Some(GoalWaitReason::ExternalInput)
                            && !matches!(
                                record.wait_reason,
                                Some(GoalWaitReason::UnknownSideEffect { .. })
                            ))
                }
                GoalStatus::Blocked => matches!(
                    record.status,
                    GoalStatus::Ready | GoalStatus::Running | GoalStatus::Waiting
                ),
                GoalStatus::Cancelled => {
                    !matches!(record.status, GoalStatus::Completed | GoalStatus::Cancelled)
                }
                _ => record.status == status,
            };
            if !allowed {
                return Err(storage(
                    "invalid_goal_state",
                    "requested Goal transition is not allowed from the current state",
                ));
            }
            let (next_status, next_phase, next_wait) = if status == GoalStatus::Paused {
                let phase = match record.wait_reason {
                    Some(GoalWaitReason::PlanConfirmation) => crate::goal::GoalPhase::Planning,
                    Some(
                        GoalWaitReason::Verification | GoalWaitReason::UnknownSideEffect { .. },
                    ) => crate::goal::GoalPhase::Verifying,
                    _ => crate::goal::GoalPhase::Executing,
                };
                (GoalStatus::Paused, phase, record.wait_reason.clone())
            } else if status == GoalStatus::Ready && record.status == GoalStatus::Paused {
                resumed_state(&record)?
            } else {
                (status, phase, wait_reason)
            };
            record.status = next_status;
            record.phase = next_phase;
            record.wait_reason = next_wait;
            if next_status == GoalStatus::Cancelled {
                record.authorization = None;
            }
            if matches!(next_status, GoalStatus::Ready | GoalStatus::Completed) {
                record.failure = None;
            }
            record.updated_at = now;
            record.revision = record.revision.saturating_add(1);
            record.validate()?;
            write_connection(&transaction, &record, &secrets, None)?;
            transaction.commit().map_err(sql_error)?;
            Ok(record)
        })
        .await
        .map_err(|_| storage("goal_storage", "Goal storage worker failed"))?
    }
}
