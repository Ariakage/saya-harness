//! Tauri command definitions exposed to the desktop frontend via Specta.

use crate::{
    runtime_bridge::{DesktopState, StreamSink},
    types::{
        ActiveSessionDto, AttachmentTokenDto, BootstrapStateDto, DesktopError,
        DesktopStreamEnvelope, SessionPageDto, SubmitPromptResultDto,
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
    Ok(state.get_bootstrap_state().await)
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
    Ok(state.cancel_run())
}

#[tauri::command]
#[specta::specta]
pub async fn compact_session(
    state: State<'_, DesktopState>,
    on_event: Channel<DesktopStreamEnvelope>,
) -> Result<SubmitPromptResultDto, DesktopError> {
    state.compact_session(StreamSink::Tauri(on_event)).await
}
