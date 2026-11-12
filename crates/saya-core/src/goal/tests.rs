use super::*;
use crate::{
    contract::{PermissionMode, RunStatus, Usage},
    metrics::RunMetrics,
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

fn record() -> GoalRecord {
    GoalRecord::new(GoalSeed {
        session_id: SessionId::new(),
        objective: "Deliver a verified API".into(),
        workspace: PathBuf::from("/workspace"),
        profile: "fixture".into(),
        mode: WorkMode::Build,
        permissions: PermissionMode::Auto,
        budget: GoalBudget::default(),
        now: "2026-11-12T00:00:00Z".into(),
    })
    .unwrap()
}

fn plan(first: StepId, second: StepId) -> GoalPlan {
    GoalPlan {
        acceptance: vec![
            AcceptanceCriterion {
                id: "api".into(),
                text: "API tests pass".into(),
                verification: VerificationSpec::Manual,
            },
            AcceptanceCriterion {
                id: "docs".into(),
                text: "Documentation exists".into(),
                verification: VerificationSpec::FileExists {
                    path: PathBuf::from("README.md"),
                },
            },
        ],
        steps: vec![
            PlannedStep {
                id: first,
                title: "Implement API".into(),
                instruction: "Implement and test the API".into(),
                depends_on: vec![],
            },
            PlannedStep {
                id: second,
                title: "Document API".into(),
                instruction: "Document the API".into(),
                depends_on: vec![first],
            },
        ],
    }
}

fn summary(run_id: RunId, now: &str) -> GoalRunSummary {
    GoalRunSummary {
        run_id,
        status: RunStatus::Completed,
        stop_reason: None,
        text: Some("step completed".into()),
        error_code: None,
        usage: Some(Usage {
            input_tokens: Some(100),
            output_tokens: Some(20),
            cached_input_tokens: None,
            estimated: false,
        }),
        artifacts: vec![],
        persistence_saved: true,
        finished_at: now.into(),
    }
}

fn metrics() -> RunMetrics {
    RunMetrics {
        execution_requests: 2,
        review_requests: 1,
        compaction_requests: 0,
        tool_calls: 3,
        tool_executions: 3,
        duration_ms: 2_500,
        execution_usage: None,
        review_usage: None,
        compaction_usage: None,
        execution_cost: None,
        review_cost: None,
        compaction_cost: None,
    }
}

fn failed_summary(run_id: RunId, now: &str, error_code: &str, saved: bool) -> GoalRunSummary {
    GoalRunSummary {
        run_id,
        status: RunStatus::Failed,
        stop_reason: Some("model_failure".into()),
        text: None,
        error_code: Some(error_code.into()),
        usage: None,
        artifacts: vec![],
        persistence_saved: saved,
        finished_at: now.into(),
    }
}

#[test]
fn plans_require_acyclic_dependencies_and_confirmation() {
    let mut goal = record();
    let first = StepId::new();
    let second = StepId::new();
    goal.apply_plan(plan(first, second), "2026-11-12T00:01:00Z".into())
        .unwrap();
    assert_eq!(goal.status, GoalStatus::Waiting);
    assert_eq!(goal.wait_reason, Some(GoalWaitReason::PlanConfirmation));
    goal.confirm_plan("2026-11-12T00:02:00Z".into()).unwrap();
    assert_eq!(goal.status, GoalStatus::Ready);
    assert_eq!(goal.next_pending_step().unwrap().id, first);

    let mut cyclic = record();
    let mut invalid = plan(first, second);
    invalid.steps[0].depends_on = vec![second];
    assert!(
        cyclic
            .apply_plan(invalid, "2026-11-12T00:01:00Z".into())
            .is_err()
    );
    let parsed = GoalPlan::parse(&format!(
        "```json\n{}\n```",
        serde_json::to_string(&plan(first, second)).unwrap()
    ))
    .unwrap();
    assert_eq!(parsed.steps.len(), 2);
    assert!(GoalPlan::parse(r#"{"acceptance":[],"steps":[],"unknown":true}"#).is_err());
}

#[test]
fn file_evidence_is_bounded_relative_and_never_follows_links() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(root.path().join("result.txt"), b"hello").unwrap();
    fs::write(outside.path().join("secret.txt"), b"secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();
    let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let evidence = verify_file_criterion(
        root.path(),
        &AcceptanceCriterion {
            id: "criterion-1".into(),
            text: "Result digest".into(),
            verification: VerificationSpec::FileSha256 {
                path: "result.txt".into(),
                sha256: digest.into(),
            },
        },
        "2026-11-12T00:00:00Z".into(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(evidence.kind, EvidenceKind::FileDigest);
    assert_eq!(evidence.sha256.as_deref(), Some(digest));
    assert!(
        verify_file_criterion(
            root.path(),
            &AcceptanceCriterion {
                id: "criterion-2".into(),
                text: "Outside file".into(),
                verification: VerificationSpec::FileExists {
                    path: "linked/secret.txt".into(),
                },
            },
            "2026-11-12T00:00:00Z".into(),
        )
        .is_err()
    );
    assert!(
        verify_file_criterion(
            root.path(),
            &AcceptanceCriterion {
                id: "criterion-3".into(),
                text: "Wrong digest".into(),
                verification: VerificationSpec::FileSha256 {
                    path: "result.txt".into(),
                    sha256: "0".repeat(64),
                },
            },
            "2026-11-12T00:00:00Z".into(),
        )
        .is_err()
    );
}

#[tokio::test]
async fn store_keeps_private_revisioned_goals_and_exclusive_leases() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("workspace")).unwrap();
    let store = GoalStore::open(&root.path().join("data"), vec![]).unwrap();
    let mut goal = record();
    goal.workspace = fs::canonicalize(root.path().join("workspace")).unwrap();
    let first = StepId::new();
    let second = StepId::new();
    goal.apply_plan(plan(first, second), "2026-11-12T00:01:00Z".into())
        .unwrap();
    goal.confirm_plan("2026-11-12T00:02:00Z".into()).unwrap();
    let mut goal = store.create(goal).await.unwrap();
    let loaded = store.load(goal.id).await.unwrap();
    assert_eq!(loaded, goal);
    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(store.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let owner = "worker-a".to_owned();
    store
        .acquire_lease(goal.id, owner.clone(), 1_000, Some(10_000))
        .await
        .unwrap();
    assert!(
        store
            .acquire_lease(goal.id, "worker-b".into(), 2_000, Some(10_000))
            .await
            .is_err()
    );

    goal.objective = "Updated objective".into();
    goal.updated_at = "2026-11-12T00:03:00Z".into();
    let stale = goal.revision;
    goal = store.save(goal, stale).await.unwrap();
    assert_eq!(goal.revision, stale + 1);
    let mut conflicting = goal.clone();
    conflicting.updated_at = "2026-11-12T00:04:00Z".into();
    assert!(store.save(conflicting, stale).await.is_err());
}

#[tokio::test]
async fn goal_worker_lease_excludes_another_goal_in_the_same_workspace() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("workspace")).unwrap();
    let store = GoalStore::open(&root.path().join("data"), vec![]).unwrap();
    let workspace = fs::canonicalize(root.path().join("workspace")).unwrap();
    let mut goals = Vec::new();
    for index in 0..2 {
        let mut goal = record();
        goal.workspace = workspace.clone();
        goal.objective = format!("Goal {index}");
        let first = StepId::new();
        goal.apply_plan(
            GoalPlan {
                acceptance: vec![AcceptanceCriterion {
                    id: "criterion-1".into(),
                    text: "Verified".into(),
                    verification: VerificationSpec::Manual,
                }],
                steps: vec![PlannedStep {
                    id: first,
                    title: "Step".into(),
                    instruction: "Do work".into(),
                    depends_on: vec![],
                }],
            },
            format!("2026-11-12T03:0{index}:00Z"),
        )
        .unwrap();
        goal.confirm_plan(format!("2026-11-12T03:1{index}:00Z"))
            .unwrap();
        goals.push(store.create(goal).await.unwrap());
    }
    let lease = store
        .acquire_lease(goals[0].id, "worker-a".into(), 1_000, Some(60_000))
        .await
        .unwrap();
    let error = store
        .acquire_lease(goals[1].id, "worker-b".into(), 1_000, Some(60_000))
        .await
        .unwrap_err();
    assert_eq!(error.0.code, "goal_workspace_busy");
    store.release_lease(&lease).await.unwrap();
    store
        .acquire_lease(goals[1].id, "worker-b".into(), 1_001, Some(60_000))
        .await
        .unwrap();
}

#[tokio::test]
async fn goal_store_rejects_configured_credentials_without_publishing() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("workspace")).unwrap();
    let store =
        GoalStore::open(&root.path().join("data"), vec!["goal-secret-token".into()]).unwrap();
    let mut goal = record();
    goal.workspace = fs::canonicalize(root.path().join("workspace")).unwrap();
    goal.objective = "Use goal-secret-token without persisting it".into();
    let error = store.create(goal).await.unwrap_err();
    assert_eq!(error.0.code, "storage_sensitive_data");
    assert!(store.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn dispatch_intent_usage_and_unknown_recovery_never_replay_a_run() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("workspace")).unwrap();
    let store = GoalStore::open(&root.path().join("data"), vec![]).unwrap();
    let mut goal = record();
    goal.workspace = fs::canonicalize(root.path().join("workspace")).unwrap();
    let first = StepId::new();
    let second = StepId::new();
    goal.apply_plan(plan(first, second), "2026-11-12T00:01:00Z".into())
        .unwrap();
    goal.confirm_plan("2026-11-12T00:02:00Z".into()).unwrap();
    let goal = store.create(goal).await.unwrap();
    let lease = store
        .acquire_lease(goal.id, "worker".into(), 1_000, Some(60_000))
        .await
        .unwrap();
    let run_id = RunId::new();
    let dispatch = store
        .begin_dispatch(&lease, run_id, 2_000, "2026-11-12T00:03:00Z".into())
        .await
        .unwrap();
    store.mark_running(&dispatch, &lease, 3_000).await.unwrap();
    assert!(
        store
            .recover_unknown("2026-11-12T00:04:00Z".into(), 4_000)
            .await
            .unwrap()
            .is_empty()
    );
    let recovered = store
        .recover_unknown("2026-11-12T00:04:00Z".into(), u64::MAX)
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].status, GoalStatus::Waiting);
    assert!(matches!(
        recovered[0].wait_reason,
        Some(GoalWaitReason::UnknownSideEffect { step_id }) if step_id == first
    ));
    assert!(
        store
            .begin_dispatch(&lease, RunId::new(), 4_000, "2026-11-12T00:05:00Z".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn successful_run_records_usage_and_goal_completion_needs_every_criterion() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("workspace")).unwrap();
    let store = GoalStore::open(&root.path().join("data"), vec![]).unwrap();
    let mut goal = record();
    goal.workspace = fs::canonicalize(root.path().join("workspace")).unwrap();
    let first = StepId::new();
    let second = StepId::new();
    goal.apply_plan(plan(first, second), "2026-11-12T00:01:00Z".into())
        .unwrap();
    goal.confirm_plan("2026-11-12T00:02:00Z".into()).unwrap();
    let mut goal = store.create(goal).await.unwrap();
    let lease = store
        .acquire_lease(goal.id, "worker".into(), 1_000, Some(60_000))
        .await
        .unwrap();
    for (index, step) in [first, second].into_iter().enumerate() {
        let dispatch = store
            .begin_dispatch(
                &lease,
                RunId::new(),
                u64::try_from(index).unwrap() * 10 + 2_000,
                format!("2026-11-12T00:0{}:00Z", index + 3),
            )
            .await
            .unwrap();
        assert_eq!(dispatch.step_id, step);
        store.mark_running(&dispatch, &lease, 3_000).await.unwrap();
        goal = store
            .finish_dispatch(
                &lease,
                &dispatch,
                summary(
                    dispatch.run_id,
                    &format!("2026-11-12T00:0{}:30Z", index + 3),
                ),
                &metrics(),
                format!("2026-11-12T00:0{}:31Z", index + 3),
            )
            .await
            .unwrap();
    }
    assert_eq!(goal.status, GoalStatus::Waiting);
    assert_eq!(goal.wait_reason, Some(GoalWaitReason::Verification));
    assert_eq!(goal.usage.runs, 2);
    assert_eq!(goal.usage.model_requests, 6);
    assert_eq!(goal.usage.tool_calls, 6);
    assert_eq!(goal.usage.input_tokens, 200);
    assert_eq!(goal.usage.output_tokens, 40);

    goal = store
        .record_evidence(
            goal.id,
            GoalEvidence {
                id: EvidenceId::new(),
                criterion_id: "api".into(),
                step_id: Some(first),
                run_id: None,
                kind: EvidenceKind::Manual,
                summary: "Acceptance test output was reviewed".into(),
                path: None,
                sha256: None,
                observed_at: "2026-11-12T00:06:00Z".into(),
            },
            "2026-11-12T00:06:01Z".into(),
        )
        .await
        .unwrap();
    assert_eq!(goal.status, GoalStatus::Waiting);
    goal = store
        .record_evidence(
            goal.id,
            GoalEvidence {
                id: EvidenceId::new(),
                criterion_id: "docs".into(),
                step_id: Some(second),
                run_id: None,
                kind: EvidenceKind::FileExists,
                summary: "README.md exists".into(),
                path: Some("README.md".into()),
                sha256: None,
                observed_at: "2026-11-12T00:07:00Z".into(),
            },
            "2026-11-12T00:07:01Z".into(),
        )
        .await
        .unwrap();
    assert_eq!(goal.status, GoalStatus::Completed);
}

#[tokio::test]
async fn failed_known_work_is_replanned_once_but_unknown_side_effects_wait() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("workspace")).unwrap();
    let store = GoalStore::open(&root.path().join("data"), vec![]).unwrap();
    for (index, (code, saved, expected_status)) in [
        ("model_failure", true, GoalStatus::Ready),
        ("tool_outcome_unknown", true, GoalStatus::Waiting),
        ("model_failure", false, GoalStatus::Waiting),
    ]
    .into_iter()
    .enumerate()
    {
        let mut goal = record();
        goal.workspace = fs::canonicalize(root.path().join("workspace")).unwrap();
        goal.budget.max_replans = 1;
        let first = StepId::new();
        goal.apply_plan(
            GoalPlan {
                acceptance: vec![AcceptanceCriterion {
                    id: "criterion-1".into(),
                    text: "The work is verified".into(),
                    verification: VerificationSpec::Manual,
                }],
                steps: vec![PlannedStep {
                    id: first,
                    title: "Attempt work".into(),
                    instruction: "Perform the work".into(),
                    depends_on: vec![],
                }],
            },
            format!("2026-11-12T01:0{index}:00Z"),
        )
        .unwrap();
        goal.confirm_plan(format!("2026-11-12T01:1{index}:00Z"))
            .unwrap();
        let goal = store.create(goal).await.unwrap();
        let lease = store
            .acquire_lease(goal.id, format!("worker-{index}"), 2_000, Some(60_000))
            .await
            .unwrap();
        let dispatch = store
            .begin_dispatch(
                &lease,
                RunId::new(),
                2_000,
                format!("2026-11-12T01:2{index}:00Z"),
            )
            .await
            .unwrap();
        store.mark_running(&dispatch, &lease, 2_001).await.unwrap();
        let failed = store
            .finish_dispatch(
                &lease,
                &dispatch,
                failed_summary(
                    dispatch.run_id,
                    &format!("2026-11-12T01:3{index}:00Z"),
                    code,
                    saved,
                ),
                &RunMetrics::default(),
                format!("2026-11-12T01:4{index}:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(failed.status, GoalStatus::Blocked);
        let replanned = store
            .replan_failed_step(failed.id, format!("2026-11-12T01:5{index}:00Z"))
            .await
            .unwrap_or_else(|error| panic!("replan index {index}: {error}"));
        assert_eq!(replanned.status, expected_status);
        if expected_status == GoalStatus::Ready {
            assert_eq!(replanned.usage.replans, 1);
            assert!(
                replanned.steps[0]
                    .instruction
                    .contains("Previous attempt failed")
            );
        } else {
            assert_eq!(replanned.usage.replans, 0);
            assert!(matches!(
                replanned.wait_reason,
                Some(GoalWaitReason::UnknownSideEffect { .. })
            ));
        }
        store.release_lease(&lease).await.unwrap();
    }
}
