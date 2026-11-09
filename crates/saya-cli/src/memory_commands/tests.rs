use super::*;
use clap::Parser;
use saya_core::{
    config::{Environment, Overrides},
    global_memory::{
        MemoryEntry, MemoryFact, MemoryKind, MemorySource, MemorySourceKind, MemorySourceRef,
    },
};
use std::os::unix::fs::symlink;

struct Fixture {
    _temp: tempfile::TempDir,
    config: Config,
}
impl Fixture {
    fn new(secret: Option<&str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.toml");
        let mut text = "schema_version=1\n".to_owned();
        let mut environment = vec![("HOME".into(), temp.path().as_os_str().to_owned())];
        if let Some(secret) = secret {
            text.push_str("[profiles.fixture]\nprotocol=\"openai-chat-completions\"\nbase_url=\"https://example.invalid/v1\"\nmodel=\"fixture\"\nauth=\"bearer\"\napi_key_env=\"FIXTURE_KEY\"\ncontext_window_tokens=128000\n");
            environment.push(("FIXTURE_KEY".into(), secret.into()));
        }
        std::fs::write(&path, text).unwrap();
        let config = saya_core::config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            Environment::from_pairs(environment),
            temp.path(),
        )
        .unwrap();
        Self {
            _temp: temp,
            config,
        }
    }
    fn store(&self) -> GlobalMemoryStore {
        GlobalMemoryStore::open(
            &self.config.memory_root().unwrap(),
            WorkspaceIdentity::from_path(&self.config.cwd).unwrap(),
            MemoryLimits::default(),
            known_registry(&self.config).unwrap(),
        )
        .unwrap()
    }
    fn revision(&self) -> String {
        self.store().snapshot().unwrap().revision
    }
    fn change(&self, text: &str) -> PathBuf {
        let source = MemorySource {
            id: "src_user".into(),
            kind: MemorySourceKind::User,
            scope: MemoryScope::Global,
            reference: "explicit-admin-input".into(),
            revision: "a".repeat(64),
            observed_at: "not_recorded".into(),
            description: "Explicit administrative source".into(),
            metadata_withheld: false,
        };
        let change = MemoryChange::Upsert {
            entry: MemoryEntry {
                id: "mem_preferences".into(),
                title: "Current preferences".into(),
                scope: MemoryScope::Global,
                updated_at: "not_recorded".into(),
                resources: Vec::new(),
                facts: vec![MemoryFact {
                    id: "fact_preference".into(),
                    kind: MemoryKind::ExplicitPreference,
                    text: text.into(),
                    sources: vec![MemorySourceRef {
                        source_id: source.id.clone(),
                        revision: source.revision.clone(),
                    }],
                    aliases: Vec::new(),
                    derived_from: Vec::new(),
                }],
            },
            sources: vec![source],
        };
        let path = self.config.cwd.join("change.json");
        std::fs::write(&path, serde_json::to_vec(&change).unwrap()).unwrap();
        path
    }
    fn apply(&self, operation_id: &str, text: &str) -> Value {
        run(
            &MemoryAction::Apply {
                change: self.change(text),
                expected_revision: self.revision(),
                operation_id: operation_id.into(),
            },
            &self.config,
        )
        .unwrap()
    }
}
fn code(result: Result<Value, BoundaryError>) -> String {
    match result {
        Ok(_) => panic!("expected memory command error"),
        Err(error) => error.0.code,
    }
}

#[test]
fn empty_read_commands_remain_offline_without_creating_memory() {
    let fixture = Fixture::new(None);
    assert_eq!(
        run(
            &MemoryAction::List {
                page: 1,
                page_size: 20
            },
            &fixture.config
        )
        .unwrap()["data"]["total"],
        0
    );
    assert_eq!(
        run(&MemoryAction::Version, &fixture.config).unwrap()["data"]["exists"],
        false
    );
    assert!(
        run(
            &MemoryAction::Show {
                entry_id: None,
                fact_id: None,
                cursor: None,
                max_bytes: 16384,
                max_items: 16
            },
            &fixture.config
        )
        .is_ok()
    );
    assert!(!fixture.config.memory_root().unwrap().exists());
    assert!(!fixture.config.data_dir.exists());
}

#[test]
fn review_is_version_bound_and_apply_does_not_return_before_text() {
    let fixture = Fixture::new(None);
    let change = fixture.change("Prefer focused changes.");
    let revision = fixture.revision();
    let review = run(
        &MemoryAction::Review {
            change: change.clone(),
            expected_revision: revision.clone(),
            operation_id: "op_review".into(),
            page: 1,
            page_size: 10,
        },
        &fixture.config,
    )
    .unwrap();
    assert_eq!(review["data"]["executed"], false);
    assert_eq!(review["data"]["total_changes"], 1);
    assert!(!fixture.config.memory_root().unwrap().exists());
    let result = run(
        &MemoryAction::Apply {
            change: change.clone(),
            expected_revision: revision.clone(),
            operation_id: "op_review".into(),
        },
        &fixture.config,
    )
    .unwrap();
    assert_eq!(result["data"]["status"], "committed");
    assert!(!result.to_string().contains("Prefer focused changes"));
    let retry = run(
        &MemoryAction::Apply {
            change,
            expected_revision: revision,
            operation_id: "op_review".into(),
        },
        &fixture.config,
    )
    .unwrap();
    assert_eq!(retry["data"]["status"], "already_committed");
    assert_eq!(retry["data"]["applied_now"], false);
}

#[test]
fn feature_switches_and_plan_block_actual_management_writes() {
    let mut fixture = Fixture::new(None);
    let action = MemoryAction::Apply {
        change: fixture.change("A safe fact."),
        expected_revision: fixture.revision(),
        operation_id: "op_disabled".into(),
    };
    fixture.config.memory.write = false;
    assert_eq!(code(run(&action, &fixture.config)), "memory_write_disabled");
    fixture.config.memory.read = false;
    assert_eq!(
        run(&MemoryAction::Version, &fixture.config).unwrap()["data"]["exists"],
        false
    );
    assert_eq!(
        code(run(
            &MemoryAction::List {
                page: 1,
                page_size: 20
            },
            &fixture.config
        )),
        "memory_read_disabled"
    );
    assert_eq!(
        run(&MemoryAction::Settings, &fixture.config).unwrap()["data"]["read"],
        false
    );
    fixture.config.memory.read = true;
    fixture.config.memory.write = true;
    fixture.config.mode = WorkMode::Plan;
    assert_eq!(code(run(&action, &fixture.config)), "mode_denied");
    assert_eq!(
        code(run(
            &MemoryAction::Export {
                file: fixture.config.cwd.join("blocked.json")
            },
            &fixture.config
        )),
        "mode_denied"
    );
    assert!(!fixture.config.cwd.join("blocked.json").exists());
    assert!(!fixture.config.memory_root().unwrap().exists());
}

#[test]
fn write_switch_is_independent_and_read_disabled_keeps_only_metadata_visible() {
    let mut fixture = Fixture::new(None);
    fixture.apply("op_initial", "A bounded preference.");
    let backup = fixture.config.cwd.join("backup.json");
    run(
        &MemoryAction::Export {
            file: backup.clone(),
        },
        &fixture.config,
    )
    .unwrap();
    fixture.config.memory.read = false;
    for action in [
        MemoryAction::List {
            page: 1,
            page_size: 20,
        },
        MemoryAction::Show {
            entry_id: None,
            fact_id: None,
            cursor: None,
            max_bytes: 16384,
            max_items: 16,
        },
        MemoryAction::Search {
            query: "bounded".into(),
            cursor: None,
            max_bytes: 16384,
            max_items: 16,
        },
        MemoryAction::Export {
            file: fixture.config.cwd.join("disabled-backup.json"),
        },
    ] {
        assert_eq!(code(run(&action, &fixture.config)), "memory_read_disabled");
    }
    let action = MemoryAction::Apply {
        change: fixture.change("A current correction."),
        expected_revision: fixture.revision(),
        operation_id: "op_write_without_read".into(),
    };
    let result = run(&action, &fixture.config).unwrap();
    assert_eq!(result["data"]["status"], "committed");
    assert!(!result.to_string().contains("current correction"));
    assert!(run(&MemoryAction::Version, &fixture.config).is_ok());
    assert!(
        run(
            &MemoryAction::Operation {
                operation_id: "op_write_without_read".into()
            },
            &fixture.config
        )
        .is_ok()
    );
    assert!(
        run(
            &MemoryAction::History {
                page: 1,
                page_size: 20
            },
            &fixture.config
        )
        .is_ok()
    );
    let version = fixture.store().history().unwrap()[0].version.clone();
    run(
        &MemoryAction::Rollback {
            version,
            expected_revision: fixture.revision(),
            operation_id: "op_rollback_without_read".into(),
        },
        &fixture.config,
    )
    .unwrap();
    run(
        &MemoryAction::Restore {
            file: backup,
            expected_revision: fixture.revision(),
            operation_id: "op_restore_without_read".into(),
        },
        &fixture.config,
    )
    .unwrap();
    run(
        &MemoryAction::Forget {
            scope: MemoryScopeOption::Global,
            fact_ids: vec!["fact_preference".into()],
            expected_revision: fixture.revision(),
            operation_id: "op_forget_without_read".into(),
        },
        &fixture.config,
    )
    .unwrap();
    assert!(fixture.store().snapshot().unwrap().entries.is_empty());
    assert!(run(&MemoryAction::Recover, &fixture.config).is_ok());
    fixture.config.memory.write = false;
    assert_eq!(
        code(run(&MemoryAction::Recover, &fixture.config)),
        "memory_write_disabled"
    );
    fixture.config.memory.write = true;
    fixture.config.mode = WorkMode::Plan;
    assert_eq!(
        code(run(&MemoryAction::Recover, &fixture.config)),
        "mode_denied"
    );
}

#[test]
fn cancelled_cli_administration_does_not_initialize_or_publish() {
    let fixture = Fixture::new(None);
    let action = MemoryAction::Apply {
        change: fixture.change("A bounded fact."),
        expected_revision: fixture.revision(),
        operation_id: "op_cancelled".into(),
    };
    let cancel = AtomicBool::new(true);
    assert_eq!(
        code(run_cancellable(&action, &fixture.config, &cancel)),
        "cancelled"
    );
    let target = fixture.config.cwd.join("cancelled.json");
    assert_eq!(
        code(run_cancellable(
            &MemoryAction::Export {
                file: target.clone()
            },
            &fixture.config,
            &cancel
        )),
        "cancelled"
    );
    assert_eq!(
        code(run_cancellable(
            &MemoryAction::Recover,
            &fixture.config,
            &cancel
        )),
        "cancelled"
    );
    assert!(!target.exists());
    assert!(!fixture.config.memory_root().unwrap().exists());
}

#[test]
fn cancellation_during_memory_lock_wait_preserves_version_and_operation_identity() {
    use std::os::fd::AsRawFd;
    use std::time::Duration;
    let fixture = Fixture::new(None);
    fixture.apply("op_initial", "A current fact.");
    let original = fixture.revision();
    let action = MemoryAction::Apply {
        change: fixture.change("An uncommitted replacement."),
        expected_revision: original.clone(),
        operation_id: "op_wait_cancel".into(),
    };
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(
            fixture
                .config
                .memory_root()
                .unwrap()
                .join(".saya-memory.lock"),
        )
        .unwrap();
    // SAFETY: flock uses this owned regular-file descriptor. The helper owns
    // the descriptor until it drops it after publishing the cancellation flag.
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let stopping = cancel.clone();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(25));
        stopping.store(true, Ordering::Release);
        drop(lock);
    });
    assert_eq!(
        code(run_cancellable(&action, &fixture.config, &cancel)),
        "cancelled"
    );
    release.join().unwrap();
    assert_eq!(fixture.revision(), original);
    assert!(
        fixture
            .store()
            .operation_receipt("op_wait_cancel")
            .unwrap()
            .is_none()
    );
    assert!(
        !fixture
            .config
            .memory_root()
            .unwrap()
            .join("memory/.saya-journal.json")
            .exists()
    );
}

#[test]
fn export_is_exclusive_and_restore_preserves_current_forgetting_constraints() {
    let fixture = Fixture::new(None);
    fixture.apply("op_initial", "Prefer focused changes.");
    let backup = fixture.config.cwd.join("backup.json");
    let exported = run(
        &MemoryAction::Export {
            file: backup.clone(),
        },
        &fixture.config,
    )
    .unwrap();
    assert_eq!(exported["data"]["coverage"]["topic_bodies"], false);
    assert_eq!(exported["data"]["coverage"]["historical_versions"], false);
    let original = std::fs::read(&backup).unwrap();
    assert!(
        run(
            &MemoryAction::Export {
                file: backup.clone()
            },
            &fixture.config
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    let forget = MemoryAction::Forget {
        scope: MemoryScopeOption::Global,
        fact_ids: vec!["fact_preference".into()],
        expected_revision: fixture.revision(),
        operation_id: "op_forget".into(),
    };
    let result = run(&forget, &fixture.config).unwrap();
    assert!(!result.to_string().contains("Prefer focused changes"));
    assert!(fixture.store().snapshot().unwrap().entries.is_empty());
    let restored = run(
        &MemoryAction::Restore {
            file: backup,
            expected_revision: fixture.revision(),
            operation_id: "op_restore".into(),
        },
        &fixture.config,
    )
    .unwrap();
    assert_eq!(restored["data"]["status"], "committed");
    assert!(fixture.store().snapshot().unwrap().entries.is_empty());
}

#[test]
fn source_changes_conflicts_and_unsafe_inputs_preserve_current_memory() {
    let fixture = Fixture::new(None);
    fixture.apply("op_initial", "A current preference.");
    let before = fixture.revision();
    let action = MemoryAction::Apply {
        change: fixture.change("A replacement preference."),
        expected_revision: "0".repeat(64),
        operation_id: "op_stale".into(),
    };
    assert_eq!(code(run(&action, &fixture.config)), "memory_conflict");
    assert_eq!(fixture.revision(), before);
    let linked = fixture.config.cwd.join("linked.json");
    symlink(fixture.config.cwd.join("change.json"), &linked).unwrap();
    assert_eq!(
        code(run(
            &MemoryAction::Apply {
                change: linked,
                expected_revision: before.clone(),
                operation_id: "op_linked".into()
            },
            &fixture.config
        )),
        "memory_input_io"
    );
    std::fs::write(
        fixture.config.cwd.join("invalid.json"),
        "unsupported unstructured text",
    )
    .unwrap();
    assert_eq!(
        code(run(
            &MemoryAction::Restore {
                file: fixture.config.cwd.join("invalid.json"),
                expected_revision: before.clone(),
                operation_id: "op_invalid".into()
            },
            &fixture.config
        )),
        "memory_backup_invalid"
    );
    assert_eq!(fixture.revision(), before);
}

#[test]
fn known_credentials_are_rejected_before_writing_and_errors_are_static() {
    let secret = "memory-admin-protected-fixture";
    let fixture = Fixture::new(Some(secret));
    let action = MemoryAction::Apply {
        change: fixture.change(secret),
        expected_revision: fixture.revision(),
        operation_id: "op_sensitive".into(),
    };
    let error = run(&action, &fixture.config).unwrap_err();
    assert_eq!(error.0.code, "sensitive_data");
    assert!(!error.to_string().contains(secret));
    assert!(!fixture.config.memory_root().unwrap().exists());
    let target = fixture.config.cwd.join(secret);
    assert_eq!(
        code(run(
            &MemoryAction::Export {
                file: target.clone()
            },
            &fixture.config
        )),
        "sensitive_data"
    );
    assert!(!target.exists());
}

#[test]
fn pagination_and_clap_shapes_are_bounded_and_explicit() {
    #[derive(Parser)]
    struct TestCli {
        #[command(subcommand)]
        action: MemoryAction,
    }
    let cli = TestCli::try_parse_from(["saya-memory", "list", "--page", "2", "--page-size", "10"])
        .unwrap();
    assert!(matches!(
        cli.action,
        MemoryAction::List {
            page: 2,
            page_size: 10
        }
    ));
    assert!(TestCli::try_parse_from(["saya-memory", "apply", "--change", "file.json"]).is_err());
    let fixture = Fixture::new(None);
    assert_eq!(
        code(run(
            &MemoryAction::List {
                page: 0,
                page_size: 20
            },
            &fixture.config
        )),
        "invalid_args"
    );
    assert_eq!(
        code(run(
            &MemoryAction::History {
                page: 1,
                page_size: usize::MAX
            },
            &fixture.config
        )),
        "invalid_args"
    );
    assert_eq!(
        code(run(
            &MemoryAction::Show {
                entry_id: None,
                fact_id: None,
                cursor: Some("x".repeat(2048)),
                max_bytes: 16384,
                max_items: 16
            },
            &fixture.config
        )),
        "invalid_args"
    );
}
