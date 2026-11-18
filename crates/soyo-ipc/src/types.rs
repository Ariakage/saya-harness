//! Specta-typed IPC Data Transfer Objects for `soyo-desktop`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use soyo_core::{
    contract::{
        ApprovalSource, BoundaryError, ErrorInfo, ModelPurpose, PermissionMode, ReviewSource,
        RunResult, RunStatus, ToolResult, Usage, WorkMode,
    },
    tools::todo::{TodoItem, TodoStatus},
};
use specta::Type;
use specta_typescript::{Number, Unknown};

use crate::redaction;

/// Structured IPC error returned to the frontend; sensitive values are redacted.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct DesktopError {
    pub code: String,
    pub message: String,
}

impl DesktopError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn from_boundary(error: &BoundaryError, secrets: &[String]) -> Self {
        Self {
            code: error.code().to_owned(),
            message: redaction::redact_str(error.message(), secrets),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorkModeDto {
    Plan,
    Build,
    Code,
    Work,
}

impl From<WorkMode> for WorkModeDto {
    fn from(value: WorkMode) -> Self {
        match value {
            WorkMode::Plan => Self::Plan,
            WorkMode::Build => Self::Build,
            WorkMode::Code => Self::Code,
            WorkMode::Work => Self::Work,
        }
    }
}

impl From<WorkModeDto> for WorkMode {
    fn from(value: WorkModeDto) -> Self {
        match value {
            WorkModeDto::Plan => Self::Plan,
            WorkModeDto::Build => Self::Build,
            WorkModeDto::Code => Self::Code,
            WorkModeDto::Work => Self::Work,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PermissionModeDto {
    Auto,
    Bypass,
    Manual,
}

impl From<PermissionMode> for PermissionModeDto {
    fn from(value: PermissionMode) -> Self {
        match value {
            PermissionMode::Auto => Self::Auto,
            PermissionMode::Bypass => Self::Bypass,
            PermissionMode::Manual => Self::Manual,
        }
    }
}

impl From<PermissionModeDto> for PermissionMode {
    fn from(value: PermissionModeDto) -> Self {
        match value {
            PermissionModeDto::Auto => Self::Auto,
            PermissionModeDto::Bypass => Self::Bypass,
            PermissionModeDto::Manual => Self::Manual,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurposeDto {
    Execution,
    Review,
    Compaction,
}

impl From<ModelPurpose> for ModelPurposeDto {
    fn from(value: ModelPurpose) -> Self {
        match value {
            ModelPurpose::Execution => Self::Execution,
            ModelPurpose::Review => Self::Review,
            ModelPurpose::Compaction => Self::Compaction,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum RunStatusDto {
    Completed,
    Failed,
    NeedsConfirmation,
    NeedsInput,
    LimitReached,
    Cancelled,
}

impl From<RunStatus> for RunStatusDto {
    fn from(value: RunStatus) -> Self {
        match value {
            RunStatus::Completed => Self::Completed,
            RunStatus::Failed => Self::Failed,
            RunStatus::NeedsConfirmation => Self::NeedsConfirmation,
            RunStatus::NeedsInput => Self::NeedsInput,
            RunStatus::LimitReached => Self::LimitReached,
            RunStatus::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatusDto {
    Pending,
    InProgress,
    Completed,
}

impl From<TodoStatus> for TodoStatusDto {
    fn from(value: TodoStatus) -> Self {
        match value {
            TodoStatus::Pending => Self::Pending,
            TodoStatus::InProgress => Self::InProgress,
            TodoStatus::Completed => Self::Completed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct TodoItemDto {
    pub id: String,
    pub content: String,
    pub status: TodoStatusDto,
}

impl TodoItemDto {
    pub fn from_redacted(item: TodoItem, secrets: &[String]) -> Self {
        Self {
            id: redaction::redact_str(&item.id, secrets),
            content: redaction::redact_str(&item.content, secrets),
            status: item.status.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ErrorInfoDto {
    pub code: String,
    pub message: String,
}

impl ErrorInfoDto {
    pub fn from_redacted(info: &ErrorInfo, secrets: &[String]) -> Self {
        Self {
            code: info.code.clone(),
            message: redaction::redact_str(&info.message, secrets),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct UsageDto {
    #[specta(type = Option<Number>)]
    pub input_tokens: Option<u64>,
    #[specta(type = Option<Number>)]
    pub output_tokens: Option<u64>,
    #[specta(type = Option<Number>)]
    pub cached_input_tokens: Option<u64>,
    pub estimated: bool,
}

impl From<Usage> for UsageDto {
    fn from(value: Usage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            cached_input_tokens: value.cached_input_tokens,
            estimated: value.estimated,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ContextEntryDto {
    pub label: String,
    #[specta(type = Number)]
    pub tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ThinkingSnapshotDto {
    pub request_id: String,
    pub text: String,
    pub active: bool,
    pub truncated: bool,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ToolResultDto {
    pub ok: bool,
    #[specta(type = Option<Unknown>)]
    pub data: Option<Value>,
    pub error: Option<ErrorInfoDto>,
    pub truncated: bool,
    pub summary: Option<String>,
    pub formatted: String,
    pub media_refs: Vec<String>,
}

impl ToolResultDto {
    pub fn from_redacted(
        name: &str,
        args: Option<&Value>,
        result: &ToolResult,
        elapsed_ms: Option<u64>,
        secrets: &[String],
    ) -> Self {
        let elapsed = elapsed_ms.map(std::time::Duration::from_millis);
        let formatted_raw = match args {
            Some(args) => crate::tool_format::call_and_result(name, args, result, elapsed),
            None => crate::tool_format::result(result),
        };
        let summary_raw = crate::tool_format::summarize_call(name, args, Some(result), elapsed);
        Self {
            ok: result.ok,
            data: result
                .data
                .as_ref()
                .map(|val| redaction::redact_value(val, secrets)),
            error: result
                .error
                .as_ref()
                .map(|err| ErrorInfoDto::from_redacted(err, secrets)),
            truncated: result.truncated,
            summary: summary_raw.map(|text| redaction::redact_str(&text, secrets)),
            formatted: redaction::redact_str(&formatted_raw, secrets),
            media_refs: result
                .media_refs
                .iter()
                .map(|r| redaction::redact_str(r, secrets))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct RunResultDto {
    pub schema_version: u8,
    pub session_id: Option<String>,
    pub run_id: Option<String>,
    pub status: RunStatusDto,
    pub stop_reason: Option<String>,
    pub text: Option<String>,
    pub error: Option<ErrorInfoDto>,
    pub usage: Option<UsageDto>,
}

impl RunResultDto {
    pub fn from_redacted(result: &RunResult, secrets: &[String]) -> Self {
        Self {
            schema_version: result.schema_version,
            session_id: result.session_id.map(|id| id.0.to_string()),
            run_id: result.run_id.map(|id| id.0.to_string()),
            status: result.status.into(),
            stop_reason: result
                .stop_reason
                .as_deref()
                .map(|s| redaction::redact_str(s, secrets)),
            text: result
                .text
                .as_deref()
                .map(|s| redaction::redact_str(s, secrets)),
            error: result
                .error
                .as_ref()
                .map(|err| ErrorInfoDto::from_redacted(err, secrets)),
            usage: result.usage.clone().map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSourceDto {
    Rule,
    Model,
}

impl From<ReviewSource> for ReviewSourceDto {
    fn from(value: ReviewSource) -> Self {
        match value {
            ReviewSource::Rule => Self::Rule,
            ReviewSource::Model => Self::Model,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSourceDto {
    User,
    Preauthorization,
}

impl From<ApprovalSource> for ApprovalSourceDto {
    fn from(value: ApprovalSource) -> Self {
        match value {
            ApprovalSource::User => Self::User,
            ApprovalSource::Preauthorization => Self::Preauthorization,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "type", content = "data")]
pub enum SchemaEventDto {
    #[serde(rename = "session.created")]
    SessionCreated {
        work_mode: WorkModeDto,
        permissions: PermissionModeDto,
        workspace: String,
        profile: String,
    },
    #[serde(rename = "session.resumed")]
    SessionResumed {
        work_mode: WorkModeDto,
        permissions: PermissionModeDto,
        workspace: String,
        profile: String,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        work_mode: WorkModeDto,
        permissions: PermissionModeDto,
        profile: String,
    },
    #[serde(rename = "mode.change_requested")]
    ModeChangeRequested { from: WorkModeDto, to: WorkModeDto },
    #[serde(rename = "mode.changed")]
    ModeChanged { from: WorkModeDto, to: WorkModeDto },
    #[serde(rename = "model.started")]
    ModelStarted {
        request_id: String,
        purpose: ModelPurposeDto,
        profile: String,
    },
    #[serde(rename = "model.text_delta")]
    ModelTextDelta { request_id: String, text: String },
    #[serde(rename = "model.finished")]
    ModelFinished {
        request_id: String,
        purpose: ModelPurposeDto,
        ok: bool,
        usage: Option<UsageDto>,
        error: Option<ErrorInfoDto>,
    },
    #[serde(rename = "tool.requested")]
    ToolRequested {
        call_id: String,
        name: String,
        #[specta(type = Unknown)]
        args: Value,
        summary: Option<String>,
        formatted: String,
    },
    #[serde(rename = "permission.reviewed")]
    PermissionReviewed {
        call_id: String,
        decision: String,
        source: ReviewSourceDto,
        reason: String,
    },
    #[serde(rename = "permission.requested")]
    PermissionRequested {
        approval_id: String,
        call_id: String,
        operation_summary: String,
        work_mode: WorkModeDto,
    },
    #[serde(rename = "permission.resolved")]
    PermissionResolved {
        approval_id: String,
        call_id: String,
        decision: String,
        source: ApprovalSourceDto,
    },
    #[serde(rename = "tool.started")]
    ToolStarted { call_id: String, name: String },
    #[serde(rename = "tool.finished")]
    ToolFinished {
        call_id: String,
        name: String,
        result: ToolResultDto,
    },
    #[serde(rename = "input.requested")]
    InputRequested {
        input_id: String,
        question: String,
        options: Vec<String>,
    },
    #[serde(rename = "input.received")]
    InputReceived { input_id: String, answer: String },
    #[serde(rename = "mcp.status")]
    McpStatus { servers: Vec<McpStatusBadgeDto> },
    #[serde(rename = "run.finished")]
    RunFinished(RunResultDto),
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct McpStatusBadgeDto {
    pub server: String,
    pub transport: String,
    pub state: String,
    pub tools: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SchemaEnvelopeDto {
    pub schema_version: u8,
    pub session_id: String,
    pub run_id: Option<String>,
    #[specta(type = Number)]
    pub seq: u64,
    pub timestamp: String,
    pub payload: SchemaEventDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ApprovalRequestDto {
    pub approval_id: String,
    pub call_id: String,
    pub run_id: String,
    pub tool: String,
    pub operation_summary: String,
    #[specta(type = Unknown)]
    pub arguments: Value,
    pub formatted_call: String,
    pub action: Option<String>,
    pub cwd: String,
    pub work_mode: WorkModeDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct InputRequestDto {
    pub input_id: String,
    pub question: String,
    pub options: Vec<String>,
}

/// Transport-neutral attachment payload returned by `fetch_attachment`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AttachmentPayloadDto {
    pub status: u16,
    pub content_type: String,
    /// Base64-encoded body (attachments can be 32 MiB of binary).
    pub body_b64: String,
}

/// Unified channel message pushed during an active run.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum DesktopStreamEnvelope {
    SchemaEvent(SchemaEnvelopeDto),
    /// Raw schema-v1 event passthrough produced only by `run_prompt`. CLI
    /// consumers print it verbatim for `--output jsonl` parity; desktop
    /// streams never carry it, so it stays out of the generated TS union.
    #[specta(skip)]
    RawSchemaEvent {
        #[specta(type = Unknown)]
        event: Value,
    },
    Thinking(Vec<ThinkingSnapshotDto>),
    Context {
        entries: Vec<ContextEntryDto>,
        skills_count: Option<u32>,
        estimated: bool,
    },
    ModeState {
        current: WorkModeDto,
        pending: Option<WorkModeDto>,
    },
    ApprovalNeeded(ApprovalRequestDto),
    InputNeeded(InputRequestDto),
    RunCompleted {
        status: String,
        summary_line: String,
        todos: Vec<TodoItemDto>,
        error: Option<DesktopError>,
        /// Clarification question carried when `status == "needs_input"`;
        /// mirrors the CLI's session-history lookup for text output.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        needs_input_question: Option<String>,
    },
}

/// Attachment reference sent from the desktop composer.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct AttachmentTokenDto {
    pub token: String,
    pub path: String,
}

/// Parameters for the interactive `submit_prompt` helper method. Extra fields
/// are optional so existing desktop `{prompt, attachments}` requests decode
/// unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct SubmitPromptParams {
    pub prompt: String,
    #[serde(default)]
    pub attachments: Vec<AttachmentTokenDto>,
    /// Emit `RawSchemaEvent` passthrough alongside the DTO envelopes — the CLI
    /// and TUI consume raw schema-v1 events for full-fidelity rendering.
    #[serde(default)]
    pub raw_events: bool,
    /// Run media capture on the prompt text before appending it (`soyo chat`
    /// parity for pasted media markers).
    #[serde(default)]
    pub capture_media: bool,
    /// Session the prompt must land on. When the helper's prepared session is
    /// empty the helper resumes this id; a different prepared id is rejected
    /// with `session_changed` instead of silently writing to the wrong session.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Personal context for this run only (`soyo chat` passes preferences and
    /// session memory on every prompt). Not persisted.
    #[serde(default)]
    pub personal_context: Option<PersonalContextDto>,
    #[serde(default)]
    pub anthropic_prompt_cache: bool,
}

/// Live run/session control state for interactive clients (`/status`, mode
/// display, post-run compaction checks).
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct RunStateDto {
    pub current: WorkModeDto,
    pub pending: Option<WorkModeDto>,
    pub active_run: bool,
    pub compaction_pending: bool,
    /// Monotonic revision of the helper's mode state; bumped on every
    /// `set_work_mode` so clients can detect changes like `ModeSnapshot::revision`.
    #[specta(type = Number)]
    pub revision: u64,
}

/// Parameters for `resume_session`; the desktop path only sends
/// `session_id`/`accept_config_change`, the CLI resume command passes the
/// full `ResumeOptions` equivalents.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ResumeSessionDto {
    pub session_id: String,
    #[serde(default)]
    pub accept_config_change: bool,
    #[serde(default)]
    pub cwd_explicit: bool,
    #[serde(default)]
    pub profile_explicit: bool,
    #[serde(default)]
    pub mode_override: Option<WorkModeDto>,
}

/// Paginated session metadata returned by `list_sessions`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SessionItemDto {
    pub id: String,
    pub workspace: String,
    pub work_mode: WorkModeDto,
    pub profile: String,
    pub title: Option<String>,
    #[specta(type = Number)]
    pub updated_at: i64,
    pub last_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SessionPageDto {
    pub page: u32,
    pub page_size: u32,
    pub items: Vec<SessionItemDto>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MessageRoleDto {
    User,
    Assistant,
    Thinking,
    Tool,
    System,
    RunSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct RestoredMessageDto {
    pub id: String,
    pub role: MessageRoleDto,
    pub label: String,
    pub text: String,
    pub streaming: bool,
    pub truncated: bool,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub tool_summary: Option<String>,
    pub tool_ok: Option<bool>,
    pub media_refs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ProfileOptionDto {
    pub name: String,
    pub model: String,
    pub protocol: String,
    #[specta(type = Number)]
    pub context_window_tokens: u64,
    pub thinking_effort: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ActiveSessionDto {
    pub id: String,
    pub title: String,
    pub workspace: String,
    pub work_mode: WorkModeDto,
    pub profile: String,
    pub model: String,
    pub messages: Vec<RestoredMessageDto>,
    pub todos: Vec<TodoItemDto>,
    pub history_omitted: bool,
    pub last_usage: Option<UsageDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct BootstrapStateDto {
    pub version: String,
    pub config_ready: bool,
    pub config_path: String,
    pub config_error: Option<String>,
    pub workspace: String,
    pub selected_profile: Option<String>,
    pub selected_model: Option<String>,
    #[specta(type = Option<Number>)]
    pub context_window_tokens: Option<u64>,
    pub thinking_effort: Option<String>,
    pub work_mode: WorkModeDto,
    pub pending_mode: Option<WorkModeDto>,
    pub permission_mode: PermissionModeDto,
    pub sandbox_backend: String,
    pub auto_compact: bool,
    #[specta(type = Option<Number>)]
    pub auto_compact_trigger_tokens: Option<u64>,
    pub auto_compact_trigger_percent: u16,
    pub profiles: Vec<ProfileOptionDto>,
    pub session: Option<ActiveSessionDto>,
    pub run_active: bool,
    pub active_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct SubmitPromptResultDto {
    pub action: String,
    pub session_id: String,
    pub run_id: String,
    /// Interrupted runs recovered while preparing the session (`run_prompt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_runs: Option<u32>,
    /// Tool outcomes left unknown by recovery (`run_prompt`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_tools: Option<u32>,
}

/// Personal context supplied with a `run_prompt` request; mirrors the
/// runtime's `PersonalContext` (explicitly provided, never persisted).
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct PersonalContextDto {
    #[serde(default)]
    pub preferences: Option<String>,
    #[serde(default)]
    pub memory: Option<String>,
}

/// Parameters for the `run_prompt` helper method — the socket-backed form of
/// `soyo run`. `interactive` wires approval/input replies over the socket;
/// headless callers leave it off so runs end `needs_confirmation`/`needs_input`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct RunPromptParams {
    pub prompt: String,
    /// Resume an existing session instead of creating a new one.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub accept_config_change: bool,
    /// Explicit workspace override (`--cwd`); also marks resume `cwd_explicit`.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub mode: Option<WorkModeDto>,
    #[serde(default)]
    pub permissions: Option<PermissionModeDto>,
    #[serde(default)]
    pub approval_file: Option<String>,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub anthropic_prompt_cache: bool,
    #[serde(default)]
    pub personal_context: Option<PersonalContextDto>,
    #[serde(default)]
    pub attachments: Vec<AttachmentTokenDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ModeStateDto {
    pub current: WorkModeDto,
    pub pending: Option<WorkModeDto>,
    pub active_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ResumeSessionResultDto {
    pub bootstrap: BootstrapStateDto,
    pub recovered_runs: u32,
    pub recovered_tools: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackendDto {
    Auto,
    Seatbelt,
    Bubblewrap,
    Host,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ExtensionItemDto {
    pub kind: String,
    pub id: String,
    pub name: String,
    pub description: String,
    pub scope: String,
    pub version: Option<String>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct BackgroundJobDto {
    pub job_id: String,
    pub run_id: String,
    pub parent_call_id: String,
    pub tool: Option<String>,
    pub state: String,
    pub ok: Option<bool>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ModuleStatusDto {
    pub module: String,
    pub description: String,
    pub enabled: bool,
    pub configured_enabled: bool,
    pub override_enabled: Option<bool>,
    pub source: String,
    pub active: bool,
    pub issue: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct McpServerDto {
    pub name: String,
    pub transport: String,
    pub enabled: bool,
    pub target: String,
    pub scopes: Vec<String>,
    pub credential_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct CredentialRecordDto {
    pub reference: String,
    pub server: String,
    pub audience: String,
    pub scopes: Vec<String>,
    pub kind: String,
    #[specta(type = Number)]
    pub revision: u64,
    pub revoked: bool,
    pub oauth_resource: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct MemoryQueryResultDto {
    #[specta(type = Unknown)]
    pub settings: Value,
    #[specta(type = Unknown)]
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct PreparedMemoryReviewDto {
    pub action_id: String,
    pub operation_id: String,
    pub expected_revision: String,
    #[specta(type = Unknown)]
    pub review: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct MemoryConfirmResultDto {
    #[specta(type = Unknown)]
    pub result: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormatDto {
    Json,
    Markdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportScopeDto {
    Transcript,
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct ExportResultDto {
    pub path: String,
    pub format: ExportFormatDto,
    pub scope: ExportScopeDto,
    #[specta(type = Number)]
    pub bytes: u64,
    #[specta(type = Unknown)]
    pub receipt: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct StagedAttachmentDto {
    pub id: String,
    pub token: String,
    pub path: String,
    pub filename: String,
    pub mime: String,
    #[specta(type = Number)]
    pub size_bytes: u64,
    pub kind: String,
    pub preview_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct DesktopPrefsDto {
    pub nickname: Option<String>,
    /// Data URL (`data:image/...;base64,...`) for the configured avatar, if any.
    pub avatar_data: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct GlobalPromptDto {
    /// Editable content of the user-scope instructions file.
    pub content: String,
    /// Absolute path the editor writes to (`~/.soyo/AGENTS.md`).
    pub path: String,
    /// Path the content was read from, when a legacy file was used.
    pub source_path: Option<String>,
}
