//! Durable SQLite store. The caller must obtain a session lock before using this
//! for real side effects; crash recovery and locks are separate P5 steps.
use crate::{
    boundary::{Operation, Store, StoreChange},
    contract::{
        BoundaryError, ErrorInfo, EventPayload, HistoryItem, SCHEMA_VERSION, Session, SessionId,
        ToolState,
    },
};
use rusqlite::{
    Connection, Error as SqlError, ErrorCode, OpenFlags, OptionalExtension, Transaction,
    TransactionBehavior, params,
};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const DB_NAME: &str = "sessions.sqlite3";
const APPLICATION_ID: i64 = 0x5341_5941;
const VERSION: i64 = 1;
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
"#;
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
fn json<T: Serialize>(value: &T, secrets: &[String]) -> Result<String, BoundaryError> {
    let value = serde_json::to_value(value)
        .map_err(|_| error("storage_error", "cannot serialize storage data"))?;
    if contains_secret(&value, secrets, 0) {
        return Err(error(
            "storage_sensitive_data",
            "storage data contains a configured secret",
        ));
    }
    serde_json::to_string(&value)
        .map_err(|_| error("storage_error", "cannot serialize storage data"))
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
                let item = json(item, secrets)?;
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
        StoreChange::History { session_id, item } => {
            let key = id(session_id.0);
            let data = json(item, secrets)?;
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
            let data = json(call, secrets)?;
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
            let data = json(result, secrets)?;
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
            let data = json(request, secrets)?;
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
            let data = json(result, secrets)?;
            let count=tx.execute("UPDATE runs SET result=?2,finished_at=unixepoch() WHERE id=?1 AND result IS NULL",params![run,data]).map_err(sql_error)?;
            if count != 1 {
                return Err(error("storage_error", "run already finished"));
            }
            journal(tx, &session, Some(&run), "run_finished", None)?;
        }
        StoreChange::Event(event) => {
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
            let count=tx.execute("UPDATE sessions SET event_seq=?2,updated_at=unixepoch() WHERE id=?1 AND event_seq<?2",params![session,seq]).map_err(sql_error)?;
            if count != 1 {
                return Err(error("storage_error", "event sequence is not increasing"));
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
impl SqliteStore {
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
    /// A live WAL is read through SQLite's read-only shared-memory mode.
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
        let mut uri = reqwest::Url::from_file_path(&path)
            .map_err(|_| error("storage_error", "invalid database path"))?;
        uri.set_query(Some(if live_wal {
            "readonly_shm=1"
        } else {
            "immutable=1"
        }));
        let conn = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sql_error)?;
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
    /// Atomically create a new session together with its credential-free profile fingerprint.
    pub async fn create_session(
        &self,
        session: &Session,
        fingerprint: &str,
    ) -> Result<(), BoundaryError> {
        let inner = self.inner.clone();
        let secrets = self.secrets.clone();
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
            apply(&tx, &StoreChange::Session(session), &secrets)?;
            tx.commit().map_err(sql_error)
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
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(sql_error)?;
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
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let mut conn = inner
                    .lock()
                    .map_err(|_| error("storage_error", "database lock failed"))?;
                let tx = conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(sql_error)?;
                for change in &changes {
                    apply(&tx, change, &secrets)?;
                }
                tx.commit().map_err(sql_error)
            })
            .await
            .map_err(|_| error("storage_error", "database worker failed"))?
        })
    }
}
