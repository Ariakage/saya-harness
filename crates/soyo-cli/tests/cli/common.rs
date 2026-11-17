use serde_json::json;
use std::{fs, path::Path, process::Command};

pub(crate) fn command(root: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_soyo"));
    cmd.env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .current_dir(root);
    cmd
}
pub(crate) fn config(root: &Path, url: &str, permissions: &str) -> std::path::PathBuf {
    let p = root.join("config.toml");
    fs::write(
        &p,
        format!(
            r#"schema_version = 1
[defaults]
profile = "test"
permissions = "{permissions}"
[profiles.test]
protocol = "openai-chat-completions"
base_url = "{url}"
model = "fake"
auth = "none"
context_window_tokens = 128000
stream = false
"#
        ),
    )
    .unwrap();
    p
}
pub(crate) fn response(message: serde_json::Value, finish: &str) -> String {
    json!({"choices":[{"index":0,"finish_reason":finish,"message":message}]}).to_string()
}
pub(crate) fn read_call() -> String {
    response(
        json!({"role":"assistant","content":null,"tool_calls":[{"id":"call1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"input.txt\",\"start_line\":1,\"line_count\":10}"}}]}),
        "tool_calls",
    )
}

pub(crate) fn tool_response(id: &str, name: &str, arguments: serde_json::Value) -> String {
    response(
        json!({"role":"assistant","content":null,"tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":arguments.to_string()}}]}),
        "tool_calls",
    )
}
