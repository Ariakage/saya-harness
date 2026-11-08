#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;
use serde_json::{Value, json};
use std::{
    fs,
    process::{Command, Stdio},
    time::Duration,
};

fn message(text: &str) -> Value {
    json!({"id":"message","type":"message","role":"assistant","status":"completed",
        "content":[{"type":"output_text","text":text,"annotations":[]}]})
}
fn completed(output: Vec<Value>) -> Value {
    json!({"type":"response.completed","response":{"status":"completed","output":output}})
}
fn delta(index: usize, part: usize, text: &str) -> Value {
    json!({"type":"response.output_text.delta","output_index":index,"content_index":part,"delta":text})
}
fn run(
    protocol: &str,
    responses: Vec<(&'static str, String)>,
) -> (std::process::Output, Vec<Value>) {
    run_with_stream(protocol, responses, true)
}
fn run_with_stream(
    protocol: &str,
    responses: Vec<(&'static str, String)>,
    stream: bool,
) -> (std::process::Output, Vec<Value>) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    fs::write(workspace.join("input.txt"), "local data").unwrap();
    let expected = responses.len();
    let (url, requests) = fixture::server(responses).unwrap();
    let config = root.path().join("config.toml");
    fs::write(&config,format!("schema_version=1\n[defaults]\nprofile='fixture'\n[profiles.fixture]\nprotocol='{protocol}'\nbase_url='{url}'\nmodel='fixture'\nauth='none'\ncontext_window_tokens=128000\nstream={stream}\n")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_saya"))
        .env_clear()
        .env("HOME", root.path())
        .env("PATH", "/usr/bin:/bin")
        .current_dir(&workspace)
        .arg("--config")
        .arg(&config)
        .args(["run", "visible completion fixture"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let mut seen = Vec::new();
    for _ in 0..expected {
        seen.push(requests.recv_timeout(Duration::from_secs(2)).unwrap());
    }
    (output, seen)
}
fn assert_text(output: std::process::Output, text: &str) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8(output.stdout).unwrap(), text);
}

#[test]
fn completed_only_and_nonstream_http_json_display_only_normalized_assistant_text() {
    let visible = "可见回答 🌸";
    let opaque =
        json!({"type":"reasoning","id":"thought","encrypted_content":"opaque-must-stay-private"});
    let final_response = completed(vec![opaque, message(visible)]);
    for (protocol,stream,content_type,body) in [
        ("openai-responses",true,"text/event-stream",fixture::sse(std::slice::from_ref(&final_response))),
        ("openai-responses",false,"application/json",final_response["response"].to_string()),
        ("openai-chat-completions",false,"application/json",json!({"choices":[{"index":0,"finish_reason":"stop","message":{"role":"assistant","content":visible,"reasoning_content":"thinking-must-stay-private"}}]}).to_string()),
        ("anthropic-messages",false,"application/json",json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"thinking-must-stay-private","signature":"opaque-signature"},{"type":"text","text":visible}]}).to_string()),
    ] {
        let (output,seen)=run_with_stream(protocol,vec![(content_type,body)],stream);
        assert_eq!(seen[0]["stream"],stream);
        assert_text(output,&format!("{visible}\n"));
    }
}

#[test]
fn completion_adds_only_unstreamed_suffix_and_normal_deltas_are_not_repeated() {
    let mut final_message = message("hello ");
    final_message["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"output_text","text":"world","annotations":[]}));
    for events in [
        vec![
            delta(0, 0, "hello "),
            json!({"type":"response.output_text.done","output_index":0,"content_index":1,"text":"world"}),
            completed(vec![final_message]),
        ],
        vec![
            delta(0, 0, "hello "),
            delta(0, 0, "world"),
            completed(vec![message("hello world")]),
        ],
    ] {
        let (output, _) = run(
            "openai-responses",
            vec![("text/event-stream", fixture::sse(&events))],
        );
        assert_text(output, "hello world\n");
    }
}

#[test]
fn reordered_valid_text_deltas_are_followed_by_the_complete_validated_answer() {
    let mut second = message("second");
    second["id"] = "second-message".into();
    let events = vec![
        delta(1, 0, "second"),
        json!({"type":"response.output_text.done","output_index":0,"content_index":0,"text":"first "}),
        completed(vec![message("first "), second]),
    ];
    let (output, _) = run(
        "openai-responses",
        vec![("text/event-stream", fixture::sse(&events))],
    );
    assert_text(output, "second\nfirst second\n");
}

#[test]
fn completed_only_final_turn_is_visible_after_an_earlier_streamed_tool_turn() {
    let call = json!({"type":"function_call","id":"function-item","call_id":"read-input","status":"completed",
        "name":"read_file","arguments":json!({"path":"input.txt","start_line":1,"line_count":1}).to_string()});
    let first = fixture::sse(&[
        delta(0, 0, "Inspecting. "),
        completed(vec![message("Inspecting. "), call]),
    ]);
    let last = fixture::sse(&[completed(vec![message("All done.")])]);
    let (output, seen) = run(
        "openai-responses",
        vec![("text/event-stream", first), ("text/event-stream", last)],
    );
    assert!(seen[1].to_string().contains("local data"));
    assert_text(output, "Inspecting. All done.\n");
}

#[test]
fn interrupted_stream_preserves_printed_text_without_inventing_a_final_completion() {
    let (output, _) = run(
        "openai-responses",
        vec![(
            "text/event-stream",
            fixture::sse(&[delta(0, 0, "partial 🌸")]),
        )],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "partial 🌸\n");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("incomplete_model_response")
    );
}
