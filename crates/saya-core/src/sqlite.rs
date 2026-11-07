//! Durable SQLite store. The caller must obtain a session lock before using this
//! for real side effects; crash recovery and locks are separate P5 steps.
use crate::{
    boundary::{Operation, Store, StoreChange},
    contract::{
        BoundaryError, ErrorInfo, EventPayload, HistoryItem, Persistence, RunId, RunResult,
        RunStatus, SCHEMA_VERSION, Session, SessionId, ToolResult, ToolState, WorkMode,
    },
};
use rusqlite::{
    Connection, Error as SqlError, ErrorCode, OpenFlags, OptionalExtension, Transaction,
    TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

mod snapshot;
pub use snapshot::{SessionSnapshot, SnapshotLimits};

const DB_NAME: &str = "sessions.sqlite3";
const APPLICATION_ID: i64 = 0x5341_5941;
const VERSION: i64 = 2;

/// One possible durable session location during an interrupted metadata update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardCandidate {
    pub workspace: PathBuf,
    pub work_mode: WorkMode,
}
#[derive(Debug, Serialize, Deserialize)]
struct SessionGuard {
    session_id: SessionId,
    candidates: Vec<GuardCandidate>,
}
fn candidate(session: &Session) -> GuardCandidate {
    GuardCandidate {
        workspace: session.workspace.clone(),
        work_mode: session.work_mode,
    }
}
fn read_guard(root: &Path, session_id: SessionId) -> Result<Option<SessionGuard>, BoundaryError> {
    let directory = root.join("locks");
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        _ => return Err(error("storage_corrupt", "invalid session guard directory")),
    }
    let path = directory.join(format!("{}.guard", session_id.0));
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(error("storage_corrupt", "cannot open session guard")),
    };
    if !file
        .metadata()
        .map_err(|_| error("storage_corrupt", "invalid session guard"))?
        .is_file()
    {
        return Err(error("storage_corrupt", "invalid session guard"));
    }
    let mut bytes = Vec::new();
    file.take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|_| error("storage_corrupt", "cannot read session guard"))?;
    if bytes.len() > 65_536 {
        return Err(error("storage_corrupt", "session guard is too large"));
    }
    let guard: SessionGuard = serde_json::from_slice(&bytes)
        .map_err(|_| error("storage_corrupt", "invalid session guard"))?;
    if guard.session_id != session_id
        || guard.candidates.is_empty()
        || guard.candidates.len() > 32
        || guard.candidates.iter().any(|c| !c.workspace.is_absolute())
    {
        return Err(error("storage_corrupt", "invalid session guard"));
    }
    Ok(Some(guard))
}
fn write_guard(root: &Path, guard: &SessionGuard, secrets: &[String]) -> Result<(), BoundaryError> {
    let content = json(guard, secrets)?;
    if content.len() > 65_536
        || guard
            .candidates
            .iter()
            .any(|item| !item.workspace.is_absolute())
    {
        return Err(error("storage_error", "invalid session guard data"));
    }
    let directory = root.join("locks");
    std::fs::create_dir_all(&directory)
        .map_err(|_| error("storage_error", "cannot create session guard directory"))?;
    if !std::fs::symlink_metadata(&directory)
        .map_err(|_| error("storage_error", "cannot inspect session guard directory"))?
        .is_dir()
    {
        return Err(error("storage_error", "invalid session guard directory"));
    }
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        guard.session_id.0,
        uuid::Uuid::now_v7()
    ));
    let target = directory.join(format!("{}.guard", guard.session_id.0));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|_| error("storage_error", "cannot create session guard"))?;
        file.write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| error("storage_error", "cannot save session guard"))?;
        std::fs::rename(&temporary, &target)
            .map_err(|_| error("storage_error", "cannot replace session guard"))?;
        File::open(&directory)
            .and_then(|file| file.sync_all())
            .map_err(|_| error("storage_error", "cannot sync session guard directory"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
fn prepare_guard(
    conn: &Connection,
    root: &Path,
    session: &Session,
    secrets: &[String],
) -> Result<(), BoundaryError> {
    let mut guard = read_guard(root, session.id)?.unwrap_or(SessionGuard {
        session_id: session.id,
        candidates: vec![],
    });
    let prior: Option<String> = conn
        .query_row(
            "SELECT metadata FROM sessions WHERE id=?1",
            [id(session.id.0)],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(prior) = prior {
        let prior: Session = serde_json::from_str(&prior)
            .map_err(|_| error("storage_corrupt", "invalid session data"))?;
        if prior.id != session.id {
            return Err(error("storage_corrupt", "invalid session identity"));
        }
        let old = candidate(&prior);
        if !guard.candidates.contains(&old) {
            guard.candidates.push(old);
        }
    }
    let next = candidate(session);
    if !guard.candidates.contains(&next) {
        guard.candidates.push(next);
    }
    if guard.candidates.len() > 32 {
        return Err(error(
            "storage_error",
            "too many unresolved session locations",
        ));
    }
    write_guard(root, &guard, secrets)
}
fn finalize_guard(root: &Path, session: &Session, secrets: &[String]) -> Result<(), BoundaryError> {
    write_guard(
        root,
        &SessionGuard {
            session_id: session.id,
            candidates: vec![candidate(session)],
        },
        secrets,
    )
}
const SCHEMA: &str = r#"
CREATE TABLE sessions (
 id TEXT PRIMARY KEY, metadata TEXT NOT NULL, created_at INTEGER NOT NULL DEFAULT (unixepoch()),
 updated_at INTEGER NOT NULL DEFAULT (unixepoch()), event_seq INTEGER NOT NULL DEFAULT 0,
 change_seq INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE messages (
 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
 ordinal INTEGER NOT NULL, item TEXT NOT NULL, PRIMARY KEY(session_id,ordinal)
);
CREATE TABLE session_config (
 session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
 fingerprint TEXT NOT NULL
);
CREATE TABLE runs (
 id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id), run TEXT NOT NULL,
 result TEXT, started_at INTEGER NOT NULL DEFAULT (unixepoch()), finished_at INTEGER
);
CREATE TABLE tool_calls (
 run_id TEXT NOT NULL REFERENCES runs(id), call_id TEXT NOT NULL, call TEXT NOT NULL,
 state TEXT NOT NULL, result TEXT, PRIMARY KEY(run_id,call_id)
);
CREATE TABLE approval_requests (
 approval_id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id), request TEXT NOT NULL
);
CREATE TABLE events (
 session_id TEXT NOT NULL REFERENCES sessions(id), seq INTEGER NOT NULL,
 run_id TEXT REFERENCES runs(id), event TEXT NOT NULL, PRIMARY KEY(session_id,seq)
);
CREATE TABLE change_log (
 session_id TEXT NOT NULL REFERENCES sessions(id), seq INTEGER NOT NULL,
 run_id TEXT REFERENCES runs(id), kind TEXT NOT NULL, reference TEXT,
 PRIMARY KEY(session_id,seq)
);
CREATE INDEX runs_by_session ON runs(session_id,started_at);
CREATE INDEX sessions_updated ON sessions(updated_at DESC);
"#;
const V2_MIGRATION: &str = "CREATE INDEX sessions_updated ON sessions(updated_at DESC)";
fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn sql_error(e: SqlError) -> BoundaryError {
    if let SqlError::SqliteFailure(ref inner, _) = e
        && matches!(
            inner.code,
            ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
        )
    {
        return error("storage_corrupt", "database is corrupt or is not SQLite");
    }
    error("storage_error", "database operation failed")
}
fn session_metadata(
    conn: &Connection,
    session_id: SessionId,
) -> Result<Option<Session>, BoundaryError> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(sql_error)?;
    let app: i64 = conn
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(sql_error)?;
    if !(1..=VERSION).contains(&version) || app != APPLICATION_ID {
        return Err(error(
            if version > VERSION {
                "storage_version"
            } else {
                "storage_corrupt"
            },
            "unsupported or unrecognized database",
        ));
    }
    let raw: Option<String> = conn
        .query_row(
            "SELECT metadata FROM sessions WHERE id=?1",
            [id(session_id.0)],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    raw.map(|raw| {
        let session: Session = serde_json::from_str(&raw)
            .map_err(|_| error("storage_corrupt", "invalid session data"))?;
        if session.id != session_id || !session.history.is_empty() {
            return Err(error("storage_corrupt", "invalid session identity"));
        }
        Ok(session)
    })
    .transpose()
}
fn check_schema(conn: &Connection) -> Result<(), BoundaryError> {
    let checked: String = conn
        .query_row("PRAGMA quick_check(1)", [], |r| r.get(0))
        .map_err(sql_error)?;
    if checked != "ok" {
        return Err(error("storage_corrupt", "database integrity check failed"));
    }
    for (table, columns) in [
        (
            "sessions",
            &[
                "id",
                "metadata",
                "created_at",
                "updated_at",
                "event_seq",
                "change_seq",
            ][..],
        ),
        ("messages", &["session_id", "ordinal", "item"][..]),
        ("session_config", &["session_id", "fingerprint"][..]),
        (
            "runs",
            &[
                "id",
                "session_id",
                "run",
                "result",
                "started_at",
                "finished_at",
            ][..],
        ),
        (
            "tool_calls",
            &["run_id", "call_id", "call", "state", "result"][..],
        ),
        (
            "approval_requests",
            &["approval_id", "run_id", "request"][..],
        ),
        ("events", &["session_id", "seq", "run_id", "event"][..]),
        (
            "change_log",
            &["session_id", "seq", "run_id", "kind", "reference"][..],
        ),
    ] {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(sql_error)?;
        let actual = stmt
            .query_map([table], |row| row.get::<_, String>(0))
            .map_err(sql_error)?
            .collect::<Result<std::collections::BTreeSet<_>, _>>()
            .map_err(sql_error)?;
        if columns.iter().any(|name| !actual.contains(*name)) {
            return Err(error("storage_corrupt", "database schema is incomplete"));
        }
    }
    let invalid: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_foreign_key_check)",
            [],
            |r| r.get(0),
        )
        .map_err(|_| error("storage_corrupt", "database foreign keys are invalid"))?;
    if invalid {
        return Err(error(
            "storage_corrupt",
            "database foreign keys are inconsistent",
        ));
    }
    Ok(())
}
fn backup_v1(conn: &Connection, root: &Path) -> Result<(), BoundaryError> {
    let path = root.join(format!("{DB_NAME}.v1-{}.backup", uuid::Uuid::now_v7()));
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|_| error("storage_error", "cannot create migration backup"))?;
    let mut destination = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sql_error)?;
    {
        let backup = rusqlite::backup::Backup::new(conn, &mut destination).map_err(sql_error)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match backup.step(128).map_err(sql_error)? {
                rusqlite::backup::StepResult::Done => break,
                rusqlite::backup::StepResult::More => {}
                _ if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                _ => return Err(error("storage_busy", "migration backup is busy")),
            }
        }
    }
    check_schema(&destination)?;
    let version: i64 = destination
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(sql_error)?;
    let app: i64 = destination
        .pragma_query_value(None, "application_id", |r| r.get(0))
        .map_err(sql_error)?;
    if version != 1 || app != APPLICATION_ID {
        return Err(error("storage_corrupt", "invalid migration backup"));
    }
    drop(destination);
    File::open(&path)
        .and_then(|file| file.sync_all())
        .map_err(|_| error("storage_error", "cannot sync migration backup"))?;
    File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| error("storage_error", "cannot sync migration backup directory"))?;
    Ok(())
}
fn json<T: Serialize>(value: &T, secrets: &[String]) -> Result<String, BoundaryError> {
    let value = serde_json::to_value(value)
        .map_err(|_| error("storage_error", "cannot serialize storage data"))?;
    if contains_secret(&value, secrets, 0) {
        return Err(error(
            "storage_sensitive_data",
            "storage data contains a configured secret",
        ));
    }
    let encoded = serde_json::to_string(&value)
        .map_err(|_| error("storage_error", "cannot serialize storage data"))?;
    if crate::redaction::contains_secret(&encoded, secrets) {
        return Err(error(
            "storage_sensitive_data",
            "storage data contains a configured secret",
        ));
    }
    Ok(encoded)
}
fn json_redacted<T: Serialize + serde::de::DeserializeOwned>(
    value: &T,
    secrets: &[String],
) -> Result<String, BoundaryError> {
    let mut value = serde_json::to_value(value)
        .map_err(|_| error("storage_error", "cannot serialize storage data"))?;
    crate::redaction::redact(&mut value, secrets).map_err(|_| {
        error(
            "storage_sensitive_data",
            "storage data cannot be safely redacted",
        )
    })?;
    serde_json::from_value::<T>(value.clone()).map_err(|_| {
        error(
            "storage_sensitive_data",
            "redaction changes a required storage field",
        )
    })?;
    serde_json::to_string(&value)
        .map_err(|_| error("storage_error", "cannot serialize storage data"))
}
fn history_json(item: &HistoryItem, secrets: &[String]) -> Result<String, BoundaryError> {
    match item {
        HistoryItem::ProviderItem { .. } => json(item, secrets),
        HistoryItem::ToolCall(call) => {
            json(&call.call_id, secrets)?;
            json(&call.name, secrets)?;
            json_redacted(item, secrets)
        }
        HistoryItem::ToolResult { call_id, .. } => {
            json(call_id, secrets)?;
            json_redacted(item, secrets)
        }
        _ => json_redacted(item, secrets),
    }
}
fn contains_secret(value: &Value, secrets: &[String], depth: usize) -> bool {
    match value {
        Value::String(s) => {
            if secrets
                .iter()
                .filter(|v| !v.is_empty())
                .any(|secret| s.contains(secret))
            {
                return true;
            }
            if depth < 4
                && let Ok(nested) = serde_json::from_str::<Value>(s)
                && !matches!(&nested,Value::String(inner) if inner==s)
            {
                return contains_secret(&nested, secrets, depth + 1);
            }
            false
        }
        Value::Array(values) => values
            .iter()
            .any(|v| contains_secret(v, secrets, depth + 1)),
        Value::Object(values) => values.iter().any(|(k, v)| {
            contains_secret(&Value::String(k.clone()), secrets, depth + 1)
                || contains_secret(v, secrets, depth + 1)
        }),
        _ => false,
    }
}
fn id(id: impl std::fmt::Display) -> String {
    id.to_string()
}
fn run_session(tx: &Transaction<'_>, run_id: &str) -> Result<String, BoundaryError> {
    tx.query_row("SELECT session_id FROM runs WHERE id=?1", [run_id], |row| {
        row.get(0)
    })
    .optional()
    .map_err(sql_error)?
    .ok_or_else(|| error("storage_error", "run not found"))
}
fn journal(
    tx: &Transaction<'_>,
    session: &str,
    run: Option<&str>,
    kind: &str,
    reference: Option<&str>,
) -> Result<(), BoundaryError> {
    let seq:i64=tx.query_row("UPDATE sessions SET change_seq=change_seq+1,updated_at=unixepoch() WHERE id=?1 RETURNING change_seq",[session],|r|r.get(0)).map_err(sql_error)?;
    tx.execute(
        "INSERT INTO change_log(session_id,seq,run_id,kind,reference) VALUES(?1,?2,?3,?4,?5)",
        params![session, seq, run, kind, reference],
    )
    .map_err(sql_error)?;
    Ok(())
}
fn mode_name(state: ToolState) -> &'static str {
    match state {
        ToolState::Requested => "requested",
        ToolState::WaitingConfirmation => "waiting_confirmation",
        ToolState::Running => "running",
        ToolState::Succeeded => "succeeded",
        ToolState::Failed => "failed",
        ToolState::Denied => "denied",
        ToolState::Unknown => "unknown",
    }
}
fn apply(
    tx: &Transaction<'_>,
    change: &StoreChange,
    secrets: &[String],
) -> Result<(), BoundaryError> {
    match change {
        StoreChange::Session(session) => {
            let key = id(session.id.0);
            let mut metadata = session.clone();
            metadata.history.clear();
            let data = json(&metadata, secrets)?;
            tx.execute("INSERT INTO sessions(id,metadata) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET metadata=excluded.metadata,updated_at=unixepoch()",params![key,data]).map_err(sql_error)?;
            tx.execute("DELETE FROM messages WHERE session_id=?1", [&key])
                .map_err(sql_error)?;
            for (index, item) in session.history.iter().enumerate() {
                let item = history_json(item, secrets)?;
                let index = i64::try_from(index)
                    .map_err(|_| error("storage_error", "too many messages"))?;
                tx.execute(
                    "INSERT INTO messages(session_id,ordinal,item) VALUES(?1,?2,?3)",
                    params![key, index, item],
                )
                .map_err(sql_error)?;
            }
            journal(tx, &key, None, "session", None)?;
        }
        StoreChange::RunStarted(run) => {
            let key = id(run.id.0);
            let session = id(run.session_id.0);
            let data = json(run, secrets)?;
            tx.execute(
                "INSERT INTO runs(id,session_id,run) VALUES(?1,?2,?3)",
                params![key, session, data],
            )
            .map_err(sql_error)?;
            journal(tx, &session, Some(&key), "run_started", None)?;
        }
        StoreChange::ProjectContext { run_id, record } => {
            let run = id(run_id.0);
            let session = run_session(tx, &run)?;
            json(&record.request_id, secrets)?;
            json(&record.fingerprint, secrets)?;
            let record = json_redacted(record, secrets)?;
            journal(tx, &session, Some(&run), "project_context", Some(&record))?;
        }
        StoreChange::Activity { run_id, payload } => {
            let run = id(run_id.0);
            let session = run_session(tx, &run)?;
            match payload {
                EventPayload::PermissionReviewed { call_id, .. } => {
                    json(call_id, secrets)?;
                }
                EventPayload::PermissionRequested {
                    approval_id,
                    call_id,
                    ..
                }
                | EventPayload::PermissionResolved {
                    approval_id,
                    call_id,
                    ..
                } => {
                    json(approval_id, secrets)?;
                    json(call_id, secrets)?;
                }
                EventPayload::InputRequested { input_id, .. }
                | EventPayload::InputReceived { input_id, .. } => {
                    json(input_id, secrets)?;
                }
                _ => {}
            }
            let record = json_redacted(payload, secrets)?;
            journal(tx, &session, Some(&run), "activity", Some(&record))?;
        }
        StoreChange::RunMetrics { run_id, metrics } => {
            let run = id(run_id.0);
            let session = run_session(tx, &run)?;
            let record = json(metrics, secrets)?;
            journal(tx, &session, Some(&run), "run_metrics", Some(&record))?;
        }
        StoreChange::History { session_id, item } => {
            let key = id(session_id.0);
            let data = history_json(item, secrets)?;
            let next: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(ordinal)+1,0) FROM messages WHERE session_id=?1",
                    [&key],
                    |r| r.get(0),
                )
                .map_err(sql_error)?;
            tx.execute(
                "INSERT INTO messages(session_id,ordinal,item) VALUES(?1,?2,?3)",
                params![key, next, data],
            )
            .map_err(sql_error)?;
            journal(tx, &key, None, "history", None)?;
        }
        StoreChange::ToolIntent { run_id, call } => {
            let run = id(run_id.0);
            let session = run_session(tx, &run)?;
            json(&call.call_id, secrets)?;
            json(&call.name, secrets)?;
            let data = json_redacted(call, secrets)?;
            tx.execute(
                "INSERT INTO tool_calls(run_id,call_id,call,state) VALUES(?1,?2,?3,'running')",
                params![run, call.call_id, data],
            )
            .map_err(sql_error)?;
            journal(tx, &session, Some(&run), "tool_intent", Some(&call.call_id))?;
        }
        StoreChange::ToolFinished {
            run_id,
            call_id,
            result,
        } => {
            let run = id(run_id.0);
            let session = run_session(tx, &run)?;
            json(call_id, secrets)?;
            let data = json_redacted(result, secrets)?;
            let count=tx.execute("UPDATE tool_calls SET state=?3,result=?4 WHERE run_id=?1 AND call_id=?2 AND state='running'",params![run,call_id,if result.ok{"succeeded"}else{"failed"},data]).map_err(sql_error)?;
            if count != 1 {
                return Err(error("storage_error", "tool is not running"));
            }
            journal(tx, &session, Some(&run), "tool_finished", Some(call_id))?;
        }
        StoreChange::ToolState {
            run_id,
            call_id,
            state,
        } => {
            let run = id(run_id.0);
            let session = run_session(tx, &run)?;
            json(call_id, secrets)?;
            let count = tx
                .execute(
                    "UPDATE tool_calls SET state=?3 WHERE run_id=?1 AND call_id=?2",
                    params![run, call_id, mode_name(*state)],
                )
                .map_err(sql_error)?;
            if count != 1 {
                return Err(error("storage_error", "tool not found"));
            }
            journal(tx, &session, Some(&run), "tool_state", Some(call_id))?;
        }
        StoreChange::ApprovalRequested(request) => {
            let run = id(request.run_id.0);
            let session = run_session(tx, &run)?;
            json(&request.approval_id, secrets)?;
            json(&request.call_id, secrets)?;
            let data = json_redacted(request, secrets)?;
            tx.execute(
                "INSERT INTO approval_requests(approval_id,run_id,request) VALUES(?1,?2,?3)",
                params![request.approval_id, run, data],
            )
            .map_err(sql_error)?;
            journal(
                tx,
                &session,
                Some(&run),
                "approval_requested",
                Some(&request.approval_id),
            )?;
        }
        StoreChange::RunFinished(result) => {
            let run = id(result
                .run_id
                .ok_or_else(|| error("storage_error", "run ID missing"))?
                .0);
            let session = run_session(tx, &run)?;
            if result.session_id.map(|v| id(v.0)).as_deref() != Some(&session) {
                return Err(error("storage_error", "run session mismatch"));
            }
            let data = json_redacted(result, secrets)?;
            let count=tx.execute("UPDATE runs SET result=?2,finished_at=unixepoch() WHERE id=?1 AND result IS NULL",params![run,data]).map_err(sql_error)?;
            if count != 1 {
                return Err(error("storage_error", "run already finished"));
            }
            journal(tx, &session, Some(&run), "run_finished", None)?;
        }
        StoreChange::Event(event) | StoreChange::ReservedEvent(event) => {
            if event.schema_version != SCHEMA_VERSION
                || event.seq == 0
                || matches!(event.payload, EventPayload::ModelTextDelta { .. })
            {
                return Err(error("storage_error", "invalid or non-persistent event"));
            }
            let session = id(event.session_id.0);
            let run = event.run_id.map(|v| id(v.0));
            if let Some(run_id) = &run
                && run_session(tx, run_id)? != session
            {
                return Err(error("storage_error", "event session mismatch"));
            }
            let seq = i64::try_from(event.seq)
                .map_err(|_| error("storage_error", "event sequence out of range"))?;
            if matches!(change, StoreChange::ReservedEvent(_)) {
                let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND event_seq>=?2) AND ?2>COALESCE((SELECT MAX(seq) FROM events WHERE session_id=?1),0)", params![session,seq], |r| r.get(0)).map_err(sql_error)?;
                if !valid {
                    return Err(error(
                        "storage_error",
                        "event sequence was not reserved or is not increasing",
                    ));
                }
            } else {
                let count=tx.execute("UPDATE sessions SET event_seq=?2,updated_at=unixepoch() WHERE id=?1 AND event_seq<?2",params![session,seq]).map_err(sql_error)?;
                if count != 1 {
                    return Err(error("storage_error", "event sequence is not increasing"));
                }
            }
            let data = json(event, secrets)?;
            tx.execute(
                "INSERT INTO events(session_id,seq,run_id,event) VALUES(?1,?2,?3,?4)",
                params![session, seq, run, data],
            )
            .map_err(sql_error)?;
        }
    }
    Ok(())
}
fn recover_transaction(
    conn: &mut Connection,
    session_id: SessionId,
    secrets: &[String],
) -> Result<RecoverySummary, BoundaryError> {
    use std::collections::{BTreeMap, BTreeSet};
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql_error)?;
    let key = id(session_id.0);
    let runs: Vec<String> = {
        let mut stmt = tx.prepare("SELECT id FROM runs WHERE session_id=?1 AND finished_at IS NULL ORDER BY started_at,id").map_err(sql_error)?;
        let rows = stmt.query_map([&key], |r| r.get(0)).map_err(sql_error)?;
        rows.collect::<Result<_, _>>().map_err(sql_error)?
    };
    if runs.is_empty() {
        return Ok(RecoverySummary::default());
    }
    let mut summary = RecoverySummary {
        interrupted_runs: runs.len(),
        ..Default::default()
    };
    let mut states: BTreeMap<String, (String, String, Option<String>)> = BTreeMap::new();
    for run in &runs {
        let mut stmt = tx
            .prepare("SELECT call_id,state,result FROM tool_calls WHERE run_id=?1")
            .map_err(sql_error)?;
        let rows = stmt
            .query_map([run], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(sql_error)?;
        for row in rows {
            let (call_id, state, result) = row.map_err(sql_error)?;
            if states
                .insert(call_id, (run.clone(), state, result))
                .is_some()
            {
                return Err(error("storage_corrupt", "duplicate interrupted tool call"));
            }
        }
    }
    let items: Vec<HistoryItem> = {
        let mut stmt = tx
            .prepare("SELECT item FROM messages WHERE session_id=?1 ORDER BY ordinal")
            .map_err(sql_error)?;
        let rows = stmt
            .query_map([&key], |r| r.get::<_, String>(0))
            .map_err(sql_error)?;
        rows.map(|row| {
            serde_json::from_str(&row.map_err(sql_error)?)
                .map_err(|_| error("storage_corrupt", "invalid stored history"))
        })
        .collect::<Result<_, _>>()?
    };
    let results: BTreeSet<String> = items
        .iter()
        .filter_map(|item| match item {
            HistoryItem::ToolResult { call_id, .. } => Some(call_id.clone()),
            _ => None,
        })
        .collect();
    let current_input = items
        .iter()
        .rposition(|item| matches!(item, HistoryItem::UserText(_)))
        .unwrap_or(0);
    if items[..current_input]
        .iter()
        .any(|item| matches!(item, HistoryItem::ToolCall(call) if !results.contains(&call.call_id)))
    {
        return Err(error(
            "storage_corrupt",
            "an earlier run has an unresolved tool call",
        ));
    }
    let mut calls = BTreeSet::new();
    let mut closed_inputs = BTreeSet::new();
    let mut background_notices = Vec::new();
    for item in &items[current_input..] {
        if let HistoryItem::ToolCall(call) = item {
            if !calls.insert(call.call_id.clone()) {
                return Err(error("storage_corrupt", "duplicate historical tool call"));
            }
            if results.contains(&call.call_id) {
                if call.name == "background_start" {
                    let recorded = items.iter().find_map(|item| match item {
                        HistoryItem::ToolResult { call_id, result }
                            if call_id == &call.call_id && result.ok =>
                        {
                            result
                                .data
                                .as_ref()
                                .and_then(|data| data.get("job_id"))
                                .and_then(Value::as_str)
                        }
                        _ => None,
                    });
                    if let Some(job_id) = recorded {
                        let linked =
                            job_id.starts_with(&crate::background::child_prefix(&call.call_id));
                        let (outcome, result) = if linked {
                            match states.get(job_id) {
                                Some((_, state, _)) if state == "running" || state == "unknown" => {
                                    ("unknown", None)
                                }
                                Some((_, _, Some(raw))) => (
                                    "finished",
                                    Some(serde_json::from_str::<ToolResult>(raw).map_err(
                                        |_| error("storage_corrupt", "invalid background result"),
                                    )?),
                                ),
                                Some(_) => ("unknown", None),
                                None => ("not_executed", None),
                            }
                        } else {
                            ("unknown", None)
                        };
                        let active = if outcome == "unknown" {
                            None
                        } else {
                            Some(false)
                        };
                        background_notices.push(serde_json::json!({"job_id":job_id,"outcome":outcome,"result":result,"active":active,"managed":false}));
                    }
                }
                continue;
            }
            let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM approval_requests WHERE run_id IN (SELECT id FROM runs WHERE session_id=?1 AND finished_at IS NULL) AND json_extract(request,'$.call_id')=?2)", params![key,call.call_id], |r|r.get(0)).map_err(sql_error)?;
            let result = if call.name == "background_start" {
                let prefix = crate::background::child_prefix(&call.call_id);
                let associated: Vec<_> = states
                    .iter()
                    .filter(|(id, _)| id.starts_with(&prefix))
                    .collect();
                if associated.len() > 1 {
                    return Err(error(
                        "storage_corrupt",
                        "multiple background jobs share a launch call",
                    ));
                }
                if let Some((job_id, (_, state, raw))) = associated.first() {
                    if state == "running" || state == "unknown" {
                        ToolResult { ok:false, data:Some(serde_json::json!({"job_id":job_id,"status":"interrupted","outcome":"unknown"})), error:Some(ErrorInfo {code:"outcome_unknown".into(), message:"background operation may have started; do not replay automatically".into()}), truncated:false }
                    } else if let Some(raw) = raw {
                        let child: ToolResult = serde_json::from_str(raw)
                            .map_err(|_| error("storage_corrupt", "invalid background result"))?;
                        ToolResult {
                            ok: true,
                            data: Some(
                                serde_json::json!({"job_id":job_id,"status":"interrupted","outcome":"finished","result":child}),
                            ),
                            error: None,
                            truncated: false,
                        }
                    } else {
                        return Err(error("storage_corrupt", "background result is missing"));
                    }
                } else {
                    let child_pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM approval_requests WHERE run_id IN (SELECT id FROM runs WHERE session_id=?1 AND finished_at IS NULL) AND substr(json_extract(request,'$.call_id'),1,?2)=?3)", params![key,prefix.len() as i64,prefix], |r|r.get(0)).map_err(sql_error)?;
                    let legacy = states
                        .keys()
                        .any(|id| id.starts_with("background-") && id.len() < 100);
                    let code = if child_pending {
                        "needs_confirmation"
                    } else if legacy {
                        "outcome_unknown"
                    } else {
                        "not_executed"
                    };
                    ToolResult {ok:false,data:None,error:Some(ErrorInfo {code:code.into(), message:"background submission was interrupted; no authorization or job was restored".into()}),truncated:false}
                }
            } else {
                match states.get(&call.call_id) {
                Some((_, state, _)) if state == "running" && call.name == "ask_user" => ToolResult { ok:false, data:None, error:Some(ErrorInfo { code:"input_interrupted".into(), message:"clarification did not complete durably; no answer was restored".into() }), truncated:false },
                Some((_, state, _)) if state == "running" || state == "unknown" => ToolResult { ok:false, data:None, error:Some(ErrorInfo { code:"outcome_unknown".into(), message:"operation may have started before interruption; do not replay automatically".into() }), truncated:false },
                Some((_, _, Some(raw))) => serde_json::from_str(raw).map_err(|_| error("storage_corrupt", "invalid stored tool result"))?,
                Some((_, state, None)) if state == "succeeded" || state == "failed" || state == "denied" => return Err(error("storage_corrupt", "completed tool result is missing")),
                Some((_, state, None)) if state == "waiting_confirmation" => ToolResult { ok:false, data:None, error:Some(ErrorInfo { code:"needs_confirmation".into(), message:"approval was interrupted; no authorization was restored".into() }), truncated:false },
                Some((_, state, None)) if state != "requested" => return Err(error("storage_corrupt", "invalid stored tool state")),
                _ if pending => ToolResult { ok:false, data:None, error:Some(ErrorInfo { code:"needs_confirmation".into(), message:"approval was interrupted; no authorization was restored".into() }), truncated:false },
                _ => ToolResult { ok:false, data:None, error:Some(ErrorInfo { code:"not_executed".into(), message:"run ended before this operation was dispatched".into() }), truncated:false },
            }
            };
            if (call.name == "ask_user" || call.name == "background_start")
                && let Some((run, state, _)) = states.get(&call.call_id)
                && state == "running"
            {
                let run_id: RunId = serde_json::from_value(serde_json::json!(run))
                    .map_err(|_| error("storage_corrupt", "invalid run ID"))?;
                apply(
                    &tx,
                    &StoreChange::ToolFinished {
                        run_id,
                        call_id: call.call_id.clone(),
                        result: result.clone(),
                    },
                    secrets,
                )?;
                closed_inputs.insert(call.call_id.clone());
            }
            apply(
                &tx,
                &StoreChange::History {
                    session_id,
                    item: HistoryItem::ToolResult {
                        call_id: call.call_id.clone(),
                        result,
                    },
                },
                secrets,
            )?;
            summary.closed_calls += 1;
        }
    }
    for (call_id, (run, state, _)) in &states {
        if state == "running" && !closed_inputs.contains(call_id) {
            let run_id: RunId = serde_json::from_value(serde_json::json!(run))
                .map_err(|_| error("storage_corrupt", "invalid run ID"))?;
            apply(
                &tx,
                &StoreChange::ToolState {
                    run_id,
                    call_id: call_id.clone(),
                    state: ToolState::Unknown,
                },
                secrets,
            )?;
            summary.unknown_tools += 1;
        }
    }
    if !background_notices.is_empty() {
        apply(
            &tx,
            &StoreChange::History {
                session_id,
                item: HistoryItem::UserText(
                    serde_json::json!({"saya_background_recovery_v1":{"jobs":background_notices}})
                        .to_string(),
                ),
            },
            secrets,
        )?;
    }
    for run in runs {
        let run_id: RunId = serde_json::from_value(serde_json::json!(run))
            .map_err(|_| error("storage_corrupt", "invalid run ID"))?;
        apply(
            &tx,
            &StoreChange::RunFinished(RunResult {
                schema_version: SCHEMA_VERSION,
                session_id: Some(session_id),
                run_id: Some(run_id),
                status: RunStatus::Failed,
                stop_reason: Some("interrupted".into()),
                text: None,
                error: Some(ErrorInfo {
                    code: "interrupted".into(),
                    message: "run ended unexpectedly; unfinished operations were not replayed"
                        .into(),
                }),
                usage: None,
                artifacts: vec![],
                persistence: Some(Persistence {
                    saved: true,
                    error: None,
                }),
            }),
            secrets,
        )?;
    }
    tx.commit().map_err(sql_error)?;
    Ok(summary)
}

/// SQLite WAL backed Store. Data are committed atomically with FULL sync.
/// Secret values are supplied by the host and cause fail-closed rejection if
/// they appear in serialized data; they are never formatted or written.
#[derive(Clone)]
pub struct SqliteStore {
    inner: Arc<Mutex<Connection>>,
    secrets: Arc<Vec<String>>,
    path: PathBuf,
}
/// Holds an operating-system session lock until dropped or the process exits.
pub struct SessionLock(File);
impl Drop for SessionLock {
    fn drop(&mut self) {
        // SAFETY: the descriptor remains open for the lifetime of this guard.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
/// A compact session listing, without loading complete conversation histories.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub id: SessionId,
    pub workspace: PathBuf,
    pub work_mode: crate::contract::WorkMode,
    pub profile: String,
    pub title: Option<String>,
    pub updated_at: i64,
    pub last_status: Option<crate::contract::RunStatus>,
}
/// Durable repairs made when exclusively opening an interrupted session.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RecoverySummary {
    pub interrupted_runs: usize,
    pub unknown_tools: usize,
    pub closed_calls: usize,
}
/// Persisted background outcome; these jobs are never restarted on resume.
#[derive(Debug, Serialize)]
pub struct BackgroundJobSummary {
    pub job_id: String,
    pub run_id: RunId,
    pub parent_call_id: String,
    pub tool: Option<String>,
    pub state: String,
    pub result: Option<ToolResult>,
}
/// Persisted per-run metrics and their associated run.
#[derive(Debug, Serialize)]
pub struct RunMetricsRecord {
    pub run_id: RunId,
    pub metrics: crate::metrics::RunMetrics,
}
impl SqliteStore {
    /// Return persisted metrics in session order.
    pub async fn run_metrics(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<RunMetricsRecord>, BoundaryError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn=inner.lock().map_err(|_|error("storage_error", "database lock failed"))?;
            let mut stmt=conn.prepare("SELECT run_id,reference FROM change_log WHERE session_id=?1 AND kind='run_metrics' ORDER BY seq").map_err(sql_error)?;
            let rows=stmt.query_map([id(session_id.0)], |r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(sql_error)?;
            rows.map(|row| {
                let (run,raw)=row.map_err(sql_error)?;
                let run_id=serde_json::from_value(serde_json::json!(run)).map_err(|_|error("storage_corrupt","invalid metrics run ID"))?;
                let metrics=serde_json::from_str(&raw).map_err(|_|error("storage_corrupt","invalid run metrics"))?;
                Ok(RunMetricsRecord {run_id,metrics})
            }).collect()
        }).await.map_err(|_|error("storage_error","database worker failed"))?
    }
    /// Reserve a sequence range while holding the session writer lock. Unused
    /// numbers remain gaps after interruption; visible numbers are never reused.
    pub async fn reserve_event_sequences(
        &self,
        session_id: SessionId,
        count: u32,
    ) -> Result<(u64, u64), BoundaryError> {
        if count == 0 {
            return Err(error("storage_error", "empty event sequence reservation"));
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().map_err(|_| error("storage_error", "database lock failed"))?;
            let end: i64 = conn.query_row("UPDATE sessions SET event_seq=event_seq+?2 WHERE id=?1 AND event_seq<=9223372036854775807-?2 RETURNING event_seq", params![id(session_id.0), i64::from(count)], |r| r.get(0)).map_err(sql_error)?;
            let end = u64::try_from(end).map_err(|_| error("storage_error", "invalid event sequence"))?;
            Ok((end-u64::from(count)+1,end))
        }).await.map_err(|_|error("storage_error", "database worker failed"))?
    }
    /// Return source metadata supplied to model requests in this session.
    pub async fn project_context_records(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<crate::project_context::ContextRecord>, BoundaryError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().map_err(|_| error("storage_error", "database lock failed"))?;
            let mut statement = conn.prepare("SELECT reference FROM change_log WHERE session_id=?1 AND kind='project_context' ORDER BY seq").map_err(sql_error)?;
            let rows = statement.query_map([id(session_id.0)], |row| row.get::<_, String>(0)).map_err(sql_error)?;
            rows.map(|row| {
                let raw = row.map_err(sql_error)?;
                serde_json::from_str(&raw).map_err(|_| error("storage_corrupt", "invalid project context record"))
            }).collect()
        }).await.map_err(|_| error("storage_error", "database worker failed"))?
    }
    /// List known submitted jobs and their durable child outcomes for a session.
    pub async fn background_jobs(
        &self,
        session_id: SessionId,
    ) -> Result<Vec<BackgroundJobSummary>, BoundaryError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner.lock().map_err(|_| error("storage_error", "database lock failed"))?;
            let tx = conn.transaction().map_err(sql_error)?;
            let parents: Vec<(String,String,Option<String>,bool)> = {
                let mut statement = tx.prepare("SELECT c.run_id,c.call_id,c.result,r.finished_at IS NOT NULL FROM tool_calls c JOIN runs r ON r.id=c.run_id WHERE r.session_id=?1 AND json_extract(c.call,'$.name')='background_start' ORDER BY r.started_at,c.rowid").map_err(sql_error)?;
                let rows=statement.query_map([id(session_id.0)], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).map_err(sql_error)?;
                rows.collect::<Result<_,_>>().map_err(sql_error)?
            };
            let mut jobs=Vec::new();
            for (run,parent,raw,finished) in parents {
                let prefix=crate::background::child_prefix(&parent);
                let mut statement=tx.prepare("SELECT call_id,call,state,result FROM tool_calls WHERE run_id=?1 AND substr(call_id,1,?2)=?3").map_err(sql_error)?;
                let rows=statement.query_map(params![run,prefix.len() as i64,prefix],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?))).map_err(sql_error)?;
                let children=rows.collect::<Result<Vec<_>,_>>().map_err(sql_error)?;
                if children.len()>1 {return Err(error("storage_corrupt", "multiple jobs share a launch call"));}
                let acknowledged: Option<ToolResult> = raw.map(|raw|serde_json::from_str(&raw).map_err(|_|error("storage_corrupt", "invalid launch result"))).transpose()?;
                let acknowledged_id=acknowledged.as_ref().and_then(|result|result.data.as_ref()).and_then(|data|data.get("job_id")).and_then(Value::as_str);
                let (job_id,tool,state,result) = if let Some((job_id,call,state,result))=children.into_iter().next() {
                    if acknowledged_id.is_some_and(|value|value!=job_id) {return Err(error("storage_corrupt", "background job identity mismatch"));}
                    let call:crate::contract::ToolCall=serde_json::from_str(&call).map_err(|_|error("storage_corrupt", "invalid background call"))?;
                    let result:Option<ToolResult>=result.map(|raw|serde_json::from_str(&raw).map_err(|_|error("storage_corrupt", "invalid background result"))).transpose()?;
                    let state=if state=="running" && finished {"unknown".to_owned()} else if result.as_ref().and_then(|r|r.error.as_ref()).is_some_and(|e|e.code=="cancelled") {"cancelled".to_owned()} else {state};
                    (job_id,Some(call.name),state,result)
                } else if let Some(job_id)=acknowledged_id {
                    let state=if !job_id.starts_with(&prefix) {"unknown"} else if finished {"not_executed"} else {"accepted"};
                    (job_id.to_owned(),None,state.to_owned(),None)
                } else {continue};
                let run_id:RunId=serde_json::from_value(serde_json::json!(run)).map_err(|_|error("storage_corrupt", "invalid run ID"))?;
                jobs.push(BackgroundJobSummary {job_id,run_id,parent_call_id:parent,tool,state,result});
            }
            tx.commit().map_err(sql_error)?;
            Ok(jobs)
        }).await.map_err(|_|error("storage_error", "database worker failed"))?
    }
    /// Read conservative possible session locations without opening SQLite.
    pub fn guard_candidates(
        data_dir: &Path,
        session_id: SessionId,
    ) -> Result<Option<Vec<GuardCandidate>>, BoundaryError> {
        let root = std::fs::canonicalize(data_dir)
            .map_err(|_| error("storage_error", "data directory is unavailable"))?;
        Ok(read_guard(&root, session_id)?.map(|guard| guard.candidates))
    }
    /// Lock an existing session without creating any files. Call after a
    /// read-only preflight and retain until all session writes are complete.
    pub fn lock_existing(
        data_dir: &Path,
        session_id: SessionId,
    ) -> Result<SessionLock, BoundaryError> {
        let root = std::fs::canonicalize(data_dir)
            .map_err(|_| error("storage_error", "data directory is unavailable"))?;
        let directory = root.join("locks");
        if !std::fs::symlink_metadata(&directory)
            .map_err(|_| error("storage_error", "session lock directory is unavailable"))?
            .is_dir()
        {
            return Err(error("storage_error", "invalid lock directory"));
        }
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join(format!("{}.lock", session_id.0)))
            .map_err(|_| error("storage_error", "session lock is unavailable"))?;
        // SAFETY: the descriptor is valid and flock retains no pointer.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let cause = std::io::Error::last_os_error();
            return Err(if cause.kind() == std::io::ErrorKind::WouldBlock {
                error("session_busy", "session is already in use")
            } else {
                error("storage_error", "cannot acquire session lock")
            });
        }
        Ok(SessionLock(file))
    }
    /// Read session metadata without creating or modifying SQLite side files.
    /// WAL contents are reconstructed in memory before SQLite reads them.
    pub fn peek_session(
        data_dir: &Path,
        session_id: SessionId,
    ) -> Result<Option<Session>, BoundaryError> {
        let directory = std::fs::canonicalize(data_dir)
            .map_err(|_| error("storage_error", "data directory is unavailable"))?;
        let path = directory.join(DB_NAME);
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| error("storage_error", "database is unavailable"))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(error(
                "storage_corrupt",
                "database is not a valid existing file",
            ));
        }
        let wal = directory.join(format!("{DB_NAME}-wal"));
        let live_wal = match std::fs::symlink_metadata(wal) {
            Ok(meta) if !meta.is_file() => {
                return Err(error("storage_corrupt", "invalid database WAL"));
            }
            Ok(meta) => meta.len() > 0,
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(error("storage_error", "cannot inspect database WAL"));
            }
            _ => false,
        };
        if live_wal {
            let snapshot = crate::wal_recovery::snapshot(&directory)?;
            return session_metadata(&snapshot, session_id);
        }
        let mut uri = reqwest::Url::from_file_path(&path)
            .map_err(|_| error("storage_error", "invalid database path"))?;
        uri.set_query(Some("immutable=1"));
        let conn = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sql_error);
        conn.and_then(|conn| session_metadata(&conn, session_id))
    }
    /// Try to acquire exclusive session ownership without waiting.
    pub fn lock_session(&self, session_id: SessionId) -> Result<SessionLock, BoundaryError> {
        let root = self
            .path
            .parent()
            .ok_or_else(|| error("storage_error", "invalid data directory"))?;
        let directory = root.join("locks");
        std::fs::create_dir_all(&directory)
            .map_err(|_| error("storage_error", "cannot create lock directory"))?;
        let meta = std::fs::symlink_metadata(&directory)
            .map_err(|_| error("storage_error", "cannot inspect lock directory"))?;
        if !meta.is_dir() {
            return Err(error("storage_error", "invalid lock directory"));
        }
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.join(format!("{}.lock", session_id.0)))
            .map_err(|_| error("storage_error", "cannot open session lock"))?;
        // SAFETY: the open descriptor is valid; flock does not retain pointers.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let cause = std::io::Error::last_os_error();
            return Err(if cause.kind() == std::io::ErrorKind::WouldBlock {
                error("session_busy", "session is already in use")
            } else {
                error("storage_error", "cannot acquire session lock")
            });
        }
        Ok(SessionLock(file))
    }
    /// Return one page in descending update order. Unfinished runs have no final status.
    pub async fn list_sessions(
        &self,
        offset: u64,
        limit: u64,
    ) -> Result<Vec<SessionSummary>, BoundaryError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner.lock().map_err(|_| error("storage_error", "database lock failed"))?;
            let offset = i64::try_from(offset).map_err(|_| error("invalid_argument", "offset is too large"))?;
            let limit = i64::try_from(limit).map_err(|_| error("invalid_argument", "limit is too large"))?;
            let mut stmt = conn.prepare("SELECT s.metadata,s.updated_at,(SELECT item FROM messages WHERE session_id=s.id AND json_extract(item,'$.kind')='user_text' ORDER BY ordinal LIMIT 1),(SELECT result FROM runs WHERE session_id=s.id ORDER BY started_at DESC, rowid DESC LIMIT 1) FROM sessions s ORDER BY s.updated_at DESC,s.rowid DESC LIMIT ?1 OFFSET ?2").map_err(sql_error)?;
            let rows = stmt.query_map(params![limit, offset], |r| Ok((r.get::<_, String>(0)?,r.get::<_, i64>(1)?,r.get::<_, Option<String>>(2)?,r.get::<_, Option<String>>(3)?))).map_err(sql_error)?;
            let mut result = Vec::new();
            for row in rows {
                let (metadata, updated_at, title, last) = row.map_err(sql_error)?;
                let session: Session = serde_json::from_str(&metadata).map_err(|_| error("storage_corrupt", "invalid session data"))?;
                let title = title.map(|v| serde_json::from_str::<HistoryItem>(&v).map_err(|_| error("storage_corrupt", "invalid stored history"))).transpose()?.and_then(|v| match v { HistoryItem::UserText(s) => Some(s.chars().take(80).collect()), _ => None });
                let last_status = last.map(|v| serde_json::from_str::<crate::contract::RunResult>(&v).map(|v| v.status).map_err(|_| error("storage_corrupt", "invalid stored run"))).transpose()?;
                result.push(SessionSummary { id: session.id, workspace: session.workspace, work_mode: session.work_mode, profile: session.profile, title, updated_at, last_status });
            }
            Ok(result)
        }).await.map_err(|_| error("storage_error", "database worker failed"))?
    }

    /// Until recovery is available, prevent resuming an interrupted run.
    pub async fn has_unfinished_run(&self, session_id: SessionId) -> Result<bool, BoundaryError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner
                .lock()
                .map_err(|_| error("storage_error", "database lock failed"))?;
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id=?1 AND finished_at IS NULL)",
                [id(session_id.0)],
                |r| r.get(0),
            )
            .map_err(sql_error)
        })
        .await
        .map_err(|_| error("storage_error", "database worker failed"))?
    }
    /// Close interrupted runs and dangling provider calls atomically. The caller
    /// must hold the exclusive session lock; no tool is executed or replayed.
    pub async fn recover_interrupted(
        &self,
        session_id: SessionId,
    ) -> Result<RecoverySummary, BoundaryError> {
        let inner = self.inner.clone();
        let secrets = self.secrets.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner
                .lock()
                .map_err(|_| error("storage_error", "database lock failed"))?;
            recover_transaction(&mut conn, session_id, &secrets)
        })
        .await
        .map_err(|_| error("storage_error", "database worker failed"))?
    }

    /// Atomically create a new session together with its credential-free profile fingerprint.
    pub async fn create_session(
        &self,
        session: &Session,
        fingerprint: &str,
    ) -> Result<(), BoundaryError> {
        let inner = self.inner.clone();
        let secrets = self.secrets.clone();
        let root = self
            .path
            .parent()
            .ok_or_else(|| error("storage_error", "invalid data directory"))?
            .to_owned();
        let session = session.clone();
        let fingerprint = fingerprint.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner
                .lock()
                .map_err(|_| error("storage_error", "database lock failed"))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let key = id(session.id.0);
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
                    [&key],
                    |r| r.get(0),
                )
                .map_err(sql_error)?;
            if exists {
                return Err(error("session_exists", "session already exists"));
            }
            let fingerprint = json(&fingerprint, &secrets)?;
            prepare_guard(&tx, &root, &session, &secrets)?;
            apply(&tx, &StoreChange::Session(session), &secrets)?;
            tx.execute(
                "INSERT INTO session_config(session_id,fingerprint) VALUES(?1,?2)",
                params![key, fingerprint],
            )
            .map_err(sql_error)?;
            tx.commit().map_err(sql_error)
        })
        .await
        .map_err(|_| error("storage_error", "database worker failed"))?
    }

    /// Read the stored fingerprint; missing metadata is treated as corruption.
    pub async fn fingerprint(&self, session_id: SessionId) -> Result<String, BoundaryError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            let conn = inner
                .lock()
                .map_err(|_| error("storage_error", "database lock failed"))?;
            let value: Option<String> = conn
                .query_row(
                    "SELECT fingerprint FROM session_config WHERE session_id=?1",
                    [id(session_id.0)],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql_error)?;
            let value =
                value.ok_or_else(|| error("storage_corrupt", "session fingerprint is missing"))?;
            serde_json::from_str(&value)
                .map_err(|_| error("storage_corrupt", "invalid session fingerprint"))
        })
        .await
        .map_err(|_| error("storage_error", "database worker failed"))?
    }

    /// Update a previously created session and its accepted fingerprint atomically.
    pub async fn update_session_config(
        &self,
        session: &Session,
        fingerprint: &str,
    ) -> Result<(), BoundaryError> {
        let inner = self.inner.clone();
        let secrets = self.secrets.clone();
        let root = self
            .path
            .parent()
            .ok_or_else(|| error("storage_error", "invalid data directory"))?
            .to_owned();
        let session = session.clone();
        let fingerprint = fingerprint.to_owned();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner
                .lock()
                .map_err(|_| error("storage_error", "database lock failed"))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            let key = id(session.id.0);
            let fingerprint = json(&fingerprint, &secrets)?;
            let updated = tx
                .execute(
                    "UPDATE session_config SET fingerprint=?2 WHERE session_id=?1",
                    params![key, fingerprint],
                )
                .map_err(sql_error)?;
            if updated != 1 {
                return Err(error("storage_corrupt", "session fingerprint is missing"));
            }
            prepare_guard(&tx, &root, &session, &secrets)?;
            apply(&tx, &StoreChange::Session(session.clone()), &secrets)?;
            tx.commit().map_err(sql_error)?;
            let _ = finalize_guard(&root, &session, &secrets);
            Ok(())
        })
        .await
        .map_err(|_| error("storage_error", "database worker failed"))?
    }

    /// Create or open `$data_dir/sessions.sqlite3`. Creates the data directory,
    /// refuses symlink database files and incompatible/corrupt databases.
    /// This does not acquire a session lock or enable workspace side effects.
    pub fn open(data_dir: &Path, secrets: Vec<String>) -> Result<Self, BoundaryError> {
        std::fs::create_dir_all(data_dir)
            .map_err(|_| error("storage_error", "cannot create data directory"))?;
        let root = std::fs::canonicalize(data_dir)
            .map_err(|_| error("storage_error", "cannot resolve data directory"))?;
        let init = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(root.join(".database-init.lock"))
            .map_err(|_| error("storage_error", "cannot open database initialization lock"))?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // SAFETY: the file owns a live descriptor; flock retains no pointer.
            if unsafe { libc::flock(init.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let cause = std::io::Error::last_os_error();
            if cause.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error(
                    "storage_error",
                    "cannot acquire database initialization lock",
                ));
            }
            if Instant::now() >= deadline {
                return Err(error("storage_busy", "database initialization is busy"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _init_guard = SessionLock(init);
        let path = root.join(DB_NAME);
        let created = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(_) => return Err(error("storage_error", "cannot create database")),
        };
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|_| error("storage_error", "cannot inspect database"))?;
        if !metadata.is_file() || (!created && metadata.len() == 0) {
            return Err(error(
                "storage_corrupt",
                "database is not a valid existing file",
            ));
        }
        let mut connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sql_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(sql_error)?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(sql_error)?;
        let app: i64 = connection
            .pragma_query_value(None, "application_id", |r| r.get(0))
            .map_err(sql_error)?;
        if !created && (!(1..=VERSION).contains(&version) || app != APPLICATION_ID) {
            return Err(error(
                if version > VERSION {
                    "storage_version"
                } else {
                    "storage_corrupt"
                },
                "unsupported or unrecognized database",
            ));
        }
        if created && (version != 0 || app != 0) {
            return Err(error("storage_corrupt", "new database is not empty"));
        }
        if !created {
            check_schema(&connection)?;
        }
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(sql_error)?;
        if !created && version == 1 {
            backup_v1(&connection, &root)?;
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            tx.execute_batch(V2_MIGRATION).map_err(sql_error)?;
            tx.pragma_update(None, "user_version", VERSION)
                .map_err(sql_error)?;
            tx.commit().map_err(sql_error)?;
        }
        let mode: String = connection
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .map_err(sql_error)?;
        if !mode.eq_ignore_ascii_case("wal") {
            let mode: String = connection
                .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
                .map_err(sql_error)?;
            if !mode.eq_ignore_ascii_case("wal") {
                return Err(error("storage_error", "SQLite WAL is unavailable"));
            }
        }
        if created {
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            tx.execute_batch(SCHEMA).map_err(sql_error)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(sql_error)?;
            tx.pragma_update(None, "user_version", VERSION)
                .map_err(sql_error)?;
            tx.commit().map_err(sql_error)?;
        }
        Ok(Self {
            inner: Arc::new(Mutex::new(connection)),
            secrets: Arc::new(secrets),
            path,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Return actual connection settings for diagnostics, never secrets.
    pub fn settings(&self) -> Result<(String, i64, bool), BoundaryError> {
        let conn = self
            .inner
            .lock()
            .map_err(|_| error("storage_error", "database lock failed"))?;
        let mode = conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .map_err(sql_error)?;
        let sync = conn
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .map_err(sql_error)?;
        let foreign: i64 = conn
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .map_err(sql_error)?;
        Ok((mode, sync, foreign == 1))
    }
}
impl Store for SqliteStore {
    fn load_session(&self, session_id: SessionId) -> Operation<'_, Option<Session>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let conn = inner
                    .lock()
                    .map_err(|_| error("storage_error", "database lock failed"))?;
                let key = id(session_id.0);
                let raw: Option<String> = conn
                    .query_row("SELECT metadata FROM sessions WHERE id=?1", [&key], |r| {
                        r.get(0)
                    })
                    .optional()
                    .map_err(sql_error)?;
                let Some(raw) = raw else { return Ok(None) };
                let mut session: Session = serde_json::from_str(&raw)
                    .map_err(|_| error("storage_corrupt", "invalid session data"))?;
                if session.id != session_id || !session.history.is_empty() {
                    return Err(error("storage_corrupt", "invalid session identity"));
                }
                let mut stmt = conn
                    .prepare("SELECT item FROM messages WHERE session_id=?1 ORDER BY ordinal")
                    .map_err(sql_error)?;
                let rows = stmt
                    .query_map([key], |r| r.get::<_, String>(0))
                    .map_err(sql_error)?;
                for row in rows {
                    let raw = row.map_err(sql_error)?;
                    let item: HistoryItem = serde_json::from_str(&raw)
                        .map_err(|_| error("storage_corrupt", "invalid stored history"))?;
                    session.history.push(item);
                }
                Ok(Some(session))
            })
            .await
            .map_err(|_| error("storage_error", "database worker failed"))?
        })
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        let inner = self.inner.clone();
        let secrets = self.secrets.clone();
        let changes = changes.to_vec();
        let root = self.path.parent().map(Path::to_owned);
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut conn = inner
                    .lock()
                    .map_err(|_| error("storage_error", "database lock failed"))?;
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(sql_error)?;
                let root = root.ok_or_else(|| error("storage_error", "invalid data directory"))?;
                for change in &changes {
                    if let StoreChange::Session(session) = change {
                        prepare_guard(&tx, &root, session, &secrets)?;
                    }
                }
                for change in &changes {
                    apply(&tx, change, &secrets)?;
                }
                tx.commit().map_err(sql_error)?;
                for change in &changes {
                    if let StoreChange::Session(session) = change {
                        let _ = finalize_guard(&root, session, &secrets);
                    }
                }
                Ok(())
            })
            .await
            .map_err(|_| error("storage_error", "database worker failed"))?
        })
    }
}
