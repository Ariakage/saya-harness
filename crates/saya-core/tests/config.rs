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
            assert!(!temp.path().join(".saya").exists());
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
    let default_path = write(home, ".config/saya/config.toml", fixture());
    write(home, "workspace/saya.toml", "not valid TOML");
    write(home, "workspace/.saya/config.toml", "not valid TOML");
    write(home, "workspace/.agents/config.toml", "not valid TOML");
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
    assert_eq!(c.profiles["chat"].max_output_tokens, 65_536);
    assert!(!c.profiles["chat"].strict_tools);
    assert_eq!(c.review.max_output_tokens, None);
    assert_eq!(c.limits.max_model_requests, 10);
    assert_eq!(c.sandbox.backend, saya_core::execution::BackendKind::Auto);
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
            ("SAYA_CONFIG", "settings/config.toml"),
            ("SAYA_CWD", "workspace"),
            ("SAYA_DATA_DIR", "session-data/nested"),
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
    use saya_core::{config::SandboxNetworkSettings, execution::BackendKind};
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
    use saya_core::execution::{BackendKind, NetworkPolicy};
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
cgroup_root='/delegated/saya'
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
        Some(Path::new("/delegated/saya"))
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
    assert_eq!(config.memory_root().unwrap(), temp.path().join(".saya"));
    assert!(!temp.path().join(".saya").exists());
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
        assert!(!temp.path().join(".saya").exists());
    }
}
