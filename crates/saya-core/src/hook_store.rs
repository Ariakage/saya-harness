//! Private durable hook intents and outcomes. Opening acquires an exclusive
//! owner lease, then fences unfinished work from the previous owner as Unknown.
//! Callers protect this directory from tools and open it on a blocking worker.
use crate::{
    boundary::Operation,
    contract::{BoundaryError, ErrorInfo},
    credentials::SecretRegistry,
    hooks::{HookClaim, HookDiagnostic, HookIntent, HookJournal, HookRecord},
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

mod disk;
#[cfg(test)]
mod tests;
mod validation;

const APPLICATION_ID: i64 = 0x5348_4f4b;
const VERSION: i64 = 1;
const MAX_INTENT_BYTES: usize = 16 * 1024;
const MAX_RECORD_BYTES: usize = 128 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 4096;

const RUNS_SCHEMA: &str = "CREATE TABLE runs (run_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, invocation_limit INTEGER NOT NULL CHECK(invocation_limit BETWEEN 1 AND 4096))";
const INVOCATIONS_SCHEMA: &str = "CREATE TABLE invocations (invocation_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, run_id TEXT NOT NULL REFERENCES runs(run_id), intent TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('running','completed','unknown')), record TEXT)";
const DIAGNOSTICS_SCHEMA: &str = "CREATE TABLE diagnostics (digest TEXT PRIMARY KEY, invocation_id TEXT NOT NULL, event_id TEXT NOT NULL, diagnostic TEXT NOT NULL)";

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
fn sql_error(_: rusqlite::Error) -> BoundaryError {
    error("hook_journal", "Hook journal operation failed")
}

#[derive(Debug, Clone, Copy)]
pub struct HookStoreLimits {
    pub max_invocations: usize,
    pub max_diagnostics: usize,
    pub max_database_bytes: u64,
}
impl Default for HookStoreLimits {
    fn default() -> Self {
        Self {
            max_invocations: 65_536,
            max_diagnostics: 65_536,
            max_database_bytes: 64 * 1024 * 1024,
        }
    }
}
impl HookStoreLimits {
    fn validate(self) -> Result<(), BoundaryError> {
        if self.max_invocations == 0
            || self.max_invocations > 1_000_000
            || self.max_diagnostics == 0
            || self.max_diagnostics > 1_000_000
            || self.max_database_bytes < 1024 * 1024
            || self.max_database_bytes > 1024 * 1024 * 1024
        {
            return Err(error(
                "hook_journal_config",
                "Hook journal limits are invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookJournalState {
    Running,
    Completed,
    Unknown,
}
#[derive(Debug, Clone)]
pub struct HookJournalEntry {
    pub intent: HookIntent,
    pub state: HookJournalState,
    pub record: Option<HookRecord>,
}

struct Inner {
    // The connection closes before the disk's owner lock is released.
    connection: Mutex<Connection>,
    disk: disk::Disk,
    secrets: Arc<SecretRegistry>,
    limits: HookStoreLimits,
    #[cfg(test)]
    fail_after_claim_commit: std::sync::atomic::AtomicBool,
}

#[derive(Clone)]
pub struct HookStore {
    inner: Arc<Inner>,
}
impl std::fmt::Debug for HookStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HookStore([PRIVATE])")
    }
}

impl HookStore {
    /// Open a dedicated private directory. A second live owner fails with
    /// hook_journal_busy. Restart recovery records uncertainty without dispatch.
    pub fn open(
        path: &Path,
        secrets: Arc<SecretRegistry>,
        limits: HookStoreLimits,
    ) -> Result<Self, BoundaryError> {
        limits.validate()?;
        let (disk, created) = disk::Disk::open(path, limits.max_database_bytes)?;
        let mut connection = Connection::open_with_flags(
            disk.database_path(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(sql_error)?;
        disk.check()?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "trusted_schema", false)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "temp_store", "MEMORY")
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(sql_error)?;
        let app: i64 = connection
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .map_err(sql_error)?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_error)?;
        if created {
            if app != 0 || version != 0 {
                return Err(error(
                    "hook_journal_corrupt",
                    "New hook journal is not empty",
                ));
            }
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(sql_error)?;
            for schema in [RUNS_SCHEMA, INVOCATIONS_SCHEMA, DIAGNOSTICS_SCHEMA] {
                transaction.execute_batch(schema).map_err(sql_error)?;
            }
            transaction
                .pragma_update(None, "application_id", APPLICATION_ID)
                .map_err(sql_error)?;
            transaction
                .pragma_update(None, "user_version", VERSION)
                .map_err(sql_error)?;
            transaction.commit().map_err(sql_error)?;
        } else if app != APPLICATION_ID || version != VERSION {
            return Err(error(
                "hook_journal_version",
                "Hook journal format is unsupported",
            ));
        }
        verify_schema(&connection)?;
        let integrity: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(sql_error)?;
        if integrity != "ok" {
            return Err(error(
                "hook_journal_corrupt",
                "Hook journal integrity check failed",
            ));
        }
        let page_size: u64 = connection
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .map_err(sql_error)?;
        if page_size == 0 || page_size > 65_536 {
            return Err(error(
                "hook_journal_corrupt",
                "Hook journal page size is invalid",
            ));
        }
        connection
            .pragma_update(
                None,
                "max_page_count",
                limits.max_database_bytes / page_size,
            )
            .map_err(sql_error)?;
        let mode: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(sql_error)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(error("hook_journal", "Hook journal WAL is unavailable"));
        }
        connection
            .pragma_update(None, "wal_autocheckpoint", 32)
            .map_err(sql_error)?;
        connection
            .pragma_update(None, "journal_size_limit", 1024 * 1024)
            .map_err(sql_error)?;
        validate_capacity(&connection, limits)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql_error)?;
        transaction
            .execute(
                "UPDATE invocations SET state='unknown' WHERE state='running'",
                [],
            )
            .map_err(sql_error)?;
        transaction.commit().map_err(sql_error)?;
        checkpoint(&connection)?;
        disk.check()?;
        disk.sync()?;
        Ok(Self {
            inner: Arc::new(Inner {
                connection: Mutex::new(connection),
                disk,
                secrets,
                limits,
                #[cfg(test)]
                fail_after_claim_commit: std::sync::atomic::AtomicBool::new(false),
            }),
        })
    }

    /// Add this whole directory to the runtime's protected private roots.
    pub fn protected_path(&self) -> &Path {
        self.inner.disk.path()
    }

    /// Read current state without reconciling or retrying an uncertain process.
    pub async fn inspect(
        &self,
        invocation_id: &str,
    ) -> Result<Option<HookJournalEntry>, BoundaryError> {
        if !validation::digest(invocation_id) {
            return Err(error(
                "hook_journal_input",
                "Hook invocation identity is invalid",
            ));
        }
        let id = invocation_id.to_string();
        self.worker(move |inner, connection| read_entry(connection, &id, &inner.secrets))
            .await
    }

    async fn worker<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Inner, &mut Connection) -> Result<T, BoundaryError> + Send + 'static,
    ) -> Result<T, BoundaryError> {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            inner.disk.check()?;
            let mut connection = inner
                .connection
                .lock()
                .map_err(|_| error("hook_journal", "Hook journal lock failed"))?;
            inner.disk.check()?;
            let result = operation(&inner, &mut connection);
            inner.disk.check()?;
            result
        })
        .await
        .map_err(|_| error("hook_journal", "Hook journal worker failed"))?
    }
}

impl HookJournal for HookStore {
    fn claim<'a>(&'a self, intent: &'a HookIntent) -> Operation<'a, HookClaim> {
        Box::pin(async move {
            validation::intent(intent, &self.inner.secrets)?;
            let intent = intent.clone();
            self.worker(move |inner, connection| {
                let text = validation::intent(&intent, &inner.secrets)?;
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
                if let Some(entry) = read_entry(&transaction, &intent.invocation_id, &inner.secrets)? {
                    if entry.intent != intent { return Ok(HookClaim::Conflict); }
                    return Ok(match entry.state {
                        HookJournalState::Running => HookClaim::Running,
                        HookJournalState::Unknown => HookClaim::Unknown,
                        HookJournalState::Completed => HookClaim::Completed(Box::new(entry.record.ok_or_else(|| error("hook_journal_corrupt", "Completed hook outcome is absent"))?)),
                    });
                }
                let run_id = intent.run.id.0.to_string();
                let run: Option<(String, usize)> = transaction.query_row("SELECT session_id,invocation_limit FROM runs WHERE run_id=?1", [&run_id], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(sql_error)?;
                if let Some((session_id, limit)) = &run
                    && (*session_id != intent.run.session_id.0.to_string() || *limit != intent.run_invocation_limit) { return Ok(HookClaim::Conflict); }
                let count: usize = transaction.query_row("SELECT count(*) FROM invocations WHERE run_id=?1", [&run_id], |row| row.get(0)).map_err(sql_error)?;
                if count >= intent.run_invocation_limit { return Ok(HookClaim::BudgetExhausted); }
                let total: usize = transaction.query_row("SELECT count(*) FROM invocations", [], |row| row.get(0)).map_err(sql_error)?;
                if total >= inner.limits.max_invocations { return Err(error("hook_journal_full", "Hook journal invocation capacity is exhausted")); }
                if run.is_none() {
                    transaction.execute("INSERT INTO runs(run_id,session_id,invocation_limit) VALUES(?1,?2,?3)", params![run_id, intent.run.session_id.0.to_string(), intent.run_invocation_limit]).map_err(sql_error)?;
                }
                // Refresh the registry at the write boundary as credentials can rotate.
                validation::intent(&intent, &inner.secrets)?;
                transaction.execute("INSERT INTO invocations(invocation_id,fingerprint,run_id,intent,state) VALUES(?1,?2,?3,?4,'running')", params![intent.invocation_id, intent.fingerprint, run_id, text]).map_err(sql_error)?;
                transaction.commit().map_err(sql_error)?;
                #[cfg(test)]
                if inner.fail_after_claim_commit.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    return Err(error("hook_journal_unknown", "Hook intent committed before an injected checkpoint interruption"));
                }
                checkpoint(connection)?;
                Ok(HookClaim::Execute)
            }).await
        })
    }

    fn finish<'a>(&'a self, record: &'a HookRecord) -> Operation<'a, ()> {
        Box::pin(async move {
            let record = validation::record(record, &self.inner.secrets)?.0;
            self.worker(move |inner, connection| {
                let (record, text) = validation::record(&record, &inner.secrets)?;
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
                let existing = read_entry(&transaction, &record.intent.invocation_id, &inner.secrets)?
                    .ok_or_else(|| error("hook_journal_conflict", "Hook result has no durable intent"))?;
                if existing.intent != record.intent { return Err(error("hook_journal_conflict", "Hook result differs from its intent")); }
                if let Some(previous) = existing.record {
                    return if previous == record { Ok(()) } else { Err(error("hook_journal_conflict", "Hook result has already been recorded differently")) };
                }
                if existing.state != HookJournalState::Running {
                    return Err(error("hook_journal_unknown", "Uncertain hook execution requires separate reconciliation"));
                }
                let state = if record.status == crate::hooks::HookStatus::Unknown { "unknown" } else { "completed" };
                let (_, current_text) = validation::record(&record, &inner.secrets)?;
                if text != current_text { return Err(error("hook_journal_secret", "Known credentials changed during hook result persistence")); }
                transaction.execute("UPDATE invocations SET state=?2,record=?3 WHERE invocation_id=?1 AND state='running'", params![record.intent.invocation_id, state, text]).map_err(sql_error)?;
                transaction.commit().map_err(sql_error)?;
                checkpoint(connection)
            }).await
        })
    }

    fn diagnostic<'a>(&'a self, note: &'a HookDiagnostic) -> Operation<'a, ()> {
        Box::pin(async move {
            let note = validation::diagnostic(note, &self.inner.secrets)?.0;
            self.worker(move |inner, connection| {
                let (note, text) = validation::diagnostic(&note, &inner.secrets)?;
                let digest = validation::sha256(text.as_bytes());
                let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql_error)?;
                let existing: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM diagnostics WHERE digest=?1)", [&digest], |row| row.get(0)).map_err(sql_error)?;
                if existing { return Ok(()); }
                let count: usize = transaction.query_row("SELECT count(*) FROM diagnostics", [], |row| row.get(0)).map_err(sql_error)?;
                if count >= inner.limits.max_diagnostics { return Err(error("hook_journal_full", "Hook journal diagnostic capacity is exhausted")); }
                let (_, current_text) = validation::diagnostic(&note, &inner.secrets)?;
                if text != current_text { return Err(error("hook_journal_secret", "Known credentials changed during hook diagnostic persistence")); }
                transaction.execute("INSERT INTO diagnostics(digest,invocation_id,event_id,diagnostic) VALUES(?1,?2,?3,?4)", params![digest, note.invocation_id, note.event_id, text]).map_err(sql_error)?;
                transaction.commit().map_err(sql_error)?;
                checkpoint(connection)
            }).await
        })
    }
}

fn read_entry(
    connection: &Connection,
    id: &str,
    secrets: &SecretRegistry,
) -> Result<Option<HookJournalEntry>, BoundaryError> {
    let row: Option<(String, String, String, String, Option<String>)> = connection
        .query_row(
            "SELECT fingerprint,run_id,intent,state,record FROM invocations WHERE invocation_id=?1",
            [id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let Some((fingerprint, run_id, text, state, record)) = row else {
        return Ok(None);
    };
    if text.len() > MAX_INTENT_BYTES
        || record
            .as_ref()
            .is_some_and(|value| value.len() > MAX_RECORD_BYTES)
    {
        return Err(error(
            "hook_journal_corrupt",
            "Stored hook data exceeds its size limit",
        ));
    }
    let intent: HookIntent = serde_json::from_str(&text)
        .map_err(|_| error("hook_journal_corrupt", "Stored hook intent is invalid"))?;
    validation::intent(&intent, secrets)?;
    if intent.invocation_id != id
        || intent.fingerprint != fingerprint
        || intent.run.id.0.to_string() != run_id
    {
        return Err(error(
            "hook_journal_corrupt",
            "Stored hook identity is inconsistent",
        ));
    }
    let record = record
        .map(|text| {
            let record: HookRecord = serde_json::from_str(&text)
                .map_err(|_| error("hook_journal_corrupt", "Stored hook outcome is invalid"))?;
            let record = validation::record(&record, secrets)?.0;
            if record.intent != intent {
                return Err(error(
                    "hook_journal_corrupt",
                    "Stored hook outcome belongs to another intent",
                ));
            }
            Ok(record)
        })
        .transpose()?;
    let state = match state.as_str() {
        "running" if record.is_none() => HookJournalState::Running,
        "completed"
            if record
                .as_ref()
                .is_some_and(|record| record.status != crate::hooks::HookStatus::Unknown) =>
        {
            HookJournalState::Completed
        }
        "unknown"
            if record
                .as_ref()
                .is_none_or(|record| record.status == crate::hooks::HookStatus::Unknown) =>
        {
            HookJournalState::Unknown
        }
        _ => {
            return Err(error(
                "hook_journal_corrupt",
                "Stored hook state is inconsistent",
            ));
        }
    };
    Ok(Some(HookJournalEntry {
        intent,
        state,
        record,
    }))
}

fn verify_schema(connection: &Connection) -> Result<(), BoundaryError> {
    let mut statement = connection
        .prepare(
            "SELECT name,type,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(sql_error)?;
    let actual = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    let expected: Vec<_> = [
        ("diagnostics", DIAGNOSTICS_SCHEMA),
        ("invocations", INVOCATIONS_SCHEMA),
        ("runs", RUNS_SCHEMA),
    ]
    .into_iter()
    .map(|(name, schema)| (name.to_string(), "table".to_string(), schema.to_string()))
    .collect();
    if actual != expected {
        return Err(error(
            "hook_journal_corrupt",
            "Hook journal schema is incompatible",
        ));
    }
    Ok(())
}
fn validate_capacity(
    connection: &Connection,
    limits: HookStoreLimits,
) -> Result<(), BoundaryError> {
    let invocations: usize = connection
        .query_row("SELECT count(*) FROM invocations", [], |row| row.get(0))
        .map_err(sql_error)?;
    let diagnostics: usize = connection
        .query_row("SELECT count(*) FROM diagnostics", [], |row| row.get(0))
        .map_err(sql_error)?;
    let oversize: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM invocations WHERE length(CAST(intent AS BLOB))>?1 OR length(CAST(record AS BLOB))>?2) OR EXISTS(SELECT 1 FROM diagnostics WHERE length(CAST(diagnostic AS BLOB))>?3)", params![MAX_INTENT_BYTES, MAX_RECORD_BYTES, MAX_DIAGNOSTIC_BYTES], |row| row.get(0)).map_err(sql_error)?;
    if invocations > limits.max_invocations || diagnostics > limits.max_diagnostics || oversize {
        return Err(error(
            "hook_journal_full",
            "Hook journal stored data exceeds its configured limits",
        ));
    }
    Ok(())
}
fn checkpoint(connection: &Connection) -> Result<(), BoundaryError> {
    let busy: i64 = connection
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
        .map_err(sql_error)?;
    if busy != 0 {
        return Err(error(
            "hook_journal_unknown",
            "Hook journal commit exists but WAL compaction remains pending",
        ));
    }
    Ok(())
}
