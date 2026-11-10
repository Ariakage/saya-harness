//! Selector effects remain explicit and do not reuse approvals or model grants.
use super::*;
use crate::view::PickerKind;
use saya_core::{config::Environment, contract::ErrorInfo};
use std::{ffi::OsString, time::Duration};

fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let config_path = root.path().join("config.toml");
    std::fs::write(
        &config_path,
        r#"schema_version = 1
[defaults]
profile = "fixture"
[profiles.fixture]
protocol = "openai-chat-completions"
base_url = "http://127.0.0.1:9/v1"
model = "fixture"
auth = "none"
context_window_tokens = 128000
stream = false
"#,
    )
    .unwrap();
    let config = saya_core::config::load(
        saya_core::config::Overrides {
            config: Some(config_path),
            ..Default::default()
        },
        Environment::from_pairs([(OsString::from("HOME"), root.path().as_os_str().to_owned())]),
        &workspace,
    )
    .unwrap();
    let app = App::new(
        config,
        Options {
            personal_context: PersonalContext::default(),
            anthropic_prompt_cache: false,
            cwd_explicit: false,
            profile_explicit: false,
            mode_override: None,
        },
    )
    .unwrap();
    (root, app)
}

async fn complete(app: &mut App) {
    let result = tokio::time::timeout(Duration::from_secs(8), services::next_job(&mut app.ui_job))
        .await
        .unwrap()
        .unwrap_or_else(|error| {
            panic!(
                "selector failed: {}",
                BoundaryError(
                    ErrorInfo {
                        code: "selector_failed".into(),
                        message: error.0.message,
                    },
                    None,
                    None,
                )
            )
        });
    app.finish_ui(Ok(result)).await.unwrap();
}

#[tokio::test]
async fn mcp_selector_toggles_enablement_for_subsequent_runs() {
    let (_root, mut app) = fixture();
    app.config.mcp = serde_json::from_value(serde_json::json!({"servers":{
        "fixture":{"transport":{"type":"stdio","argv":["/usr/bin/true"]}}
    }}))
    .unwrap();
    app.command("/mcps").await.unwrap();
    let panel = app.state.picker_panel.as_ref().unwrap();
    assert_eq!(panel.kind, PickerKind::Mcps);
    assert_eq!(panel.items.len(), 1);
    assert!(panel.items[0].active);
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(!app.config.mcp.servers["fixture"].enabled);
    assert!(!app.state.picker_panel.as_ref().unwrap().items[0].active);
    assert!(app.state.mcp_servers.is_empty());
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        .await
        .unwrap();
    assert!(app.config.mcp.servers["fixture"].enabled);
    assert!(app.state.picker_panel.as_ref().unwrap().items[0].active);
}

#[tokio::test]
async fn empty_skill_and_plugin_selectors_are_safe_and_bounded() {
    let (_root, mut app) = fixture();
    for command in ["/skills", "/plugins"] {
        app.command(command).await.unwrap();
        complete(&mut app).await;
        let panel = app.state.picker_panel.as_ref().unwrap();
        assert!(panel.items.is_empty());
        assert!(!panel.hint.is_empty());
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            .await
            .unwrap();
        assert!(app.state.picker_panel.is_none());
    }
}
