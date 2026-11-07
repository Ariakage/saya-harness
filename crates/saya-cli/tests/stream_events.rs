//! Protocol streams exercised through application assembly and the JSONL sink.
#[path = "../../saya-core/tests/fixtures/http_support/mod.rs"]
mod fixture;
use serde_json::{Value, json};
use std::{fs, process::Command, time::Duration};

fn event(kind: &str, mut data: Value) -> Value {
    data["type"] = kind.into();
    data
}
fn streams() -> [(&'static str, String); 3] {
    let message = json!({"id":"message-1","type":"message","status":"completed","role":"assistant","content":[{"type":"output_text","text":"hello 🌸","annotations":[]}]});
    let anthropic = [
        event(
            "message_start",
            json!({"message":{"role":"assistant","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
        ),
        event(
            "content_block_start",
            json!({"index":0,"content_block":{"type":"text","text":""}}),
        ),
        event(
            "content_block_delta",
            json!({"index":0,"delta":{"type":"text_delta","text":"hello "}}),
        ),
        event(
            "content_block_delta",
            json!({"index":0,"delta":{"type":"text_delta","text":"🌸"}}),
        ),
        event("content_block_stop", json!({"index":0})),
        event(
            "message_delta",
            json!({"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":2}}),
        ),
        event("message_stop", json!({})),
    ];
    [
        (
            "openai-chat-completions",
            fixture::sse(&[
                json!({"choices":[{"index":0,"delta":{"content":"hello "},"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{"content":"🌸"},"finish_reason":null}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
            ]),
        ),
        (
            "openai-responses",
            fixture::sse(&[
                event(
                    "response.output_text.delta",
                    json!({"output_index":0,"content_index":0,"delta":"hello "}),
                ),
                event(
                    "response.output_text.delta",
                    json!({"output_index":0,"content_index":0,"delta":"🌸"}),
                ),
                event(
                    "response.output_item.done",
                    json!({"output_index":0,"item":message}),
                ),
                event(
                    "response.completed",
                    json!({"response":{"status":"completed","output":[message],"usage":{"input_tokens":1,"output_tokens":2}}}),
                ),
            ]),
        ),
        (
            "anthropic-messages",
            anthropic
                .iter()
                .map(|value| {
                    format!(
                        "event: {}\ndata: {value}\n\n",
                        value["type"].as_str().unwrap()
                    )
                })
                .collect(),
        ),
    ]
}

#[test]
fn three_protocol_streams_reach_shared_jsonl_events_with_complete_identity()
-> Result<(), Box<dyn std::error::Error>> {
    for (protocol, body) in streams() {
        let root = tempfile::tempdir()?;
        let (url, requests) = fixture::server(vec![("text/event-stream", body)])?;
        let config = root.path().join("config.toml");
        fs::write(
            &config,
            format!(
                r#"schema_version = 1
[defaults]
profile = "test"
[profiles.test]
protocol = "{protocol}"
base_url = "{url}"
model = "fake"
auth = "none"
context_window_tokens = 128000
stream = true
"#
            ),
        )?;
        let output = Command::new(env!("CARGO_BIN_EXE_saya"))
            .env_clear()
            .env("HOME", root.path())
            .env("PATH", "/usr/bin:/bin")
            .current_dir(root.path())
            .arg("--config")
            .arg(&config)
            .args([
                "--output",
                "jsonl",
                "run",
                "respond with the streamed fixture",
            ])
            .output()?;
        assert!(
            output.status.success(),
            "{protocol}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let events = String::from_utf8(output.stdout)?
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<Result<Vec<_>, _>>()?;
        let finished = events
            .iter()
            .filter(|value| value["type"] == "run.finished")
            .collect::<Vec<_>>();
        assert_eq!(finished.len(), 1, "{protocol}");
        let terminal = finished[0];
        assert_eq!(events.last(), Some(terminal), "{protocol}");
        assert_eq!(terminal["data"]["status"], "completed", "{protocol}");
        assert_eq!(terminal["data"]["text"], "hello 🌸", "{protocol}");
        let session = terminal["data"]["session_id"].as_str().unwrap();
        let run = terminal["data"]["run_id"].as_str().unwrap();
        assert!(!session.is_empty() && !run.is_empty());
        assert_eq!(events[0]["type"], "session.created", "{protocol}");
        for value in &events {
            assert_eq!(value["schema_version"], 1, "{protocol}");
            assert_eq!(value["session_id"], session, "{protocol}");
            if value["type"] != "session.created" {
                assert_eq!(value["run_id"], run, "{protocol}");
            }
        }
        for pair in events.windows(2) {
            assert!(
                pair[0]["seq"].as_u64().unwrap() < pair[1]["seq"].as_u64().unwrap(),
                "{protocol}"
            );
        }
        let started = events
            .iter()
            .filter(|value| value["type"] == "model.started")
            .collect::<Vec<_>>();
        let model_finished = events
            .iter()
            .filter(|value| value["type"] == "model.finished")
            .collect::<Vec<_>>();
        assert_eq!(started.len(), 1, "{protocol}");
        assert_eq!(model_finished.len(), 1, "{protocol}");
        let request = &started[0]["data"]["request_id"];
        assert_eq!(
            &model_finished[0]["data"]["request_id"], request,
            "{protocol}"
        );
        assert_eq!(model_finished[0]["data"]["ok"], true, "{protocol}");
        let deltas = events
            .iter()
            .filter(|value| value["type"] == "model.text_delta")
            .collect::<Vec<_>>();
        assert!(deltas.len() >= 2, "{protocol}");
        let mut text = String::new();
        for delta in deltas {
            assert_eq!(&delta["data"]["request_id"], request, "{protocol}");
            text.push_str(delta["data"]["text"].as_str().unwrap());
        }
        assert_eq!(text, "hello 🌸", "{protocol}");
        assert_eq!(
            requests.recv_timeout(Duration::from_secs(2))?["stream"],
            true,
            "{protocol}"
        );
    }
    Ok(())
}
