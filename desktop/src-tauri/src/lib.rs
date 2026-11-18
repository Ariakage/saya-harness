//! Tauri 2 desktop shell for Soyo Harness — a thin client of the `soyo helper`
//! daemon (`soyo-host` + `soyo-ipc`). All runtime work happens in the helper;
//! this crate exposes Specta commands, the attachment URI scheme proxy, and
//! window plumbing only.

pub mod attachment_protocol;
pub mod client_state;
pub mod commands;
pub mod peripheral_commands;
pub mod types;

pub use client_state::DesktopState;
pub use types::{DesktopError, SessionItemDto, SessionPageDto, WorkModeDto};

use specta_typescript::Typescript;
use tauri::Manager;
use tauri_specta::{Builder, collect_commands};

pub fn specta_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new().commands(collect_commands![
        commands::list_sessions,
        commands::get_bootstrap_state,
        commands::new_session,
        commands::submit_prompt,
        commands::cancel_run,
        commands::compact_session,
        commands::get_run_state,
        commands::get_usage_stats,
        commands::reply_approval,
        commands::reply_input,
        commands::set_work_mode,
        commands::set_permission_mode,
        commands::resume_session,
        peripheral_commands::set_workspace,
        peripheral_commands::select_profile,
        peripheral_commands::set_thinking_effort,
        peripheral_commands::set_sandbox_backend,
        peripheral_commands::set_autocompact,
        peripheral_commands::list_extensions,
        peripheral_commands::list_jobs,
        peripheral_commands::cancel_background_job,
        peripheral_commands::list_modules,
        peripheral_commands::set_module_enabled,
        peripheral_commands::list_mcp_servers,
        peripheral_commands::toggle_mcp_server,
        peripheral_commands::list_credentials,
        peripheral_commands::import_credential,
        peripheral_commands::revoke_credential,
        peripheral_commands::query_memory,
        peripheral_commands::prepare_memory_action,
        peripheral_commands::confirm_memory_action,
        peripheral_commands::export_session,
        peripheral_commands::stage_attachments,
        peripheral_commands::get_user_name,
        peripheral_commands::get_desktop_prefs,
        peripheral_commands::set_nickname,
        peripheral_commands::set_avatar,
        peripheral_commands::get_global_prompt,
        peripheral_commands::set_global_prompt,
    ])
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = specta_builder();
    let desktop_state = DesktopState::new();
    let app_builder = attachment_protocol::register_attachment_protocols(tauri::Builder::default());
    app_builder
        .manage(desktop_state)
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Soyo desktop application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app_handle.state::<DesktopState>();
                tauri::async_runtime::block_on(state.shutdown());
            }
        });
}

pub fn export_bindings_to(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    specta_builder().export(Typescript::default(), path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn export_specta_bindings() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings_to(&path).expect("specta bindings export should succeed");
        let content = std::fs::read_to_string(&path).expect("bindings.ts should exist");
        for symbol in [
            "listSessions",
            "getBootstrapState",
            "newSession",
            "submitPrompt",
            "cancelRun",
            "compactSession",
            "replyApproval",
            "replyInput",
            "setWorkMode",
            "setPermissionMode",
            "resumeSession",
            "setWorkspace",
            "selectProfile",
            "setThinkingEffort",
            "setSandboxBackend",
            "setAutocompact",
            "listExtensions",
            "listJobs",
            "cancelBackgroundJob",
            "listModules",
            "setModuleEnabled",
            "listMcpServers",
            "toggleMcpServer",
            "listCredentials",
            "importCredential",
            "revokeCredential",
            "queryMemory",
            "prepareMemoryAction",
            "confirmMemoryAction",
            "exportSession",
            "stageAttachments",
        ] {
            assert!(content.contains(symbol), "missing {symbol} in bindings.ts");
        }
    }
}
