//! Tauri 2 in-process desktop shell for Soyo Harness.

pub mod commands;
pub mod redaction;
pub mod runtime_bridge;
pub mod tool_format;
pub mod types;

pub use runtime_bridge::DesktopState;
pub use types::{DesktopError, SessionItemDto, SessionPageDto, WorkModeDto};

use specta_typescript::Typescript;
use tauri::Manager;
use tauri_specta::{Builder, collect_commands};

pub fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        commands::list_sessions,
        commands::get_bootstrap_state,
        commands::new_session,
        commands::submit_prompt,
        commands::cancel_run,
        commands::compact_session,
    ])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = specta_builder();
    let desktop_state = DesktopState::from_environment();
    tauri::Builder::default()
        .manage(desktop_state)
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Soyo desktop application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app_handle.state::<DesktopState>();
                tauri::async_runtime::block_on(state.shutdown());
            }
        });
}

pub fn export_bindings_to(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    specta_builder().export(Typescript::default(), path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_bridge::StreamSink;
    use crate::types::DesktopStreamEnvelope;
    use soyo_core::{
        boundary::{Cancellation, Model, ModelRequest, ModelStream, ModelUpdate, Operation},
        config::{self, Environment, Overrides},
        contract::{BoundaryError, HistoryItem, Usage},
    };
    use std::{path::PathBuf, sync::Arc};
    use tokio::sync::mpsc;

    struct ScriptedModel {
        chunks: Vec<String>,
        thinking: Option<String>,
        delay_ms: u64,
    }

    struct ScriptedStream {
        chunks: Vec<String>,
        thinking: Option<String>,
        step: usize,
        sent_thinking: bool,
        delay_ms: u64,
    }

    impl Model for ScriptedModel {
        fn start<'a>(
            &'a self,
            _request: &'a ModelRequest,
            _cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Box<dyn ModelStream>> {
            let chunks = self.chunks.clone();
            let thinking = self.thinking.clone();
            let delay_ms = self.delay_ms;
            Box::pin(async move {
                Ok(Box::new(ScriptedStream {
                    chunks,
                    thinking,
                    step: 0,
                    sent_thinking: false,
                    delay_ms,
                }) as Box<dyn ModelStream>)
            })
        }
    }

    impl ModelStream for ScriptedStream {
        fn next<'a>(
            &'a mut self,
            cancel: &'a dyn Cancellation,
        ) -> Operation<'a, Option<ModelUpdate>> {
            Box::pin(async move {
                if self.delay_ms > 0 {
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            return Err(BoundaryError::new("cancelled", "model stream cancelled"));
                        }
                        () = tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms)) => {}
                    }
                }
                if cancel.is_cancelled() {
                    return Err(BoundaryError::new("cancelled", "model stream cancelled"));
                }
                if let Some(thought) = self.thinking.clone()
                    && !self.sent_thinking
                {
                    self.sent_thinking = true;
                    return Ok(Some(ModelUpdate::ThinkingDelta { text: thought }));
                }
                if self.step < self.chunks.len() {
                    let text = self.chunks[self.step].clone();
                    self.step += 1;
                    return Ok(Some(ModelUpdate::TextDelta(text)));
                }
                let full: String = self.chunks.concat();
                Ok(Some(ModelUpdate::Finished {
                    items: vec![HistoryItem::AssistantText(full)],
                    usage: Some(Usage {
                        input_tokens: Some(42),
                        output_tokens: Some(12),
                        cached_input_tokens: None,
                        estimated: false,
                    }),
                }))
            })
        }
    }

    fn test_config(root: &std::path::Path) -> config::Config {
        let cfg_path = root.join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
schema_version = 1

[defaults]
profile = "fixture"
mode = "work"
permissions = "auto"

[profiles.fixture]
protocol = "openai-responses"
base_url = "http://127.0.0.1:9/v1"
model = "fixture-model"
auth = "none"
context_window_tokens = 128000
max_output_tokens = 4096

[sandbox]
backend = "host"
"#,
        )
        .unwrap();
        config::load(
            Overrides {
                config: Some(cfg_path),
                cwd: Some(root.to_path_buf()),
                ..Default::default()
            },
            Environment::from_pairs([("HOME".into(), root.as_os_str().to_owned())]),
            root,
        )
        .unwrap()
    }

    #[test]
    fn export_specta_bindings() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings_to(&path).expect("specta bindings export should succeed");
        let content = std::fs::read_to_string(&path).expect("bindings.ts should exist");
        for symbol in [
            "listSessions",
            "getBootstrapState",
            "newSession",
            "submitPrompt",
            "cancelRun",
            "compactSession",
        ] {
            assert!(content.contains(symbol), "missing {symbol} in bindings.ts");
        }
    }

    #[tokio::test]
    async fn list_sessions_returns_redacted_empty_page_for_fresh_store() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = DesktopState::with_config(cfg);
        let page = state.list_sessions_page(Some(1)).await.unwrap();
        assert_eq!(page.page, 1);
        assert!(page.items.is_empty());
    }

    #[tokio::test]
    async fn submit_prompt_streams_events_and_finishes_via_channel() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = DesktopState::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["Hello ".into(), "desktop!".into()],
                thinking: Some("Analyzing prompt".into()),
                delay_ms: 0,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        let res = state
            .submit_prompt("Say hello".into(), vec![], StreamSink::Mpsc(tx))
            .await
            .unwrap();
        assert_eq!(res.action, "started");

        let mut deltas = String::new();
        let mut saw_run_started = false;
        let mut saw_completed = false;
        while let Some(env) = rx.recv().await {
            match env {
                DesktopStreamEnvelope::SchemaEvent(ev) => match ev.payload {
                    crate::types::SchemaEventDto::RunStarted { .. } => saw_run_started = true,
                    crate::types::SchemaEventDto::ModelTextDelta { text, .. } => {
                        deltas.push_str(&text);
                    }
                    _ => {}
                },
                DesktopStreamEnvelope::RunCompleted { status, .. } => {
                    assert_eq!(status, "completed");
                    saw_completed = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_run_started);
        assert!(saw_completed);
        assert_eq!(deltas, "Hello desktop!");

        let bootstrap = state.get_bootstrap_state().await;
        assert!(!bootstrap.run_active);
        assert!(bootstrap.session.is_some());
        let session = bootstrap.session.unwrap();
        assert!(session.messages.iter().any(|m| m.text == "Hello desktop!"));
    }

    #[tokio::test]
    async fn cancel_run_interrupts_streaming_and_cleans_up() {
        let root = tempfile::tempdir().unwrap();
        let cfg = test_config(root.path());
        let state = DesktopState::with_config(cfg).with_test_model(Arc::new(|| {
            Arc::new(ScriptedModel {
                chunks: vec!["chunk1".into(), "chunk2".into(), "chunk3".into()],
                thinking: None,
                delay_ms: 250,
            })
        }));

        let (tx, mut rx) = mpsc::unbounded_channel();
        state
            .submit_prompt("Slow task".into(), vec![], StreamSink::Mpsc(tx))
            .await
            .unwrap();

        // Wait until first event arrives, then cancel.
        let _ = rx.recv().await;
        assert!(state.cancel_run());

        let mut completed_status = None;
        while let Some(env) = rx.recv().await {
            if let DesktopStreamEnvelope::RunCompleted { status, .. } = env {
                completed_status = Some(status);
                break;
            }
        }
        assert_eq!(completed_status.as_deref(), Some("cancelled"));
        assert!(!state.get_bootstrap_state().await.run_active);
    }
}
