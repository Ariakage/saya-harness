use soyo_app::goal::run_step_verifier;
use soyo_core::{
    boundary::Cancellation,
    config::{Environment, Overrides},
    contract::{PermissionMode, RunId, SessionId, WorkMode},
    goal::{
        AcceptanceCriterion, GoalBudget, GoalPlan, GoalRecord, GoalSeed, PlannedStep, StepId,
        StepVerificationStatus, StepVerifier, VerificationSpec,
    },
};
use std::{
    ffi::OsString,
    fs,
    future::{Future, pending},
    path::Path,
    pin::Pin,
    sync::Arc,
};

struct NoCancel;
impl Cancellation for NoCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(pending())
    }
}

fn load_config(root: &Path, extra_toml: &str) -> soyo_core::config::Config {
    let config_path = root.join("config.toml");
    fs::write(
        &config_path,
        format!(
            r#"
schema_version = 1
[defaults]
profile = "chat"
mode = "work"
permissions = "auto"
output = "text"
[sandbox]
backend = "host"
[profiles.chat]
protocol = "openai-chat-completions"
base_url = "https://example.invalid/v1"
model = "example"
auth = "bearer"
api_key_env = "EXAMPLE_KEY"
context_window_tokens = 100000
{extra_toml}
"#
        ),
    )
    .unwrap();
    soyo_core::config::load(
        Overrides {
            config: Some(config_path),
            data_dir: Some(root.join("data")),
            ..Default::default()
        },
        Environment::from_pairs([
            (OsString::from("HOME"), root.as_os_str().to_owned()),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (
                OsString::from("EXAMPLE_KEY"),
                OsString::from("verifier-secret-value-99"),
            ),
        ]),
        root,
    )
    .unwrap()
}

fn test_exe() -> String {
    if Path::new("/bin/test").exists() {
        "/bin/test".into()
    } else {
        "/usr/bin/test".into()
    }
}

fn touch_exe() -> String {
    if Path::new("/usr/bin/touch").exists() {
        "/usr/bin/touch".into()
    } else {
        "/bin/touch".into()
    }
}

fn build_goal(
    workspace: &Path,
    step_id: StepId,
    mode: WorkMode,
    verifier: StepVerifier,
) -> GoalRecord {
    let mut goal = GoalRecord::new(GoalSeed {
        session_id: SessionId::new(),
        objective: "Verify step verifier execution".into(),
        workspace: workspace.to_path_buf(),
        profile: "chat".into(),
        mode,
        permissions: PermissionMode::Auto,
        budget: GoalBudget::default(),
        now: "2026-11-19T00:00:00Z".into(),
    })
    .unwrap();
    goal.apply_plan(
        GoalPlan {
            acceptance: vec![AcceptanceCriterion {
                id: "criterion-1".into(),
                text: "Verified".into(),
                verification: VerificationSpec::Manual,
            }],
            steps: vec![PlannedStep {
                id: step_id,
                title: "Step".into(),
                instruction: "Run step".into(),
                depends_on: vec![],
                verifier: Some(verifier),
            }],
        },
        "2026-11-19T00:00:01Z".into(),
    )
    .unwrap();
    goal
}

#[tokio::test]
async fn run_step_verifier_enforces_confirmation_plan_deny_redaction_and_outcomes() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("workspace");
    fs::create_dir_all(&ws).unwrap();
    let ws = fs::canonicalize(&ws).unwrap();
    let step_id = StepId::new();
    let cancel: Arc<dyn Cancellation> = Arc::new(NoCancel);

    // 1. Unconfirmed verifier fails closed without executing.
    let cfg = load_config(temp.path(), "");
    let marker = ws.join("should-not-exist.txt");
    let unconfirmed = build_goal(
        &ws,
        step_id,
        WorkMode::Work,
        StepVerifier {
            argv: vec![touch_exe(), "should-not-exist.txt".into()],
            cwd: None,
            timeout_seconds: 15,
            max_output_bytes: 1024,
        },
    );
    let res = run_step_verifier(
        &cfg,
        &unconfirmed,
        &unconfirmed.steps[0],
        1,
        RunId::new(),
        PermissionMode::Auto,
        None,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(res.status, StepVerificationStatus::Denied);
    assert_eq!(res.error_code.as_deref(), Some("goal_verifier_unconfirmed"));
    assert!(!marker.exists());

    // 2. PLAN mode skips execution even when confirmed.
    let mut plan_goal = unconfirmed.clone();
    plan_goal.mode = WorkMode::Plan;
    plan_goal.steps[0].mode = WorkMode::Plan;
    plan_goal
        .confirm_plan("2026-11-19T00:00:02Z".into())
        .unwrap();
    let res = run_step_verifier(
        &cfg,
        &plan_goal,
        &plan_goal.steps[0],
        1,
        RunId::new(),
        PermissionMode::Auto,
        None,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(res.status, StepVerificationStatus::SkippedPlan);
    assert_eq!(res.error_code.as_deref(), Some("mode_denied"));
    assert!(!marker.exists());

    // 3. Explicit deny rule blocks even in Bypass mode.
    let deny_cfg = load_config(
        temp.path(),
        r#"
[[permissions.rules]]
effect = "deny"
tool = "exec"
executable = "/bin/echo"
"#,
    );
    let mut deny_goal = build_goal(
        &ws,
        step_id,
        WorkMode::Work,
        StepVerifier {
            argv: vec!["/bin/echo".into(), "blocked".into()],
            cwd: None,
            timeout_seconds: 15,
            max_output_bytes: 1024,
        },
    );
    deny_goal
        .confirm_plan("2026-11-19T00:00:02Z".into())
        .unwrap();
    let res = run_step_verifier(
        &deny_cfg,
        &deny_goal,
        &deny_goal.steps[0],
        1,
        RunId::new(),
        PermissionMode::Bypass,
        None,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(res.status, StepVerificationStatus::Denied);
    assert_eq!(res.error_code.as_deref(), Some("permission_denied"));

    // 4. Pass and Fail on direct structured binary (`/bin/test` / `/usr/bin/test`).
    let mut test_goal = build_goal(
        &ws,
        step_id,
        WorkMode::Work,
        StepVerifier {
            argv: vec![test_exe(), "-f".into(), "ready.txt".into()],
            cwd: None,
            timeout_seconds: 15,
            max_output_bytes: 1024,
        },
    );
    test_goal
        .confirm_plan("2026-11-19T00:00:02Z".into())
        .unwrap();
    let failed = run_step_verifier(
        &cfg,
        &test_goal,
        &test_goal.steps[0],
        1,
        RunId::new(),
        PermissionMode::Auto,
        None,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(failed.status, StepVerificationStatus::Failed, "{failed:?}");
    assert_eq!(failed.exit_code, Some(1), "{failed:?}");

    fs::write(ws.join("ready.txt"), b"done\n").unwrap();
    let passed = run_step_verifier(
        &cfg,
        &test_goal,
        &test_goal.steps[0],
        2,
        RunId::new(),
        PermissionMode::Auto,
        None,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(passed.status, StepVerificationStatus::Passed);
    assert_eq!(passed.exit_code, Some(0));

    // 5. Secret printed by child output is redacted before returning.
    let mut secret_goal = build_goal(
        &ws,
        step_id,
        WorkMode::Work,
        StepVerifier {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf '%s%s' 'verifier-secret-' 'value-99'".into(),
            ],
            cwd: None,
            timeout_seconds: 15,
            max_output_bytes: 1024,
        },
    );
    secret_goal
        .confirm_plan("2026-11-19T00:00:02Z".into())
        .unwrap();
    let redacted = run_step_verifier(
        &cfg,
        &secret_goal,
        &secret_goal.steps[0],
        1,
        RunId::new(),
        PermissionMode::Auto,
        None,
        cancel.clone(),
    )
    .await
    .unwrap();
    assert_eq!(redacted.status, StepVerificationStatus::Passed);
    assert!(!redacted.stdout_excerpt.contains("verifier-secret-value-99"));
    assert!(redacted.stdout_excerpt.contains("[REDACTED]"));

    // 6. Timeout produces TimedOut.
    let mut slow_goal = build_goal(
        &ws,
        step_id,
        WorkMode::Work,
        StepVerifier {
            argv: vec!["/bin/sleep".into(), "3".into()],
            cwd: None,
            timeout_seconds: 1,
            max_output_bytes: 1024,
        },
    );
    slow_goal
        .confirm_plan("2026-11-19T00:00:02Z".into())
        .unwrap();
    let timed_out = run_step_verifier(
        &cfg,
        &slow_goal,
        &slow_goal.steps[0],
        1,
        RunId::new(),
        PermissionMode::Auto,
        None,
        cancel,
    )
    .await
    .unwrap();
    assert_eq!(timed_out.status, StepVerificationStatus::TimedOut);
}
