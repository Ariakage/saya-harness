//! Thin adapter from the Tauri command layer to the `soyo helper` daemon.
//! Method names mirror the former in-process `DesktopState` so command
//! wrappers stay unchanged; every call is an NDJSON request over the local
//! socket. The helper is spawned on first use when it is not already running.

use crate::types::{
    ActiveSessionDto, AttachmentPayloadDto, AttachmentTokenDto, BackgroundJobDto,
    BootstrapStateDto, CredentialRecordDto, DesktopError, DesktopPrefsDto, DesktopStreamEnvelope,
    ExportFormatDto, ExportResultDto, ExportScopeDto, ExtensionItemDto, GlobalPromptDto,
    HelperStatusDto, McpServerDto, MemoryConfirmResultDto, MemoryQueryResultDto, ModeStateDto,
    ModuleStatusDto, PermissionModeDto, PreparedMemoryReviewDto, ResumeSessionResultDto,
    RunStateDto, SandboxBackendDto, SessionPageDto, SetupEnvironmentParams, StagedAttachmentDto,
    SubmitPromptResultDto, UpdateDesktopPrefsParams, UsageStatsDto, WorkModeDto,
};
use serde::Deserialize;
use serde_json::json;
use soyo_ipc::HelperClient;
use std::sync::Arc;
use tauri::ipc::Channel;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

/// Clipboard-provided attachment written to a temp file before staging, since
/// pasted files in a webview carry bytes but no real filesystem path.
#[derive(Debug, Clone, Deserialize, specta::Type)]
pub struct PastedAttachmentItem {
    /// Suggested filename; the extension drives mime/kind inference.
    pub name: String,
    /// Raw file bytes.
    pub data: Vec<u8>,
}

fn sanitize_paste_name(raw: &str, index: usize) -> String {
    let filtered: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(80)
        .collect();
    let trimmed = filtered.trim_matches('.');
    if trimmed.is_empty() {
        format!("pasted-{index}.bin")
    } else {
        trimmed.to_string()
    }
}

/// Event channel used by streaming commands (kept for command-signature parity).
pub enum StreamSink {
    Tauri(Channel<DesktopStreamEnvelope>),
    Mpsc(mpsc::UnboundedSender<DesktopStreamEnvelope>),
}

fn is_disconnect_error(err: &DesktopError) -> bool {
    matches!(
        err.code.as_str(),
        "helper_disconnected" | "helper_io" | "helper_shutdown" | "client_detached"
    )
}

pub struct DesktopState {
    client: AsyncMutex<Option<Arc<HelperClient>>>,
}

impl DesktopState {
    pub fn new() -> Self {
        Self {
            client: AsyncMutex::new(None),
        }
    }

    async fn client_with_flag(&self) -> Result<(Arc<HelperClient>, bool), DesktopError> {
        let mut guard = self.client.lock().await;
        if let Some(existing) = guard.as_ref()
            && !existing.is_closed()
        {
            return Ok((Arc::clone(existing), true));
        }
        *guard = None;
        let client = HelperClient::connect_or_spawn().await?;
        // Recycle a stale daemon built from another version/protocol —
        // but never kill one while a run or another client is active.
        let compat = client.helper_compat().await;
        let client = if compat.mismatch && !compat.busy {
            client.restart().await?
        } else {
            client
        };
        *guard = Some(Arc::clone(&client));
        Ok((client, false))
    }

    async fn reset_client_if(&self, stale: &Arc<HelperClient>) {
        let mut guard = self.client.lock().await;
        if guard
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, stale))
        {
            *guard = None;
        }
    }

    async fn call_rpc<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: &serde_json::Value,
    ) -> Result<T, DesktopError> {
        let (client, reused) = self.client_with_flag().await?;
        match client.call_typed(method, params).await {
            Ok(value) => Ok(value),
            Err(err) if is_disconnect_error(&err) => {
                self.reset_client_if(&client).await;
                if reused {
                    let (fresh, _) = self.client_with_flag().await?;
                    fresh.call_typed(method, params).await
                } else {
                    Err(err)
                }
            }
            Err(err) => Err(err),
        }
    }

    fn forward_channel(
        sink: StreamSink,
    ) -> (
        mpsc::UnboundedSender<DesktopStreamEnvelope>,
        tokio::task::JoinHandle<()>,
    ) {
        let (tx, mut rx) = mpsc::unbounded_channel::<DesktopStreamEnvelope>();
        let forwarder = tokio::spawn(async move {
            match sink {
                StreamSink::Tauri(channel) => {
                    while let Some(env) = rx.recv().await {
                        if channel.send(env).is_err() {
                            break;
                        }
                    }
                }
                StreamSink::Mpsc(out) => {
                    while let Some(env) = rx.recv().await {
                        if out.send(env).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        (tx, forwarder)
    }

    pub async fn get_bootstrap_state(&self) -> Result<BootstrapStateDto, DesktopError> {
        self.call_rpc("get_bootstrap_state", &json!({})).await
    }

    pub async fn list_sessions_page(
        &self,
        page: Option<u32>,
    ) -> Result<SessionPageDto, DesktopError> {
        self.call_rpc("list_sessions_page", &json!({ "page": page }))
            .await
    }

    pub async fn new_session(&self) -> Result<ActiveSessionDto, DesktopError> {
        self.call_rpc("new_session", &json!({})).await
    }

    pub async fn submit_prompt(
        &self,
        prompt: String,
        attachments: Vec<AttachmentTokenDto>,
        session_id: Option<String>,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let (tx, forwarder) = Self::forward_channel(sink);
        let (client, _) = self.client_with_flag().await?;
        let result = client
            .call_streaming(
                "submit_prompt",
                &json!({
                    "prompt": prompt,
                    "attachments": attachments,
                    "session_id": session_id,
                }),
                tx,
            )
            .await;
        let _ = forwarder.await;
        if let Err(err) = &result
            && is_disconnect_error(err)
        {
            self.reset_client_if(&client).await;
        }
        result
    }

    pub async fn compact_session(
        &self,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let (tx, forwarder) = Self::forward_channel(sink);
        let (client, _) = self.client_with_flag().await?;
        let result = client
            .call_streaming("compact_session", &json!({}), tx)
            .await;
        let _ = forwarder.await;
        if let Err(err) = &result
            && is_disconnect_error(err)
        {
            self.reset_client_if(&client).await;
        }
        result
    }

    pub async fn cancel_run(&self) -> Result<bool, DesktopError> {
        self.call_rpc("cancel_run", &json!({})).await
    }

    pub async fn get_run_state(&self) -> Result<RunStateDto, DesktopError> {
        self.call_rpc("get_run_state", &json!({})).await
    }

    pub async fn usage_stats(
        &self,
        days: u32,
        utc_offset_minutes: Option<i32>,
    ) -> Result<UsageStatsDto, DesktopError> {
        self.call_rpc(
            "usage_stats",
            &json!({
                "days": days,
                "utc_offset_minutes": utc_offset_minutes,
            }),
        )
        .await
    }

    pub async fn get_helper_status(&self) -> HelperStatusDto {
        match self
            .call_rpc::<crate::types::HelperInfoDto>("helper_info", &json!({}))
            .await
        {
            Ok(info) => HelperStatusDto {
                connected: true,
                version: Some(info.version),
                pid: Some(info.pid),
                active_runs: info.active_runs,
                active_clients: info.active_clients,
                detail: None,
            },
            Err(err) => HelperStatusDto {
                connected: false,
                version: None,
                pid: None,
                active_runs: 0,
                active_clients: 0,
                detail: Some(err.message),
            },
        }
    }

    pub async fn setup_environment(
        &self,
        params: SetupEnvironmentParams,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.call_rpc("setup_environment", &json!(params)).await
    }

    pub async fn reply_approval(
        &self,
        approval_id: &str,
        call_id: &str,
        approved: bool,
    ) -> Result<(), DesktopError> {
        self.call_rpc::<serde_json::Value>(
            "reply_approval",
            &json!({
                "approval_id": approval_id,
                "call_id": call_id,
                "approved": approved,
            }),
        )
        .await
        .map(|_| ())
    }

    pub async fn reply_input(&self, input_id: &str, answer: String) -> Result<(), DesktopError> {
        self.call_rpc::<serde_json::Value>(
            "reply_input",
            &json!({ "input_id": input_id, "answer": answer }),
        )
        .await
        .map(|_| ())
    }

    pub async fn set_work_mode(&self, mode: WorkModeDto) -> Result<ModeStateDto, DesktopError> {
        self.call_rpc("set_work_mode", &json!({ "mode": mode }))
            .await
    }

    pub async fn set_permission_mode(
        &self,
        permissions: PermissionModeDto,
    ) -> Result<PermissionModeDto, DesktopError> {
        self.call_rpc(
            "set_permission_mode",
            &json!({ "permissions": permissions }),
        )
        .await
    }

    pub async fn resume_session(
        &self,
        session_id: &str,
        accept_config_change: bool,
    ) -> Result<ResumeSessionResultDto, DesktopError> {
        self.call_rpc(
            "resume_session",
            &json!({
                "session_id": session_id,
                "accept_config_change": accept_config_change,
            }),
        )
        .await
    }

    pub async fn set_workspace(&self, path: &str) -> Result<BootstrapStateDto, DesktopError> {
        self.call_rpc("set_workspace", &json!({ "path": path }))
            .await
    }

    pub async fn select_profile(&self, profile: &str) -> Result<BootstrapStateDto, DesktopError> {
        self.call_rpc("select_profile", &json!({ "profile": profile }))
            .await
    }

    pub async fn set_thinking_effort(
        &self,
        effort: Option<String>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.call_rpc("set_thinking_effort", &json!({ "effort": effort }))
            .await
    }

    pub async fn set_sandbox_backend(
        &self,
        backend: SandboxBackendDto,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.call_rpc("set_sandbox_backend", &json!({ "backend": backend }))
            .await
    }

    pub async fn set_autocompact(
        &self,
        enabled: bool,
        trigger_tokens: Option<u64>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.call_rpc(
            "set_autocompact",
            &json!({ "enabled": enabled, "trigger_tokens": trigger_tokens }),
        )
        .await
    }

    pub async fn list_extensions(&self) -> Result<Vec<ExtensionItemDto>, DesktopError> {
        self.call_rpc("list_extensions", &json!({})).await
    }

    pub async fn list_jobs(&self) -> Result<Vec<BackgroundJobDto>, DesktopError> {
        self.call_rpc("list_jobs", &json!({})).await
    }

    pub async fn cancel_background_job(&self, job_id: &str) -> Result<bool, DesktopError> {
        self.call_rpc("cancel_background_job", &json!({ "job_id": job_id }))
            .await
    }

    pub async fn list_modules(&self) -> Result<Vec<ModuleStatusDto>, DesktopError> {
        self.call_rpc("list_modules", &json!({})).await
    }

    pub async fn set_module_enabled(
        &self,
        module: &str,
        enabled: bool,
    ) -> Result<Vec<ModuleStatusDto>, DesktopError> {
        self.call_rpc(
            "set_module_enabled",
            &json!({ "module": module, "enabled": enabled }),
        )
        .await
    }

    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServerDto>, DesktopError> {
        self.call_rpc("list_mcp_servers", &json!({})).await
    }

    pub async fn toggle_mcp_server(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<McpServerDto>, DesktopError> {
        self.call_rpc(
            "toggle_mcp_server",
            &json!({ "name": name, "enabled": enabled }),
        )
        .await
    }

    pub async fn list_credentials(&self) -> Result<Vec<CredentialRecordDto>, DesktopError> {
        self.call_rpc("list_credentials", &json!({})).await
    }

    pub async fn import_credential(
        &self,
        server: &str,
        env_var: &str,
        scopes: Vec<String>,
        approve_scopes: bool,
    ) -> Result<CredentialRecordDto, DesktopError> {
        self.call_rpc(
            "import_credential",
            &json!({
                "server": server,
                "env_var": env_var,
                "scopes": scopes,
                "approve_scopes": approve_scopes,
            }),
        )
        .await
    }

    pub async fn revoke_credential(&self, reference: &str) -> Result<bool, DesktopError> {
        self.call_rpc("revoke_credential", &json!({ "reference": reference }))
            .await
    }

    pub async fn query_memory(&self, command: &str) -> Result<MemoryQueryResultDto, DesktopError> {
        self.call_rpc("query_memory", &json!({ "command": command }))
            .await
    }

    pub async fn prepare_memory_action(
        &self,
        command: &str,
    ) -> Result<PreparedMemoryReviewDto, DesktopError> {
        self.call_rpc("prepare_memory_action", &json!({ "command": command }))
            .await
    }

    pub async fn confirm_memory_action(
        &self,
        action_id: &str,
    ) -> Result<MemoryConfirmResultDto, DesktopError> {
        self.call_rpc("confirm_memory_action", &json!({ "action_id": action_id }))
            .await
    }

    pub async fn export_session(
        &self,
        session_id: Option<String>,
        target_path: &str,
        format: ExportFormatDto,
        scope: ExportScopeDto,
        include_tools: bool,
    ) -> Result<ExportResultDto, DesktopError> {
        self.call_rpc(
            "export_session",
            &json!({
                "session_id": session_id,
                "target_path": target_path,
                "format": format,
                "scope": scope,
                "include_tools": include_tools,
            }),
        )
        .await
    }

    pub async fn stage_attachments(
        &self,
        paths: Vec<String>,
    ) -> Result<Vec<StagedAttachmentDto>, DesktopError> {
        self.call_rpc("stage_attachments", &json!({ "paths": paths }))
            .await
    }

    /// Materialize clipboard blobs to per-process temp files and stage them
    /// through the normal path-based attachment pipeline. Written files are
    /// removed when staging fails so rejected payloads do not linger.
    pub async fn stage_pasted_attachments(
        &self,
        items: Vec<PastedAttachmentItem>,
    ) -> Result<Vec<StagedAttachmentDto>, DesktopError> {
        if items.is_empty() {
            return Ok(Vec::new());
        }
        let dir = std::env::temp_dir().join(format!("soyo-paste-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|err| {
            DesktopError::new(
                "attachment_io",
                format!("Cannot prepare paste directory: {err}"),
            )
        })?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let mut paths = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let name = sanitize_paste_name(&item.name, index);
            let path = dir.join(format!("{stamp}-{index}-{name}"));
            std::fs::write(&path, &item.data).map_err(|err| {
                DesktopError::new("attachment_io", format!("Cannot write pasted file: {err}"))
            })?;
            paths.push(path.to_string_lossy().into_owned());
        }
        let result = self.stage_attachments(paths.clone()).await;
        if result.is_err() {
            for path in &paths {
                let _ = std::fs::remove_file(path);
            }
        }
        result
    }

    pub async fn fetch_attachment(
        &self,
        method: &str,
        uri: &str,
    ) -> Result<AttachmentPayloadDto, DesktopError> {
        self.call_rpc("fetch_attachment", &json!({ "method": method, "uri": uri }))
            .await
    }

    pub async fn get_desktop_prefs(&self) -> Result<DesktopPrefsDto, DesktopError> {
        self.call_rpc("get_desktop_prefs", &json!({})).await
    }

    pub async fn update_desktop_prefs(
        &self,
        params: UpdateDesktopPrefsParams,
    ) -> Result<DesktopPrefsDto, DesktopError> {
        self.call_rpc("update_desktop_prefs", &json!(params)).await
    }

    pub async fn set_nickname(
        &self,
        nickname: Option<String>,
    ) -> Result<DesktopPrefsDto, DesktopError> {
        self.call_rpc("set_nickname", &json!({ "nickname": nickname }))
            .await
    }

    pub async fn set_avatar(
        &self,
        source_path: Option<String>,
    ) -> Result<DesktopPrefsDto, DesktopError> {
        self.call_rpc("set_avatar", &json!({ "source_path": source_path }))
            .await
    }

    pub async fn get_user_name(&self) -> Result<Option<String>, DesktopError> {
        self.call_rpc("get_user_name", &json!({})).await
    }

    pub async fn get_global_prompt(&self) -> Result<GlobalPromptDto, DesktopError> {
        self.call_rpc("get_global_prompt", &json!({})).await
    }

    pub async fn set_global_prompt(
        &self,
        content: String,
    ) -> Result<GlobalPromptDto, DesktopError> {
        self.call_rpc("set_global_prompt", &json!({ "content": content }))
            .await
    }

    /// Detach from the helper on app exit — the daemon keeps running (it is
    /// shared with CLI/TUI). Runs in progress continue in the background.
    pub async fn shutdown(&self) {
        let client = self.client.lock().await.take();
        if let Some(client) = client {
            client.detach().await;
        }
    }
}

impl Default for DesktopState {
    fn default() -> Self {
        Self::new()
    }
}
