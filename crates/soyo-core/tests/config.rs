use soyo_core::{
    config::{self, Environment, Output, Overrides, Protocol, SandboxSettings, ToolchainPreset},
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
fn model_timeouts_allow_unlimited_total_without_disabling_other_bounds() {
    let defaults = config::Limits::default();
    assert_eq!(defaults.model_connect_timeout_seconds, 300);
    assert_eq!(defaults.model_idle_timeout_seconds, 900);
    assert_eq!(defaults.model_timeout_seconds, 0);
    let temp = TempDir::new().unwrap();
    let text = fixture().replace(
        "max_model_requests = 10",
        "max_model_requests = 10\nmodel_connect_timeout_seconds = 300\nmodel_idle_timeout_seconds = 900\nmodel_timeout_seconds = 0",
    );
    let path = write(temp.path(), "config.toml", &text);
    let loaded = config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    assert_eq!(loaded.limits.model_connect_timeout_seconds, 300);
    assert_eq!(loaded.limits.model_idle_timeout_seconds, 900);
    assert_eq!(loaded.limits.model_timeout_seconds, 0);
    let invalid = write(
        temp.path(),
        "invalid.toml",
        &fixture().replace(
            "max_model_requests = 10",
            "max_model_requests = 10\nmodel_idle_timeout_seconds = 0",
        ),
    );
    let Err(error) = config::load(
        Overrides {
            config: Some(invalid),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    ) else {
        panic!("zero idle timeout was accepted")
    };
    assert_eq!(error.0.code, "invalid_config");
}

#[test]
fn model_output_defaults_resolve_after_explicit_targets_for_all_protocols() {
    let temp = TempDir::new().unwrap();
    for protocol in [
        "openai-chat-completions",
        "openai-responses",
        "anthropic-messages",
    ] {
        for (declared, explicit, expected) in [
            (None, None, 65_536),
            (Some(64000), None, 64000),
            (None, Some(12000), 12000),
            (Some(64000), Some(12000), 12000),
            (Some(4096), Some(64000), 64000),
        ] {
            let mut text = format!(
                "schema_version=1\n[defaults]\nprofile='selected'\n[profiles.selected]\nprotocol='{protocol}'\nbase_url='https://example.invalid/v1'\nmodel='fixture'\nauth='none'\ncontext_window_tokens=100000\n"
            );
            if let Some(tokens) = explicit {
                text.push_str(&format!("max_output_tokens={tokens}\n"));
            }
            if let Some(tokens) = declared {
                text.push_str(&format!("[profiles.selected.limit]\noutput={tokens}\n"));
            }
            let path = write(temp.path(), "config.toml", &text);
            let loaded = config::load(
                Overrides {
                    config: Some(path),
                    ..Default::default()
                },
                test_env(temp.path()),
                temp.path(),
            )
            .unwrap();
            let profile = &loaded.profiles["selected"];
            assert_eq!(
                profile.max_output_tokens, expected,
                "{protocol}: declared={declared:?}, explicit={explicit:?}"
            );
            assert_eq!(profile.limit.as_ref().map(|limit| limit.output), declared);
            assert_eq!(loaded.review.max_output_tokens, None);
            assert_eq!(loaded.context.summary_max_output_tokens, None);
            let serialized = serde_json::to_value(profile).unwrap();
            assert_eq!(serialized["max_output_tokens"], expected);
            assert_eq!(serialized.get("limit").is_some(), declared.is_some());
            let round_trip: config::Profile = serde_json::from_value(serialized).unwrap();
            assert_eq!(
                round_trip.fingerprint().unwrap(),
                profile.fingerprint().unwrap()
            );
            assert!(!temp.path().join(".soyo").exists());
        }
    }
}

#[test]
fn declared_output_metadata_and_effective_target_both_bind_profile_identity() {
    let source = "protocol='openai-chat-completions'\nbase_url='https://example.invalid/v1'\nmodel='fixture'\nauth='none'\ncontext_window_tokens=100000\nmax_output_tokens=12000\n";
    let explicit: config::Profile = toml::from_str(source).unwrap();
    let declared: config::Profile =
        toml::from_str(&format!("{source}[limit]\noutput=64000\n")).unwrap();
    let updated: config::Profile =
        toml::from_str(&format!("{source}[limit]\noutput=96000\n")).unwrap();
    assert_eq!(explicit.max_output_tokens, declared.max_output_tokens);
    assert_ne!(
        explicit.fingerprint().unwrap(),
        declared.fingerprint().unwrap()
    );
    assert_ne!(
        declared.fingerprint().unwrap(),
        updated.fingerprint().unwrap()
    );
    assert!(
        !serde_json::to_string(&explicit)
            .unwrap()
            .contains("\"limit\"")
    );
    let same_target: config::Profile = toml::from_str(&source.replace("12000", "16000")).unwrap();
    assert_ne!(
        explicit.fingerprint().unwrap(),
        same_target.fingerprint().unwrap()
    );
}

#[test]
fn purpose_output_overrides_are_optional_nonzero_and_have_no_legacy_small_cap() {
    let temp = TempDir::new().unwrap();
    let path = write(
        temp.path(),
        "config.toml",
        &format!(
            "{}\n[context]\nsummary_max_output_tokens=64000\n",
            fixture().replace(
                "[review]\nprofile = \"responses\"",
                "[review]\nprofile = \"responses\"\nmax_output_tokens=48000"
            )
        ),
    );
    let loaded = config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    assert_eq!(loaded.review.max_output_tokens, Some(48000));
    assert_eq!(loaded.context.summary_max_output_tokens, Some(64000));
    assert_eq!(loaded.profiles["responses"].max_output_tokens, 65_536);
}

#[test]
fn output_configuration_rejects_zero_overflow_and_unknown_metadata_without_source_echo() {
    let temp = TempDir::new().unwrap();
    let profile = "schema_version=1\n[profiles.selected]\nprotocol='openai-chat-completions'\nbase_url='https://example.invalid/v1'\nmodel='fixture'\nauth='none'\ncontext_window_tokens=100000\n";
    for extra in [
        "max_output_tokens=0",
        "max_output_tokens=1000001",
        "max_output_tokens=4294967296",
        "max_output_tokens=64000\n[profiles.selected.limit]\noutput=0",
        "[profiles.selected.limit]\noutput=0",
        "[profiles.selected.limit]\noutput=1000001",
        "[profiles.selected.limit]\noutput=4294967296",
        "[profiles.selected.limit]\noutput=32000\nunknown='SYNTHETIC-PRIVATE-VALUE'",
        "[profiles.selected.limit]\nunknown='SYNTHETIC-PRIVATE-VALUE'",
        "unknown='SYNTHETIC-PRIVATE-VALUE'",
        "[review]\nmax_output_tokens=0",
        "[review]\nmax_output_tokens=1000001",
        "[review]\nmax_output_tokens=4294967296",
        "[context]\nsummary_max_output_tokens=0",
        "[context]\nsummary_max_output_tokens=1000001",
        "[context]\nsummary_max_output_tokens=4294967296",
    ] {
        let path = write(temp.path(), "config.toml", &format!("{profile}{extra}\n"));
        let failure = config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        )
        .err()
        .expect("invalid output settings accepted");
        assert_eq!(failure.0.code, "invalid_config");
        assert!(!failure.to_string().contains("SYNTHETIC-PRIVATE-VALUE"));
    }
}

#[test]
fn loads_selected_file_and_resolves_precedence_without_implicit_project_config() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    let default_path = write(home, ".config/soyo/config.toml", fixture());
    write(home, "workspace/soyo.toml", "not valid TOML");
    write(home, "workspace/.soyo/config.toml", "not valid TOML");
    write(home, "workspace/.agents/config.toml", "not valid TOML");
    let workspace = home.join("workspace");
    let e = env(&[
        ("HOME", home.to_str().unwrap()),
        ("EXAMPLE_KEY", "fixture-secret"),
        ("SOYO_PROFILE", "responses"),
        ("SOYO_MODE", "plan"),
        ("SOYO_PERMISSIONS", "manual"),
        ("SOYO_CWD", workspace.to_str().unwrap()),
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
    assert_eq!(c.data_dir, home.join(".local/share/soyo"));
    assert_eq!(c.profiles["chat"].protocol, Protocol::ChatCompletions);
    assert_eq!(c.profiles["chat"].max_output_tokens, 65_536);
    assert!(!c.profiles["chat"].strict_tools);
    assert_eq!(c.review.max_output_tokens, None);
    assert_eq!(c.limits.max_model_requests, 10);
    assert_eq!(c.sandbox.backend, soyo_core::execution::BackendKind::Auto);
    assert!(c.mcp.servers.is_empty());
    assert_eq!(
        c.secret_env_names().into_iter().collect::<Vec<_>>(),
        vec!["EXAMPLE_KEY"]
    );
}

#[test]
fn relative_launch_paths_use_the_process_directory_and_remain_side_effect_free() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    let selected = write(root, "settings/config.toml", "schema_version = 1\n");
    let workspace = root.join("workspace");
    fs::create_dir(&workspace).unwrap();
    for environment_selected in [false, true] {
        let environment = env(&[
            ("HOME", root.to_str().unwrap()),
            ("SOYO_CONFIG", "settings/config.toml"),
            ("SOYO_CWD", "workspace"),
            ("SOYO_DATA_DIR", "session-data/nested"),
        ]);
        let overrides = if environment_selected {
            Overrides::default()
        } else {
            Overrides {
                config: Some("settings/config.toml".into()),
                cwd: Some("workspace".into()),
                data_dir: Some("session-data/nested".into()),
                ..Default::default()
            }
        };
        let config = config::load(overrides, environment, root).unwrap();
        assert_eq!(config.config_path, selected);
        assert_eq!(config.cwd, workspace);
        assert_eq!(config.data_dir, root.join("session-data/nested"));
        for path in [config.config_path, config.cwd, config.data_dir] {
            assert!(path.is_absolute());
            assert!(
                !path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
            );
        }
    }
    assert!(!root.join("session-data").exists());
}

#[test]
fn launch_parent_components_preserve_existing_symlink_traversal() {
    let temp = TempDir::new().unwrap();
    let root = temp.path();
    write(root, "config.toml", "schema_version = 1\n");
    fs::create_dir_all(root.join("actual/storage")).unwrap();
    std::os::unix::fs::symlink(root.join("actual/storage"), root.join("alias")).unwrap();
    let config = config::load(
        Overrides {
            config: Some("alias/../../config.toml".into()),
            cwd: Some("alias/..".into()),
            data_dir: Some("alias/../session-data/nested".into()),
            ..Default::default()
        },
        test_env(root),
        root,
    )
    .unwrap();
    let canonical = root.canonicalize().unwrap();
    assert_eq!(config.config_path, canonical.join("config.toml"));
    assert_eq!(config.cwd, canonical.join("actual"));
    assert_eq!(
        config.data_dir,
        canonical.join("actual/session-data/nested")
    );
    assert!(!config.data_dir.exists());
}

#[test]
fn explicit_config_replaces_rather_than_merges_and_cli_overrides_environment() {
    let temp = TempDir::new().unwrap();
    let home = temp.path();
    write(home, ".config/soyo/config.toml", fixture());
    let selected = write(home, "chosen.toml", "schema_version = 1\n");
    let e = env(&[
        ("HOME", home.to_str().unwrap()),
        ("SOYO_CONFIG", "does-not-exist"),
        ("SOYO_MODE", "invalid"),
        ("SOYO_PERMISSIONS", "invalid"),
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
        ("SOYO_CONFIG", path.to_str().unwrap()),
        ("XDG_DATA_HOME", root.join("data").to_str().unwrap()),
        ("XDG_STATE_HOME", root.join("state").to_str().unwrap()),
        ("XDG_CACHE_HOME", "relative"),
    ]);
    let c = config::load(Overrides::default(), e, root).unwrap();
    assert_eq!(c.data_dir, root.join("data/soyo"));
    assert_eq!(c.state_dir, root.join("state/soyo"));
    assert_eq!(c.cache_dir, root.join(".cache/soyo"));
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
        ("SOYO_CONFIG", first.to_str().unwrap()),
        ("SOYO_PROFILE", "anthropic"),
        ("SOYO_MODE", "build"),
        ("SOYO_DATA_DIR", root.join("chosen-data").to_str().unwrap()),
    ]);
    let c = config::load(Overrides::default(), e, root).unwrap();
    assert_eq!(c.config_path, first);
    assert_eq!(c.profile.as_deref(), Some("anthropic"));
    assert_eq!(c.mode, WorkMode::Build);
    assert_eq!(c.data_dir, root.join("chosen-data"));
    assert_ne!(c.config_path, second);
}

#[test]
fn explicit_pricing_is_validated_without_changing_provider_identity() {
    let temp = TempDir::new().unwrap();
    let path = write(temp.path(), "config.toml", fixture());
    let overrides = || Overrides {
        config: Some(path.clone()),
        ..Default::default()
    };
    let before = config::load(overrides(), test_env(temp.path()), temp.path())
        .unwrap()
        .profiles["chat"]
        .fingerprint()
        .unwrap();
    let mut text = fixture().to_string();
    text.push_str("\n[profiles.chat.pricing]\ncurrency = \"USD\"\ninput_microunits_per_million = 1000000\noutput_microunits_per_million = 2000000\n");
    fs::write(&path, &text).unwrap();
    let loaded = config::load(overrides(), test_env(temp.path()), temp.path()).unwrap();
    assert_eq!(loaded.profiles["chat"].fingerprint().unwrap(), before);
    assert_eq!(
        loaded.profiles["chat"].pricing.as_ref().unwrap().currency,
        "USD"
    );
    fs::write(&path, text.replace("USD", "usd")).unwrap();
    assert!(config::load(overrides(), test_env(temp.path()), temp.path()).is_err());
}

#[test]
fn omitted_thinking_effort_preserves_the_legacy_profile_fingerprint() {
    let temp = TempDir::new().unwrap();
    let explicit_legacy_target = fixture().replacen(
        "context_window_tokens = 100000",
        "context_window_tokens = 100000\nmax_output_tokens = 8192",
        1,
    );
    let path = write(temp.path(), "config.toml", &explicit_legacy_target);
    let loaded = config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    let mut profile = loaded.profiles["chat"].clone();
    assert_eq!(profile.thinking_effort, None);
    // Pin the pre-effort serialization for the same explicit request target.
    let legacy = r#"{"protocol":"openai-chat-completions","base_url":"https://example.invalid/v1","model":"example","auth":"bearer","api_key_env":"EXAMPLE_KEY","context_window_tokens":100000,"max_output_tokens":8192,"allow_remote_http":false,"headers_env":{},"strict_tools":false,"stream":true,"api_version":"2026-10-21","legacy_max_tokens":false,"omit_stream_usage":false,"omit_reasoning_include":false}"#;
    assert_eq!(serde_json::to_string(&profile).unwrap(), legacy);
    let original = profile.fingerprint().unwrap();
    assert_eq!(
        original,
        "af8d44cbd9e2f4feda225ca593a9874838d4216848377342a8ba2e2cc201adbe"
    );
    profile.thinking_effort = Some(config::ThinkingEffort::Max);
    let maximum = profile.fingerprint().unwrap();
    assert_ne!(maximum, original);
    profile.thinking_effort = Some(config::ThinkingEffort::High);
    assert_ne!(profile.fingerprint().unwrap(), maximum);
    profile.thinking_effort = None;
    assert_eq!(profile.fingerprint().unwrap(), original);
}

#[test]
fn multimodal_capabilities_are_explicit_and_bind_profile_identity() {
    let temp = TempDir::new().unwrap();
    let path = write(temp.path(), "config.toml", fixture());
    let loaded = config::load(
        Overrides {
            config: Some(path.clone()),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    let original = loaded.profiles["chat"].fingerprint().unwrap();
    assert!(!loaded.profiles["chat"].multimodal.image);

    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("\n[profiles.chat.multimodal]\nimage = true\naudio = true\nvideo = false\n");
    fs::write(&path, text).unwrap();
    let loaded = config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    let profile = &loaded.profiles["chat"];
    assert!(profile.multimodal.image);
    assert!(profile.multimodal.audio);
    assert!(!profile.multimodal.video);
    assert_ne!(profile.fingerprint().unwrap(), original);
}

#[test]
fn explicit_thinking_effort_accepts_five_levels_and_rejects_unknown_values() {
    let temp = TempDir::new().unwrap();
    let path = write(temp.path(), "config.toml", fixture());
    for level in [
        "low",
        "medium",
        "high",
        "xhigh",
        "max",
        "none",
        "minimal",
        "automatic",
        "HIGH",
        "",
    ] {
        let text = fixture().replacen(
            "model = \"example\"",
            &format!("model = \"example\"\nthinking_effort = \"{level}\""),
            1,
        );
        fs::write(&path, text).unwrap();
        let result = config::load(
            Overrides {
                config: Some(path.clone()),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        );
        if ["low", "medium", "high", "xhigh", "max"].contains(&level) {
            let loaded = result.unwrap();
            let effort = loaded.profiles["chat"].thinking_effort.unwrap();
            assert_eq!(effort.as_str(), level);
            assert_eq!(serde_json::to_value(effort).unwrap(), level);
            assert_eq!(loaded.profiles["responses"].thinking_effort, None);
        } else {
            assert!(result.is_err(), "{level}");
        }
    }
}

#[test]
fn sandbox_defaults_to_native_auto_and_host_requires_explicit_configuration() {
    use soyo_core::{config::SandboxNetworkSettings, execution::BackendKind};
    let temp = TempDir::new().unwrap();
    for backend in ["auto", "host", "seatbelt", "bubblewrap"] {
        let path = write(
            temp.path(),
            "config.toml",
            &format!("schema_version=1\n[sandbox]\nbackend='{backend}'\n"),
        );
        let config = config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(config.sandbox.backend).unwrap(),
            backend
        );
        assert!(matches!(
            config.sandbox.network,
            SandboxNetworkSettings::Deny {}
        ));
        assert_eq!(config.permissions_mode, PermissionMode::Auto);
        if backend == "auto" {
            assert_eq!(config.sandbox.backend, BackendKind::Auto);
        }
    }
}

#[test]
fn sandbox_resolves_paths_resources_and_broker_without_creating_runtime_artifacts() {
    use soyo_core::execution::{BackendKind, NetworkPolicy};
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().canonicalize().unwrap();
    let path = write(
        &workspace,
        "config.toml",
        r#"schema_version=1
[sandbox]
read_roots=['readonly']
write_roots=['cache']
protected_paths=['.secrets']
cgroup_root='/delegated/soyo'
[sandbox.resources]
cpu_seconds=10
file_size_bytes=1048576
open_files=128
[sandbox.network]
mode='proxy'
allowed_origins=['https://example.invalid','http://localhost:4567']
localhost_development_origin='http://localhost:4567'
[sandbox.network.limits]
max_connections=2
"#,
    );
    let config = config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        test_env(&workspace),
        &workspace,
    )
    .unwrap();
    let (policy, broker) = config.sandbox.resolve(&workspace).unwrap();
    assert_eq!(policy.backend, BackendKind::Auto);
    assert_eq!(policy.workspace, workspace);
    assert_eq!(
        policy.read_roots,
        [workspace.clone(), workspace.join("readonly")]
    );
    assert_eq!(
        policy.write_roots,
        [workspace.clone(), workspace.join("cache")]
    );
    assert_eq!(policy.protected_paths, [workspace.join(".secrets")]);
    assert_eq!(policy.resources.cpu_seconds, Some(10));
    assert_eq!(policy.resources.file_size_bytes, Some(1048576));
    assert_eq!(policy.resources.open_files, Some(128));
    assert_eq!(
        policy.cgroup_root.as_deref(),
        Some(Path::new("/delegated/soyo"))
    );
    assert_eq!(policy.network, NetworkPolicy::Deny);
    let broker = broker.unwrap();
    assert_eq!(broker.limits.max_connections, 2);
    assert_eq!(broker.allowed_origins.len(), 2);
    assert_eq!(
        broker.localhost_development_origin.as_deref(),
        Some("http://localhost:4567")
    );
    assert!(broker.unix_socket.is_none());
    assert_eq!(fs::read_dir(&workspace).unwrap().count(), 1);
    assert!(!config.data_dir.exists());
    assert!(!config.state_dir.exists());
    assert!(!config.cache_dir.exists());
}

#[test]
fn rejects_unknown_or_invalid_sandbox_policy_without_echoing_policy_data() {
    let temp = TempDir::new().unwrap();
    for (case, fragment) in [
        "[sandbox]\nbackend='unexpected'",
        "[sandbox]\nunknown='SYNTHETIC-SENSITIVE-POLICY'",
        "[sandbox]\nread_roots=['']",
        "[sandbox]\nprotected_paths=[\"SYNTHETIC-SENSITIVE-POLICY\\n\"]",
        "[sandbox]\ncgroup_root='relative'",
        "[sandbox.resources]\ncpu_seconds=0",
        "[sandbox.resources.cpu_quota]\nquota_micros=1\nperiod_micros=1000",
        "[sandbox.network]\nmode='deny'\nallowed_origins=['https://example.invalid']",
        "[sandbox.network]\nmode='proxy'\nallowed_origins=['https://SYNTHETIC-SENSITIVE-POLICY@example.invalid']",
        "[sandbox.network]\nmode='proxy'\nallowed_origins=['https://example.invalid/path']",
        "[sandbox.network]\nmode='proxy'\nallowed_origins=['https://example.invalid']\nlocalhost_development_origin='https://example.invalid'",
        "[sandbox.network]\nmode='proxy'\nunix_socket='/SYNTHETIC-SENSITIVE-POLICY'",
        "[sandbox.network]\nmode='proxy'\n[sandbox.network.limits]\nmax_connections=0",
    ]
    .into_iter()
    .enumerate()
    {
        let path = write(
            temp.path(),
            "config.toml",
            &format!("schema_version=1\n{fragment}\n"),
        );
        let error = config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        )
        .err()
        .unwrap_or_else(|| panic!("invalid sandbox policy case {case} accepted"));
        assert_eq!(error.0.code, "invalid_config");
        assert!(!error.to_string().contains("SYNTHETIC-SENSITIVE-POLICY"));
    }
}

#[test]
fn enabled_mcp_sources_join_model_secret_environment_names() {
    let temp = TempDir::new().unwrap();
    let text = format!(
        "{}\n{}",
        fixture(),
        r#"[mcp.servers.local.transport]
type='stdio'
argv=['server','--stdio']
env={DESTINATION_TOKEN='SOURCE_TOKEN'}
[mcp.servers.remote.transport]
type='http'
endpoint='https://example.invalid/mcp'
bearer_env='HTTP_BEARER'
headers_env={X-Service-Key='HTTP_HEADER'}
[mcp.servers.disabled]
enabled=false
[mcp.servers.disabled.transport]
type='stdio'
argv=['disabled-server']
env={UNUSED_TOKEN='MISSING_DISABLED_SOURCE'}
"#
    );
    let path = write(temp.path(), "config.toml", &text);
    let environment = env(&[
        ("HOME", temp.path().to_str().unwrap()),
        ("EXAMPLE_KEY", "SYNTHETIC-MODEL-KEY"),
        ("SOURCE_TOKEN", "SYNTHETIC-LOCAL-TOKEN"),
        ("HTTP_BEARER", "SYNTHETIC-BEARER-TOKEN"),
        ("HTTP_HEADER", "SYNTHETIC-HEADER-TOKEN"),
    ]);
    let loaded = config::load(
        Overrides {
            config: Some(path),
            ..Default::default()
        },
        environment,
        temp.path(),
    )
    .unwrap();
    assert_eq!(loaded.mcp.servers.len(), 3);
    assert_eq!(
        loaded.secret_env_names().into_iter().collect::<Vec<_>>(),
        ["EXAMPLE_KEY", "HTTP_BEARER", "HTTP_HEADER", "SOURCE_TOKEN"]
    );
    assert_eq!(
        loaded.secret("SOURCE_TOKEN").unwrap(),
        "SYNTHETIC-LOCAL-TOKEN"
    );
}

#[test]
fn mcp_references_fail_closed_without_disclosing_values() {
    let temp = TempDir::new().unwrap();
    for (transport, value) in [
        ("type='stdio'\nargv=['server']\nenv={TOKEN='MISSING'}", None),
        (
            "type='stdio'\nargv=['server']\nenv={TOKEN='SOURCE'}",
            Some("SYNTHETIC-TOKEN\0VALUE"),
        ),
        (
            "type='http'\nendpoint='https://example.invalid/mcp'\nbearer_env='MISSING'",
            None,
        ),
        (
            "type='http'\nendpoint='https://example.invalid/mcp'\nbearer_env='SOURCE'",
            Some("SYNTHETIC-TOKEN\nVALUE"),
        ),
        (
            "type='http'\nendpoint='https://example.invalid/mcp'\nheaders_env={X-Key='SOURCE'}",
            Some("SYNTHETIC-TOKEN\rVALUE"),
        ),
        ("type='http'\nendpoint='http://example.invalid/mcp'", None),
        (
            "type='stdio'\nargv=['server']\nsecret='SYNTHETIC-TOKEN'",
            None,
        ),
    ] {
        let path = write(
            temp.path(),
            "config.toml",
            &format!("schema_version=1\n[mcp.servers.test.transport]\n{transport}\n"),
        );
        let mut pairs = vec![("HOME", temp.path().to_str().unwrap())];
        if let Some(value) = value {
            pairs.push(("SOURCE", value));
        }
        let error = config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            env(&pairs),
            temp.path(),
        )
        .err()
        .expect("invalid MCP configuration accepted");
        assert_eq!(error.0.code, "invalid_config");
        assert!(!error.to_string().contains("SYNTHETIC-TOKEN"));
    }
}

#[test]
fn n3_defaults_and_explicit_memory_switches_remain_host_selected_and_readonly() {
    let temp = TempDir::new().unwrap();
    let config_path = write(temp.path(), "config.toml", fixture());
    let overrides = Overrides {
        config: Some(config_path.clone()),
        ..Default::default()
    };
    let config = config::load(
        Overrides {
            config: Some(config_path.clone()),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    assert!(config.context.auto_compact);
    let budget = config.context.budget(&config.profiles["chat"]);
    assert_eq!(budget.window_tokens, 100000);
    assert_eq!(budget.output_reserve, 65_536);
    assert_eq!(budget.safety_margin, 10001);
    assert_eq!(budget.trigger_per_mille, 800);
    assert_eq!(budget.target_per_mille, 550);
    assert!(config.memory.read && config.memory.inject && config.memory.write);
    assert_eq!(config.memory_root().unwrap(), temp.path().join(".soyo"));
    assert!(!temp.path().join(".soyo").exists());
    let explicit = temp.path().join("private-memory");
    fs::write(&config_path,format!("{}\n[memory]\nread=false\ninject=true\nwrite=true\nroot={}\n[context]\nextra_safety_tokens=123\n",fixture(),toml::Value::String(explicit.to_string_lossy().into_owned()))).unwrap();
    let config = config::load(overrides, test_env(temp.path()), temp.path()).unwrap();
    assert!(!config.memory.read);
    assert!(config.memory.inject && config.memory.write);
    assert_eq!(config.memory_root().unwrap(), explicit);
    assert_eq!(
        config
            .context
            .budget(&config.profiles["chat"])
            .safety_margin,
        10124
    );
    assert!(!explicit.exists());
}

#[test]
fn n3_context_limits_and_unknown_settings_fail_closed_without_echoing_source() {
    let temp = TempDir::new().unwrap();
    for table in [
        "[context]\ntarget_percent=80\ntrigger_percent=80",
        "[context]\ntrigger_percent=101",
        "[context]\nsummary_max_output_tokens=0",
        "[context]\nmax_compaction_calls=65",
        "[context]\ncompaction_timeout_seconds=0",
        "[memory]\nsecret='SYNTHETIC-PRIVATE-VALUE'",
        "[plugins]\nsecret='SYNTHETIC-PRIVATE-VALUE'",
    ] {
        let path = write(
            temp.path(),
            "config.toml",
            &format!("{}\n{table}\n", fixture()),
        );
        let error = config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        )
        .err()
        .expect("invalid N3 setting accepted");
        assert_eq!(error.0.code, "invalid_config");
        assert!(!error.to_string().contains("SYNTHETIC-PRIVATE-VALUE"));
        assert!(!temp.path().join(".soyo").exists());
    }
}

#[test]
fn dev079_web_and_browser_settings_default_off_and_validate_strictly() {
    let temp = TempDir::new().unwrap();
    let path = write(temp.path(), "config.toml", &format!("{}\n", fixture()));
    let config = config::load(
        Overrides {
            config: Some(path.clone()),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    assert!(!config.web.enabled);
    assert!(!config.browser.enabled);
    assert!(!config.browser.worker.enabled);
    assert!(!config.browser.vm.enabled);
    assert!(!config.browser.network.enabled);
    assert!(config.browser.network.allowed_origins.is_empty());
    assert!(!config.browser.network.public_web);
    assert_eq!(config.browser.vm.connect_timeout_ms, 20_000);
    assert_eq!(config.browser.vm.request_timeout_ms, 120_000);
    assert_eq!(config.browser.vm.cleanup_timeout_ms, 10_000);
    assert_eq!(config.browser.vm.stderr_limit_bytes, 64 * 1024);
    assert_eq!(config.browser.worker.connect_timeout_ms, 20_000);
    assert_eq!(config.browser.worker.request_timeout_ms, 120_000);
    assert_eq!(config.browser.worker.stderr_limit_bytes, 64 * 1024);
    assert!(config.web.allowed_origins.is_empty());
    assert!(config.web.search.is_none());

    let worker_root = temp.path().join("worker");
    let digest = "0".repeat(64);
    let worker = format!(
        "[browser.worker]\nenabled=true\nnode=\"{}\"\nworker_root=\"{}\"\nchromium=\"{}\"\nidentity_manifest=\"{}\"\nidentity_sha256=\"{digest}\"\ntemp_root=\"{}\"\nproxy=\"http://127.0.0.1:3128\"\n",
        temp.path().join("node").display(),
        worker_root.display(),
        temp.path().join("chromium").display(),
        temp.path().join("identity.json").display(),
        temp.path().display(),
    );
    let path = write(
        temp.path(),
        "config.toml",
        &format!("{}\n{worker}\n", fixture()),
    );
    let config = config::load(
        Overrides {
            config: Some(path.clone()),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    assert!(config.browser.worker.enabled);
    assert_eq!(config.browser.worker.identity_sha256, Some(digest));
    assert_eq!(
        config.browser.worker.proxy.as_deref(),
        Some("http://127.0.0.1:3128")
    );

    let valid = format!(
        "{}\n[web]\nenabled=true\nallowed_origins=[\"http://127.0.0.1:8080\"]\nlocalhost_development_origin=\"http://127.0.0.1:8080\"\n[web.search]\nendpoint=\"http://127.0.0.1:8080/search\"\nmax_results=5\n",
        fixture()
    );
    fs::write(&path, valid).unwrap();
    let config = config::load(
        Overrides {
            config: Some(path.clone()),
            ..Default::default()
        },
        test_env(temp.path()),
        temp.path(),
    )
    .unwrap();
    assert!(config.web.enabled);
    assert!(config.web.search.is_some());

    for table in [
        "[web]\nenabled=true",
        "[web]\nenabled=false\nmax_redirects=6",
        "[web]\nenabled=false\nselector=\"body\"",
        "[web]\nenabled=true\nallowed_origins=[\"https://example.com\"]\n[web.search]\nendpoint=\"https://other.example/search\"",
        "[browser]\nmax_wait_ms=0",
        "[browser]\nmax_snapshot_bytes=0",
        "[browser]\ninstall=true",
        "[browser.worker]\nenabled=true",
        "[browser.worker]\nnode=\"relative/node\"",
        "[browser.worker]\nidentity_sha256=\"ABC\"",
        "[browser.worker]\nproxy=\"http://user:pass@127.0.0.1:3128\"",
        "[browser.worker]\nproxy=\"https://127.0.0.1:3128\"",
        "[browser.worker]\nproxy=\"http://192.0.2.1:3128\"",
        "[browser.worker]\nproxy=\"http://localhost:3128\"",
        "[browser.worker]\nconnect_timeout_ms=1",
        "[browser.worker]\nstderr_limit_bytes=1",
        "[browser.vm]\nenabled=true",
        "[browser.vm]\nbackend=\"docker\"",
        "[browser.vm]\nssh=\"relative/ssh\"",
        "[browser.vm]\nhost=\"example.com\"",
        "[browser.vm]\nuser=\"root@example.com\"",
        "[browser.vm]\nport=0",
        "[browser.vm]\nnode=\"/opt/node;id\"",
        "[browser.vm]\nidentity_sha256=\"ABC\"",
        "[browser.vm]\ncleanup_timeout_ms=1",
        "[browser.vm]\nstderr_limit_bytes=1",
        "[browser.network]\nenabled=true",
        "[browser.network]\nenabled=true\nallowed_origins=[\"https://example.com/path\"]",
        "[browser.network]\nenabled=true\npublic_web=false\nallowed_origins=[]",
        "[browser.network]\nenabled=true\nallowed_origins=[\"https://example.com\"]\nlocalhost_development_origin=\"http://127.0.0.1:8080\"",
        "[browser.network]\nenabled=true\npublic_web=true\n[browser.network.limits]\nmax_connections=0",
        "[browser.network]\nenabled=true\nallowed_origins=[\"http://127.0.0.1:8080\"]",
    ] {
        let path = write(
            temp.path(),
            "config.toml",
            &format!("{}\n{table}\n", fixture()),
        );
        let error = config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        )
        .err()
        .expect("invalid DEV-079 setting accepted");
        assert_eq!(error.0.code, "invalid_config", "{table}");
    }
}

#[test]
fn browser_credentials_validate_without_browser_enablement_and_require_https() {
    let temp = TempDir::new().unwrap();
    let store = temp.path().join("credentials");
    let credential_ref = "018f4f7e-7a3c-7c4e-9f5a-123456789abc";
    for (origin, accepted) in [
        ("https://login.example", true),
        ("http://localhost:8080", true),
        ("http://127.0.0.1:8080", true),
        ("http://login.example", false),
    ] {
        let text = format!(
            "{}\n[browser]\nenabled = false\n[browser.credentials]\nenabled = true\nstore = \"{}\"\n[[browser.credentials.slots]]\nalias = \"login\"\ncredential_ref = \"{credential_ref}\"\norigin = \"{origin}\"\npurpose = \"form_fill\"\nmax_uses = 1\n",
            fixture(),
            store.display(),
        );
        let path = write(temp.path(), "config.toml", &text);
        let result = config::load(
            Overrides {
                config: Some(path),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        );
        if accepted {
            let config = result.expect("valid credential slot rejected");
            assert!(!config.browser.enabled);
            assert!(config.browser.credentials.enabled);
        } else {
            let error = result.err().expect("HTTP credential slot accepted");
            assert_eq!(error.0.code, "invalid_config");
        }
    }
}

#[test]
fn sandbox_presets_add_safe_existing_toolchain_roots_without_changing_defaults() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("home");
    let rustup = home.join(".rustup");
    let cargo = home.join(".cargo");
    let soyo = home.join(".soyo");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&rustup).unwrap();
    fs::create_dir_all(&cargo).unwrap();
    fs::create_dir_all(&soyo).unwrap();

    let base = config::load(
        Overrides {
            config: Some(write(temp.path(), "base.toml", fixture())),
            ..Default::default()
        },
        env(&[
            ("HOME", home.to_str().unwrap()),
            ("EXAMPLE_KEY", "fixture-secret"),
        ]),
        &workspace,
    )
    .unwrap();
    assert!(!base.profiles["anthropic"].prompt_cache);
    let (base_policy, _) = base
        .sandbox
        .resolve_with_environment(&workspace, base.environment())
        .unwrap();
    assert_eq!(base_policy.read_roots, std::slice::from_ref(&workspace));
    assert!(!base_policy.synthetic_etc_identity);
    assert_eq!(
        serde_json::to_vec(&base.sandbox).unwrap(),
        serde_json::to_vec(&SandboxSettings::default()).unwrap()
    );

    let preset_toml = format!(
        "{}\n[sandbox]\nca_certs = true\ntoolchain_presets = [\"rust\", \"python\", \"minimal_etc\"]\n",
        fixture().replace(
            "api_version = \"2023-06-01\"",
            "api_version = \"2023-06-01\"\nprompt_cache = true"
        )
    );
    let configured = config::load(
        Overrides {
            config: Some(write(temp.path(), "presets.toml", &preset_toml)),
            ..Default::default()
        },
        env(&[
            ("HOME", home.to_str().unwrap()),
            ("EXAMPLE_KEY", "fixture-secret"),
            ("RUSTUP_HOME", rustup.to_str().unwrap()),
            // Hostile overrides pointing at root, HOME, and .soyo must all be skipped.
            ("CARGO_HOME", "/"),
            ("PYENV_ROOT", home.to_str().unwrap()),
            ("VIRTUAL_ENV", soyo.to_str().unwrap()),
        ]),
        &workspace,
    )
    .unwrap();
    assert!(configured.profiles["anthropic"].prompt_cache);
    assert!(configured.sandbox.ca_certs);
    assert_eq!(
        configured.sandbox.toolchain_presets,
        vec![
            ToolchainPreset::Rust,
            ToolchainPreset::Python,
            ToolchainPreset::MinimalEtc,
        ]
    );
    let (policy, _) = configured
        .sandbox
        .resolve_with_environment(&workspace, configured.environment())
        .unwrap();
    assert!(policy.synthetic_etc_identity);
    assert!(policy.read_roots.contains(&rustup));
    assert!(policy.read_roots.contains(&cargo));
    assert!(!policy.read_roots.iter().any(|p| p == Path::new("/")));
    assert!(!policy.read_roots.iter().any(|p| p == &home));
    assert!(!policy.read_roots.iter().any(|p| p == &soyo));
    if Path::new("/etc/ssl").is_dir() {
        assert!(policy.read_roots.iter().any(|p| p == Path::new("/etc/ssl")));
    }
}

#[test]
fn anthropic_official_provider_flag_supports_alias_and_validates_protocol_and_version() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir(&workspace).unwrap();

    let base_env = || {
        env(&[
            ("HOME", temp.path().to_str().unwrap()),
            ("EXAMPLE_KEY", "fixture-secret"),
        ])
    };
    let base_toml = fixture().replace(
        "api_version = \"2023-06-01\"",
        "api_version = \"2026-10-21\"",
    );
    let base = config::load(
        Overrides {
            config: Some(write(temp.path(), "base.toml", &base_toml)),
            ..Default::default()
        },
        base_env(),
        &workspace,
    )
    .unwrap();
    assert!(!base.profiles["anthropic"].anthropic_official_provider);

    let official_toml = base_toml.replace(
        "api_version = \"2026-10-21\"",
        "api_version = \"2026-10-21\"\nanthropic_official_provider = true",
    );
    let official = config::load(
        Overrides {
            config: Some(write(temp.path(), "official.toml", &official_toml)),
            ..Default::default()
        },
        base_env(),
        &workspace,
    )
    .unwrap();
    assert!(official.profiles["anthropic"].anthropic_official_provider);
    assert_ne!(
        base.profiles["anthropic"].fingerprint().unwrap(),
        official.profiles["anthropic"].fingerprint().unwrap()
    );

    let alias_toml = base_toml.replace(
        "api_version = \"2026-10-21\"",
        "api_version = \"2026-10-21\"\nanthropic_offical_provider = true",
    );
    let aliased = config::load(
        Overrides {
            config: Some(write(temp.path(), "alias.toml", &alias_toml)),
            ..Default::default()
        },
        base_env(),
        &workspace,
    )
    .unwrap();
    assert!(aliased.profiles["anthropic"].anthropic_official_provider);
    assert_eq!(
        official.profiles["anthropic"].fingerprint().unwrap(),
        aliased.profiles["anthropic"].fingerprint().unwrap()
    );

    let wrong_version = fixture().replace(
        "api_version = \"2023-06-01\"",
        "api_version = \"2023-06-01\"\nanthropic_official_provider = true",
    );
    let err = config::load(
        Overrides {
            config: Some(write(temp.path(), "old-version.toml", &wrong_version)),
            ..Default::default()
        },
        base_env(),
        &workspace,
    )
    .err()
    .unwrap();
    assert_eq!(err.0.code, "invalid_config");
    assert!(err.0.message.contains("2026-10-21"));

    let wrong_protocol = base_toml.replace(
        "protocol = \"openai-chat-completions\"",
        "protocol = \"openai-chat-completions\"\nanthropic_official_provider = true",
    );
    let err = config::load(
        Overrides {
            config: Some(write(temp.path(), "wrong-protocol.toml", &wrong_protocol)),
            ..Default::default()
        },
        base_env(),
        &workspace,
    )
    .err()
    .unwrap();
    assert_eq!(err.0.code, "invalid_config");
    assert!(err.0.message.contains("anthropic-messages"));
}

#[test]
fn soyo_env_names_preferred_and_saya_names_still_resolve() {
    let temp = TempDir::new().unwrap();
    let config_path = write(temp.path(), "config.toml", fixture());
    // Legacy SAYA_* values still drive config resolution.
    let legacy = config::load(
        Overrides::default(),
        env(&[
            ("HOME", temp.path().to_str().unwrap()),
            ("EXAMPLE_KEY", "fixture-secret"),
            ("SAYA_CONFIG", config_path.to_str().unwrap()),
            ("SAYA_PROFILE", "responses"),
            ("SAYA_MODE", "plan"),
        ]),
        temp.path(),
    )
    .unwrap();
    assert!(legacy.profiles.contains_key("responses"));
    assert_eq!(legacy.mode, WorkMode::Plan);
    // SOYO_* wins when both namespaces are set.
    let shadow = write(temp.path(), "shadow.toml", "not valid TOML");
    let preferred = config::load(
        Overrides::default(),
        env(&[
            ("HOME", temp.path().to_str().unwrap()),
            ("EXAMPLE_KEY", "fixture-secret"),
            ("SAYA_CONFIG", shadow.to_str().unwrap()),
            ("SOYO_CONFIG", config_path.to_str().unwrap()),
        ]),
        temp.path(),
    );
    assert!(preferred.is_ok(), "SOYO_CONFIG must win over SAYA_CONFIG");
}

#[test]
fn memory_root_prefers_soyo_and_falls_back_to_saya() {
    let temp = TempDir::new().unwrap();
    let config_path = write(temp.path(), "config.toml", fixture());
    let load = || {
        config::load(
            Overrides {
                config: Some(config_path.clone()),
                ..Default::default()
            },
            test_env(temp.path()),
            temp.path(),
        )
        .unwrap()
    };
    assert_eq!(load().memory_root().unwrap(), temp.path().join(".soyo"));
    // Non-memory files in ~/.soyo (e.g. desktop preferences or global prompt) must
    // not shadow existing global memory stored under legacy ~/.saya.
    fs::create_dir_all(temp.path().join(".soyo")).unwrap();
    fs::write(temp.path().join(".soyo/desktop_prefs.json"), "{}").unwrap();
    fs::create_dir_all(temp.path().join(".saya/memory")).unwrap();
    assert_eq!(load().memory_root().unwrap(), temp.path().join(".saya"));
    fs::write(temp.path().join(".soyo/MEMORY.md"), "# memory").unwrap();
    assert_eq!(load().memory_root().unwrap(), temp.path().join(".soyo"));
}
