use saya_app::goal::{configured_auto_goal, goal_profile_fingerprint};
use saya_core::{
    config::{Environment, Overrides},
    contract::{PermissionMode, SessionId, WorkMode},
    goal::{GoalAuthorization, GoalBudget, GoalRecord, GoalSeed},
};
use std::{ffi::OsString, fs, path::Path};

fn config(root: &Path) -> saya_core::config::Config {
    let path = root.join("config.toml");
    fs::write(
        &path,
        r#"
schema_version = 1
[defaults]
profile = "chat"
mode = "work"
permissions = "auto"
output = "text"
[profiles.chat]
protocol = "openai-chat-completions"
base_url = "https://example.invalid/v1"
model = "example"
auth = "bearer"
api_key_env = "EXAMPLE_KEY"
context_window_tokens = 100000
[web]
enabled = false
[browser]
enabled = false
"#,
    )
    .unwrap();
    saya_core::config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        Environment::from_pairs([
            (OsString::from("HOME"), root.as_os_str().to_owned()),
            (OsString::from("EXAMPLE_KEY"), OsString::from("test-key")),
        ]),
        root,
    )
    .unwrap()
}

fn goal(config: &saya_core::config::Config) -> GoalRecord {
    let mut goal = GoalRecord::new(GoalSeed {
        session_id: SessionId::new(),
        objective: "fingerprint drift".into(),
        workspace: config.cwd.clone(),
        profile: "chat".into(),
        mode: WorkMode::Work,
        permissions: config.permissions_mode,
        budget: GoalBudget::default(),
        now: "2026-11-12T00:00:00Z".into(),
    })
    .unwrap();
    goal.authorization = Some(GoalAuthorization {
        auto: true,
        mode: WorkMode::Work,
        permissions: config.permissions_mode,
        config_fingerprint: goal_profile_fingerprint(config, "chat").unwrap(),
        authorized_at: "2026-11-12T00:00:00Z".into(),
    });
    goal
}

#[test]
fn web_and_browser_configuration_participate_in_the_goal_fingerprint() {
    let temp = tempfile::tempdir().unwrap();
    let base = config(temp.path());
    assert!(configured_auto_goal(&base, &goal(&base)).is_ok());

    let mut web = base.clone();
    web.web.enabled = true;
    web.web.allowed_origins = vec!["https://example.com".into()];
    assert_ne!(
        goal_profile_fingerprint(&base, "chat").unwrap(),
        goal_profile_fingerprint(&web, "chat").unwrap()
    );
    assert_eq!(
        configured_auto_goal(&web, &goal(&base)).unwrap_err().0.code,
        "goal_authorization"
    );

    let mut browser = base.clone();
    browser.browser.enabled = true;
    browser.browser.max_wait_ms = 10_000;
    assert_ne!(
        goal_profile_fingerprint(&base, "chat").unwrap(),
        goal_profile_fingerprint(&browser, "chat").unwrap()
    );
    assert_eq!(
        configured_auto_goal(&browser, &goal(&base))
            .unwrap_err()
            .0
            .code,
        "goal_authorization"
    );

    // The permission mode stays a separate drift dimension.
    let mut permissions = base.clone();
    permissions.permissions_mode = PermissionMode::Manual;
    assert_eq!(
        configured_auto_goal(&permissions, &goal(&base))
            .unwrap_err()
            .0
            .code,
        "goal_authorization"
    );
}
