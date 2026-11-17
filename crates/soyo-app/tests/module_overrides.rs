use soyo_app::{apply_module_overrides, assemble_browser_provider, goal::goal_profile_fingerprint};
use soyo_core::{
    browser::vm::BrowserVmSettings,
    config::{self, Environment, Overrides},
    modules::{ModuleKind, ModuleStore, module_db_path},
    tools::exec::ExecConfig,
};
use std::{ffi::OsString, fs, path::PathBuf};
use tempfile::TempDir;

struct Fixture {
    _root: TempDir,
    home: PathBuf,
    cwd: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let cwd = root.path().join("work");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&cwd).unwrap();
        Self {
            _root: root,
            home,
            cwd,
        }
    }

    fn load(&self, extra_toml: &str) -> config::Config {
        let cfg = self._root.path().join("config.toml");
        fs::write(
            &cfg,
            format!(
                r#"schema_version = 1

[defaults]
profile = "main"

[profiles.main]
protocol = "openai-chat-completions"
base_url = "https://example.invalid/v1"
model = "test-model"
auth = "none"
context_window_tokens = 128000
stream = false

{extra_toml}
"#
            ),
        )
        .unwrap();
        let env =
            Environment::from_pairs([(OsString::from("HOME"), self.home.clone().into_os_string())]);
        config::load(
            Overrides {
                config: Some(cfg),
                ..Default::default()
            },
            env,
            &self.cwd,
        )
        .unwrap()
    }
}

fn full_vm_settings() -> BrowserVmSettings {
    BrowserVmSettings {
        enabled: false,
        colima: Some(PathBuf::from("/nonexistent/colima")),
        ssh: Some(PathBuf::from("/nonexistent/ssh")),
        profile: Some("soyo-browser".into()),
        host: Some("127.0.0.1".into()),
        user: Some("soyo".into()),
        port: Some(2222),
        identity_file: Some(PathBuf::from("/nonexistent/key")),
        known_hosts: Some(PathBuf::from("/nonexistent/known_hosts")),
        node: Some("/opt/soyo/node".into()),
        worker_root: Some("/opt/soyo/worker".into()),
        chromium: Some("/opt/soyo/chromium".into()),
        identity_manifest: Some("/opt/soyo/identity.json".into()),
        identity_sha256: Some("0".repeat(64)),
        remote_temp_root: Some("/tmp/saya-runs".into()),
        ..Default::default()
    }
}

#[test]
fn module_browser_toggle_controls_assembly_without_creating_db_on_read() {
    let fixture = Fixture::new();
    let cfg = fixture.load("");
    assert!(!module_db_path(&cfg.data_dir).exists());

    assert!(
        assemble_browser_provider(&cfg, ExecConfig::default(), "r1")
            .unwrap()
            .is_none()
    );
    assert!(!module_db_path(&cfg.data_dir).exists());

    let mut store = ModuleStore::open(&cfg.data_dir).unwrap();
    store.set(&cfg.browser, ModuleKind::Browser, true).unwrap();

    // Browser is enabled via module store without any backend enabled: fail closed.
    let err = assemble_browser_provider(&cfg, ExecConfig::default(), "r2").unwrap_err();
    assert_eq!(err.0.code, "invalid_config");

    store.set(&cfg.browser, ModuleKind::Browser, false).unwrap();
    assert!(
        assemble_browser_provider(&cfg, ExecConfig::default(), "r3")
            .unwrap()
            .is_none()
    );
}

#[test]
fn module_vm_toggle_controls_d066_without_native_fallback() {
    let fixture = Fixture::new();
    let mut cfg = fixture.load(
        "[browser]\nenabled = true\n[browser.network]\nenabled = true\npublic_web = true\n[sandbox]\nbackend = \"host\"\n",
    );
    cfg.browser.vm = full_vm_settings();

    // With vm disabled via config, enabling vm in module store switches assembly
    // to VM validation/preflight instead of native/worker fallback.
    let mut store = ModuleStore::open(&cfg.data_dir).unwrap();
    store.set(&cfg.browser, ModuleKind::Vm, true).unwrap();
    let err = assemble_browser_provider(&cfg, ExecConfig::default(), "r1").unwrap_err();
    assert!(
        matches!(
            err.0.code.as_str(),
            "invalid_config" | "browser_vm_unavailable"
        ),
        "unexpected error code: {}",
        err.0.code
    );

    // Disabling browser via module store turns off provider assembly cleanly even
    // when config.toml has [browser].enabled = true.
    store.set(&cfg.browser, ModuleKind::Browser, false).unwrap();
    assert!(
        assemble_browser_provider(&cfg, ExecConfig::default(), "r2")
            .unwrap()
            .is_none()
    );

    let mut applied = cfg.clone();
    apply_module_overrides(&mut applied).unwrap();
    assert!(!applied.browser.enabled);
    assert!(applied.browser.vm.enabled);
}

#[test]
fn module_toggles_change_goal_profile_fingerprint() {
    let fixture = Fixture::new();
    let cfg = fixture.load("");
    let baseline = goal_profile_fingerprint(&cfg, "main").unwrap();
    assert!(!module_db_path(&cfg.data_dir).exists());

    {
        let mut store = ModuleStore::open(&cfg.data_dir).unwrap();
        store.set(&cfg.browser, ModuleKind::Browser, true).unwrap();
    }
    let with_browser = goal_profile_fingerprint(&cfg, "main").unwrap();
    assert_ne!(baseline, with_browser);

    {
        let mut store = ModuleStore::open(&cfg.data_dir).unwrap();
        store.set(&cfg.browser, ModuleKind::Browser, false).unwrap();
    }
    let disabled_browser = goal_profile_fingerprint(&cfg, "main").unwrap();
    assert_eq!(baseline, disabled_browser);
}
