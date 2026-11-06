mod support;
use saya_core::{
    boundary::{Cancellation, Operation, Store, StoreChange, Tool, ToolContext},
    contract::{
        BoundaryError, ErrorInfo, ModelProtocol, PermissionMode, Run, RunId, Session, SessionId,
        ToolCall, WorkMode,
    },
    memory::MemoryStore,
    tools::{
        read::{ReadLimits, ReadTools},
        write::{DEFAULT_WRITE_MAX_BYTES, WriteGate, WriteTool, WriteToolKind},
    },
};
use std::{
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

struct NeverCancel;
impl Cancellation for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}
struct Gate {
    count: AtomicUsize,
    allowed_checks: usize,
}
impl WriteGate for Gate {
    fn authorize(&self, _: &ToolCall, _: &ToolContext) -> Result<bool, BoundaryError> {
        Ok(self.count.fetch_add(1, Ordering::SeqCst) < self.allowed_checks)
    }
}
fn fixture(path: &Path) -> (Session, Run) {
    let session = Session {
        id: SessionId::new(),
        workspace: path.into(),
        work_mode: WorkMode::Work,
        profile: "fake".into(),
        protocol: ModelProtocol::Responses,
        model: "fake".into(),
        history: vec![],
    };
    let run = Run {
        id: RunId::new(),
        session_id: session.id,
        work_mode: WorkMode::Work,
        permissions: PermissionMode::Auto,
    };
    (session, run)
}
async fn store_for(session: &Session, run: &Run) -> Result<Arc<MemoryStore>, BoundaryError> {
    let store = Arc::new(MemoryStore::default());
    store
        .commit(&[
            StoreChange::Session(session.clone()),
            StoreChange::RunStarted(run.clone()),
        ])
        .await?;
    Ok(store)
}
fn call(name: &str, id: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

#[tokio::test]
async fn create_update_conflict_and_permissions() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let gate = Arc::new(Gate {
        count: AtomicUsize::new(0),
        allowed_checks: usize::MAX,
    });
    let tool = WriteTool::new(
        files.clone(),
        store,
        gate,
        WriteToolKind::WriteFile,
        DEFAULT_WRITE_MAX_BYTES,
    );
    let ctx = ToolContext {
        run,
        workspace: temp.path().to_owned(),
    };
    let initial = tool
        .execute(
            &call(
                "write_file",
                "a",
                serde_json::json!({"path":"x.txt","content":"first\n","expected_version":null}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert!(initial.ok);
    let version = files.read_file(Path::new("x.txt"), 1, 10)?.version;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            temp.path().join("x.txt"),
            std::fs::Permissions::from_mode(0o640),
        )?;
    }
    let updated = tool
        .execute(
            &call(
                "write_file",
                "b",
                serde_json::json!({"path":"x.txt","content":"second\n","expected_version":version}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert!(updated.ok);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("x.txt"))?,
        "second\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(temp.path().join("x.txt"))?
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }
    let conflict = tool
        .execute(
            &call(
                "write_file",
                "c",
                serde_json::json!({"path":"x.txt","content":"wrong","expected_version":version}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        conflict.error.as_ref().map(|e| e.code.as_str()),
        Some("conflict")
    );
    let overwrite = tool
        .execute(
            &call(
                "write_file",
                "d",
                serde_json::json!({"path":"x.txt","content":"wrong","expected_version":null}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        overwrite.error.as_ref().map(|e| e.code.as_str()),
        Some("conflict")
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("x.txt"))?,
        "second\n"
    );
    Ok(())
}

struct FailIntent;
impl Store for FailIntent {
    fn load_session(&self, _: SessionId) -> Operation<'_, Option<Session>> {
        Box::pin(async { Ok(None) })
    }
    fn commit<'a>(&'a self, _: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async {
            Err(BoundaryError(
                ErrorInfo {
                    code: "storage_error".into(),
                    message: "injected".into(),
                },
                None,
                None,
            ))
        })
    }
}
#[tokio::test]
async fn no_write_when_plan_or_intent_save_fails() -> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (_, run) = fixture(temp.path());
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let gate = Arc::new(Gate {
        count: AtomicUsize::new(0),
        allowed_checks: usize::MAX,
    });
    let tool = WriteTool::new(
        files,
        Arc::new(FailIntent),
        gate,
        WriteToolKind::WriteFile,
        DEFAULT_WRITE_MAX_BYTES,
    );
    let call = call(
        "write_file",
        "a",
        serde_json::json!({"path":"x.txt","content":"data","expected_version":null}),
    );
    let ctx = ToolContext {
        run: run.clone(),
        workspace: temp.path().to_owned(),
    };
    assert_eq!(
        tool.execute(&call, &ctx, &NeverCancel)
            .await
            .expect_err("intent failure")
            .0
            .code,
        "storage_error"
    );
    assert!(!temp.path().join("x.txt").exists());
    let ctx = ToolContext {
        run: Run {
            work_mode: WorkMode::Plan,
            ..run
        },
        workspace: temp.path().to_owned(),
    };
    let rejected = tool.execute(&call, &ctx, &NeverCancel).await?;
    assert_eq!(
        rejected.error.as_ref().map(|e| e.code.as_str()),
        Some("mode_denied")
    );
    assert!(!temp.path().join("x.txt").exists());
    Ok(())
}

#[tokio::test]
async fn applies_only_single_matching_patch_and_never_deletes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    std::fs::write(temp.path().join("file.txt"), "old\n")?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let version = files.read_file(Path::new("file.txt"), 1, 10)?.version;
    let tool = WriteTool::new(
        files,
        store,
        Arc::new(Gate {
            count: AtomicUsize::new(0),
            allowed_checks: usize::MAX,
        }),
        WriteToolKind::ApplyPatch,
        DEFAULT_WRITE_MAX_BYTES,
    );
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let patch = "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n";
    let result = tool
        .execute(
            &call(
                "apply_patch",
                "a",
                serde_json::json!({"path":"file.txt","patch":patch,"expected_version":version}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert!(result.ok, "{:?}", result.error);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("file.txt"))?,
        "new\n"
    );
    Ok(())
}

struct FailResult {
    inner: Arc<MemoryStore>,
}
impl Store for FailResult {
    fn load_session(&self, id: SessionId) -> Operation<'_, Option<Session>> {
        self.inner.load_session(id)
    }
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()> {
        Box::pin(async move {
            if changes
                .iter()
                .any(|c| matches!(c, StoreChange::ToolFinished { .. }))
            {
                return Err(BoundaryError(
                    ErrorInfo {
                        code: "storage_error".into(),
                        message: "result save failed".into(),
                    },
                    None,
                    None,
                ));
            }
            self.inner.commit(changes).await
        })
    }
}
#[tokio::test]
async fn post_write_storage_failure_preserves_observed_tool_result()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = Arc::new(FailResult {
        inner: store_for(&session, &run).await?,
    });
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let gate = Arc::new(Gate {
        count: AtomicUsize::new(0),
        allowed_checks: usize::MAX,
    });
    let tool = WriteTool::new(
        files,
        store,
        gate,
        WriteToolKind::WriteFile,
        DEFAULT_WRITE_MAX_BYTES,
    );
    let ctx = ToolContext {
        run,
        workspace: temp.path().to_owned(),
    };
    let failure = tool
        .execute(
            &call(
                "write_file",
                "a",
                serde_json::json!({"path":"x.txt","content":"saved","expected_version":null}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await
        .expect_err("injected result save failure");
    assert_eq!(failure.0.code, "storage_error");
    assert!(failure.2.as_ref().is_some_and(|result| result.ok));
    assert_eq!(std::fs::read_to_string(temp.path().join("x.txt"))?, "saved");
    Ok(())
}

#[tokio::test]
async fn stale_authorization_is_recorded_and_does_not_write()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let gate = Arc::new(Gate {
        count: AtomicUsize::new(0),
        allowed_checks: 1,
    });
    let tool = WriteTool::new(
        files,
        store.clone(),
        gate,
        WriteToolKind::WriteFile,
        DEFAULT_WRITE_MAX_BYTES,
    );
    let ctx = ToolContext {
        run,
        workspace: temp.path().to_owned(),
    };
    let result = tool
        .execute(
            &call(
                "write_file",
                "a",
                serde_json::json!({"path":"x.txt","content":"blocked","expected_version":null}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        result.error.as_ref().map(|e| e.code.as_str()),
        Some("permission_denied")
    );
    assert!(!temp.path().join("x.txt").exists());
    let saved = store.load_session(session.id).await?.ok_or("missing")?;
    assert!(saved.history.iter().any(|item| matches!(item, saya_core::contract::HistoryItem::ToolResult { call_id, result } if call_id == "a" && !result.ok)));
    Ok(())
}

#[tokio::test]
async fn rejects_missing_version_multi_patch_deletion_and_outside_create()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let external = support::temp_workspace()?;
    std::fs::write(temp.path().join("file.txt"), "old\n")?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let version = files.read_file(Path::new("file.txt"), 1, 10)?.version;
    let gate = Arc::new(Gate {
        count: AtomicUsize::new(0),
        allowed_checks: usize::MAX,
    });
    let write = WriteTool::new(
        files.clone(),
        store.clone(),
        gate.clone(),
        WriteToolKind::WriteFile,
        64,
    );
    let patch = WriteTool::new(
        files,
        store,
        gate,
        WriteToolKind::ApplyPatch,
        DEFAULT_WRITE_MAX_BYTES,
    );
    let ctx = ToolContext {
        run,
        workspace: temp.path().to_owned(),
    };
    let missing = write
        .execute(
            &call(
                "write_file",
                "a",
                serde_json::json!({"path":"missing.txt","content":"x"}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        missing.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_args")
    );
    let outside = write.execute(&call("write_file", "b", serde_json::json!({"path":external.path().join("x"),"content":"x","expected_version":null})), &ctx, &NeverCancel).await?;
    assert_eq!(
        outside.error.as_ref().map(|e| e.code.as_str()),
        Some("path_denied")
    );
    let multi = "--- a/file.txt\n+++ b/file.txt\n@@ -1 +1 @@\n-old\n+new\n--- a/other.txt\n+++ b/other.txt\n@@ -1 +1 @@\n-a\n+b\n";
    let rejected = patch
        .execute(
            &call(
                "apply_patch",
                "c",
                serde_json::json!({"path":"file.txt","patch":multi,"expected_version":version}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        rejected.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_patch")
    );
    let deletion = "--- a/file.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n";
    let rejected = patch
        .execute(
            &call(
                "apply_patch",
                "d",
                serde_json::json!({"path":"file.txt","patch":deletion,"expected_version":version}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        rejected.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_patch")
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("file.txt"))?,
        "old\n"
    );
    Ok(())
}

#[tokio::test]
async fn write_rejects_oversized_binary_and_symlink_escape()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = support::temp_workspace()?;
    let external = support::temp_workspace()?;
    std::fs::write(external.path().join("outside.txt"), "outside")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        external.path().join("outside.txt"),
        temp.path().join("link.txt"),
    )?;
    let (session, run) = fixture(temp.path());
    let store = store_for(&session, &run).await?;
    let files = Arc::new(ReadTools::new(
        temp.path(),
        &[],
        false,
        ReadLimits::default(),
    )?);
    let gate = Arc::new(Gate {
        count: AtomicUsize::new(0),
        allowed_checks: usize::MAX,
    });
    let tool = WriteTool::new(files, store, gate, WriteToolKind::WriteFile, 4);
    let ctx = ToolContext {
        run,
        workspace: temp.path().into(),
    };
    let large = tool
        .execute(
            &call(
                "write_file",
                "a",
                serde_json::json!({"path":"large.txt","content":"12345","expected_version":null}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        large.error.as_ref().map(|e| e.code.as_str()),
        Some("write_too_large")
    );
    let binary = tool
        .execute(
            &call(
                "write_file",
                "b",
                serde_json::json!({"path":"binary.txt","content":"a\0b","expected_version":null}),
            ),
            &ctx,
            &NeverCancel,
        )
        .await?;
    assert_eq!(
        binary.error.as_ref().map(|e| e.code.as_str()),
        Some("invalid_text")
    );
    #[cfg(unix)]
    {
        let link = tool
            .execute(
                &call(
                    "write_file",
                    "c",
                    serde_json::json!({"path":"link.txt","content":"x","expected_version":"any"}),
                ),
                &ctx,
                &NeverCancel,
            )
            .await?;
        assert_eq!(
            link.error.as_ref().map(|e| e.code.as_str()),
            Some("path_denied")
        );
    }
    assert_eq!(
        std::fs::read_to_string(external.path().join("outside.txt"))?,
        "outside"
    );
    assert!(!temp.path().join("large.txt").exists());
    assert!(!temp.path().join("binary.txt").exists());
    Ok(())
}
