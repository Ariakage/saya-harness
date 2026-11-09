#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use tempfile::TempDir;

fn example(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
}
fn command(root: &Path, home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_saya"));
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root)
        .stdin(Stdio::null());
    command
}
#[test]
fn example_config_checks_and_init_does_not_overwrite() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let check = command(root.path(), home.path())
        .arg("--config")
        .arg(example("config.example.toml"))
        .args(["config", "check"])
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    let destination = root.path().join("new-config.toml");
    let init = command(root.path(), home.path())
        .arg("--config")
        .arg(&destination)
        .args(["config", "init"])
        .output()
        .unwrap();
    assert!(init.status.success());
    let original = fs::read(&destination).unwrap();
    let again = command(root.path(), home.path())
        .arg("--config")
        .arg(&destination)
        .args(["config", "init"])
        .output()
        .unwrap();
    assert_eq!(again.status.code(), Some(2));
    assert_eq!(fs::read(destination).unwrap(), original);
}
#[test]
fn example_approval_authorizes_only_the_documented_exact_command() {
    let root = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let arguments =
        json!({"argv":["/bin/echo","hello"],"cwd":null,"timeout_seconds":null}).to_string();
    let first=json!({"choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"tool_calls":[{"id":"echo","type":"function","function":{"name":"exec","arguments":arguments}}]}}]}).to_string();
    let final_response=json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":"done"}}]}).to_string();
    let (url, requests) = fixture::server(vec![
        ("application/json", first),
        ("application/json", final_response),
    ])
    .unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config,format!("schema_version = 1\n[defaults]\nprofile = \"test\"\npermissions = \"manual\"\n[profiles.test]\nprotocol = \"openai-chat-completions\"\nbase_url = \"{url}\"\nmodel = \"fixture\"\nauth = \"none\"\ncontext_window_tokens = 128000\nstream = false\n")).unwrap();
    let output = command(root.path(), home.path())
        .arg("--config")
        .arg(config)
        .args(["--output", "json", "run", "--approval-file"])
        .arg(example("approval.example.json"))
        .arg("Echo hello")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "completed");
    let _ = requests
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let request = requests
        .recv_timeout(std::time::Duration::from_secs(3))
        .unwrap();
    let tool = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "tool")
        .unwrap();
    let result: Value = serde_json::from_str(tool["content"].as_str().unwrap()).unwrap();
    assert_eq!(result["data"]["stdout"], "hello\n");
}
