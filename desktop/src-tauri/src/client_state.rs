//! Thin adapter from the Tauri command layer to the `soyo helper` daemon.
//! Method names mirror the former in-process `DesktopState` so command
//! wrappers stay unchanged; every call is an NDJSON request over the local
//! socket. The helper is spawned on first use when it is not already running.

use crate::types::{
    ActiveSessionDto, AttachmentPayloadDto, AttachmentTokenDto, BackgroundJobDto,
    BootstrapStateDto, CredentialRecordDto, DesktopError, DesktopPrefsDto, DesktopStreamEnvelope,
    ExportFormatDto, ExportResultDto, ExportScopeDto, ExtensionItemDto, GlobalPromptDto,
    McpServerDto, MemoryConfirmResultDto, MemoryQueryResultDto, ModeStateDto, ModuleStatusDto,
    PermissionModeDto, PreparedMemoryReviewDto, ResumeSessionResultDto, SandboxBackendDto,
    SessionPageDto, StagedAttachmentDto, SubmitPromptResultDto, WorkModeDto,
};
use serde_json::json;
use soyo_ipc::HelperClient;
use std::sync::Arc;
use tauri::ipc::Channel;
use tokio::sync::{OnceCell, mpsc};

/// Event channel used by streaming commands (kept for command-signature parity).
pub enum StreamSink {
    Tauri(Channel<DesktopStreamEnvelope>),
    Mpsc(mpsc::UnboundedSender<DesktopStreamEnvelope>),
}

pub struct DesktopState {
    client: OnceCell<Arc<HelperClient>>,
}

impl DesktopState {
    pub fn new() -> Self {
        Self {
            client: OnceCell::new(),
        }
    }

    async fn client(&self) -> Result<&Arc<HelperClient>, DesktopError> {
        self.client
            .get_or_try_init(HelperClient::connect_or_spawn)
            .await
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
        self.client()
            .await?
            .call_typed("get_bootstrap_state", &json!({}))
            .await
    }

    pub async fn list_sessions_page(
        &self,
        page: Option<u32>,
    ) -> Result<SessionPageDto, DesktopError> {
        self.client()
            .await?
            .call_typed("list_sessions_page", &json!({ "page": page }))
            .await
    }

    pub async fn new_session(&self) -> Result<ActiveSessionDto, DesktopError> {
        self.client()
            .await?
            .call_typed("new_session", &json!({}))
            .await
    }

    pub async fn submit_prompt(
        &self,
        prompt: String,
        attachments: Vec<AttachmentTokenDto>,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let (tx, forwarder) = Self::forward_channel(sink);
        let result = self
            .client()
            .await?
            .call_streaming(
                "submit_prompt",
                &json!({ "prompt": prompt, "attachments": attachments }),
                tx,
            )
            .await;
        let _ = forwarder.await;
        result
    }

    pub async fn compact_session(
        &self,
        sink: StreamSink,
    ) -> Result<SubmitPromptResultDto, DesktopError> {
        let (tx, forwarder) = Self::forward_channel(sink);
        let result = self
            .client()
            .await?
            .call_streaming("compact_session", &json!({}), tx)
            .await;
        let _ = forwarder.await;
        result
    }

    pub async fn cancel_run(&self) -> Result<bool, DesktopError> {
        self.client()
            .await?
            .call_typed("cancel_run", &json!({}))
            .await
    }

    pub async fn reply_approval(
        &self,
        approval_id: &str,
        call_id: &str,
        approved: bool,
    ) -> Result<(), DesktopError> {
        self.client()
            .await?
            .call_typed::<_, serde_json::Value>(
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
        self.client()
            .await?
            .call_typed::<_, serde_json::Value>(
                "reply_input",
                &json!({ "input_id": input_id, "answer": answer }),
            )
            .await
            .map(|_| ())
    }

    pub async fn set_work_mode(&self, mode: WorkModeDto) -> Result<ModeStateDto, DesktopError> {
        self.client()
            .await?
            .call_typed("set_work_mode", &json!({ "mode": mode }))
            .await
    }

    pub async fn set_permission_mode(
        &self,
        permissions: PermissionModeDto,
    ) -> Result<PermissionModeDto, DesktopError> {
        self.client()
            .await?
            .call_typed(
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
        self.client()
            .await?
            .call_typed(
                "resume_session",
                &json!({
                    "session_id": session_id,
                    "accept_config_change": accept_config_change,
                }),
            )
            .await
    }

    pub async fn set_workspace(&self, path: &str) -> Result<BootstrapStateDto, DesktopError> {
        self.client()
            .await?
            .call_typed("set_workspace", &json!({ "path": path }))
            .await
    }

    pub async fn select_profile(&self, profile: &str) -> Result<BootstrapStateDto, DesktopError> {
        self.client()
            .await?
            .call_typed("select_profile", &json!({ "profile": profile }))
            .await
    }

    pub async fn set_thinking_effort(
        &self,
        effort: Option<String>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.client()
            .await?
            .call_typed("set_thinking_effort", &json!({ "effort": effort }))
            .await
    }

    pub async fn set_sandbox_backend(
        &self,
        backend: SandboxBackendDto,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.client()
            .await?
            .call_typed("set_sandbox_backend", &json!({ "backend": backend }))
            .await
    }

    pub async fn set_autocompact(
        &self,
        enabled: bool,
        trigger_tokens: Option<u64>,
    ) -> Result<BootstrapStateDto, DesktopError> {
        self.client()
            .await?
            .call_typed(
                "set_autocompact",
                &json!({ "enabled": enabled, "trigger_tokens": trigger_tokens }),
            )
            .await
    }

    pub async fn list_extensions(&self) -> Result<Vec<ExtensionItemDto>, DesktopError> {
        self.client()
            .await?
            .call_typed("list_extensions", &json!({}))
            .await
    }

    pub async fn list_jobs(&self) -> Result<Vec<BackgroundJobDto>, DesktopError> {
        self.client()
            .await?
            .call_typed("list_jobs", &json!({}))
            .await
    }

    pub async fn cancel_background_job(&self, job_id: &str) -> Result<bool, DesktopError> {
        self.client()
            .await?
            .call_typed("cancel_background_job", &json!({ "job_id": job_id }))
            .await
    }

    pub async fn list_modules(&self) -> Result<Vec<ModuleStatusDto>, DesktopError> {
        self.client()
            .await?
            .call_typed("list_modules", &json!({}))
            .await
    }

    pub async fn set_module_enabled(
        &self,
        module: &str,
        enabled: bool,
    ) -> Result<Vec<ModuleStatusDto>, DesktopError> {
        self.client()
            .await?
            .call_typed(
                "set_module_enabled",
                &json!({ "module": module, "enabled": enabled }),
            )
            .await
    }

    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServerDto>, DesktopError> {
        self.client()
            .await?
            .call_typed("list_mcp_servers", &json!({}))
            .await
    }

    pub async fn toggle_mcp_server(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Vec<McpServerDto>, DesktopError> {
        self.client()
            .await?
            .call_typed(
                "toggle_mcp_server",
                &json!({ "name": name, "enabled": enabled }),
            )
            .await
    }

    pub async fn list_credentials(&self) -> Result<Vec<CredentialRecordDto>, DesktopError> {
        self.client()
            .await?
            .call_typed("list_credentials", &json!({}))
            .await
    }

    pub async fn import_credential(
        &self,
        server: &str,
        env_var: &str,
        scopes: Vec<String>,
        approve_scopes: bool,
    ) -> Result<CredentialRecordDto, DesktopError> {
        self.client()
            .await?
            .call_typed(
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
        self.client()
            .await?
            .call_typed("revoke_credential", &json!({ "reference": reference }))
            .await
    }

    pub async fn query_memory(&self, command: &str) -> Result<MemoryQueryResultDto, DesktopError> {
        self.client()
            .await?
            .call_typed("query_memory", &json!({ "command": command }))
            .await
    }

    pub async fn prepare_memory_action(
        &self,
        command: &str,
    ) -> Result<PreparedMemoryReviewDto, DesktopError> {
        self.client()
            .await?
            .call_typed("prepare_memory_action", &json!({ "command": command }))
            .await
    }

    pub async fn confirm_memory_action(
        &self,
        action_id: &str,
    ) -> Result<MemoryConfirmResultDto, DesktopError> {
        self.client()
            .await?
            .call_typed("confirm_memory_action", &json!({ "action_id": action_id }))
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
        self.client()
            .await?
            .call_typed(
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
        self.client()
            .await?
            .call_typed("stage_attachments", &json!({ "paths": paths }))
            .await
    }

    pub async fn fetch_attachment(
        &self,
        method: &str,
        uri: &str,
    ) -> Result<AttachmentPayloadDto, DesktopError> {
        self.client()
            .await?
            .call_typed("fetch_attachment", &json!({ "method": method, "uri": uri }))
            .await
    }

    pub async fn get_desktop_prefs(&self) -> Result<DesktopPrefsDto, DesktopError> {
        self.client()
            .await?
            .call_typed("get_desktop_prefs", &json!({}))
            .await
    }

    pub async fn set_nickname(
        &self,
        nickname: Option<String>,
    ) -> Result<DesktopPrefsDto, DesktopError> {
        self.client()
            .await?
            .call_typed("set_nickname", &json!({ "nickname": nickname }))
            .await
    }

    pub async fn set_avatar(
        &self,
        source_path: Option<String>,
    ) -> Result<DesktopPrefsDto, DesktopError> {
        self.client()
            .await?
            .call_typed("set_avatar", &json!({ "source_path": source_path }))
            .await
    }

    pub async fn get_user_name(&self) -> Result<Option<String>, DesktopError> {
        self.client()
            .await?
            .call_typed("get_user_name", &json!({}))
            .await
    }

    pub async fn get_global_prompt(&self) -> Result<GlobalPromptDto, DesktopError> {
        self.client()
            .await?
            .call_typed("get_global_prompt", &json!({}))
            .await
    }

    pub async fn set_global_prompt(
        &self,
        content: String,
    ) -> Result<GlobalPromptDto, DesktopError> {
        self.client()
            .await?
            .call_typed("set_global_prompt", &json!({ "content": content }))
            .await
    }

    /// Detach from the helper on app exit — the daemon keeps running (it is
    /// shared with CLI/TUI). Runs in progress continue in the background.
    pub async fn shutdown(&self) {
        if let Some(client) = self.client.get() {
            client.detach().await;
        }
    }
}

impl Default for DesktopState {
    fn default() -> Self {
        Self::new()
    }
}
