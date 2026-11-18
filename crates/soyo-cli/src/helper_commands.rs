//! `soyo helper` (run the shared backend daemon) and `soyo kill` (stop it).

use soyo_host::{HelperServer, HostService};
use soyo_ipc::{HelperClient, helper_pid_path};
use std::sync::Arc;

/// Run the helper daemon in the foreground (clients spawn it detached).
pub async fn run_helper() -> u8 {
    let service = HostService::from_environment();
    let server = HelperServer::new(service);
    match server.run().await {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("soyo helper: {}: {}", err.code, err.message);
            1
        }
    }
}

/// Ask the helper to shut down; falls back to the pid file when the socket is
/// unreachable (stale daemon after a crash).
pub async fn kill_helper() -> u8 {
    match HelperClient::connect().await {
        Ok(client) => {
            client.shutdown_daemon().await;
            // Give the daemon a moment to exit before reporting.
            for _ in 0..20 {
                if HelperClient::connect().await.is_err() {
                    println!("soyo helper stopped.");
                    return 0;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            eprintln!("soyo helper: shutdown requested but still accepting connections");
            1
        }
        Err(_) => {
            // Socket is gone — check whether the pid file still names a live pid.
            let pid = std::fs::read_to_string(helper_pid_path())
                .ok()
                .and_then(|raw| raw.trim().parse::<u32>().ok());
            match pid {
                Some(pid) if pid_is_alive(pid) => {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(pid as i32, libc::SIGTERM);
                    }
                    println!("soyo helper terminated (pid {pid}).");
                    0
                }
                _ => {
                    let _ = std::fs::remove_file(helper_pid_path());
                    eprintln!("soyo helper is not running.");
                    1
                }
            }
        }
    }
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn pid_is_alive(pid: u32) -> bool {
    // Best-effort: assume alive; the socket check above already failed.
    let _ = pid;
    true
}

/// Ensure a helper is running (spawn if necessary). Used by interactive entry
/// points before they do their first runtime work.
pub async fn ensure_helper() -> Result<(), String> {
    match connect_checked(true).await {
        Ok(client) => {
            client.detach().await;
            Ok(())
        }
        Err(err) => Err(err.message),
    }
}

/// Connect to the helper, reconciling a stale daemon first.
///
/// When the running helper is a different version or binary than this client
/// would spawn, an idle helper is recycled: interactive callers (`prompt`)
/// confirm first, non-interactive callers recycle silently. A busy helper is
/// never killed — the client proceeds against it so in-flight runs survive.
pub async fn connect_checked(
    prompt: bool,
) -> Result<Arc<HelperClient>, soyo_ipc::types::DesktopError> {
    let client = HelperClient::connect_or_spawn().await?;
    let compat = client.helper_compat().await;
    if !compat.mismatch {
        return Ok(client);
    }
    if compat.run_active {
        eprintln!(
            "soyo: helper version mismatch ({}); leaving it running",
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
