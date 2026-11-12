#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;
use saya_core::{
    contract::{PermissionMode, RunId},
    goal::{GoalAuthorization, GoalStore},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn config(root: &Path, url: &str) -> std::path::PathBuf {
    config_with_stream(root, url, false)
}

fn config_with_stream(root: &Path, url: &str, stream: bool) -> std::path::PathBuf {
    let path = root.join("config.toml");
    fs::write(
        &path,
        format!(
            r#"schema_version = 1
[defaults]
profile = "fixture"
mode = "build"
permissions = "auto"
[sandbox]
backend = "host"
[profiles.fixture]
protocol = "openai-chat-completions"
base_url = "{url}"
model = "fixture"
auth = "none"
context_window_tokens = 128000
stream = {stream}
"#
        ),
    )
    .unwrap();
    path
}

fn text_command(root: &Path, home: &Path, config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_saya"));
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root)
        .arg("--config")
        .arg(config)
        .arg("--data-dir")
        .arg(root.join("data"));
    command
}

fn command(root: &Path, home: &Path, config: &Path) -> Command {
    let mut command = text_command(root, home, config);
    command.arg("--output").arg("json");
    command
}

fn json_output(command: &mut Command) -> Value {
    let output: Output = command.output().unwrap();
    assert!(
        output.status.success(),
        "stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn plan(step_ids: &[&str], criterion: &str) -> String {
    let steps = step_ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            json!({
                "id":id,
                "title":format!("Step {}",index + 1),
                "instruction":format!("Perform the bounded task for step {}",index + 1),
                "depends_on":if index == 0 { Vec::<String>::new() } else { vec![step_ids[index - 1].to_owned()] }
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&json!({
        "acceptance":[{"id":criterion,"text":"The result is verified","verification":{"kind":"manual"}}],
        "steps":steps,
    }))
    .unwrap()
}

fn completion(text: &str) -> String {
    json!({
        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}],
        "usage":{"prompt_tokens":10,"completion_tokens":4}
    })
    .to_string()
}

fn tool_call(id: &str, name: &str, arguments: Value) -> String {
    json!({
        "choices":[{
            "index":0,
            "finish_reason":"tool_calls",
            "message":{"role":"assistant","content":null,"tool_calls":[{
                "id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}
            }]}
        }],
        "usage":{"prompt_tokens":10,"completion_tokens":4}
    })
    .to_string()
}

#[test]
fn goal_cli_creates_plans_confirms_reports_and_controls_lifecycle() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let config = config(root.path(), "http://127.0.0.1:1/v1");
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Deliver a documented result",
        "--accept",
        "The result is verified",
    ]));
    let goal = &created["data"]["goal"];
    let goal_id = goal["id"].as_str().unwrap();
    assert_eq!(goal["status"], "draft");
    assert_eq!(goal["acceptance"][0]["id"], "criterion-1");

    let plan_path = root.path().join("plan.json");
    fs::write(
        &plan_path,
        plan(
            &[
                "018bcfe5-6800-7000-8000-000000000001",
                "018bcfe5-6800-7000-8000-000000000002",
            ],
            "criterion-1",
        ),
    )
    .unwrap();
    let planned = json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    assert_eq!(planned["data"]["goal"]["status"], "waiting");
    assert_eq!(
        planned["data"]["goal"]["wait_reason"]["kind"],
        "plan_confirmation"
    );
    assert_eq!(planned["data"]["confirmation_required"], true);

    let paused_before_confirmation =
        json_output(command(root.path(), home.path(), &config).args(["goal", "pause", goal_id]));
    assert_eq!(
        paused_before_confirmation["data"]["goal"]["status"],
        "paused"
    );
    let resumed_before_confirmation =
        json_output(command(root.path(), home.path(), &config).args(["goal", "resume", goal_id]));
    assert_eq!(
        resumed_before_confirmation["data"]["goal"]["status"],
        "waiting"
    );
    assert_eq!(
        resumed_before_confirmation["data"]["goal"]["wait_reason"]["kind"],
        "plan_confirmation"
    );

    let confirmed =
        json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", goal_id]));
    assert_eq!(confirmed["data"]["goal"]["status"], "ready");
    assert_eq!(confirmed["data"]["goal"]["steps"][0]["ordinal"], 1);

    let report = root.path().join("goal.md");
    let receipt = json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "report", goal_id, "--format", "markdown", "--file"])
            .arg(&report),
    );
    assert_eq!(receipt["data"]["goal_id"], goal_id);
    let markdown = fs::read_to_string(report).unwrap();
    assert!(markdown.contains("# Saya Goal report"));
    assert!(markdown.contains("Deliver a documented result"));
    assert!(markdown.contains("The result is verified"));

    let paused =
        json_output(command(root.path(), home.path(), &config).args(["goal", "pause", goal_id]));
    assert_eq!(paused["data"]["goal"]["status"], "paused");
    let resumed =
        json_output(command(root.path(), home.path(), &config).args(["goal", "resume", goal_id]));
    assert_eq!(resumed["data"]["goal"]["status"], "ready");
    let cancelled =
        json_output(command(root.path(), home.path(), &config).args(["goal", "cancel", goal_id]));
    assert_eq!(cancelled["data"]["goal"]["status"], "cancelled");
}

#[test]
fn automatic_goal_runs_each_step_once_then_waits_for_explicit_evidence() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let response = |text: &str| {
        json!({
            "choices":[{
                "index":0,
                "finish_reason":"stop",
                "message":{"role":"assistant","content":text}
            }],
            "usage":{"prompt_tokens":10,"completion_tokens":4}
        })
        .to_string()
    };
    let (url, requests) = fixture::server(vec![
        ("application/json", response("first step complete")),
        ("application/json", response("second step complete")),
        (
            "application/json",
            json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"satisfied\":true,\"reason\":\"Host evidence was reviewed\"}"}}]}).to_string(),
        ),
    ])
    .unwrap();
    let config = config(root.path(), &url);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Execute two bounded steps",
        "--accept",
        "The result is verified",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap().to_owned();
    let plan_path = root.path().join("auto-plan.json");
    fs::write(
        &plan_path,
        plan(
            &[
                "018bcfe5-6800-7000-8000-000000000011",
                "018bcfe5-6800-7000-8000-000000000012",
            ],
            "criterion-1",
        ),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", &goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", &goal_id]));
    let started = json_output(
        command(root.path(), home.path(), &config).args(["goal", "start", &goal_id, "--auto"]),
    );
    assert_eq!(started["data"]["runs"].as_array().unwrap().len(), 2);
    assert_eq!(started["data"]["goal"]["status"], "waiting");
    assert_eq!(
        started["data"]["goal"]["wait_reason"]["kind"],
        "verification"
    );
    assert_eq!(started["data"]["goal"]["usage"]["runs"], 2);
    assert_eq!(started["data"]["goal"]["steps"][0]["status"], "completed");
    assert_eq!(started["data"]["goal"]["steps"][1]["status"], "completed");
    for _ in 0..2 {
        requests.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    let verified = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "verify",
        &goal_id,
        "criterion-1",
        "--summary",
        "Reviewed both completed step results",
    ]));
    assert_eq!(verified["data"]["goal"]["status"], "completed");
    assert_eq!(
        verified["data"]["goal"]["evidence"][0]["criterion_id"],
        "criterion-1"
    );
    requests.recv_timeout(Duration::from_secs(1)).unwrap();
}

#[test]
fn model_generated_plan_requires_confirmation_before_execution() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let generated = plan(&["018bcfe5-6800-7000-8000-000000000021"], "criterion-1");
    let (url, requests) = fixture::server(vec![(
        "application/json",
        json!({
            "choices":[{
                "index":0,
                "finish_reason":"stop",
                "message":{"role":"assistant","content":generated}
            }]
        })
        .to_string(),
    )])
    .unwrap();
    let config = config(root.path(), &url);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Plan a generated implementation",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    let planned =
        json_output(command(root.path(), home.path(), &config).args(["goal", "plan", goal_id]));
    assert_eq!(planned["data"]["goal"]["status"], "waiting");
    assert_eq!(
        planned["data"]["goal"]["steps"].as_array().unwrap().len(),
        1
    );
    let request = requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_str())
            .any(|content| content.contains("Plan a generated implementation"))
    );
}

#[test]
fn finite_auto_goal_replans_a_known_failed_step_once() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let partial = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"PARTIAL_FAILURE\"},\"finish_reason\":null}]}\n\n";
    let recovered = concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"RECOVERED_STEP\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4}}\n\n",
        "data: [DONE]\n\n",
    );
    let (url, _requests) = fixture::server(vec![
        ("text/event-stream", partial.into()),
        ("text/event-stream", recovered.into()),
        (
            "application/json",
            json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"{\"satisfied\":true,\"reason\":\"Recovered evidence reviewed\"}"}}]}).to_string(),
        ),
    ])
    .unwrap();
    let config = config_with_stream(root.path(), &url, true);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Recover one failed step",
        "--accept",
        "The result is verified",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap().to_owned();
    let plan_path = root.path().join("replan.json");
    fs::write(
        &plan_path,
        plan(&["018bcfe5-6800-7000-8000-000000000031"], "criterion-1"),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", &goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", &goal_id]));

    // The first stream ends without a completed response, so it is a known
    // failure rather than a cancellation or unconfirmed side effect.
    let started = json_output(
        command(root.path(), home.path(), &config).args(["goal", "start", &goal_id, "--auto"]),
    );
    assert_eq!(started["data"]["goal"]["usage"]["replans"], 1);
    assert_eq!(started["data"]["goal"]["status"], "waiting");
}

#[test]
fn goal_run_budget_stops_automatic_scheduling_before_the_next_step() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let body = json!({
        "choices":[{
            "index":0,
            "finish_reason":"stop",
            "message":{"role":"assistant","content":"first budgeted step"}
        }]
    })
    .to_string();
    let (url, requests) = fixture::server(vec![("application/json", body)]).unwrap();
    let config = config(root.path(), &url);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Stop before the second step",
        "--accept",
        "The result is verified",
        "--max-runs",
        "1",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap().to_owned();
    let plan_path = root.path().join("budget-plan.json");
    fs::write(
        &plan_path,
        plan(
            &[
                "018bcfe5-6800-7000-8000-000000000041",
                "018bcfe5-6800-7000-8000-000000000042",
            ],
            "criterion-1",
        ),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", &goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", &goal_id]));
    let output = command(root.path(), home.path(), &config)
        .args(["goal", "start", &goal_id, "--auto"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["goal"]["status"], "limit_reached");
    assert_eq!(value["data"]["goal"]["usage"]["runs"], 1);
    assert_eq!(value["data"]["goal"]["steps"][0]["status"], "completed");
    assert_eq!(value["data"]["goal"]["steps"][1]["status"], "pending");
    requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(requests.try_recv().is_err());
}

#[tokio::test]
async fn goal_worker_recovers_an_expired_dispatch_as_unknown_without_replay() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let config = config(root.path(), "http://127.0.0.1:1/v1");
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Recover a crashed Goal worker",
        "--accept",
        "The result is verified",
    ]));
    let goal_id_text = created["data"]["goal"]["id"].as_str().unwrap().to_owned();
    let plan_path = root.path().join("crash-plan.json");
    fs::write(
        &plan_path,
        plan(&["018bcfe5-6800-7000-8000-000000000051"], "criterion-1"),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", &goal_id_text, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "confirm",
        &goal_id_text,
    ]));

    let store = GoalStore::open(&root.path().join("data"), vec![]).unwrap();
    let goal_id = goal_id_text.parse().unwrap();
    let lease = store
        .acquire_lease(goal_id, "crashed-worker".into(), 0, Some(1))
        .await
        .unwrap();
    let dispatch = store
        .begin_dispatch(&lease, RunId::new(), 0, "2026-11-12T02:00:00Z".into())
        .await
        .unwrap();
    store.mark_running(&dispatch, &lease, 0).await.unwrap();
    drop(lease);

    let recovered =
        json_output(command(root.path(), home.path(), &config).args(["goal", "worker", "--once"]));
    assert_eq!(
        recovered["data"]["recovered_unknown"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let status = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "status",
        &goal_id_text,
    ]));
    assert_eq!(status["data"]["goal"]["status"], "waiting");
    assert_eq!(
        status["data"]["goal"]["wait_reason"]["kind"],
        "unknown_side_effect"
    );
    assert_eq!(status["data"]["goal"]["usage"]["runs"], 1);
}

#[tokio::test]
async fn goal_worker_blocks_when_permissions_change_after_automatic_authorization() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let config = config(root.path(), "http://127.0.0.1:1/v1");
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Require stable automatic authorization",
        "--accept",
        "The result is verified",
    ]));
    let goal_id_text = created["data"]["goal"]["id"].as_str().unwrap().to_owned();
    let plan_path = root.path().join("permission-plan.json");
    fs::write(
        &plan_path,
        plan(&["018bcfe5-6800-7000-8000-000000000061"], "criterion-1"),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", &goal_id_text, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "confirm",
        &goal_id_text,
    ]));

    let store = GoalStore::open(&root.path().join("data"), vec![]).unwrap();
    let goal_id = goal_id_text.parse().unwrap();
    let mut goal = store.load(goal_id).await.unwrap();
    goal.authorization = Some(GoalAuthorization {
        auto: true,
        mode: goal.mode,
        permissions: PermissionMode::Auto,
        config_fingerprint: "authorized-configuration".into(),
        authorized_at: "2026-11-12T02:00:00Z".into(),
    });
    let revision = goal.revision;
    store.save(goal, revision).await.unwrap();
    let text = fs::read_to_string(&config).unwrap();
    fs::write(
        &config,
        text.replace(r#"permissions = "auto""#, r#"permissions = "manual""#),
    )
    .unwrap();

    json_output(command(root.path(), home.path(), &config).args(["goal", "worker", "--once"]));
    let status = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "status",
        &goal_id_text,
    ]));
    assert_eq!(status["data"]["goal"]["status"], "blocked");
    assert_eq!(
        status["data"]["goal"]["wait_reason"]["kind"],
        "authorization"
    );
    assert_eq!(status["data"]["goal"]["usage"]["runs"], 0);
}

#[test]
fn plan_goal_queries_do_not_create_storage_in_the_workspace() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let config = config(root.path(), "http://127.0.0.1:1/v1");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace(r#"mode = "build""#, r#"mode = "plan""#),
    )
    .unwrap();
    let output = command(root.path(), home.path(), &config)
        .args(["goal", "list"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!root.path().join("data/goals/goals.sqlite3").exists());
}

#[test]
fn planning_exhaustion_blocks_manual_step_dispatch() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let generated = plan(&["018bcfe5-6800-7000-8000-000000000071"], "criterion-1");
    let (url, requests) = fixture::server(vec![(
        "application/json",
        json!({
            "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":generated}}],
            "usage":{"prompt_tokens":10,"completion_tokens":4}
        })
        .to_string(),
    )])
    .unwrap();
    let config = config(root.path(), &url);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Enforce the planning budget",
        "--accept",
        "The result is verified",
        "--max-model-requests",
        "1",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    json_output(command(root.path(), home.path(), &config).args(["goal", "plan", goal_id]));
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", goal_id]));
    requests.recv_timeout(Duration::from_secs(1)).unwrap();
    let output = command(root.path(), home.path(), &config)
        .args(["goal", "continue", goal_id])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["goal"]["status"], "limit_reached");
    assert_eq!(value["data"]["goal"]["usage"]["model_requests"], 1);
    assert!(requests.try_recv().is_err());
}

#[test]
fn draft_and_cancelled_goals_cannot_be_verified_complete() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let response = json!({
        "choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"done"}}],
        "usage":{"prompt_tokens":10,"completion_tokens":4}
    })
    .to_string();
    let (url, requests) = fixture::server(vec![("application/json", response)]).unwrap();
    let config = config(root.path(), &url);

    let draft = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Draft remains incomplete",
        "--accept",
        "The result is verified",
    ]));
    let draft_id = draft["data"]["goal"]["id"].as_str().unwrap();
    let output = command(root.path(), home.path(), &config)
        .args([
            "goal",
            "verify",
            draft_id,
            "criterion-1",
            "--summary",
            "none",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));

    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Cancelled remains terminal",
        "--accept",
        "The result is verified",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    let plan_path = root.path().join("cancel-plan.json");
    fs::write(
        &plan_path,
        plan(&["018bcfe5-6800-7000-8000-000000000072"], "criterion-1"),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", goal_id]));
    json_output(
        command(root.path(), home.path(), &config).args(["goal", "start", goal_id, "--auto"]),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "cancel", goal_id]));
    let output = command(root.path(), home.path(), &config)
        .args([
            "goal",
            "verify",
            goal_id,
            "criterion-1",
            "--summary",
            "none",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let status =
        json_output(command(root.path(), home.path(), &config).args(["goal", "status", goal_id]));
    assert_eq!(status["data"]["goal"]["status"], "cancelled");
    requests.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(requests.try_recv().is_err());
}

#[test]
fn file_evidence_is_deferred_refreshed_and_host_rechecked() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    let ids = [
        "018bcfe5-6800-7000-8000-000000000081",
        "018bcfe5-6800-7000-8000-000000000082",
        "018bcfe5-6800-7000-8000-000000000083",
    ];
    let steps = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            json!({
                "id":id,
                "title":format!("Step {}", index + 1),
                "instruction":format!("Perform file step {}", index + 1),
                "depends_on":if index == 0 { Vec::<String>::new() } else { vec![ids[index - 1].to_owned()] }
            })
        })
        .collect::<Vec<_>>();
    let (url, requests) = fixture::server(vec![
        ("application/json", completion("prepare")),
        (
            "application/json",
            tool_call(
                "create-file",
                "write_file",
                json!({"path":"final.txt","content":"hello","expected_version":null}),
            ),
        ),
        ("application/json", completion("created")),
        (
            "application/json",
            tool_call(
                "change-file",
                "write_file",
                json!({"path":"final.txt","content":"changed","expected_version":digest}),
            ),
        ),
        ("application/json", completion("changed")),
    ])
    .unwrap();
    let config = config(root.path(), &url);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Refresh final file evidence",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    let plan_path = root.path().join("file-plan.json");
    fs::write(
        &plan_path,
        serde_json::to_vec_pretty(&json!({
            "acceptance":[{"id":"file","text":"Exact final content","verification":{"kind":"file_sha256","path":"final.txt","sha256":digest}}],
            "steps":steps,
        }))
        .unwrap(),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", goal_id]));
    let started = json_output(
        command(root.path(), home.path(), &config).args(["goal", "start", goal_id, "--auto"]),
    );
    assert_eq!(started["data"]["goal"]["status"], "waiting");
    assert_eq!(started["data"]["goal"]["evidence"], json!([]));
    assert!(
        started["data"]["goal"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step["status"] == "completed")
    );
    assert_eq!(
        fs::read_to_string(root.path().join("final.txt")).unwrap(),
        "changed"
    );
    for _ in 0..5 {
        requests.recv_timeout(Duration::from_secs(1)).unwrap();
    }

    let manual = command(root.path(), home.path(), &config)
        .args(["goal", "verify", goal_id, "file", "--summary", "pretend"])
        .output()
        .unwrap();
    assert_eq!(manual.status.code(), Some(2));
    let mismatch = command(root.path(), home.path(), &config)
        .args(["goal", "verify", goal_id, "file"])
        .output()
        .unwrap();
    assert_eq!(mismatch.status.code(), Some(2));
    assert!(requests.try_recv().is_err());

    fs::write(root.path().join("final.txt"), "hello").unwrap();
    let verified = json_output(
        command(root.path(), home.path(), &config).args(["goal", "verify", goal_id, "file"]),
    );
    assert_eq!(verified["data"]["goal"]["status"], "completed");
    assert_eq!(
        verified["data"]["goal"]["evidence"][0]["kind"],
        "file_digest"
    );
    assert_eq!(verified["data"]["goal"]["evidence"][0]["sha256"], digest);
    assert!(requests.try_recv().is_err());
}

#[test]
fn automatic_preauthorization_is_limited_to_the_first_run() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let command_line = "printf x >> preauth-count.txt";
    let arguments = json!({"argv":["/bin/sh","-c",command_line]});
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            tool_call("first-exec", "exec", arguments.clone()),
        ),
        ("application/json", completion("first done")),
        (
            "application/json",
            tool_call("second-exec", "exec", arguments),
        ),
    ])
    .unwrap();
    let config = config(root.path(), &url);
    fs::write(
        &config,
        format!(
            "{}\n[[permissions.rules]]\neffect = \"ask\"\ntool = \"exec\"\n",
            fs::read_to_string(&config).unwrap()
        ),
    )
    .unwrap();
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Do not reuse preauthorization",
        "--accept",
        "The result is verified",
        "--max-replans",
        "0",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    let plan_path = root.path().join("preauth-plan.json");
    fs::write(
        &plan_path,
        plan(
            &[
                "018bcfe5-6800-7000-8000-000000000091",
                "018bcfe5-6800-7000-8000-000000000092",
            ],
            "criterion-1",
        ),
    )
    .unwrap();
    let approvals = root.path().join("approval.json");
    fs::write(
        &approvals,
        serde_json::to_vec(&json!({
            "schema_version":1,
            "commands":[{"argv":["/bin/sh","-c",command_line],"cwd":"."}]
        }))
        .unwrap(),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", goal_id]));
    let output = command(root.path(), home.path(), &config)
        .args(["goal", "start", goal_id, "--auto", "--approval-file"])
        .arg(&approvals)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(
        fs::read_to_string(root.path().join("preauth-count.txt")).unwrap(),
        "x"
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["data"]["goal"]["usage"]["runs"], 2);
    assert_eq!(value["data"]["goal"]["status"], "limit_reached");
    for _ in 0..3 {
        requests.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    assert!(requests.try_recv().is_err());
}

#[test]
fn plan_goal_report_protects_current_workspace_and_uses_its_relative_root() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let goal_workspace = root.path().join("goal-workspace");
    let current_workspace = root.path().join("current-workspace");
    fs::create_dir(&goal_workspace).unwrap();
    fs::create_dir(&current_workspace).unwrap();
    let config = config(root.path(), "http://127.0.0.1:1/v1");
    fs::write(
        &config,
        fs::read_to_string(&config)
            .unwrap()
            .replace(r#"mode = "build""#, r#"mode = "plan""#),
    )
    .unwrap();
    let created = json_output(
        command(root.path(), home.path(), &config)
            .arg("--cwd")
            .arg(&goal_workspace)
            .args(["goal", "create", "Protect both workspaces"]),
    );
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    let output = command(root.path(), home.path(), &config)
        .arg("--cwd")
        .arg(&current_workspace)
        .args(["goal", "report", goal_id, "--file", "report.md"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!current_workspace.join("report.md").exists());
    assert!(!root.path().join("report.md").exists());
}

#[test]
fn goal_text_output_replaces_terminal_control_characters() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, _requests) = fixture::server(vec![(
        "application/json",
        completion("BEFORE\u{1b}]0;goal-title\u{7}AFTER"),
    )])
    .unwrap();
    let config = config(root.path(), &url);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Sanitize final text",
        "--accept",
        "The result is verified",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    let plan_path = root.path().join("terminal-plan.json");
    fs::write(
        &plan_path,
        plan(&["018bcfe5-6800-7000-8000-000000000093"], "criterion-1"),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", goal_id]));
    let output = text_command(root.path(), home.path(), &config)
        .args(["goal", "continue", goal_id])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("BEFORE"));
    assert!(text.contains("AFTER"));
    assert!(!text.contains('\u{1b}'));
    assert!(!text.contains('\u{7}'));
}

#[test]
fn cancelling_an_active_goal_cancels_the_in_flight_model_run() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let (url, requests, release) = fixture::server_gated(completion("late response")).unwrap();
    let config = config(root.path(), &url);
    let created = json_output(command(root.path(), home.path(), &config).args([
        "goal",
        "create",
        "Cancel an active Run",
        "--accept",
        "The result is verified",
    ]));
    let goal_id = created["data"]["goal"]["id"].as_str().unwrap();
    let plan_path = root.path().join("active-plan.json");
    fs::write(
        &plan_path,
        plan(&["018bcfe5-6800-7000-8000-000000000094"], "criterion-1"),
    )
    .unwrap();
    json_output(
        command(root.path(), home.path(), &config)
            .args(["goal", "plan", goal_id, "--plan-file"])
            .arg(&plan_path),
    );
    json_output(command(root.path(), home.path(), &config).args(["goal", "confirm", goal_id]));

    let mut running = command(root.path(), home.path(), &config)
        .args(["goal", "start", goal_id, "--auto"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let cancelled = command(root.path(), home.path(), &config)
        .args(["goal", "cancel", goal_id])
        .output()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline {
        if running.try_wait().unwrap().is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !exited {
        running.kill().unwrap();
    }
    let output = running.wait_with_output().unwrap();
    let _ = release.send(());
    assert!(cancelled.status.success());
    assert!(exited, "active Goal did not observe cancellation");
    assert_eq!(output.status.code(), Some(130));
    let status =
        json_output(command(root.path(), home.path(), &config).args(["goal", "status", goal_id]));
    assert_eq!(status["data"]["goal"]["status"], "cancelled");
}
