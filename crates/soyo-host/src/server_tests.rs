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
        // SAFETY: tests in this crate set SOYO_HELPER_DIR before any
        // server/client reads it; runs are serialized by the Once + this
        // being the only module touching the variable.
        unsafe {
            std::env::set_var("SOYO_HELPER_DIR", dir.path());
        }
        dir
    }

    async fn running_server() -> (tempfile::TempDir, tokio::task::JoinHandle<()>) {
        let dir = use_temp_helper_dir();
        let server = HelperServer::new(HostService::from_environment());
        let handle = tokio::spawn(async move {
            let _ = server.run().await;
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
        let dir = use_temp_helper_dir();
        std::fs::create_dir_all(dir.path()).unwrap();
        let stale = dir.path().join("helper.sock");
        std::fs::write(&stale, b"stale").unwrap();
        let (_dir, handle) = running_server().await;
        let client = HelperClient::connect().await.expect("connect");
        client.shutdown_daemon().await;
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
    }

    #[tokio::test]
    async fn server_exits_when_socket_file_removed() {
        let _guard = serial().await;
        let (dir, handle) = running_server().await;
        let sock = dir.path().join("helper.sock");
        assert!(sock.exists());
        // Simulate tempdir/cleanup removal: the helper is unreachable forever.
        std::fs::remove_file(&sock).unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        assert!(
            result.is_ok(),
            "helper server should exit after its socket path is removed"
        );
    }
}
