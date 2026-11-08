use rusqlite::Connection;
use saya_core::{
    boundary::{Store, StoreChange},
    contract::{
        Event, EventPayload, HistoryItem, ModelProtocol, PermissionMode, Run, RunId, RunResult,
        RunStatus, SCHEMA_VERSION, Session, SessionId, ToolCall, ToolResult, WorkMode,
    },
    sqlite::SqliteStore,
};
use std::{fs, path::Path};
use tempfile::TempDir;

fn fixture(root: &Path) -> (Session, Run) {
    let s = Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::Responses,
        model: "fake".into(),
        history: vec![HistoryItem::UserText("task".into())],
    };
    let run = Run {
        id: RunId::new(),
        session_id: s.id,
        work_mode: s.work_mode,
        permissions: PermissionMode::Auto,
    };
    (s, run)
}
fn result(session: &Session, run: &Run) -> RunResult {
    RunResult {
        schema_version: SCHEMA_VERSION,
        session_id: Some(session.id),
        run_id: Some(run.id),
        status: RunStatus::Completed,
        stop_reason: Some("completed".into()),
        text: Some("done".into()),
        error: None,
        usage: None,
        artifacts: vec![],
        persistence: None,
    }
}
fn event(session: &Session, run: &Run, seq: u64) -> Event {
    Event {
        schema_version: SCHEMA_VERSION,
        session_id: session.id,
        run_id: Some(run.id),
        seq,
        timestamp: "2026-11-07T00:00:00Z".into(),
        payload: EventPayload::RunStarted {
            work_mode: run.work_mode,
            permissions: run.permissions,
            profile: session.profile.clone(),
        },
    }
}
fn call() -> ToolCall {
    ToolCall {
        call_id: "call-1".into(),
        name: "write_file".into(),
        arguments: r#"{"path":"example","content":"safe"}"#.into(),
    }
}
fn tool_result() -> ToolResult {
    ToolResult {
        ok: true,
        data: Some(serde_json::json!({"result":"saved"})),
        error: None,
        truncated: false,
    }
}

#[tokio::test]
async fn session_creation_listing_and_config_are_atomic() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = TempDir::new()?;
    let (session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec!["secret-value".into()])?;
    assert!(
        store
            .create_session(&session, "secret-value")
            .await
            .is_err()
    );
    assert!(store.load_session(session.id).await?.is_none());
    store.create_session(&session, "profile-hash").await?;
    assert!(store.create_session(&session, "other").await.is_err());
    assert_eq!(store.fingerprint(session.id).await?, "profile-hash");
    let listed = store.list_sessions(0, 20).await?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, session.id);
    assert_eq!(listed[0].title.as_deref(), Some("task"));
    assert!(!store.has_unfinished_run(session.id).await?);
    store.commit(&[StoreChange::RunStarted(run)]).await?;
    assert!(store.has_unfinished_run(session.id).await?);
    let mut modified = session.clone();
    modified.work_mode = WorkMode::Plan;
    assert!(
        store
            .update_session_config(&modified, "secret-value")
            .await
            .is_err()
    );
    assert_eq!(store.load_session(session.id).await?, Some(session));
    store.update_session_config(&modified, "new-hash").await?;
    assert_eq!(store.fingerprint(modified.id).await?, "new-hash");
    assert_eq!(store.load_session(modified.id).await?, Some(modified));
    Ok(())
}

#[tokio::test]
async fn wal_transactions_and_history_survive_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, run) = fixture(temp.path());
    let store = SqliteStore::open(&temp.path().join("data"), vec![])?;
    assert_eq!(store.settings()?, ("wal".into(), 2, true));
    let c = call();
    let tr = tool_result();
    let history = HistoryItem::ToolCall(c.clone());
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
            StoreChange::History {
                session_id: session.id,
                item: history.clone(),
            },
            StoreChange::ToolIntent {
                run_id: run.id,
                call: c.clone(),
            },
            StoreChange::Event(event(&session, &run, 1)),
        ])
        .await?;
    store
        .commit(&[
            StoreChange::ToolFinished {
                run_id: run.id,
                call_id: c.call_id.clone(),
                result: tr.clone(),
            },
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::ToolResult {
                    call_id: c.call_id.clone(),
                    result: tr,
                },
            },
            StoreChange::RunFinished(result(&session, &run)),
        ])
        .await?;
    let mut expected = session.clone();
    expected.history.push(history);
    expected.history.push(HistoryItem::ToolResult {
        call_id: c.call_id,
        result: tool_result(),
    });
    assert_eq!(
        store.load_session(session.id).await?,
        Some(expected.clone())
    );
    let path = store.path().to_owned();
    drop(store);
    let store = SqliteStore::open(&temp.path().join("data"), vec![])?;
    assert_eq!(store.load_session(session.id).await?, Some(expected));
    store
        .commit(&[StoreChange::Event(event(&session, &run, 2))])
        .await?;
    let db = Connection::open(path)?;
    let count: i64 = db.query_row(
        "SELECT COUNT(*) FROM events WHERE session_id=?1",
        [session.id.0.to_string()],
        |r| r.get(0),
    )?;
    assert_eq!(count, 2);
    let state: String = db.query_row(
        "SELECT state FROM tool_calls WHERE run_id=?1",
        [run.id.0.to_string()],
        |r| r.get(0),
    )?;
    assert_eq!(state, "succeeded");
    let seq: i64 = db.query_row(
        "SELECT change_seq FROM sessions WHERE id=?1",
        [session.id.0.to_string()],
        |r| r.get(0),
    )?;
    assert!(seq >= 7);
    Ok(())
}
#[tokio::test]
async fn transaction_failure_rolls_back_every_change() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    let missing = RunId::new();
    let result = store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::ToolIntent {
                run_id: missing,
                call: call(),
            },
        ])
        .await;
    assert!(result.is_err());
    assert!(store.load_session(session.id).await?.is_none());
    Ok(())
}
#[tokio::test]
async fn event_sequences_remain_monotonic_and_deltas_are_not_persisted()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
            StoreChange::Event(event(&session, &run, 4)),
        ])
        .await?;
    assert!(
        store
            .commit(&[StoreChange::Event(event(&session, &run, 4))])
            .await
            .is_err()
    );
    let mut delta = event(&session, &run, 5);
    delta.payload = EventPayload::ModelTextDelta {
        request_id: "x".into(),
        text: "delta".into(),
    };
    assert!(store.commit(&[StoreChange::Event(delta)]).await.is_err());
    drop(store);
    let store = SqliteStore::open(temp.path(), vec![])?;
    assert!(
        store
            .commit(&[StoreChange::Event(event(&session, &run, 3))])
            .await
            .is_err()
    );
    store
        .commit(&[StoreChange::Event(event(&session, &run, 5))])
        .await?;
    Ok(())
}
#[tokio::test]
async fn known_secrets_are_redacted_and_structural_ids_fail_closed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let secret = "fake\"secret\\value";
    let (mut session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![secret.into()])?;
    session
        .history
        .push(HistoryItem::UserText(format!("before {secret} after")));
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    let saved = store
        .load_session(session.id)
        .await?
        .ok_or("missing session")?;
    assert_eq!(
        saved.history.last(),
        Some(&HistoryItem::UserText("before [REDACTED] after".into()))
    );
    let mut c = call();
    c.arguments = serde_json::json!({"content":secret}).to_string();
    store
        .commit(&[StoreChange::ToolIntent {
            run_id: run.id,
            call: c,
        }])
        .await?;
    let mut invalid = call();
    invalid.call_id = secret.into();
    assert!(
        store
            .commit(&[StoreChange::ToolIntent {
                run_id: run.id,
                call: invalid
            }])
            .await
            .is_err()
    );
    for entry in fs::read_dir(temp.path())? {
        let path = entry?.path();
        if path.is_file() {
            let bytes = fs::read(path)?;
            assert!(
                !bytes
                    .windows(secret.len())
                    .any(|part| part == secret.as_bytes())
            );
            let encoded = serde_json::to_string(secret)?;
            let fragment = &encoded[1..encoded.len() - 1];
            assert!(
                !bytes
                    .windows(fragment.len())
                    .any(|part| part == fragment.as_bytes())
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn encoded_secret_in_prose_is_rejected_before_session_commit()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec!["fixture-secret".into()])?;
    store.create_session(&session, "profile-hash").await?;
    let mut changed = session.clone();
    changed.history.push(HistoryItem::UserText(
        r#"Notes: {"token":"fixture-\u0073ecret"}"#.into(),
    ));
    assert!(
        store
            .commit(&[StoreChange::Session(changed)])
            .await
            .is_err()
    );
    assert_eq!(store.load_session(session.id).await?, Some(session));
    Ok(())
}

#[test]
fn corrupt_foreign_and_newer_databases_are_never_silently_replaced()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let path = temp.path().join("sessions.sqlite3");
    fs::write(&path, b"not sqlite")?;
    let before = fs::read(&path)?;
    let error = match SqliteStore::open(temp.path(), vec![]) {
        Ok(_) => panic!("corrupt database opened"),
        Err(e) => e,
    };
    assert_eq!(error.0.code, "storage_corrupt");
    assert_eq!(fs::read(&path)?, before);
    fs::remove_file(&path)?;
    let c = Connection::open(&path)?;
    c.pragma_update(None, "user_version", 99)?;
    drop(c);
    let before = fs::read(&path)?;
    let error = match SqliteStore::open(temp.path(), vec![]) {
        Ok(_) => panic!("newer database opened"),
        Err(e) => e,
    };
    assert_eq!(error.0.code, "storage_version");
    assert_eq!(fs::read(&path)?, before);
    Ok(())
}

#[cfg(unix)]
#[test]
fn database_is_private_and_symlinks_are_never_followed() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = TempDir::new()?;
    let data = temp.path().join("data");
    let store = SqliteStore::open(&data, vec![])?;
    assert_eq!(fs::metadata(store.path())?.permissions().mode() & 0o077, 0);
    drop(store);
    let other = temp.path().join("other");
    fs::write(&other, b"preserve")?;
    let linked = temp.path().join("linked");
    fs::create_dir(&linked)?;
    symlink(&other, linked.join("sessions.sqlite3"))?;
    assert!(SqliteStore::open(&linked, vec![]).is_err());
    assert_eq!(fs::read(&other)?, b"preserve");
    Ok(())
}

#[tokio::test]
async fn session_lock_rejects_other_writer_but_allows_reads()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let first = SqliteStore::open(temp.path(), vec![])?;
    first.create_session(&session, "hash").await?;
    let second = SqliteStore::open(temp.path(), vec![])?;
    let guard = first.lock_session(session.id)?;
    assert!(second.lock_session(session.id).is_err());
    assert_eq!(
        second.load_session(session.id).await?,
        Some(session.clone())
    );
    drop(guard);
    let _next = second.lock_session(session.id)?;
    Ok(())
}

#[test]
fn sqlite_lock_child_process() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{self, Write};
    let Ok(directory) = std::env::var("SAYA_TEST_LOCK_DIR") else {
        return Ok(());
    };
    let id: SessionId =
        serde_json::from_value(serde_json::json!(std::env::var("SAYA_TEST_LOCK_ID")?))?;
    let store = SqliteStore::open(Path::new(&directory), vec![])?;
    let _lock = SqliteStore::lock_existing(Path::new(&directory), id)?;
    let runtime = tokio::runtime::Builder::new_current_thread().build()?;
    runtime.block_on(store.commit(&[StoreChange::History {
        session_id: id,
        item: HistoryItem::UserText("live WAL data".into()),
    }]))?;
    println!("SAYA_CHILD_READY");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(())
}

#[tokio::test]
async fn real_process_lock_and_readonly_live_wal_preflight()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    let lock = store.lock_session(session.id)?;
    store.create_session(&session, "hash").await?;
    drop(lock);
    drop(store);
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "sqlite_lock_child_process", "--nocapture"])
        .env("SAYA_TEST_LOCK_DIR", temp.path())
        .env("SAYA_TEST_LOCK_ID", session.id.0.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("SAYA_CHILD_READY") {
                let _ = tx.send(());
            }
        }
    });
    rx.recv_timeout(Duration::from_secs(5))?;
    let snapshot = [
        "sessions.sqlite3",
        "sessions.sqlite3-wal",
        "sessions.sqlite3-shm",
    ]
    .map(|name| fs::read(temp.path().join(name)).unwrap());
    let metadata = SqliteStore::peek_session(temp.path(), session.id)?.ok_or("session missing")?;
    assert_eq!(metadata.id, session.id);
    let after = [
        "sessions.sqlite3",
        "sessions.sqlite3-wal",
        "sessions.sqlite3-shm",
    ]
    .map(|name| fs::read(temp.path().join(name)).unwrap());
    assert_eq!(snapshot, after);
    let error = match SqliteStore::lock_existing(temp.path(), session.id) {
        Ok(_) => return Err("second process acquired lock".into()),
        Err(e) => e,
    };
    assert_eq!(error.0.code, "session_busy");
    let reader = SqliteStore::open(temp.path(), vec![])?;
    assert!(
        reader
            .load_session(session.id)
            .await?
            .ok_or("missing")?
            .history
            .iter()
            .any(|item| matches!(item, HistoryItem::UserText(v) if v == "live WAL data"))
    );
    drop(reader);
    child
        .stdin
        .take()
        .ok_or("missing stdin")?
        .write_all(b"exit\n")?;
    assert!(child.wait()?.success());
    let _lock = SqliteStore::lock_existing(temp.path(), session.id)?;
    Ok(())
}

#[tokio::test]
async fn migration_creates_a_consistent_backup_and_preserves_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store.create_session(&session, "hash").await?;
    drop(store);
    let path = temp.path().join("sessions.sqlite3");
    let db = Connection::open(&path)?;
    db.execute_batch("DROP INDEX sessions_updated; PRAGMA user_version=1;")?;
    drop(db);
    let migrated = SqliteStore::open(temp.path(), vec![])?;
    assert_eq!(
        migrated.load_session(session.id).await?,
        Some(session.clone())
    );
    drop(migrated);
    let backup = fs::read_dir(temp.path())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".backup"))
        })
        .ok_or("missing backup")?;
    let backup_db = Connection::open(backup)?;
    assert_eq!(
        backup_db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        backup_db.query_row(
            "SELECT COUNT(*) FROM sessions WHERE id=?1",
            [session.id.0.to_string()],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    let db = Connection::open(path)?;
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        2
    );
    Ok(())
}

#[tokio::test]
async fn failed_migration_keeps_original_database_and_backup()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store.create_session(&session, "hash").await?;
    drop(store);
    let path = temp.path().join("sessions.sqlite3");
    let db = Connection::open(&path)?;
    db.execute_batch(
        "DROP INDEX sessions_updated; CREATE TABLE sessions_updated(x); PRAGMA user_version=1;",
    )?;
    drop(db);
    assert!(SqliteStore::open(temp.path(), vec![]).is_err());
    let db = Connection::open(path)?;
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sessions WHERE id=?1",
            [session.id.0.to_string()],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert!(
        fs::read_dir(temp.path())?
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with(".backup"))
    );
    Ok(())
}

#[test]
fn missing_schema_is_reported_as_corrupt() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let store = SqliteStore::open(temp.path(), vec![])?;
    let path = store.path().to_owned();
    drop(store);
    let db = Connection::open(path)?;
    db.execute_batch("DROP TABLE session_config")?;
    drop(db);
    let error = match SqliteStore::open(temp.path(), vec![]) {
        Ok(_) => return Err("damaged schema accepted".into()),
        Err(error) => error,
    };
    assert_eq!(error.0.code, "storage_corrupt");
    Ok(())
}

#[tokio::test]
async fn killing_writer_releases_process_session_lock() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    let lock = store.lock_session(session.id)?;
    store.create_session(&session, "hash").await?;
    drop(lock);
    drop(store);
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--exact", "sqlite_lock_child_process", "--nocapture"])
        .env("SAYA_TEST_LOCK_DIR", temp.path())
        .env("SAYA_TEST_LOCK_ID", session.id.0.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if line.contains("SAYA_CHILD_READY") {
                let _ = send.send(());
            }
        }
    });
    receive.recv_timeout(Duration::from_secs(5))?;
    child.kill()?;
    child.wait()?;
    let _lock = SqliteStore::lock_existing(temp.path(), session.id)?;
    let reopened = SqliteStore::open(temp.path(), vec![])?;
    assert!(
        reopened
            .load_session(session.id)
            .await?
            .ok_or("session missing")?
            .history
            .iter()
            .any(|item| matches!(item, HistoryItem::UserText(value) if value == "live WAL data"))
    );
    Ok(())
}

#[tokio::test]
async fn recovery_closes_interrupted_calls_without_replay_and_is_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    use saya_core::contract::ApprovalRequest;
    let temp = TempDir::new()?;
    let (session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store.create_session(&session, "hash").await?;
    let calls: Vec<_> = ["running", "pending", "unstarted", "done"]
        .into_iter()
        .map(|id| ToolCall {
            call_id: id.into(),
            name: "write_file".into(),
            arguments: "{}".into(),
        })
        .collect();
    let mut changes = vec![StoreChange::RunStarted(run.clone())];
    changes.extend(calls.iter().cloned().map(|call| StoreChange::History {
        session_id: session.id,
        item: HistoryItem::ToolCall(call),
    }));
    let ask = ToolCall {
        call_id: "clarification".into(),
        name: "ask_user".into(),
        arguments: "{}".into(),
    };
    changes.push(StoreChange::History {
        session_id: session.id,
        item: HistoryItem::ToolCall(ask.clone()),
    });
    changes.extend([
        StoreChange::ToolIntent {
            run_id: run.id,
            call: calls[0].clone(),
        },
        StoreChange::ToolIntent {
            run_id: run.id,
            call: ask,
        },
        StoreChange::ApprovalRequested(ApprovalRequest {
            approval_id: "approval".into(),
            call_id: "pending".into(),
            run_id: run.id,
            tool: "write_file".into(),
            operation_summary: "write".into(),
            arguments: serde_json::json!({}),
            cwd: temp.path().into(),
            work_mode: WorkMode::Work,
        }),
        StoreChange::ToolIntent {
            run_id: run.id,
            call: calls[3].clone(),
        },
        StoreChange::ToolFinished {
            run_id: run.id,
            call_id: "done".into(),
            result: tool_result(),
        },
        StoreChange::History {
            session_id: session.id,
            item: HistoryItem::ToolResult {
                call_id: "done".into(),
                result: tool_result(),
            },
        },
        StoreChange::ToolIntent {
            run_id: run.id,
            call: ToolCall {
                call_id: "background-child".into(),
                name: "exec".into(),
                arguments: "{}".into(),
            },
        },
    ]);
    store.commit(&changes).await?;
    let lock = store.lock_session(session.id)?;
    let recovered = store.recover_interrupted(session.id).await?;
    assert_eq!(
        (
            recovered.interrupted_runs,
            recovered.unknown_tools,
            recovered.closed_calls
        ),
        (1, 2, 4)
    );
    let saved = store.load_session(session.id).await?.ok_or("missing")?;
    for (id, expected) in [
        ("running", "outcome_unknown"),
        ("pending", "needs_confirmation"),
        ("unstarted", "not_executed"),
        ("clarification", "input_interrupted"),
    ] {
        assert_eq!(saved.history.iter().filter(|item|matches!(item,HistoryItem::ToolResult {call_id,result} if call_id==id && result.error.as_ref().is_some_and(|e|e.code==expected))).count(),1);
    }
    assert_eq!(
        saved
            .history
            .iter()
            .filter(|item| matches!(item,HistoryItem::ToolResult {call_id,..} if call_id=="done"))
            .count(),
        1
    );
    assert!(!store.has_unfinished_run(session.id).await?);
    assert_eq!(
        store.recover_interrupted(session.id).await?,
        Default::default()
    );
    let db = Connection::open(store.path())?;
    let unknown: i64 = db.query_row(
        "SELECT COUNT(*) FROM tool_calls WHERE run_id=?1 AND state='unknown'",
        [run.id.0.to_string()],
        |r| r.get(0),
    )?;
    assert_eq!(unknown, 2);
    drop(lock);
    Ok(())
}

#[tokio::test]
async fn recovery_failure_rolls_back_all_repairs() -> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store.create_session(&session, "hash").await?;
    let c = call();
    store
        .commit(&[
            StoreChange::RunStarted(run.clone()),
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::ToolCall(c.clone()),
            },
            StoreChange::ToolIntent {
                run_id: run.id,
                call: c,
            },
        ])
        .await?;
    let db = Connection::open(store.path())?;
    db.execute_batch("CREATE TRIGGER fail_recovery BEFORE UPDATE OF finished_at ON runs BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;
    let before = store.load_session(session.id).await?.ok_or("missing")?;
    assert!(store.recover_interrupted(session.id).await.is_err());
    assert_eq!(store.load_session(session.id).await?, Some(before));
    assert!(store.has_unfinished_run(session.id).await?);
    let state: String = db.query_row(
        "SELECT state FROM tool_calls WHERE run_id=?1",
        [run.id.0.to_string()],
        |r| r.get(0),
    )?;
    assert_eq!(state, "running");
    db.execute_batch("DROP TRIGGER fail_recovery")?;
    assert_eq!(store.recover_interrupted(session.id).await?.closed_calls, 1);
    Ok(())
}

#[tokio::test]
async fn failed_session_update_retains_conservative_guard() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = TempDir::new()?;
    let (session, _) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store.create_session(&session, "hash").await?;
    let db = Connection::open(store.path())?;
    db.execute_batch("CREATE TRIGGER fail_session_update BEFORE UPDATE OF metadata ON sessions BEGIN SELECT RAISE(ABORT,'test failure'); END;")?;
    let mut next = session.clone();
    next.work_mode = WorkMode::Plan;
    assert!(
        store
            .commit(&[StoreChange::Session(next.clone())])
            .await
            .is_err()
    );
    assert_eq!(store.load_session(session.id).await?, Some(session.clone()));
    let candidates =
        SqliteStore::guard_candidates(temp.path(), session.id)?.ok_or("guard missing")?;
    assert!(
        candidates
            .iter()
            .any(|c| c.work_mode == WorkMode::Work && c.workspace == session.workspace)
    );
    assert!(
        candidates
            .iter()
            .any(|c| c.work_mode == WorkMode::Plan && c.workspace == next.workspace)
    );
    db.execute_batch("DROP TRIGGER fail_session_update")?;
    store
        .commit(&[StoreChange::Session(session.clone())])
        .await?;
    let candidates =
        SqliteStore::guard_candidates(temp.path(), session.id)?.ok_or("guard missing")?;
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].work_mode, WorkMode::Work);
    Ok(())
}

#[tokio::test]
async fn recovery_does_not_guess_outcomes_for_dangling_calls_from_an_earlier_run()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, first) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store.create_session(&session, "hash").await?;
    let first_call = call();
    store
        .commit(&[
            StoreChange::RunStarted(first.clone()),
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::ToolCall(first_call),
            },
            StoreChange::RunFinished(result(&session, &first)),
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::UserText("new task".into()),
            },
        ])
        .await?;
    let second = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    store.commit(&[StoreChange::RunStarted(second)]).await?;
    let before = store.load_session(session.id).await?;
    let error = store
        .recover_interrupted(session.id)
        .await
        .expect_err("ambiguous historical call");
    assert_eq!(error.0.code, "storage_corrupt");
    assert_eq!(store.load_session(session.id).await?, before);
    assert!(store.has_unfinished_run(session.id).await?);
    Ok(())
}

#[tokio::test]
async fn recovery_links_background_launch_and_child_without_replay()
-> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    for scenario in [
        "pending",
        "not_started",
        "running",
        "finished",
        "acknowledged_running",
        "acknowledged_not_started",
    ] {
        let temp = TempDir::new()?;
        let (session, run) = fixture(temp.path());
        let store = SqliteStore::open(temp.path(), vec![])?;
        store.create_session(&session, "hash").await?;
        let parent = ToolCall {
            call_id: "launch".into(),
            name: "background_start".into(),
            arguments: "{\"tool\":\"exec\",\"arguments\":{}}".into(),
        };
        let digest: String = Sha256::digest(parent.call_id.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let job_id = format!("background-{digest}-01890f3a-8d14-7c5c-a5f7-000000000001");
        let child = ToolCall {
            call_id: job_id.clone(),
            name: "exec".into(),
            arguments: "{}".into(),
        };
        let mut changes = vec![
            StoreChange::RunStarted(run.clone()),
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::ToolCall(parent.clone()),
            },
            StoreChange::ToolIntent {
                run_id: run.id,
                call: parent,
            },
        ];
        if scenario == "pending" {
            changes.push(StoreChange::ApprovalRequested(
                saya_core::contract::ApprovalRequest {
                    approval_id: "pending".into(),
                    call_id: job_id.clone(),
                    run_id: run.id,
                    tool: "exec".into(),
                    operation_summary: "run".into(),
                    arguments: serde_json::json!({}),
                    cwd: temp.path().into(),
                    work_mode: WorkMode::Work,
                },
            ));
        }
        if matches!(scenario, "running" | "finished" | "acknowledged_running") {
            changes.push(StoreChange::ToolIntent {
                run_id: run.id,
                call: child,
            });
        }
        if scenario == "finished" {
            changes.push(StoreChange::ToolFinished {
                run_id: run.id,
                call_id: job_id.clone(),
                result: tool_result(),
            });
        }
        if scenario.starts_with("acknowledged") {
            let ack = ToolResult {
                ok: true,
                data: Some(serde_json::json!({"status":"started","job_id":job_id})),
                error: None,
                truncated: false,
            };
            changes.push(StoreChange::ToolFinished {
                run_id: run.id,
                call_id: "launch".into(),
                result: ack.clone(),
            });
            changes.push(StoreChange::History {
                session_id: session.id,
                item: HistoryItem::ToolResult {
                    call_id: "launch".into(),
                    result: ack,
                },
            });
        }
        store.commit(&changes).await?;
        let _lock = store.lock_session(session.id)?;
        store.recover_interrupted(session.id).await?;
        let saved = store.load_session(session.id).await?.ok_or("missing")?;
        let result = saved
            .history
            .iter()
            .find_map(|item| match item {
                HistoryItem::ToolResult { call_id, result } if call_id == "launch" => Some(result),
                _ => None,
            })
            .ok_or("missing launch result")?;
        match scenario {
            "pending" => assert_eq!(
                result.error.as_ref().map(|e| e.code.as_str()),
                Some("needs_confirmation")
            ),
            "not_started" => assert_eq!(
                result.error.as_ref().map(|e| e.code.as_str()),
                Some("not_executed")
            ),
            "running" => {
                assert_eq!(
                    result.error.as_ref().map(|e| e.code.as_str()),
                    Some("outcome_unknown")
                );
                assert_eq!(result.data.as_ref().unwrap()["job_id"], job_id);
            }
            "finished" => assert_eq!(result.data.as_ref().unwrap()["outcome"], "finished"),
            "acknowledged_running" | "acknowledged_not_started" => {
                let notice = saved
                    .history
                    .iter()
                    .find_map(|item| match item {
                        HistoryItem::UserText(value)
                            if value.contains("saya_background_recovery_v1") =>
                        {
                            Some(value)
                        }
                        _ => None,
                    })
                    .ok_or("missing notice")?;
                let notice: serde_json::Value = serde_json::from_str(notice)?;
                let job = &notice["saya_background_recovery_v1"]["jobs"][0];
                assert_eq!(job["job_id"], job_id);
                assert_eq!(
                    job["outcome"],
                    if scenario == "acknowledged_running" {
                        "unknown"
                    } else {
                        "not_executed"
                    }
                );
                assert_eq!(job["managed"], false);
                if scenario == "acknowledged_running" {
                    assert!(job["active"].is_null());
                } else {
                    assert_eq!(job["active"], false);
                }
            }
            _ => unreachable!(),
        }
        let listed = store.background_jobs(session.id).await?;
        match scenario {
            "pending" | "not_started" => assert!(listed.is_empty()),
            "running" | "acknowledged_running" => {
                assert_eq!(listed.len(), 1);
                assert_eq!(listed[0].job_id, job_id);
                assert_eq!(listed[0].state, "unknown");
            }
            "finished" => {
                assert_eq!(listed.len(), 1);
                assert_eq!(listed[0].state, "succeeded");
                assert!(listed[0].result.as_ref().is_some_and(|result| result.ok));
            }
            "acknowledged_not_started" => {
                assert_eq!(listed.len(), 1);
                assert_eq!(listed[0].state, "not_executed");
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

#[tokio::test]
async fn reserved_event_sequences_are_never_reused_and_commit_atomically()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let (session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![])?;
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    let (first, end) = store.reserve_event_sequences(session.id, 10).await?;
    assert_eq!((first, end), (1, 10));
    store
        .commit(&[StoreChange::ReservedEvent(event(&session, &run, first))])
        .await?;
    assert!(
        store
            .commit(&[StoreChange::ReservedEvent(event(&session, &run, first))])
            .await
            .is_err()
    );
    assert!(
        store
            .commit(&[StoreChange::ReservedEvent(event(&session, &run, end + 1))])
            .await
            .is_err()
    );
    let child = ToolCall {
        call_id: "uncommitted".into(),
        name: "read_file".into(),
        arguments: "{}".into(),
    };
    assert!(
        store
            .commit(&[
                StoreChange::ToolIntent {
                    run_id: run.id,
                    call: child
                },
                StoreChange::ReservedEvent(event(&session, &run, first))
            ])
            .await
            .is_err()
    );
    let db = Connection::open(store.path())?;
    let count: i64 = db.query_row(
        "SELECT count(*) FROM tool_calls WHERE call_id='uncommitted'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 0);
    drop(db);
    drop(store);
    let store = SqliteStore::open(temp.path(), vec![])?;
    assert_eq!(
        store.reserve_event_sequences(session.id, 5).await?,
        (11, 15)
    );
    Ok(())
}

#[tokio::test]
async fn signed_provider_state_with_known_secret_fails_closed_without_modification()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let secret = "fixture-opaque-secret";
    let (session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![secret.into()])?;
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run),
        ])
        .await?;
    let opaque = HistoryItem::ProviderItem {
        protocol: ModelProtocol::AnthropicMessages,
        value: serde_json::json!({"signature":secret}),
    };
    assert!(
        store
            .commit(&[StoreChange::History {
                session_id: session.id,
                item: opaque
            }])
            .await
            .is_err()
    );
    assert_eq!(store.load_session(session.id).await?.unwrap(), session);
    Ok(())
}
