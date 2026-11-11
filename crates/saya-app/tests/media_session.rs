use saya_app::session::{self, ResumeOptions};
use saya_core::{
    boundary::Store,
    config::{Environment, Overrides},
    media::{self, ContentPart},
};
use std::ffi::OsString;

#[tokio::test]
async fn appended_media_reopens_from_its_snapshot_after_the_source_disappears() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let image = workspace.join("image.png");
    std::fs::write(&image, b"original image").unwrap();
    let path = root.path().join("config.toml");
    std::fs::write(
        &path,
        r#"schema_version = 1
[defaults]
profile = "fixture"
[output_capture]
enabled = false
[profiles.fixture]
protocol = "openai-chat-completions"
base_url = "http://127.0.0.1:9/v1"
model = "fixture"
auth = "none"
context_window_tokens = 128000
stream = false
multimodal.image = true
"#,
    )
    .unwrap();
    let config = saya_core::config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        Environment::from_pairs([(OsString::from("HOME"), root.path().as_os_str().to_owned())]),
        &workspace,
    )
    .unwrap();
    let mut prepared = session::create(&config, saya_app::new_session(&config).unwrap())
        .await
        .unwrap();
    let id = prepared.session.id;
    session::append(
        &mut prepared,
        format!("Inspect [Image 1](<{}>)", image.display()),
    )
    .await
    .unwrap();
    let persisted = prepared.store.load_session(id).await.unwrap().unwrap();
    let saya_core::contract::HistoryItem::UserText(persisted_text) = &persisted.history[0] else {
        panic!("missing user history")
    };
    assert!(!persisted_text.contains(&image.display().to_string()));
    let references = media::history_references(&persisted.history);
    assert_eq!(references.len(), 1);
    std::fs::write(&image, b"changed image").unwrap();
    std::fs::remove_file(&image).unwrap();
    drop(prepared);

    let resumed = session::resume(
        &config,
        &id.0.to_string(),
        ResumeOptions {
            accept_config_change: false,
            cwd_explicit: false,
            profile_explicit: false,
            mode_override: None,
        },
    )
    .await
    .unwrap();
    let media_store = resumed.media.clone();
    let attachments =
        media::load_history(&resumed.session.history, Some(&media_store), id).unwrap();
    let saya_core::contract::HistoryItem::UserText(text) = &resumed.session.history[0] else {
        panic!("missing user history")
    };
    let parts =
        media::expand_user_with_snapshots(text, resumed.media_capabilities, &attachments).unwrap();
    let data_url = parts.iter().find_map(|part| match part {
        ContentPart::Media(media) => Some(media::data_url(media)),
        _ => None,
    });
    assert_eq!(
        data_url.as_deref(),
        Some("data:image/png;base64,b3JpZ2luYWwgaW1hZ2U=")
    );
}
