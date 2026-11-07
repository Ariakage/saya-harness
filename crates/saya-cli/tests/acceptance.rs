//! Offline acceptance tasks. The scripted provider verifies runtime contracts,
//! not the quality of any real model.
#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;

use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::Duration,
};
use tempfile::TempDir;

struct Sandbox {
    workspace: TempDir,
    state: TempDir,
    config: PathBuf,
}
impl Sandbox {
    fn new(url: &str, protocol: &str, permissions: &str) -> Self {
        let workspace = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let config = state.path().join("config.toml");
        fs::write(&config,format!("schema_version = 1\n[defaults]\nprofile = \"test\"\npermissions = \"{permissions}\"\n[profiles.test]\nprotocol = \"{protocol}\"\nbase_url = \"{url}\"\nmodel = \"fixture\"\nauth = \"none\"\ncontext_window_tokens = 128000\nstream = false\n")).unwrap();
        Self {
            workspace,
            state,
            config,
        }
    }
    fn root(&self) -> PathBuf {
        fs::canonicalize(self.workspace.path()).unwrap()
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_saya"));
        command
            .env_clear()
            .env("HOME", self.state.path())
            .env("PATH", "/usr/bin:/bin")
            .env("XDG_DATA_HOME", self.state.path().join("data"))
            .env("XDG_STATE_HOME", self.state.path().join("state"))
            .env("XDG_CACHE_HOME", self.state.path().join("cache"))
            .current_dir(self.workspace.path())
            .arg("--config")
            .arg(&self.config)
            .stdin(Stdio::null());
        command
    }
    fn run(&self, mode: &str, prompt: &str, approval: Option<&Path>) -> Output {
        let mut command = self.command();
        command.args(["--mode", mode, "--output", "json", "run"]);
        if let Some(file) = approval {
            command.arg("--approval-file").arg(file);
        }
        command.arg(prompt).output().unwrap()
    }
    fn approval(&self, argv: &[String]) -> PathBuf {
        let path = self.state.path().join("approval.json");
        fs::write(
            &path,
            json!({"schema_version":1,"commands":[{"argv":argv,"cwd":self.root()}]}).to_string(),
        )
        .unwrap();
        path
    }
    fn ask_commands(&self) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&self.config)
            .unwrap();
        file.write_all(b"\n[[permissions.rules]]\neffect = \"ask\"\ntool = \"exec\"\n")
            .unwrap();
    }
}
fn text_response(text: &str) -> String {
    json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":text}}]}).to_string()
}
fn calls(calls: Vec<(&str, &str, Value)>) -> String {
    let calls:Vec<_>=calls.into_iter().map(|(id,name,args)|json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})).collect();
    json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":calls}}]}).to_string()
}
fn result(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "status {:?}: stderr={} stdout={}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["status"], "completed");
    result
}
fn rustc() -> String {
    let cargo = PathBuf::from(env!("CARGO"));
    let candidate = cargo.parent().unwrap().join("rustc");
    assert!(
        candidate.is_file(),
        "rustc missing next to {}",
        cargo.display()
    );
    candidate.to_string_lossy().into_owned()
}
fn compile_and_run() -> Vec<String> {
    vec![
        "/bin/sh".into(),
        "-c".into(),
        "\"$1\" --edition=2024 src/main.rs -o textstats && ./textstats sample.txt".into(),
        "sh".into(),
        rustc(),
    ]
}
fn exec_args(argv: &[String]) -> Value {
    json!({"argv":argv,"cwd":null,"timeout_seconds":null})
}
const FIXED: &str = r#"use std::{env, fs};
fn main() {
    let text = fs::read_to_string(env::args().nth(1).unwrap()).unwrap();
    println!("lines={} words={}", text.lines().count(), text.split_whitespace().count());
}
"#;
const BUGGY: &str = r#"use std::{env, fs};
fn main() {
    let text = fs::read_to_string(env::args().nth(1).unwrap()).unwrap();
    println!("lines={} words={}", text.lines().count(), text.trim().split(' ').count());
}
"#;

#[test]
fn plan_investigates_rust_project_without_changing_workspace() {
    let (url,requests)=fixture::server(vec![
        ("application/json",calls(vec![("read","read_file",json!({"path":"src/main.rs","start_line":1,"line_count":20}))])),
        ("application/json",text_response("Plan: inspect counting, implement whitespace-aware parsing, and verify sample cases.")),
    ]).unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
    fs::create_dir(sandbox.root().join("src")).unwrap();
    fs::write(
        sandbox.root().join("Cargo.toml"),
        "[package]\nname = \"example\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(sandbox.root().join("src/main.rs"), BUGGY).unwrap();
    let before_manifest = fs::read(sandbox.root().join("Cargo.toml")).unwrap();
    let before_source = fs::read(sandbox.root().join("src/main.rs")).unwrap();
    let output = sandbox.run("plan", "Read this Rust project and propose a plan", None);
    let answer = result(&output);
    assert!(answer["text"].as_str().unwrap().contains("Plan:"));
    assert_eq!(
        fs::read(sandbox.root().join("Cargo.toml")).unwrap(),
        before_manifest
    );
    assert_eq!(
        fs::read(sandbox.root().join("src/main.rs")).unwrap(),
        before_source
    );
    assert_eq!(fs::read_dir(sandbox.root()).unwrap().count(), 2);
    let _first = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let second = requests
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .to_string();
    assert!(second.contains("split(' ')"));
}

#[test]
fn build_creates_and_verifies_a_text_statistics_program() {
    let argv = compile_and_run();
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            calls(vec![(
                "write",
                "write_file",
                json!({"path":"src/main.rs","content":FIXED,"expected_version":null}),
            )]),
        ),
        (
            "application/json",
            calls(vec![("verify", "exec", exec_args(&argv))]),
        ),
        (
            "application/json",
            text_response("Implemented and verified the text statistics program."),
        ),
    ])
    .unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
    fs::create_dir(sandbox.root().join("src")).unwrap();
    fs::write(sandbox.root().join("sample.txt"), "alpha  beta\nnext row\n").unwrap();
    sandbox.ask_commands();
    let approval = sandbox.approval(&argv);
    let output = sandbox.run(
        "build",
        "Implement and verify a text statistics program",
        Some(&approval),
    );
    let answer = result(&output);
    assert_eq!(answer["artifacts"][0]["operation"], "created");
    assert_eq!(
        fs::read_to_string(sandbox.root().join("src/main.rs")).unwrap(),
        FIXED
    );
    let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let final_request = requests
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .to_string();
    assert!(final_request.contains("lines=2 words=4"), "{final_request}");
    let verify = Command::new(sandbox.root().join("textstats"))
        .arg(sandbox.root().join("sample.txt"))
        .output()
        .unwrap();
    assert!(verify.status.success());
    assert_eq!(String::from_utf8_lossy(&verify.stdout), "lines=2 words=4\n");
}

#[test]
fn code_reproduces_and_fixes_the_statistics_defect() {
    let argv = compile_and_run();
    let for_server = argv.clone();
    let (url, requests) = fixture::server_dynamic(4, move |index, request| match index {
        0 => (
            "application/json",
            calls(vec![(
                "read",
                "read_file",
                json!({"path":"src/main.rs","start_line":1,"line_count":30}),
            )]),
        ),
        1 => {
            let messages = request["messages"].as_array().unwrap();
            let tool = messages
                .iter()
                .find(|message| message["role"] == "tool" && message["tool_call_id"] == "read")
                .unwrap();
            let data: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
            let version = data["data"]["version"].as_str().unwrap();
            (
                "application/json",
                calls(vec![(
                    "fix",
                    "write_file",
                    json!({"path":"src/main.rs","content":FIXED,"expected_version":version}),
                )]),
            )
        }
        2 => (
            "application/json",
            calls(vec![("verify", "exec", exec_args(&for_server))]),
        ),
        _ => (
            "application/json",
            text_response("Fixed whitespace counting and verified the regression."),
        ),
    })
    .unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
    fs::create_dir(sandbox.root().join("src")).unwrap();
    fs::write(sandbox.root().join("src/main.rs"), BUGGY).unwrap();
    fs::write(sandbox.root().join("sample.txt"), "alpha    beta\n").unwrap();
    let compile = Command::new(rustc())
        .current_dir(sandbox.root())
        .args(["--edition=2024", "src/main.rs", "-o", "buggy"])
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let before = Command::new(sandbox.root().join("buggy"))
        .arg(sandbox.root().join("sample.txt"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&before.stdout), "lines=1 words=5\n");
    sandbox.ask_commands();
    let approval = sandbox.approval(&argv);
    let output = sandbox.run(
        "code",
        "Fix the whitespace counting defect and verify it",
        Some(&approval),
    );
    let answer = result(&output);
    assert_eq!(answer["artifacts"][0]["operation"], "modified");
    let mut captured = Vec::new();
    for _ in 0..4 {
        captured.push(requests.recv_timeout(Duration::from_secs(3)).unwrap());
    }
    assert!(captured[1].to_string().contains("split(' ')"));
    assert!(captured[3].to_string().contains("lines=1 words=2"));
    let after = Command::new(sandbox.root().join("textstats"))
        .arg(sandbox.root().join("sample.txt"))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&after.stdout), "lines=1 words=2\n");
}

#[test]
fn work_summarizes_three_sources_into_a_report() {
    let (url,requests)=fixture::server(vec![
        ("application/json",calls(vec![
            ("a","read_file",json!({"path":"a.txt","start_line":1,"line_count":10})),
            ("b","read_file",json!({"path":"b.txt","start_line":1,"line_count":10})),
            ("c","read_file",json!({"path":"c.txt","start_line":1,"line_count":10})),
        ])),
        ("application/json",calls(vec![("report","write_file",json!({"path":"report.txt","content":"A: apples\nB: berries\nC: citrus\n","expected_version":null}))])),
        ("application/json",text_response("Summarized all three source files in report.txt.")),
    ]).unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
    for (name, text) in [
        ("a.txt", "apples"),
        ("b.txt", "berries"),
        ("c.txt", "citrus"),
    ] {
        fs::write(sandbox.root().join(name), text).unwrap();
    }
    let output = sandbox.run(
        "work",
        "Summarize the three source files into a report",
        None,
    );
    let answer = result(&output);
    assert_eq!(answer["artifacts"][0]["operation"], "created");
    assert_eq!(
        fs::read_to_string(sandbox.root().join("report.txt")).unwrap(),
        "A: apples\nB: berries\nC: citrus\n"
    );
    let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let request = requests
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .to_string();
    for text in ["apples", "berries", "citrus"] {
        assert!(request.contains(text));
    }
}

fn provider_response(protocol: &str, tool: bool) -> String {
    let args = json!({"path":"input.txt","start_line":1,"line_count":10});
    match protocol {
        "openai-chat-completions" => {
            if tool {
                calls(vec![("read", "read_file", args)])
            } else {
                text_response("done")
            }
        }
        "openai-responses" => {
            let item = if tool {
                json!({"type":"function_call","id":"item-read","status":"completed","call_id":"read","name":"read_file","arguments":args.to_string()})
            } else {
                json!({"type":"message","id":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"done","annotations":[]}]})
            };
            json!({"status":"completed","output":[item]}).to_string()
        }
        "anthropic-messages" => {
            let block = if tool {
                json!({"type":"tool_use","id":"read","name":"read_file","input":args})
            } else {
                json!({"type":"text","text":"done"})
            };
            json!({"role":"assistant","stop_reason":if tool {"tool_use"} else {"end_turn"},"content":[block]}).to_string()
        }
        _ => unreachable!(),
    }
}
fn provider_tool_result(protocol: &str, request: &Value) -> Value {
    let raw = match protocol {
        "openai-chat-completions" => request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool" && m["tool_call_id"] == "read")
            .unwrap()["content"]
            .as_str()
            .unwrap(),
        "openai-responses" => request["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["type"] == "function_call_output" && m["call_id"] == "read")
            .unwrap()["output"]
            .as_str()
            .unwrap(),
        "anthropic-messages" => request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["content"].as_array())
            .flatten()
            .find(|m| m["type"] == "tool_result" && m["tool_use_id"] == "read")
            .unwrap()["content"]
            .as_str()
            .unwrap(),
        _ => unreachable!(),
    };
    serde_json::from_str(raw).unwrap()
}

#[test]
fn all_three_protocols_drive_cli_direct_and_tool_round_trips() {
    for protocol in [
        "openai-chat-completions",
        "openai-responses",
        "anthropic-messages",
    ] {
        let (url, requests) = fixture::server(vec![
            ("application/json", provider_response(protocol, false)),
            ("application/json", provider_response(protocol, true)),
            ("application/json", provider_response(protocol, false)),
        ])
        .unwrap();
        let sandbox = Sandbox::new(&url, protocol, "auto");
        fs::write(sandbox.root().join("input.txt"), "protocol fixture").unwrap();
        let direct = sandbox.run("work", "Answer directly", None);
        assert_eq!(result(&direct)["text"], "done");
        let output = sandbox
            .command()
            .args(["--output", "jsonl", "run", "Read input"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{protocol}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events: Vec<Value> = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events.last().unwrap()["type"], "run.finished");
        assert_eq!(events.last().unwrap()["data"]["status"], "completed");
        assert!(events.iter().any(|event|event["type"]=="tool.finished" && event["data"]["call_id"]=="read"));
        let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
        let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
        let final_request = requests.recv_timeout(Duration::from_secs(3)).unwrap();
        let tool = provider_tool_result(protocol, &final_request);
        assert_eq!(tool["ok"], true);
        assert!(tool.to_string().contains("protocol fixture"));
    }
}

#[test]
fn all_work_and_permission_modes_enforce_write_boundaries_in_cli() {
    for mode in ["plan", "build", "code", "work"] {
        for permission in ["auto", "manual", "bypass"] {
            let (url, requests) = fixture::server(vec![
                (
                    "application/json",
                    calls(vec![(
                        "write",
                        "write_file",
                        json!({"path":"boundary.txt","content":"created","expected_version":null}),
                    )]),
                ),
                ("application/json", text_response("done")),
            ])
            .unwrap();
            let sandbox = Sandbox::new(&url, "openai-chat-completions", permission);
            let output = sandbox.run(mode, "Evaluate the boundary", None);
            let value: Value = serde_json::from_slice(&output.stdout).unwrap();
            let target = sandbox.root().join("boundary.txt");
            if mode == "plan" {
                assert!(output.status.success(), "{mode}/{permission}: {value}");
                assert!(!target.exists(), "{mode}/{permission}");
                let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
                let returned = requests
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap()
                    .to_string();
                assert!(
                    returned.contains("mode_denied") || returned.contains("permission_denied"),
                    "{mode}/{permission}: {returned}"
                );
            } else if permission == "manual" {
                assert_eq!(
                    output.status.code(),
                    Some(3),
                    "{mode}/{permission}: {value}"
                );
                assert_eq!(value["status"], "needs_confirmation");
                assert!(!target.exists());
            } else {
                assert!(output.status.success(), "{mode}/{permission}: {value}");
                assert_eq!(fs::read_to_string(target).unwrap(), "created");
            }
        }
    }
}

#[test]
fn auto_review_invalid_response_falls_back_without_executing_command() {
    let argv = vec!["/usr/bin/touch".to_owned(), "not-created".to_owned()];
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            calls(vec![("command", "exec", exec_args(&argv))]),
        ),
        (
            "application/json",
            text_response("not a valid review decision"),
        ),
    ])
    .unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
    let output = sandbox.run("work", "Run the command", None);
    assert_eq!(output.status.code(), Some(3));
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "needs_confirmation");
    assert!(!sandbox.root().join("not-created").exists());
    let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let review = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(review.get("tools").is_none() || review["tools"].as_array().is_some_and(Vec::is_empty));
}

#[test]
fn bypass_accesses_outside_root_but_explicit_deny_still_wins() {
    for denied in [false, true] {
        let outside = TempDir::new().unwrap();
        let target = outside.path().join("outside.txt");
        let (url, requests) = fixture::server(vec![
            (
                "application/json",
                calls(vec![(
                    "write",
                    "write_file",
                    json!({"path":target,"content":"outside","expected_version":null}),
                )]),
            ),
            ("application/json", text_response("done")),
        ])
        .unwrap();
        let sandbox = Sandbox::new(&url, "openai-chat-completions", "bypass");
        if denied {
            use std::io::Write;
            let mut config = fs::OpenOptions::new()
                .append(true)
                .open(&sandbox.config)
                .unwrap();
            config
                .write_all(b"\n[[permissions.rules]]\neffect = \"deny\"\ntool = \"write_file\"\n")
                .unwrap();
        }
        let output = sandbox.run("work", "Write outside the workspace", None);
        result(&output);
        assert_eq!(target.exists(), !denied);
        let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
        let returned = requests
            .recv_timeout(Duration::from_secs(3))
            .unwrap()
            .to_string();
        if denied {
            assert!(returned.contains("permission_denied"), "{returned}");
        }
    }
}

#[test]
fn resumed_session_uses_current_permissions_and_never_reuses_bypass() {
    let argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        "printf x >> marker".to_owned(),
    ];
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            calls(vec![("first-command", "exec", exec_args(&argv))]),
        ),
        ("application/json", text_response("first done")),
        (
            "application/json",
            calls(vec![("second-command", "exec", exec_args(&argv))]),
        ),
        ("application/json", text_response("invalid review")),
    ])
    .unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "bypass");
    let first = sandbox.run("work", "First command", None);
    let first = result(&first);
    assert_eq!(
        fs::read_to_string(sandbox.root().join("marker")).unwrap(),
        "x"
    );
    let session = first["session_id"].as_str().unwrap();
    let second = sandbox
        .command()
        .args([
            "--permissions",
            "auto",
            "--output",
            "json",
            "run",
            "--session",
            session,
            "Repeat the command",
        ])
        .output()
        .unwrap();
    assert_eq!(
        second.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&second.stdout)
    );
    let result: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(result["status"], "needs_confirmation");
    assert_eq!(
        fs::read_to_string(sandbox.root().join("marker")).unwrap(),
        "x"
    );
    for _ in 0..4 {
        requests.recv_timeout(Duration::from_secs(3)).unwrap();
    }
}

#[test]
fn model_tool_and_budget_failures_do_not_report_completion() {
    {
        let (url, _) = fixture::server(vec![(
            "application/json",
            json!({"choices":[]}).to_string(),
        )])
        .unwrap();
        let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
        let output = sandbox.run("work", "Invalid model response", None);
        assert_eq!(output.status.code(), Some(1));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "failed");
    }
    {
        let responses = (0..3)
            .map(|index| {
                let id = format!("missing-{index}");
                (
                    "application/json",
                    calls(vec![(
                        &id,
                        "read_file",
                        json!({"path":"missing.txt","start_line":1,"line_count":1}),
                    )]),
                )
            })
            .collect();
        let (url, requests) = fixture::server(responses).unwrap();
        let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
        let output = sandbox.run("work", "Read a missing file", None);
        assert_eq!(output.status.code(), Some(1));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "failed");
        assert_eq!(value["stop_reason"], "repeated_tool_failure");
        for _ in 0..3 {
            requests.recv_timeout(Duration::from_secs(3)).unwrap();
        }
    }
    {
        let (url, requests) = fixture::server(vec![(
            "application/json",
            calls(vec![(
                "read",
                "read_file",
                json!({"path":"input.txt","start_line":1,"line_count":1}),
            )]),
        )])
        .unwrap();
        let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
        fs::write(sandbox.root().join("input.txt"), "data").unwrap();
        use std::io::Write;
        let mut config = fs::OpenOptions::new()
            .append(true)
            .open(&sandbox.config)
            .unwrap();
        config
            .write_all(b"\n[limits]\nmax_model_requests = 1\n")
            .unwrap();
        let output = sandbox.run("work", "Read with a limited budget", None);
        assert_eq!(output.status.code(), Some(5));
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["status"], "limit_reached");
        requests.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(requests.try_recv().is_err());
    }
}

#[test]
fn plan_rejects_even_exactly_preauthorized_command() {
    let argv = vec!["/usr/bin/touch".to_owned(), "forbidden".to_owned()];
    let (url, requests) = fixture::server(vec![
        (
            "application/json",
            calls(vec![("exec", "exec", exec_args(&argv))]),
        ),
        ("application/json", text_response("Plan only.")),
    ])
    .unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "manual");
    let approval = sandbox.approval(&argv);
    let output = sandbox.run("plan", "Investigate safely", Some(&approval));
    result(&output);
    assert!(!sandbox.root().join("forbidden").exists());
    let _ = requests.recv_timeout(Duration::from_secs(3)).unwrap();
    let next = requests
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .to_string();
    assert!(
        next.contains("mode_denied") || next.contains("permission_denied"),
        "{next}"
    );
}

#[test]
fn default_new_session_uses_work_and_auto() {
    let (url, _) = fixture::server(vec![("application/json", text_response("done"))]).unwrap();
    let sandbox = Sandbox::new(&url, "openai-chat-completions", "auto");
    let config = fs::read_to_string(&sandbox.config)
        .unwrap()
        .replace("permissions = \"auto\"\n", "");
    fs::write(&sandbox.config, config).unwrap();
    let output = sandbox
        .command()
        .args(["--output", "jsonl", "run", "task"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let events: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events[0]["type"], "session.created");
    assert_eq!(events[0]["data"]["work_mode"], "work");
    assert_eq!(events[0]["data"]["permissions"], "auto");
}
