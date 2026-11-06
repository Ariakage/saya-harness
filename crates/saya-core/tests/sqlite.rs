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
async fn known_secrets_are_rejected_without_database_persistence()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = TempDir::new()?;
    let secret = "fake\"secret\\value";
    let (mut session, run) = fixture(temp.path());
    let store = SqliteStore::open(temp.path(), vec![secret.into()])?;
    session.history.push(HistoryItem::UserText(secret.into()));
    assert!(
        store
            .commit(&[StoreChange::Session(session.clone())])
            .await
            .is_err()
    );
    assert!(store.load_session(session.id).await?.is_none());
    session.history.pop();
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    let mut c = call();
    c.arguments = serde_json::json!({"content":secret}).to_string();
    assert!(
        store
            .commit(&[StoreChange::ToolIntent {
                run_id: run.id,
                call: c
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
        }
    }
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
