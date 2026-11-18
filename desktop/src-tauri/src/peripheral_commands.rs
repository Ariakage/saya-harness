//! Tauri command wrappers for peripheral desktop operations.
//! Every call forwards to the `soyo helper` daemon via `DesktopState`.

use crate::{
    client_state::DesktopState,
    types::{
        BackgroundJobDto, BootstrapStateDto, CredentialRecordDto, DesktopError, DesktopPrefsDto,
        ExportFormatDto, ExportResultDto, ExportScopeDto, ExtensionItemDto, GlobalPromptDto,
        McpServerDto, MemoryConfirmResultDto, MemoryQueryResultDto, ModuleStatusDto,
        PreparedMemoryReviewDto, SandboxBackendDto, StagedAttachmentDto,
    },
};
use tauri::State;

#[tauri::command]
#[specta::specta]
pub async fn set_workspace(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<BootstrapStateDto, DesktopError> {
    state.set_workspace(&path).await
}

#[tauri::command]
#[specta::specta]
pub async fn select_profile(
    state: State<'_, DesktopState>,
    profile: String,
) -> Result<BootstrapStateDto, DesktopError> {
    state.select_profile(&profile).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_thinking_effort(
    state: State<'_, DesktopState>,
    effort: Option<String>,
) -> Result<BootstrapStateDto, DesktopError> {
    state.set_thinking_effort(effort).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_sandbox_backend(
    state: State<'_, DesktopState>,
    backend: SandboxBackendDto,
) -> Result<BootstrapStateDto, DesktopError> {
    state.set_sandbox_backend(backend).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_autocompact(
    state: State<'_, DesktopState>,
    enabled: bool,
    trigger_tokens: Option<f64>,
) -> Result<BootstrapStateDto, DesktopError> {
    let parsed_tokens = match trigger_tokens {
        Some(value) => {
            if !value.is_finite() || value <= 0.0 || value > 9_007_199_254_740_991.0 {
                return Err(DesktopError::new(
                    "invalid_args",
                    "Auto-compact trigger tokens must be a valid positive integer",
                ));
            }
            Some(value.trunc() as u64)
        }
        None => None,
    };
    state.set_autocompact(enabled, parsed_tokens).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_extensions(
    state: State<'_, DesktopState>,
) -> Result<Vec<ExtensionItemDto>, DesktopError> {
    state.list_extensions().await
}

#[tauri::command]
#[specta::specta]
pub async fn list_jobs(
    state: State<'_, DesktopState>,
) -> Result<Vec<BackgroundJobDto>, DesktopError> {
    state.list_jobs().await
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_background_job(
    state: State<'_, DesktopState>,
    job_id: String,
) -> Result<bool, DesktopError> {
    state.cancel_background_job(&job_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_modules(
    state: State<'_, DesktopState>,
) -> Result<Vec<ModuleStatusDto>, DesktopError> {
    state.list_modules().await
}

#[tauri::command]
#[specta::specta]
pub async fn set_module_enabled(
    state: State<'_, DesktopState>,
    module: String,
    enabled: bool,
) -> Result<Vec<ModuleStatusDto>, DesktopError> {
    state.set_module_enabled(&module, enabled).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_mcp_servers(
    state: State<'_, DesktopState>,
) -> Result<Vec<McpServerDto>, DesktopError> {
    state.list_mcp_servers().await
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_mcp_server(
    state: State<'_, DesktopState>,
    name: String,
    enabled: bool,
) -> Result<Vec<McpServerDto>, DesktopError> {
    state.toggle_mcp_server(&name, enabled).await
}

#[tauri::command]
#[specta::specta]
pub async fn list_credentials(
    state: State<'_, DesktopState>,
) -> Result<Vec<CredentialRecordDto>, DesktopError> {
    state.list_credentials().await
}

#[tauri::command]
#[specta::specta]
pub async fn import_credential(
    state: State<'_, DesktopState>,
    server: String,
    env_var: String,
    scopes: Vec<String>,
    approve_scopes: bool,
) -> Result<CredentialRecordDto, DesktopError> {
    state
        .import_credential(&server, &env_var, scopes, approve_scopes)
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn revoke_credential(
    state: State<'_, DesktopState>,
    reference: String,
) -> Result<bool, DesktopError> {
    state.revoke_credential(&reference).await
}

#[tauri::command]
#[specta::specta]
pub async fn query_memory(
    state: State<'_, DesktopState>,
    command: String,
) -> Result<MemoryQueryResultDto, DesktopError> {
    state.query_memory(&command).await
}

#[tauri::command]
#[specta::specta]
pub async fn prepare_memory_action(
    state: State<'_, DesktopState>,
    command: String,
) -> Result<PreparedMemoryReviewDto, DesktopError> {
    state.prepare_memory_action(&command).await
}

#[tauri::command]
#[specta::specta]
pub async fn confirm_memory_action(
    state: State<'_, DesktopState>,
    action_id: String,
) -> Result<MemoryConfirmResultDto, DesktopError> {
    state.confirm_memory_action(&action_id).await
}

#[tauri::command]
#[specta::specta]
pub async fn export_session(
    state: State<'_, DesktopState>,
    session_id: Option<String>,
    target_path: String,
    format: ExportFormatDto,
    scope: ExportScopeDto,
    include_tools: bool,
) -> Result<ExportResultDto, DesktopError> {
    state
        .export_session(session_id, &target_path, format, scope, include_tools)
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn stage_attachments(
    state: State<'_, DesktopState>,
    paths: Vec<String>,
) -> Result<Vec<StagedAttachmentDto>, DesktopError> {
    state.stage_attachments(paths).await
}

#[tauri::command]
#[specta::specta]
pub async fn stage_pasted_attachments(
    state: State<'_, DesktopState>,
    items: Vec<crate::client_state::PastedAttachmentItem>,
) -> Result<Vec<StagedAttachmentDto>, DesktopError> {
    state.stage_pasted_attachments(items).await
}
#[tauri::command]
#[specta::specta]
pub async fn get_user_name(state: State<'_, DesktopState>) -> Result<Option<String>, DesktopError> {
    state.get_user_name().await
}

#[tauri::command]
#[specta::specta]
pub async fn get_desktop_prefs(
    state: State<'_, DesktopState>,
) -> Result<DesktopPrefsDto, DesktopError> {
    state.get_desktop_prefs().await
}

#[tauri::command]
#[specta::specta]
pub async fn set_nickname(
    state: State<'_, DesktopState>,
    nickname: Option<String>,
) -> Result<DesktopPrefsDto, DesktopError> {
    state.set_nickname(nickname).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_avatar(
    state: State<'_, DesktopState>,
    source_path: Option<String>,
) -> Result<DesktopPrefsDto, DesktopError> {
    state.set_avatar(source_path).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_global_prompt(
    state: State<'_, DesktopState>,
) -> Result<GlobalPromptDto, DesktopError> {
    state.get_global_prompt().await
}

#[tauri::command]
#[specta::specta]
pub async fn set_global_prompt(
    state: State<'_, DesktopState>,
    content: String,
) -> Result<GlobalPromptDto, DesktopError> {
    state.set_global_prompt(content).await
}
