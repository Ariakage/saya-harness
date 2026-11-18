//! `soyo helper` (run the shared backend daemon) and `soyo kill` (stop it).

use soyo_core::config::Environment;
use soyo_host::{HelperServer, HostService};
use soyo_ipc::{HelperClient, HelperPidRecord, HelperTarget, process_exe_path, process_started_id};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

fn resolve_target(config: Option<&Path>, data_dir: Option<&Path>) -> HelperTarget {
    let env = Environment::capture();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    HelperTarget::from_overrides(config, data_dir, &env, &cwd).unwrap_or_else(|_| {
        HelperTarget::from_paths(
            PathBuf::from("config.toml"),
            PathBuf::from("data"),
            PathBuf::from("state"),
        )
    })
}

/// Run the helper daemon in the foreground (clients spawn it detached).
pub async fn run_helper(config: Option<&Path>, data_dir: Option<&Path>) -> u8 {
    let service = HostService::from_overrides(config, data_dir);
    let server = HelperServer::new(service);
    match server.run().await {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("soyo helper: {}: {}", err.code, err.message);
            1
        }
    }
}

/// Ask the helper matching `(config, data_dir)` to shut down; falls back to the
/// pid file only after verifying live process identity (`pid`, start timestamp,
/// exe path, and configuration fingerprint).
pub async fn kill_helper(config: Option<&Path>, data_dir: Option<&Path>) -> u8 {
    let target = resolve_target(config, data_dir);
    match HelperClient::connect_for(&target).await {
        Ok(client) => {
            client.shutdown_daemon().await;
            for _ in 0..20 {
                if HelperClient::connect_for(&target).await.is_err() {
                    println!("soyo helper stopped.");
                    return 0;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            eprintln!("soyo helper: shutdown requested but still accepting connections");
            1
        }
        Err(_) => {
            let pid_path = target.pid_path();
            let record = match soyo_fs::open_existing_private_file_read(
                &pid_path,
                "helper_pid",
                "helper pid",
            ) {
                Ok(Some((file, _))) => {
                    let mut raw = String::new();
                    if file.take(16 * 1024).read_to_string(&mut raw).is_ok() {
                        serde_json::from_str::<HelperPidRecord>(&raw).ok()
                    } else {
                        None
                    }
                }
                _ => None,
            };
            match record {
                Some(rec) if verified_helper_process(&rec, &target) => {
                    unsafe {
                        libc::kill(rec.pid as i32, libc::SIGTERM);
                    }
                    println!("soyo helper terminated (pid {}).", rec.pid);
                    0
                }
                _ => {
                    let _ = std::fs::remove_file(&pid_path);
                    eprintln!("soyo helper is not running.");
                    1
                }
            }
        }
    }
}

fn verified_helper_process(rec: &HelperPidRecord, target: &HelperTarget) -> bool {
    if rec.pid <= 1 || rec.fingerprint != target.fingerprint || rec.exe_path.trim().is_empty() {
        return false;
    }
    if unsafe { libc::kill(rec.pid as i32, 0) } != 0 {
        return false;
    }
    if process_started_id(rec.pid) != Some(rec.started_at_id) {
        return false;
    }
    let Some(live_exe) = process_exe_path(rec.pid) else {
        return false;
    };
    match (
        std::fs::canonicalize(&live_exe),
        std::fs::canonicalize(&rec.exe_path),
    ) {
        (Ok(live), Ok(recorded)) => live == recorded,
        _ => live_exe == Path::new(&rec.exe_path),
    }
}

/// Ensure a helper matching `target` is running (spawn if necessary).
pub async fn ensure_helper_for(target: &HelperTarget) -> Result<(), String> {
    match connect_checked_for(target, true).await {
        Ok(client) => {
            client.detach().await;
            Ok(())
        }
        Err(err) => Err(err.message),
    }
}

/// Connect to the helper for `target`, reconciling an incompatible daemon only
/// when neither active runs nor other connected clients are present.
pub async fn connect_checked_for(
    target: &HelperTarget,
    prompt: bool,
) -> Result<Arc<HelperClient>, soyo_ipc::types::DesktopError> {
    let client = HelperClient::connect_or_spawn_for(target).await?;
    let compat = client.helper_compat().await;
    if !compat.mismatch {
        return Ok(client);
    }
    if compat.busy {
        eprintln!(
            "soyo: helper version/protocol mismatch ({}); leaving busy daemon running",
            compat.detail
        );
        return Ok(client);
    }
    if prompt && !confirm_helper_restart(&compat.detail) {
        return Ok(client);
    }
    eprintln!("soyo: restarting stale helper ({})", compat.detail);
    client.restart().await
}

fn confirm_helper_restart(detail: &str) -> bool {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return true;
    }
    eprint!("soyo: helper version mismatch ({detail}). Restart helper? [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).is_ok()
        && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}
