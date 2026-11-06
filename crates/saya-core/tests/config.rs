use saya_core::{
    config::{self, Environment, Output, Overrides, Protocol},
    contract::{PermissionMode, WorkMode},
};
use std::{ffi::OsString, fs, path::Path};
use tempfile::TempDir;

fn env(pairs: &[(&str, &str)]) -> Environment {
    Environment::from_pairs(
        pairs
            .iter()
            .map(|(k, v)| (OsString::from(k), OsString::from(v))),
    )
}
fn write(root: &Path, name: &str, text: &str) -> std::path::PathBuf {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}
fn fixture() -> &'static str {
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
[profiles.responses]
protocol = "openai-responses"
base_url = "http://127.0.0.1:8080/v1"
model = "example"
auth = "none"
context_window_tokens = 100000
[profiles.anthropic]
protocol = "anthropic-messages"
base_url = "https://example.invalid/v1"
model = "example"
auth = "x-api-key"
api_key_env = "EXAMPLE_KEY"
context_window_tokens = 100000
api_version = "2023-06-01"
[review]
profile = "responses"
[permissions]
additional_roots = []
[[permissions.rules]]
effect = "ask"
tool = "exec"
executable = "/usr/bin/git"
args_prefix = ["push"]
[commands]
inherit_env = ["CUSTOM_VAR"]
[limits]
max_model_requests = 10
"#
}
fn test_env(home: &Path) -> Environment {
    env(&[
        ("HOME", home.to_str().unwrap()),
        ("EXAMPLE_KEY", "fixture-secret"),
    ])
}

#[test]
fn loads_selected_file_and_resolves_precedence_without_implicit_project_config() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let default_path = write(home, ".config/saya/config.toml", fixture());
    write(home, "workspace/saya.toml", "not valid TOML");
    let workspace = home.join("workspace");
    let e = env(&[
        ("HOME", home.to_str().unwrap()),
        ("EXAMPLE_KEY", "fixture-secret"),
        ("SAYA_PROFILE", "responses"),
        ("SAYA_MODE", "plan"),
        ("SAYA_PERMISSIONS", "manual"),
        ("SAYA_CWD", workspace.to_str().unwrap()),
    ]);
    let c = config::load(
        Overrides {
            mode: Some(WorkMode::Code),
            output: Some(Output::Json),
            ..Default::default()
        },
        e,
        home,
    )
    .unwrap();
    assert_eq!(c.config_path, default_path);
    assert_eq!(c.profile.as_deref(), Some("responses"));
    assert_eq!(c.mode, WorkMode::Code);
    assert_eq!(c.permissions_mode, PermissionMode::Manual);
    assert_eq!(c.cwd, workspace);
    assert_eq!(c.output, Output::Json);
    assert_eq!(c.data_dir, home.join(".local/share/saya"));
    assert_eq!(c.profiles["chat"].protocol, Protocol::ChatCompletions);
    assert_eq!(c.profiles["chat"].max_output_tokens, 8192);
    assert_eq!(c.review.max_output_tokens, 1024);
    assert_eq!(c.limits.max_model_requests, 10);
    assert_eq!(
        c.secret_env_names().into_iter().collect::<Vec<_>>(),
        vec!["EXAMPLE_KEY"]
    );
}

#[test]
fn explicit_config_replaces_rather_than_merges_and_cli_overrides_environment() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    write(home, ".config/saya/config.toml", fixture());
    let selected = write(home, "chosen.toml", "schema_version = 1\n");
    let e = env(&[
        ("HOME", home.to_str().unwrap()),
        ("SAYA_CONFIG", "does-not-exist"),
        ("SAYA_MODE", "invalid"),
        ("SAYA_PERMISSIONS", "invalid"),
    ]);
    let c = config::load(
        Overrides {
            config: Some(selected.clone()),
            mode: Some(WorkMode::Plan),
            permissions: Some(PermissionMode::Bypass),
            ..Default::default()
        },
        e,
        home,
    )
    .unwrap();
    assert_eq!(c.config_path, selected);
    assert!(c.profiles.is_empty());
    assert!(c.profile.is_none());
    assert_eq!(c.mode, WorkMode::Plan);
    assert_eq!(c.permissions_mode, PermissionMode::Bypass);
}

#[test]
fn xdg_paths_and_environment_file_selection() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let path = write(root, "custom.toml", "schema_version = 1\n");
    let e = env(&[
        ("HOME", root.to_str().unwrap()),
        ("SAYA_CONFIG", path.to_str().unwrap()),
        ("XDG_DATA_HOME", root.join("data").to_str().unwrap()),
        ("XDG_STATE_HOME", root.join("state").to_str().unwrap()),
        ("XDG_CACHE_HOME", "relative"),
    ]);
    let c = config::load(Overrides::default(), e, root).unwrap();
    assert_eq!(c.data_dir, root.join("data/saya"));
    assert_eq!(c.state_dir, root.join("state/saya"));
    assert_eq!(c.cache_dir, root.join(".cache/saya"));
}

#[test]
fn rejects_invalid_configuration_without_echoing_source_or_secrets() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let cases = [
        "schema_version = 2\n",
        "schema_version = 1\nunknown = 'fixture-secret'\n",
        "schema_version = 1\n[defaults]\nmode = 'unexpected'\n",
        "schema_version = 1\n[limits]\nmax_tool_calls = 0\n",
        "schema_version = 1\n[limits]\ncommand_timeout_seconds = 601\n",
        "schema_version = 1\n[review]\nprofile = 'missing'\n",
    ];
    for text in cases {
        let p = write(home, "input.toml", text);
        let err = match config::load(
            Overrides {
                config: Some(p),
                ..Default::default()
            },
            test_env(home),
            home,
        ) {
            Ok(_) => panic!("invalid configuration accepted"),
            Err(e) => e,
        };
        assert_eq!(err.0.code, "invalid_config");
        assert!(!err.0.message.contains("fixture-secret"));
    }
}

#[test]
fn checks_credentials_headers_protocol_versions_and_transport_security() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let cases = [
        fixture().replace("EXAMPLE_KEY", "MISSING_KEY"),
        fixture().replace("https://example.invalid/v1", "http://example.invalid/v1"),
        fixture().replace("2023-06-01", "unsupported"),
        fixture().replace(
            "context_window_tokens = 100000",
            "context_window_tokens = 0",
        ),
        fixture().replace(
            "api_key_env = \"EXAMPLE_KEY\"",
            "api_key_env = \"INVALID-NAME\"",
        ),
        fixture().replace(
            "[profiles.responses]",
            "[profiles.chat.headers_env]\nauthorization = \"EXAMPLE_KEY\"\n[profiles.responses]",
        ),
    ];
    for text in cases {
        let p = write(home, "input.toml", &text);
        let err = match config::load(
            Overrides {
                config: Some(p),
                ..Default::default()
            },
            test_env(home),
            home,
        ) {
            Ok(_) => panic!("invalid profile accepted"),
            Err(e) => e,
        };
        assert!(!err.0.message.contains("fixture-secret"));
    }
}

#[test]
fn rejects_permission_rules_before_execution_and_never_reports_rule_content() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    for rules in [
        "[[permissions.rules]]\neffect = 'allow'\ntool = 'unrecognized'\n",
        "[[permissions.rules]]\neffect = 'allow'\ntool = 'read_file'\nexecutable = '/usr/bin/git'\n",
        "[[permissions.rules]]\neffect = 'deny'\npath_glob = 'fixture-secret'\n",
        "[permissions]\nadditional_roots = ['/path/that/does/not/exist']\n",
    ] {
        let p = write(home, "input.toml", &format!("schema_version = 1\n{rules}"));
        let error = match config::load(
            Overrides {
                config: Some(p),
                ..Default::default()
            },
            test_env(home),
            home,
        ) {
            Ok(_) => panic!("invalid permission configuration accepted"),
            Err(e) => e,
        };
        assert!(!error.0.message.contains("fixture-secret"));
    }
}

#[test]
fn honors_selected_config_and_environment_over_defaults() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let first = write(root, "first.toml", fixture());
    let second = write(root, "second.toml", "schema_version = 1\n");
    let e = env(&[
        ("HOME", root.to_str().unwrap()),
        ("EXAMPLE_KEY", "fixture-secret"),
        ("SAYA_CONFIG", first.to_str().unwrap()),
        ("SAYA_PROFILE", "anthropic"),
        ("SAYA_MODE", "build"),
        ("SAYA_DATA_DIR", root.join("chosen-data").to_str().unwrap()),
    ]);
    let c = config::load(Overrides::default(), e, root).unwrap();
    assert_eq!(c.config_path, first);
    assert_eq!(c.profile.as_deref(), Some("anthropic"));
    assert_eq!(c.mode, WorkMode::Build);
    assert_eq!(c.data_dir, root.join("chosen-data"));
    assert_ne!(c.config_path, second);
}
