//! End-to-end socket round-trip tests: a real `HelperServer` on a temp
//! `SOYO_HELPER_DIR`, a real `HelperClient`, real NDJSON frames.

#[cfg(test)]
mod tests {
    use crate::server::HelperServer;
    use crate::service::HostService;
    use serde_json::json;
    use soyo_ipc::{HelperClient, helper_pid_path};
    use std::sync::OnceLock;
    use tokio::sync::Mutex;

    /// `SOYO_HELPER_DIR` is process-global; serialize socket tests.
    static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

    async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
        SERIAL.get_or_init(|| Mutex::new(())).lock().await
    }

    fn use_temp_helper_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("helper");
        // SAFETY: tests in this crate set SOYO_HELPER_DIR before any
        // server/client reads it; runs are serialized by `serial()`.
        unsafe {
            std::env::set_var("SOYO_HELPER_DIR", &helper);
        }
        dir
    }

    async fn running_server() -> (tempfile::TempDir, tokio::task::JoinHandle<()>) {
        let dir = use_temp_helper_dir();
        let server = HelperServer::new(HostService::from_environment());
        let handle = tokio::spawn(async move {
            if let Err(err) = server.run().await {
                eprintln!("test helper server error: {}: {}", err.code, err.message);
            }
        });
        // Wait for the socket to accept.
        for _ in 0..100 {
            if HelperClient::connect().await.is_ok() {
                return (dir, handle);
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("helper server did not start listening");
    }

    #[tokio::test]
    async fn client_call_round_trip_and_shutdown() {
        let _guard = serial().await;
        let (_dir, handle) = running_server().await;
        let client = HelperClient::connect().await.expect("connect");
        let bootstrap: serde_json::Value = client
            .call_typed("get_bootstrap_state", &json!({}))
            .await
            .expect("bootstrap call");
        assert!(bootstrap.get("version").is_some());
        assert!(bootstrap.get("config_ready").is_some());

        // Unknown method is a clean error frame, not a crash.
        let err = client.call("no_such_method", json!({})).await.unwrap_err();
        assert_eq!(err.code, "unknown_method");

        client.shutdown_daemon().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        // Socket must stop accepting after shutdown.
        assert!(HelperClient::connect().await.is_err());
        assert!(!helper_pid_path().exists());
    }

    #[tokio::test]
    async fn stale_socket_is_reclaimed_on_bind() {
        let _guard = serial().await;
        let _dir = use_temp_helper_dir();
        let target = soyo_ipc::HelperTarget::from_environment().unwrap();
        soyo_fs::ensure_private_dir(&target.helper_dir(), "helper", "helper").unwrap();
        std::fs::write(target.socket_path(), b"stale").unwrap();
        let (_dir, handle) = running_server().await;
        let client = HelperClient::connect().await.expect("connect");
        client.shutdown_daemon().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    async fn server_exits_when_socket_file_removed() {
        let _guard = serial().await;
        let (_dir, handle) = running_server().await;
        let target = soyo_ipc::HelperTarget::from_environment().unwrap();
        let sock = target.socket_path();
        assert!(sock.exists());
        // Simulate tempdir/cleanup removal: the helper is unreachable forever.
        std::fs::remove_file(&sock).unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(
            result.is_ok(),
            "helper server should exit after its socket path is removed"
        );
    }

    #[tokio::test]
    async fn helper_info_reports_process_identity_and_compat() {
        let _guard = serial().await;
        let (_dir, handle) = running_server().await;
        let client = HelperClient::connect().await.expect("connect");

        let info: serde_json::Value = client
            .call_typed("helper_info", &json!({}))
            .await
            .expect("helper_info call");
        assert_eq!(info["version"], env!("CARGO_PKG_VERSION"));
        assert!(info["pid"].as_u64().is_some_and(|pid| pid > 0));
        assert!(!info["exe_path"].as_str().unwrap_or_default().is_empty());
        assert_eq!(info["run_active"], false);

        // Same workspace build, indeterminate exe comparison → no mismatch.
        let compat = client.helper_compat().await;
        assert!(!compat.mismatch, "{}", compat.detail);

        client.shutdown_daemon().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }

    /// Process-global env vars must be restored on drop so other tests in the
    /// same process never observe a leaked configuration.
    struct EnvGuard(Vec<(String, Option<std::ffi::OsString>)>);

    impl EnvGuard {
        fn set(vars: &[(&str, &str)]) -> Self {
            let saved = vars
                .iter()
                .map(|(key, _)| (key.to_string(), std::env::var_os(key)))
                .collect();
            // SAFETY: all env-mutating tests in this crate hold `serial()`.
            unsafe {
                for (key, value) in vars {
                    std::env::set_var(key, value);
                }
            }
            Self(saved)
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: same serialization as `set`.
            unsafe {
                for (key, value) in &self.0 {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
            }
        }
    }

    fn setup_params() -> soyo_ipc::types::SetupEnvironmentParams {
        soyo_ipc::types::SetupEnvironmentParams {
            profile_name: "openai".into(),
            protocol: "openai-responses".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: "gpt-test".into(),
            auth: Some("bearer".into()),
            api_key_env: Some("SOYO_SETUP_TEST_KEY".into()),
            api_key: Some("sk-test-123".into()),
            context_window_tokens: Some(64_000),
            max_output_tokens: Some(4_096),
            workspace: None,
            permission_mode: Some(soyo_ipc::types::PermissionModeDto::Manual),
            work_mode: None,
        }
    }

    #[tokio::test]
    async fn setup_environment_writes_config_overlay_and_bootstrap() {
        let _guard = serial().await;
        let dir = tempfile::tempdir().unwrap();
        let helper = dir.path().join("helper");
        let state_home = dir.path().join("state");
        let config_path = dir.path().join("config.toml");
        let _env = EnvGuard::set(&[
            ("SOYO_HELPER_DIR", helper.to_str().unwrap()),
            ("XDG_STATE_HOME", state_home.to_str().unwrap()),
            ("SOYO_CONFIG", config_path.to_str().unwrap()),
        ]);
        let service = HostService::from_environment();
        assert!(!service.get_bootstrap_state().await.config_ready);

        let boot = service
            .setup_environment(setup_params())
            .await
            .expect("setup should succeed");

        assert!(boot.config_ready);
        assert_eq!(boot.selected_profile.as_deref(), Some("openai"));

        // config.toml references the variable name, never the secret value.
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(
            text.contains("api_key_env = \"SOYO_SETUP_TEST_KEY\""),
            "{text}"
        );
        assert!(text.contains("permissions = \"manual\""), "{text}");
        assert!(!text.contains("sk-test-123"), "{text}");

        // The secret lives only in `<state_dir>/secrets/env` (0700 dir, 0600 file).
        let env_path = state_home.join("soyo").join("secrets").join("env");
        let overlay = std::fs::read_to_string(&env_path).unwrap();
        assert!(
            overlay.contains("SOYO_SETUP_TEST_KEY=sk-test-123"),
            "{overlay}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(env_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&env_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }

        // Re-running merges into the same document instead of replacing it.
        let mut second = setup_params();
        second.profile_name = "secondary".into();
        second.auth = Some("none".into());
        second.api_key_env = None;
        second.api_key = None;
        second.base_url = "http://localhost:11434/v1".into();
        service
            .setup_environment(second)
            .await
            .expect("second profile merge");
        let text = std::fs::read_to_string(&config_path).unwrap();
        assert!(text.contains("[profiles.openai]"), "{text}");
        assert!(text.contains("[profiles.secondary]"), "{text}");

        // A missing workspace directory is created instead of rejected.
        let nested = dir.path().join("nested").join("work");
        let mut third = setup_params();
        third.profile_name = "third".into();
        third.auth = Some("none".into());
        third.api_key_env = None;
        third.api_key = None;
        third.base_url = "http://localhost:11434/v1".into();
        third.workspace = Some(nested.to_str().unwrap().to_owned());
        let boot = service
            .setup_environment(third)
            .await
            .expect("setup creates missing workspace");
        assert!(nested.is_dir());
        assert_eq!(
            boot.workspace,
            nested.canonicalize().unwrap().to_string_lossy().to_string()
        );
    }

    #[tokio::test]
    async fn helper_permissions_and_flock_serialization() {
        let _guard = serial().await;
        let (_dir, handle) = running_server().await;
        let target = soyo_ipc::HelperTarget::from_environment().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(target.helper_dir())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let sock_mode = std::fs::metadata(target.socket_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let pid_mode = std::fs::metadata(target.pid_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(sock_mode, 0o600);
            assert_eq!(pid_mode, 0o600);
        }

        // Launching a competing server against the same HelperTarget must fail
        // with helper_already_running without disturbing the active server.
        let rival = HelperServer::new(HostService::from_environment());
        let err = rival.run().await.unwrap_err();
        assert_eq!(err.code, "helper_already_running");

        let client = HelperClient::connect()
            .await
            .expect("live helper stays reachable");
        client.shutdown_daemon().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    async fn legacy_env_migrates_once_without_clobbering() {
        let _guard = serial().await;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let legacy_dir = home.join(".soyo");
        let legacy_env = legacy_dir.join("env");
        let state_dir = dir.path().join("state");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(&legacy_env, "SOYO_MIGRATE_TEST=migrated-secret\n").unwrap();

        let _env = EnvGuard::set(&[("HOME", home.to_str().unwrap())]);
        crate::envfile::migrate_legacy_env(&state_dir);

        let dest = crate::envfile::env_file_path_for(&state_dir);
        let migrated = std::fs::read_to_string(&dest).unwrap();
        assert!(
            migrated.contains("SOYO_MIGRATE_TEST=migrated-secret"),
            "{migrated}"
        );
        assert!(!legacy_env.exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = std::fs::metadata(dest.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let file_mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(file_mode, 0o600);
        }

        // Re-introducing a legacy env file later must not overwrite destination.
        std::fs::write(&legacy_env, "SOYO_MIGRATE_TEST=must-not-overwrite\n").unwrap();
        crate::envfile::migrate_legacy_env(&state_dir);
        let after = std::fs::read_to_string(&dest).unwrap();
        assert!(after.contains("migrated-secret"));
        assert!(!after.contains("must-not-overwrite"));
    }

    #[tokio::test]
    async fn setup_environment_rejects_invalid_params_and_restores() {
        let _guard = serial().await;
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let _env = EnvGuard::set(&[
            (
                "SOYO_HELPER_DIR",
                dir.path().join("helper").to_str().unwrap(),
            ),
            ("SOYO_CONFIG", config_path.to_str().unwrap()),
        ]);
        let service = HostService::from_environment();

        // Bearer auth requires an env-var name to reference.
        let mut params = setup_params();
        params.api_key_env = None;
        let err = service.setup_environment(params).await.unwrap_err();
        assert_eq!(err.code, "invalid_args");

        // auth `none` cannot carry key material.
        let mut params = setup_params();
        params.auth = Some("none".into());
        let err = service.setup_environment(params).await.unwrap_err();
        assert_eq!(err.code, "invalid_args");

        // Profile names are restricted to TOML-safe keys.
        let mut params = setup_params();
        params.profile_name = "bad name!".into();
        let err = service.setup_environment(params).await.unwrap_err();
        assert_eq!(err.code, "invalid_args");

        // A config that fails validation leaves no partial file behind.
        let mut params = setup_params();
        params.base_url = "http://example.com".into();
        let err = service.setup_environment(params).await.unwrap_err();
        assert_ne!(err.code, "invalid_args");
        assert!(!config_path.exists());
    }
}
