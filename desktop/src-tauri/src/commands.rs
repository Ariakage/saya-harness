//! Tauri command definitions exposed to the desktop frontend via Specta.

use crate::{
    client_state::{DesktopState, StreamSink},
    types::{
        ActiveSessionDto, AttachmentTokenDto, BootstrapStateDto, DesktopError,
        DesktopStreamEnvelope, ModeStateDto, PermissionModeDto, ResumeSessionResultDto,
        RunStateDto, SessionPageDto, SubmitPromptResultDto, WorkModeDto,
    },
};
use tauri::{State, ipc::Channel};

#[tauri::command]
#[specta::specta]
pub async fn list_sessions(
    state: State<'_, DesktopState>,
    page: Option<u32>,
) -> Result<SessionPageDto, DesktopError> {
    state.list_sessions_page(page).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_bootstrap_state(
    state: State<'_, DesktopState>,
) -> Result<BootstrapStateDto, DesktopError> {
    state.get_bootstrap_state().await
}

#[tauri::command]
#[specta::specta]
pub async fn new_session(state: State<'_, DesktopState>) -> Result<ActiveSessionDto, DesktopError> {
    state.new_session().await
}

#[tauri::command]
#[specta::specta]
pub async fn submit_prompt(
    state: State<'_, DesktopState>,
    prompt: String,
    attachments: Vec<AttachmentTokenDto>,
    on_event: Channel<DesktopStreamEnvelope>,
) -> Result<SubmitPromptResultDto, DesktopError> {
    state
        .submit_prompt(prompt, attachments, StreamSink::Tauri(on_event))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn cancel_run(state: State<'_, DesktopState>) -> Result<bool, DesktopError> {
    state.cancel_run().await
}

#[tauri::command]
#[specta::specta]
pub async fn compact_session(
    state: State<'_, DesktopState>,
    on_event: Channel<DesktopStreamEnvelope>,
) -> Result<SubmitPromptResultDto, DesktopError> {
    state.compact_session(StreamSink::Tauri(on_event)).await
}

#[tauri::command]
#[specta::specta]
pub async fn get_run_state(state: State<'_, DesktopState>) -> Result<RunStateDto, DesktopError> {
    state.get_run_state().await
}

#[tauri::command]
#[specta::specta]
pub async fn reply_approval(
    state: State<'_, DesktopState>,
    approval_id: String,
    call_id: String,
    approved: bool,
) -> Result<(), DesktopError> {
    state.reply_approval(&approval_id, &call_id, approved).await
}

#[tauri::command]
#[specta::specta]
pub async fn reply_input(
    state: State<'_, DesktopState>,
    input_id: String,
    answer: String,
) -> Result<(), DesktopError> {
    state.reply_input(&input_id, answer).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_work_mode(
    state: State<'_, DesktopState>,
    mode: WorkModeDto,
) -> Result<ModeStateDto, DesktopError> {
    state.set_work_mode(mode).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_permission_mode(
    state: State<'_, DesktopState>,
    permissions: PermissionModeDto,
) -> Result<PermissionModeDto, DesktopError> {
    state.set_permission_mode(permissions).await
}

#[tauri::command]
#[specta::specta]
pub async fn resume_session(
    state: State<'_, DesktopState>,
    session_id: String,
    accept_config_change: bool,
) -> Result<ResumeSessionResultDto, DesktopError> {
    state
        .resume_session(&session_id, accept_config_change)
        .await
}
