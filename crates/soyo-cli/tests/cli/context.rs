use crate::common::*;
use crate::fixture;
use serde_json::json;
use std::{fs, io::Write, process::Stdio, time::Duration};
use tempfile::TempDir;

#[test]
fn dynamic_system_context_and_explicit_personal_data_reach_the_model() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(temp.path().join("AGENTS.md"), "PROJECT_CONVENTION").unwrap();
    let preferences = temp.path().join("prefs.txt");
    let memory = temp.path().join("memory.txt");
    fs::write(&preferences, "PREF_CONTENT").unwrap();
    fs::write(&memory, "MEMORY_CONTENT").unwrap();
    let (url, requests) = fixture::server(vec![(
        "application/json",
        response(json!({"role":"assistant","content":"ok"}), "stop"),
    )])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let result = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .arg("--preferences-file")
        .arg(preferences)
        .arg("--memory-file")
        .arg(memory)
        .args(["run", "hello"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let messages = request["messages"].as_array().unwrap();
    let system = messages[0]["content"].as_str().unwrap();
    assert!(system.contains("# Dynamic request context"));
    let facts = system
        .split_once("# Dynamic request context\n")
        .unwrap()
        .1
        .lines()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .unwrap();
    assert_eq!(facts["host"]["os"], std::env::consts::OS);
    assert_eq!(facts["host"]["architecture"], std::env::consts::ARCH);
    assert!(facts.get("captured_at").is_none());
    assert_eq!(facts["project_context_available"], true);
    assert_eq!(facts["user_preferences_supplied"], true);
    assert_eq!(facts["long_term_memory_supplied"], true);
    let runtime = messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .filter_map(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .find_map(|value| value.get("soyo_runtime_context_v1").cloned())
        .unwrap();
    assert!(
        runtime["captured_at"]["utc"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    assert!(runtime["captured_at"]["utc_offset_seconds"].is_number());
    let execution = messages
        .iter()
        .filter_map(|message| message["content"].as_str())
        .flat_map(|content| content.lines())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|value| value.get("soyo_execution_environment_v1").cloned())
        .unwrap();
    assert_eq!(execution["identity"]["execution_os"], std::env::consts::OS);
    assert_eq!(execution["capabilities"]["filesystem"], "hard");
    assert!(!system.contains("PROJECT_CONVENTION"));
    assert!(!system.contains("PREF_CONTENT"));
    assert!(!system.contains("MEMORY_CONTENT"));
    let serialized = request.to_string();
    assert!(serialized.contains("PROJECT_CONVENTION"));
    assert!(serialized.contains("PREF_CONTENT"));
    assert!(serialized.contains("MEMORY_CONTENT"));
}

#[test]
fn oversized_personal_context_is_rejected_before_model_call() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let memory = temp.path().join("memory.txt");
    fs::write(&memory, vec![b'x'; 65_537]).unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto");
    let result = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .arg("--memory-file")
        .arg(memory)
        .args(["run", "hello"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("exceeds 64 KiB"));
}

#[test]
fn personal_context_containing_configured_credential_is_rejected() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let memory = temp.path().join("memory.txt");
    fs::write(&memory, "fake-test-key-in-memory").unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto");
    let content = fs::read_to_string(&cfg).unwrap().replace(
        "auth = \"none\"",
        "auth = \"bearer\"\napi_key_env = \"TEST_KEY\"",
    );
    fs::write(&cfg, content).unwrap();
    let result = command(temp.path(), home.path())
        .env("TEST_KEY", "fake-test-key-in-memory")
        .arg("--config")
        .arg(cfg)
        .arg("--memory-file")
        .arg(memory)
        .args(["run", "hello"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("contains a configured secret"));
    assert!(!stderr.contains("fake-test-key-in-memory"));
}

#[test]
fn encoded_credentials_in_personal_files_are_rejected_before_network_requests() {
    let secret = "fixture\"secret\\tail";
    let canonical = json!({"value": secret}).to_string();
    let unicode = format!(
        r#"{{"value":"{}"}}"#,
        secret
            .chars()
            .map(|c| format!("\\u{:04x}", c as u32))
            .collect::<String>()
    );
    let mixed = r#"{"value":"fi\u0078ture\"se\u0063ret\\tail"}"#;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(mixed).unwrap()["value"],
        secret
    );
    let markdown = format!("# PERSONAL_CONTEXT_SENTINEL\n\n```json\n{unicode}\n```\n");
    let prose =
        format!("PERSONAL_CONTEXT_SENTINEL before the example.\n{mixed}\nAfter the example.");
    let nested = json!({"record": unicode}).to_string();
    let nested_markdown =
        format!("Notes PERSONAL_CONTEXT_SENTINEL\n\n```json\n{nested}\n```\nEnd of notes.\n");
    for flag in ["--memory-file", "--preferences-file"] {
        for (case, contents) in [
            ("canonical JSON", canonical.as_str()),
            ("Unicode JSON", unicode.as_str()),
            ("Markdown Unicode JSON", markdown.as_str()),
            ("prose mixed escapes", prose.as_str()),
            ("Markdown nested JSON", nested_markdown.as_str()),
        ] {
            let temp = TempDir::new().unwrap();
            let home = TempDir::new().unwrap();
            let file = temp.path().join("personal.txt");
            fs::write(&file, contents).unwrap();
            let (url, requests) = fixture::server(vec![(
                "application/json",
                response(json!({"role":"assistant","content":"unexpected"}), "stop"),
            )])
            .unwrap();
            let cfg = config(temp.path(), &url, "auto");
            let source = fs::read_to_string(&cfg).unwrap().replace(
                "auth = \"none\"",
                "auth = \"bearer\"\napi_key_env = \"TEST_KEY\"",
            );
            fs::write(&cfg, source).unwrap();
            let result = command(temp.path(), home.path())
                .env("TEST_KEY", secret)
                .arg("--config")
                .arg(cfg)
                .arg(flag)
                .arg(file)
                .args(["run", "hello"])
                .output()
                .unwrap();
            assert_eq!(result.status.code(), Some(2), "{flag}: {case}");
            assert!(
                result.stdout.is_empty(),
                "rejected personal context was echoed on stdout"
            );
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(
                stderr.contains("contains a configured secret"),
                "{flag}: {case}"
            );
            assert!(!stderr.contains(secret));
            assert!(!stderr.contains(contents));
            assert!(!stderr.contains("PERSONAL_CONTEXT_SENTINEL"));
            assert!(
                requests.try_recv().is_err(),
                "personal credential reached the execution model"
            );
        }
    }
}

#[test]
fn nonsensitive_personal_markdown_reaches_the_model_unchanged() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let preferences = temp.path().join("preferences.md");
    let memory = temp.path().join("memory.md");
    let preferences_text = r#"# Preferences

- Keep `\u0041` literal in examples.
- Preserve Windows paths such as `C:\work\notes.md`.
"#;
    let memory_text = r#"# Memory

普通说明和 JSON 示例保持原样。

```json
{"value":"fi\u0078ture-safe","nested":"{\"note\":\"ordinary content\"}"}
```
"#;
    fs::write(&preferences, preferences_text).unwrap();
    fs::write(&memory, memory_text).unwrap();
    let (url, requests) = fixture::server(vec![(
        "application/json",
        response(json!({"role":"assistant","content":"accepted"}), "stop"),
    )])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let source = fs::read_to_string(&cfg).unwrap().replace(
        "auth = \"none\"",
        "auth = \"bearer\"\napi_key_env = \"TEST_KEY\"",
    );
    fs::write(&cfg, source).unwrap();
    let result = command(temp.path(), home.path())
        .env("TEST_KEY", "fixture\"secret\\tail")
        .arg("--config")
        .arg(cfg)
        .arg("--preferences-file")
        .arg(preferences)
        .arg("--memory-file")
        .arg(memory)
        .args(["run", "hello"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let personal = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_str())
        .filter_map(|content| serde_json::from_str::<serde_json::Value>(content).ok())
        .find_map(|context| context.get("soyo_personal_context_v1").cloned())
        .expect("personal context in the normal model request");
    assert_eq!(personal["preferences"], preferences_text);
    assert_eq!(personal["memory"], memory_text);
    assert!(
        requests.try_recv().is_err(),
        "personal context validation retried the model"
    );
}

#[test]
fn config_init_never_overwrites_and_check_accepts_empty_template() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let path = temp.path().join("config.toml");
    let run = || {
        command(temp.path(), home.path())
            .args(["--config", path.to_str().unwrap(), "config", "init"])
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    let before = fs::read(&path).unwrap();
    assert!(!run().status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(
        command(temp.path(), home.path())
            .args(["--config", path.to_str().unwrap(), "config", "check"])
            .output()
            .unwrap()
            .status
            .success()
    );
}

#[test]
fn concurrent_processes_can_initialize_the_same_database() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto");
    let mut children = Vec::new();
    for _ in 0..6 {
        children.push(
            command(temp.path(), home.path())
                .arg("--config")
                .arg(&cfg)
                .args(["session", "list"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
#[test]
fn prompt_sources_are_explicit_mutually_exclusive_and_nonempty() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let cfg = config(temp.path(), "http://127.0.0.1:1/v1", "auto");
    for args in [
        vec!["run"],
        vec!["run", " "],
        vec!["run", "hello", "--stdin"],
    ] {
        let result = command(temp.path(), home.path())
            .arg("--config")
            .arg(&cfg)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
    }
    let mut child = command(temp.path(), home.path())
        .arg("--config")
        .arg(&cfg)
        .args(["run", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"  \n").unwrap();
    assert_eq!(child.wait_with_output().unwrap().status.code(), Some(2));
}
#[test]
fn local_model_can_read_workspace_and_receives_authorized_work_tools() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "fixture data").unwrap();
    let (url, requests) = fixture::server(vec![
        ("application/json", read_call()),
        (
            "application/json",
            response(json!({"role":"assistant","content":"finished"}), "stop"),
        ),
    ])
    .unwrap();
    let cfg = config(temp.path(), &url, "auto");
    let mut child = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["run", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"inspect the input\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "finished\n");
    let first = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    let names: Vec<_> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["function"]["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"write_file"));
    assert!(names.contains(&"exec"));
    let second = requests.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(second.to_string().contains("fixture data"));
}
#[test]
fn manual_noninteractive_read_needs_confirmation() {
    let temp = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    fs::write(temp.path().join("input.txt"), "data").unwrap();
    let (url, _) = fixture::server(vec![("application/json", read_call())]).unwrap();
    let cfg = config(temp.path(), &url, "manual");
    let result = command(temp.path(), home.path())
        .arg("--config")
        .arg(cfg)
        .args(["run", "read"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&result.stderr).contains("needs_confirmation"));
}
