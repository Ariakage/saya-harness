use saya_core::{
    boundary::{Cancellation, Store, StoreChange},
    contract::{
        HistoryItem, ModelProtocol, PermissionMode, Run, RunId, Session, SessionId, ToolCall,
        WorkMode,
    },
    sqlite::{SnapshotLimits, SqliteStore},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[derive(Default)]
struct Cancel(AtomicBool);
impl Cancellation for Cancel {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn cancelled(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}
fn session(root: &std::path::Path) -> Session {
    Session {
        id: SessionId::new(),
        workspace: root.into(),
        work_mode: WorkMode::Work,
        profile: "fixture".into(),
        protocol: ModelProtocol::ChatCompletions,
        model: "fake".into(),
        history: vec![],
    }
}
#[tokio::test]
async fn active_wal_snapshot_reads_real_watermarks_without_creating_side_files()
-> Result<(), Box<dyn std::error::Error>> {
    let data = tempfile::tempdir()?;
    let copy = tempfile::tempdir()?;
    let store = SqliteStore::open(data.path(), vec![])?;
    let session = session(data.path());
    store.create_session(&session, "hash").await?;
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    store
        .commit(&[
            StoreChange::RunStarted(run),
            StoreChange::History {
                session_id: session.id,
                item: HistoryItem::UserText("committed".into()),
            },
        ])
        .await?;
    store.reserve_event_sequences(session.id, 100).await?;
    for file in ["sessions.sqlite3", "sessions.sqlite3-wal"] {
        std::fs::copy(data.path().join(file), copy.path().join(file))?;
    }
    let before: Vec<_> = std::fs::read_dir(copy.path())?
        .map(|f| f.map(|f| f.file_name()))
        .collect::<Result<_, _>>()?;
    let snapshot = SqliteStore::read_snapshot(
        copy.path(),
        session.id,
        SnapshotLimits::default(),
        Arc::new(Cancel::default()),
    )
    .await?;
    assert_eq!(snapshot.last_event_seq, 0);
    assert_eq!(snapshot.last_change_seq, 3);
    assert_eq!(
        snapshot.session.history,
        vec![HistoryItem::UserText("committed".into())]
    );
    assert!(snapshot.runs[0]["finished_at"].is_null());
    let after: Vec<_> = std::fs::read_dir(copy.path())?
        .map(|f| f.map(|f| f.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(before, after);
    Ok(())
}
#[tokio::test]
async fn closed_database_and_missing_source_do_not_initialize_files()
-> Result<(), Box<dyn std::error::Error>> {
    let data = tempfile::tempdir()?;
    let empty = tempfile::tempdir()?;
    let session = session(data.path());
    {
        let store = SqliteStore::open(data.path(), vec![])?;
        store.create_session(&session, "hash").await?;
    }
    let snapshot = SqliteStore::read_snapshot(
        data.path(),
        session.id,
        SnapshotLimits::default(),
        Arc::new(Cancel::default()),
    )
    .await?;
    assert_eq!(snapshot.session.id, session.id);
    assert!(!data.path().join("sessions.sqlite3-wal").exists());
    assert!(
        SqliteStore::read_snapshot(
            empty.path(),
            session.id,
            SnapshotLimits::default(),
            Arc::new(Cancel::default())
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read_dir(empty.path())?.count(), 0);
    Ok(())
}
#[tokio::test]
async fn old_corrupt_linked_and_oversized_sources_are_explicit_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let data = tempfile::tempdir()?;
    let session = session(data.path());
    {
        let store = SqliteStore::open(data.path(), vec![])?;
        store.create_session(&session, "hash").await?;
    }
    let db = data.path().join("sessions.sqlite3");
    let conn = rusqlite::Connection::open(&db)?;
    conn.pragma_update(None, "user_version", 1)?;
    drop(conn);
    let failure = SqliteStore::read_snapshot(
        data.path(),
        session.id,
        SnapshotLimits::default(),
        Arc::new(Cancel::default()),
    )
    .await
    .unwrap_err();
    assert_eq!(failure.0.code, "storage_migration_required");
    let conn = rusqlite::Connection::open(&db)?;
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        1
    );
    drop(conn);
    std::fs::write(&db, "not SQLite")?;
    assert!(
        SqliteStore::read_snapshot(
            data.path(),
            session.id,
            SnapshotLimits::default(),
            Arc::new(Cancel::default())
        )
        .await
        .is_err()
    );
    std::fs::remove_file(&db)?;
    let other = tempfile::NamedTempFile::new()?;
    std::os::unix::fs::symlink(other.path(), &db)?;
    assert!(
        SqliteStore::read_snapshot(
            data.path(),
            session.id,
            SnapshotLimits::default(),
            Arc::new(Cancel::default())
        )
        .await
        .is_err()
    );
    Ok(())
}
#[tokio::test]
async fn snapshot_limits_cancellation_and_deadline_are_enforced()
-> Result<(), Box<dyn std::error::Error>> {
    let data = tempfile::tempdir()?;
    let store = SqliteStore::open(data.path(), vec![])?;
    let session = session(data.path());
    store.create_session(&session, "hash").await?;
    store
        .commit(&[StoreChange::History {
            session_id: session.id,
            item: HistoryItem::UserText("some text".into()),
        }])
        .await?;
    for limits in [
        SnapshotLimits {
            max_bytes: 1,
            ..SnapshotLimits::default()
        },
        SnapshotLimits {
            max_rows: 1,
            ..SnapshotLimits::default()
        },
        SnapshotLimits {
            max_source_bytes: 1,
            ..SnapshotLimits::default()
        },
    ] {
        assert!(
            SqliteStore::read_snapshot(
                data.path(),
                session.id,
                limits,
                Arc::new(Cancel::default())
            )
            .await
            .is_err()
        );
    }
    let cancel = Arc::new(Cancel(AtomicBool::new(true)));
    assert_eq!(
        SqliteStore::read_snapshot(data.path(), session.id, SnapshotLimits::default(), cancel)
            .await
            .unwrap_err()
            .0
            .code,
        "cancelled"
    );
    assert_eq!(
        SqliteStore::read_snapshot(
            data.path(),
            session.id,
            SnapshotLimits {
                timeout: Duration::ZERO,
                ..SnapshotLimits::default()
            },
            Arc::new(Cancel::default())
        )
        .await
        .unwrap_err()
        .0
        .code,
        "snapshot_timeout"
    );
    Ok(())
}
#[tokio::test]
async fn concurrent_writer_never_produces_mixed_collections()
-> Result<(), Box<dyn std::error::Error>> {
    let data = tempfile::tempdir()?;
    let store = SqliteStore::open(data.path(), vec![])?;
    let session = session(data.path());
    store.create_session(&session, "hash").await?;
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    store
        .commit(&[StoreChange::RunStarted(run.clone())])
        .await?;
    let writer = {
        let store = store.clone();
        let run = run.clone();
        tokio::spawn(async move {
            for i in 0..30 {
                store
                    .commit(&[
                        StoreChange::ToolIntent {
                            run_id: run.id,
                            call: ToolCall {
                                call_id: format!("call-{i}"),
                                name: "read_file".into(),
                                arguments: "{}".into(),
                            },
                        },
                        StoreChange::History {
                            session_id: run.session_id,
                            item: HistoryItem::UserText(i.to_string()),
                        },
                    ])
                    .await?;
                tokio::task::yield_now().await;
            }
            Ok::<_, saya_core::contract::BoundaryError>(())
        })
    };
    let mut successes = 0;
    for _ in 0..20 {
        match SqliteStore::read_snapshot(
            data.path(),
            session.id,
            SnapshotLimits::default(),
            Arc::new(Cancel::default()),
        )
        .await
        {
            Ok(snapshot) => {
                successes += 1;
                assert_eq!(snapshot.session.history.len(), snapshot.tool_calls.len());
                assert_eq!(
                    snapshot.last_change_seq,
                    2 + 2 * snapshot.tool_calls.len() as u64
                );
            }
            Err(e) => assert_eq!(e.0.code, "storage_busy"),
        }
    }
    writer.await??;
    assert!(successes > 0);
    Ok(())
}
