//! Read-only, bounded session snapshots. Source files are never opened for write.
use super::{APPLICATION_ID, VERSION, check_schema, error, session_metadata, sql_error};
use crate::{
    boundary::Cancellation,
    contract::{
        ApprovalRequest, BoundaryError, Event, EventPayload, HistoryItem, Run, RunId, RunResult,
        SCHEMA_VERSION, Session, SessionId, ToolCall, ToolResult, ToolState,
    },
};
use rusqlite::{Connection, OptionalExtension};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

/// Limits include all collections together. Exceeding a limit returns an error
/// rather than presenting a partial snapshot as complete.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotLimits {
    /// Bounds combined source files and the reconstructed database image.
    pub max_source_bytes: u64,
    pub max_bytes: usize,
    pub max_rows: usize,
    pub timeout: Duration,
}
impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 128 * 1024 * 1024,
            max_bytes: 32 * 1024 * 1024,
            max_rows: 100_000,
            timeout: Duration::from_secs(5),
        }
    }
}
/// A normalized committed view. Provider-owned history remains internal to this
/// object and must be projected through the export service before publication.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSnapshot {
    pub created_at: String,
    pub source_database_schema: i64,
    pub last_change_seq: u64,
    pub last_event_seq: u64,
    pub session: Session,
    pub runs: Vec<Value>,
    pub tool_calls: Vec<Value>,
    pub approval_requests: Vec<Value>,
    pub events: Vec<Value>,
    pub changes: Vec<Value>,
    pub metrics: Vec<Value>,
    pub project_context: Vec<Value>,
    pub background_jobs: Vec<Value>,
}
struct Budget {
    limits: SnapshotLimits,
    check: crate::wal_recovery::SnapshotCheck,
    rows: usize,
    bytes: usize,
}
impl Budget {
    fn check(&self) -> Result<(), BoundaryError> {
        (self.check)()
    }
    fn account(&mut self, bytes: usize) -> Result<(), BoundaryError> {
        self.check()?;
        self.rows = self.rows.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
        if self.rows > self.limits.max_rows || self.bytes > self.limits.max_bytes {
            return Err(error(
                "snapshot_limit",
                "session snapshot exceeds configured read limits",
            ));
        }
        Ok(())
    }
}
fn rows(
    conn: &Connection,
    sql: &str,
    id: &str,
    budget: &mut Budget,
) -> Result<Vec<Value>, BoundaryError> {
    budget.check()?;
    let mut statement = conn.prepare(sql).map_err(sql_error)?;
    let mut rows = statement.query([id]).map_err(sql_error)?;
    let mut values = Vec::new();
    while let Some(row) = rows.next().map_err(sql_error)? {
        let raw: String = row.get(0).map_err(sql_error)?;
        budget.account(raw.len())?;
        values.push(
            serde_json::from_str(&raw)
                .map_err(|_| error("storage_corrupt", "invalid snapshot record"))?,
        );
    }
    Ok(values)
}
fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, BoundaryError> {
    serde_json::from_value(value.clone())
        .map_err(|_| error("storage_corrupt", "invalid typed snapshot record"))
}
fn encode<T: Serialize>(value: &T) -> Result<Value, BoundaryError> {
    serde_json::to_value(value)
        .map_err(|_| error("storage_corrupt", "cannot normalize snapshot record"))
}
fn validate_runs(
    rows: &mut [Value],
    session: SessionId,
    budget: &Budget,
) -> Result<HashSet<RunId>, BoundaryError> {
    let mut ids = HashSet::new();
    for row in rows {
        budget.check()?;
        let run: Run = decode(&row["run"])?;
        let stored: RunId = decode(&row["run_id"])?;
        if run.id != stored || run.session_id != session || !ids.insert(stored) {
            return Err(error("storage_corrupt", "snapshot run identity mismatch"));
        }
        row["run"] = encode(&run)?;
        if !row["result"].is_null() {
            let result: RunResult = decode(&row["result"])?;
            if result.schema_version != SCHEMA_VERSION
                || result.run_id != Some(stored)
                || result.session_id.is_some_and(|id| id != session)
            {
                return Err(error(
                    "storage_corrupt",
                    "snapshot run result identity mismatch",
                ));
            }
            row["result"] = encode(&result)?;
        }
    }
    Ok(ids)
}
fn validate_calls(
    rows: &mut [Value],
    runs: &HashSet<RunId>,
    budget: &Budget,
) -> Result<(), BoundaryError> {
    for row in rows {
        budget.check()?;
        let run: RunId = decode(&row["run_id"])?;
        let call: ToolCall = decode(&row["call"])?;
        let state: ToolState = decode(&row["state"])?;
        if !runs.contains(&run) || row["call_id"].as_str() != Some(call.call_id.as_str()) {
            return Err(error("storage_corrupt", "snapshot tool identity mismatch"));
        }
        row["call"] = encode(&call)?;
        row["state"] = encode(&state)?;
        if !row["result"].is_null() {
            let result: ToolResult = decode(&row["result"])?;
            row["result"] = encode(&result)?;
        }
    }
    Ok(())
}
fn validate_approvals(
    rows: &mut [Value],
    runs: &HashSet<RunId>,
    budget: &Budget,
) -> Result<(), BoundaryError> {
    for row in rows {
        budget.check()?;
        let run: RunId = decode(&row["run_id"])?;
        let request: ApprovalRequest = decode(&row["request"])?;
        if !runs.contains(&run)
            || request.run_id != run
            || row["approval_id"].as_str() != Some(request.approval_id.as_str())
        {
            return Err(error(
                "storage_corrupt",
                "snapshot approval identity mismatch",
            ));
        }
        row["request"] = encode(&request)?;
    }
    Ok(())
}
fn validate_events(
    rows: Vec<Value>,
    session: SessionId,
    runs: &HashSet<RunId>,
    budget: &Budget,
) -> Result<Vec<Value>, BoundaryError> {
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        budget.check()?;
        let event: Event = decode(&row["event"])?;
        let seq: u64 = decode(&row["stored_seq"])?;
        let run: Option<RunId> = decode(&row["stored_run_id"])?;
        if event.schema_version != SCHEMA_VERSION
            || event.session_id != session
            || event.seq != seq
            || seq == 0
            || event.run_id != run
            || run.is_some_and(|id| !runs.contains(&id))
        {
            return Err(error("storage_corrupt", "snapshot event identity mismatch"));
        }
        events.push(encode(&event)?);
    }
    Ok(events)
}
fn background_jobs(
    calls: &[Value],
    runs: &[Value],
    budget: &Budget,
) -> Result<Vec<Value>, BoundaryError> {
    let mut jobs = Vec::new();
    for parent in calls
        .iter()
        .filter(|c| c["call"]["name"] == "background_start")
    {
        budget.check()?;
        let Some(parent_id) = parent["call_id"].as_str() else {
            continue;
        };
        let prefix = crate::background::child_prefix(parent_id);
        let acknowledged = parent["result"]["data"]["job_id"].as_str();
        let child = calls.iter().find(|c| {
            c["run_id"] == parent["run_id"]
                && c["call_id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with(&prefix))
        });
        let finished = runs
            .iter()
            .any(|r| r["run_id"] == parent["run_id"] && !r["finished_at"].is_null());
        if let Some(child) = child {
            let state = if acknowledged.is_some_and(|id| child["call_id"] != id)
                || (child["state"] == "running" && finished)
            {
                "unknown"
            } else if child["result"]["error"]["code"] == "cancelled" {
                "cancelled"
            } else {
                child["state"].as_str().unwrap_or("unknown")
            };
            jobs.push(json!({"job_id":child["call_id"], "run_id":parent["run_id"], "parent_call_id":parent_id,
                "tool":child["call"]["name"], "state":state, "result":child["result"]}));
        } else if let Some(job_id) = acknowledged {
            let state = if !job_id.starts_with(&prefix) {
                "unknown"
            } else if finished {
                "not_executed"
            } else {
                "accepted"
            };
            jobs.push(json!({"job_id":job_id, "run_id":parent["run_id"], "parent_call_id":parent_id, "tool":null,"state":state,"result":null}));
        }
    }
    Ok(jobs)
}
fn read(
    root: PathBuf,
    session_id: SessionId,
    limits: SnapshotLimits,
    cancel: Arc<dyn Cancellation>,
) -> Result<SessionSnapshot, BoundaryError> {
    let deadline = Instant::now()
        .checked_add(limits.timeout)
        .ok_or_else(|| error("invalid_argument", "invalid snapshot timeout"))?;
    let check: crate::wal_recovery::SnapshotCheck = Arc::new(move || {
        if cancel.is_cancelled() {
            return Err(error("cancelled", "session snapshot cancelled"));
        }
        if Instant::now() >= deadline {
            return Err(error(
                "snapshot_timeout",
                "session snapshot deadline exceeded",
            ));
        }
        Ok(())
    });
    let result = read_checked(root, session_id, limits, check.clone());
    check()?;
    result
}
fn read_checked(
    root: PathBuf,
    session_id: SessionId,
    limits: SnapshotLimits,
    check: crate::wal_recovery::SnapshotCheck,
) -> Result<SessionSnapshot, BoundaryError> {
    let mut budget = Budget {
        limits,
        check: check.clone(),
        rows: 0,
        bytes: 0,
    };
    budget.check()?;
    if limits.max_source_bytes == 0
        || limits.max_source_bytes > 512 * 1024 * 1024
        || limits.max_bytes == 0
        || limits.max_rows == 0
    {
        return Err(error("invalid_argument", "invalid session snapshot limits"));
    }
    let root = std::fs::canonicalize(root)
        .map_err(|_| error("storage_error", "data directory is unavailable"))?;
    let mut conn = crate::wal_recovery::snapshot_checked(&root, limits.max_source_bytes, check)?;
    conn.execute_batch("PRAGMA query_only=ON")
        .map_err(sql_error)?;
    let tx = conn.transaction().map_err(sql_error)?;
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(sql_error)?;
    let app: i64 = tx
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(sql_error)?;
    if app != APPLICATION_ID {
        return Err(error("storage_corrupt", "unrecognized session database"));
    }
    if version != VERSION {
        return Err(error(
            if version < VERSION {
                "storage_migration_required"
            } else {
                "storage_version"
            },
            "session export requires the current database schema; open it explicitly for migration",
        ));
    }
    check_schema(&tx)?;
    budget.check()?;
    let mut session = session_metadata(&tx, session_id)?
        .ok_or_else(|| error("session_not_found", "session not found"))?;
    budget.account(
        serde_json::to_vec(&session)
            .map_err(|_| error("storage_corrupt", "invalid session metadata"))?
            .len(),
    )?;
    let key = session_id.0.to_string();
    let history = rows(
        &tx,
        "SELECT item FROM messages WHERE session_id=?1 ORDER BY ordinal",
        &key,
        &mut budget,
    )?;
    session.history = history
        .into_iter()
        .map(|v| {
            serde_json::from_value::<HistoryItem>(v)
                .map_err(|_| error("storage_corrupt", "invalid stored history"))
        })
        .collect::<Result<_, _>>()?;
    let mut runs = rows(
        &tx,
        "SELECT json_object('run_id',id,'run',json(run),'result',json(result),'started_at',started_at,'finished_at',finished_at) FROM runs WHERE session_id=?1 ORDER BY started_at,rowid",
        &key,
        &mut budget,
    )?;
    let mut tool_calls = rows(
        &tx,
        "SELECT json_object('run_id',c.run_id,'call_id',c.call_id,'call',json(c.call),'state',c.state,'result',json(c.result)) FROM tool_calls c JOIN runs r ON r.id=c.run_id WHERE r.session_id=?1 ORDER BY r.started_at,r.rowid,c.rowid",
        &key,
        &mut budget,
    )?;
    let mut approval_requests = rows(
        &tx,
        "SELECT json_object('run_id',a.run_id,'approval_id',a.approval_id,'request',json(a.request)) FROM approval_requests a JOIN runs r ON r.id=a.run_id WHERE r.session_id=?1 ORDER BY a.rowid",
        &key,
        &mut budget,
    )?;
    let events = rows(
        &tx,
        "SELECT json_object('stored_seq',seq,'stored_run_id',run_id,'event',json(event)) FROM events WHERE session_id=?1 ORDER BY seq",
        &key,
        &mut budget,
    )?;
    let run_ids = validate_runs(&mut runs, session_id, &budget)?;
    validate_calls(&mut tool_calls, &run_ids, &budget)?;
    validate_approvals(&mut approval_requests, &run_ids, &budget)?;
    let events = validate_events(events, session_id, &run_ids, &budget)?;
    let mut changes = rows(
        &tx,
        "SELECT json_object('seq',seq,'run_id',run_id,'kind',kind,'reference',reference) FROM change_log WHERE session_id=?1 ORDER BY seq",
        &key,
        &mut budget,
    )?;
    let (last_change_seq, last_event_seq): (u64, u64) = tx
        .query_row(
            "SELECT COALESCE((SELECT MAX(seq) FROM change_log WHERE session_id=?1),0),
                COALESCE((SELECT MAX(seq) FROM events WHERE session_id=?1),0)",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    let created_at: String = tx
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |r| {
            r.get(0)
        })
        .map_err(sql_error)?;
    let mut metrics = Vec::new();
    let mut project_context = Vec::new();
    for change in &mut changes {
        budget.check()?;
        let seq: u64 = decode(&change["seq"])?;
        let run: Option<RunId> = decode(&change["run_id"])?;
        if seq == 0 || run.is_some_and(|id| !run_ids.contains(&id)) {
            return Err(error(
                "storage_corrupt",
                "snapshot change identity mismatch",
            ));
        }
        let kind = change["kind"]
            .as_str()
            .ok_or_else(|| error("storage_corrupt", "invalid change kind"))?;
        if !matches!(kind, "activity" | "run_metrics" | "project_context") {
            continue;
        }
        if run.is_none() {
            return Err(error("storage_corrupt", "missing snapshot change run"));
        }
        let raw = change["reference"]
            .as_str()
            .ok_or_else(|| error("storage_corrupt", "missing snapshot reference"))?;
        let parsed: Value = serde_json::from_str(raw)
            .map_err(|_| error("storage_corrupt", "invalid snapshot reference"))?;
        let value = match kind {
            "activity" => encode(&decode::<EventPayload>(&parsed)?)?,
            "run_metrics" => encode(&decode::<crate::metrics::RunMetrics>(&parsed)?)?,
            _ => encode(&decode::<crate::project_context::ContextRecord>(&parsed)?)?,
        };
        let normalized = serde_json::to_string(&value)
            .map_err(|_| error("storage_corrupt", "cannot normalize snapshot reference"))?;
        match kind {
            "run_metrics" => {
                metrics.push(json!({"run_id":change["run_id"],"seq":seq,"data":value}))
            }
            "project_context" => {
                project_context.push(json!({"run_id":change["run_id"],"seq":seq,"data":value}))
            }
            _ => {}
        }
        change["reference"] = normalized.into();
    }
    // Read the optional config record in this transaction as an integrity check.
    let _: Option<String> = tx
        .query_row(
            "SELECT fingerprint FROM session_config WHERE session_id=?1",
            [&key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    budget.check()?;
    tx.commit().map_err(sql_error)?;
    let background_jobs = background_jobs(&tool_calls, &runs, &budget)?;
    budget.check()?;
    Ok(SessionSnapshot {
        created_at,
        source_database_schema: version,
        last_change_seq,
        last_event_seq,
        session,
        runs,
        tool_calls,
        approval_requests,
        events,
        changes,
        metrics,
        project_context,
        background_jobs,
    })
}
impl super::SqliteStore {
    /// Read a committed, bounded snapshot without creating, migrating, recovering
    /// or locking the source session. Source I/O and query iteration observe
    /// cancellation and the deadline. Busy source images can be retried by users.
    pub async fn read_snapshot(
        data_dir: &std::path::Path,
        session_id: SessionId,
        limits: SnapshotLimits,
        cancel: Arc<dyn Cancellation>,
    ) -> Result<SessionSnapshot, BoundaryError> {
        let root = data_dir.to_owned();
        tokio::task::spawn_blocking(move || read(root, session_id, limits, cancel))
            .await
            .map_err(|_| error("storage_error", "session snapshot worker failed"))?
    }
}
