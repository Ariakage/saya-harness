//! Runtime data and schema-v1 event/result types.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use uuid::Uuid;

/// Schema version for machine-readable events and run results.
pub const SCHEMA_VERSION: u8 = 1;

/// A session identifier. New sessions use UUIDv7; persisted IDs must be retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

/// A run identifier. New runs use UUIDv7; persisted IDs must be retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub Uuid);

impl SessionId {
    /// Generate a new UUIDv7 session ID. Does not persist the session.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}
impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}
impl RunId {
    /// Generate a new UUIDv7 run ID. Does not start or persist a run.
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}
impl Default for RunId {
    fn default() -> Self {
        Self::new()
    }
}

/// User-selected task mode; independent of permission mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    Plan,
    Build,
    Code,
    #[default]
    Work,
}

/// User-selected permission policy. Enforcement belongs to the scheduler.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    #[default]
    Auto,
    Bypass,
    /// Each tool invocation requires an individual human approval.
    /// `review` remains readable for sessions saved by older versions.
    #[serde(alias = "review")]
    Manual,
}

/// A persisted session's identity and configuration references, never grants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub workspace: PathBuf,
    pub work_mode: WorkMode,
    pub profile: String,
    pub protocol: ModelProtocol,
    pub model: String,
    pub history: Vec<HistoryItem>,
}

/// One of the independently implemented provider protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProtocol {
    ChatCompletions,
    Responses,
    AnthropicMessages,
}

/// Ordered context, including provider-owned opaque items required for replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HistoryItem {
    UserText(String),
    AssistantText(String),
    /// Partial visible output retained after an interrupted model stream.
    AssistantPartial(String),
    ToolCall(ToolCall),
    ToolResult {
        call_id: String,
        result: ToolResult,
    },
    /// Adapter-owned item; never treat as user-visible reasoning text.
    ProviderItem {
        protocol: ModelProtocol,
        value: Value,
    },
}

/// Provider call ID and complete raw arguments; parsing can fail without losing data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

/// Structured tool outcome. Unknown outcomes must not be replayed automatically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<ErrorInfo>,
    pub truncated: bool,
    /// Host-only durable artifact/media references produced by this exact
    /// tool action. The host inserts this field; untrusted tool data never
    /// does, and the model projection clears it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_refs: Vec<String>,
}

impl ToolResult {
    /// Construct a successful result carrying structured payload data.
    pub fn success(data: Option<Value>) -> Self {
        Self {
            ok: true,
            data,
            error: None,
            truncated: false,
            media_refs: Vec::new(),
        }
    }

    /// Construct a failed result from a boundary error code and message.
    pub fn failure(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(ErrorInfo {
                code: code.into(),
                message: message.into(),
            }),
            truncated: false,
            media_refs: Vec::new(),
        }
    }

    /// Return the structured error code when the result failed with one.
    pub fn error_code(&self) -> Option<&str> {
        self.error.as_ref().map(|error| error.code.as_str())
    }

    /// Check whether this result carries the specified error code.
    pub fn has_error_code(&self, code: &str) -> bool {
        self.error_code() == Some(code)
    }

    /// Model-facing projection. Host-only artifact references never cross the
    /// provider boundary, even when the complete result is serialized.
    pub fn model_view(&self) -> Self {
        let mut projected = self.clone();
        projected.media_refs.clear();
        projected
    }
}

/// Stable error identifier constants used across runtime control flow and boundaries.
pub mod error_codes {
    pub const BACKGROUND_ERROR: &str = "background_error";
    pub const BUDGET_ERROR: &str = "budget_error";
    pub const CANCELLED: &str = "cancelled";
    pub const CHECKPOINT_ERROR: &str = "checkpoint_error";
    pub const CONFLICT: &str = "conflict";
    pub const CONTEXT_BUDGET: &str = "context_budget";
    pub const CONTEXT_LIMIT: &str = "context_limit";
    pub const DURABILITY_REQUIRED: &str = "durability_required";
    pub const EDIT_CONFLICT: &str = "edit_conflict";
    pub const EXECUTABLE_NOT_FOUND: &str = "executable_not_found";
    pub const GOAL_BUDGET: &str = "goal_budget";
    pub const GOAL_WORKSPACE_BUSY: &str = "goal_workspace_busy";
    pub const HOOK_DENIED: &str = "hook_denied";
    pub const HOST_PROTECTION_UNAVAILABLE: &str = "host_protection_unavailable";
    pub const INCOMPLETE_MODEL_RESPONSE: &str = "incomplete_model_response";
    pub const INPUT_INTERRUPTED: &str = "input_interrupted";
    pub const INVALID_ARGS: &str = "invalid_args";
    pub const INVALID_CONFIG: &str = "invalid_config";
    pub const INVALID_MODEL_RESPONSE: &str = "invalid_model_response";
    pub const INVALID_PATCH: &str = "invalid_patch";
    pub const INVALID_PATH: &str = "invalid_path";
    pub const INVALID_RESPONSE: &str = "invalid_response";
    pub const INVALID_TOOL: &str = "invalid_tool";
    pub const INVALID_WORKER_REQUEST: &str = "invalid_worker_request";
    pub const INVALID_WORKSPACE: &str = "invalid_workspace";
    pub const LIMIT_REACHED: &str = "limit_reached";
    pub const MEMORY_ERROR: &str = "memory_error";
    pub const MODE_BUSY: &str = "mode_busy";
    pub const MODE_DENIED: &str = "mode_denied";
    pub const MODE_EVENT_LIMIT: &str = "mode_event_limit";
    pub const MODE_STATE_ERROR: &str = "mode_state_error";
    pub const MODEL_CONNECT_TIMEOUT: &str = "model_connect_timeout";
    pub const MODEL_ERROR: &str = "model_error";
    pub const MODEL_TIMEOUT: &str = "model_timeout";
    pub const NEEDS_AUTH: &str = "needs_auth";
    pub const NEEDS_CONFIRMATION: &str = "needs_confirmation";
    pub const NEEDS_INPUT: &str = "needs_input";
    pub const NOT_EXECUTED: &str = "not_executed";
    pub const OUTCOME_UNKNOWN: &str = "outcome_unknown";
    pub const OUTPUT_LIMIT: &str = "output_limit";
    pub const PATCH_CONFLICT: &str = "patch_conflict";
    pub const PATH_DENIED: &str = "path_denied";
    pub const PERMISSION_DENIED: &str = "permission_denied";
    pub const PLUGIN_VIEW_UNKNOWN: &str = "plugin_view_unknown";
    pub const PROCESS_CLEANUP_ERROR: &str = "process_cleanup_error";
    pub const PROCESS_ERROR: &str = "process_error";
    pub const PROJECT_CONTEXT_CHANGED: &str = "project_context_changed";
    pub const PROJECT_CONTEXT_ERROR: &str = "project_context_error";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const REPEATED_TOOL_FAILURE: &str = "repeated_tool_failure";
    pub const RETRY_TIMEOUT: &str = "retry_timeout";
    pub const RUNTIME_ERROR: &str = "runtime_error";
    pub const SANDBOX_CAPABILITY_UNAVAILABLE: &str = "sandbox_capability_unavailable";
    pub const SANDBOX_CLEANUP_UNKNOWN: &str = "sandbox_cleanup_unknown";
    pub const SANDBOX_ENVIRONMENT_CLOSED: &str = "sandbox_environment_closed";
    pub const SANDBOX_PREPARE_FAILED: &str = "sandbox_prepare_failed";
    pub const SERIALIZATION_ERROR: &str = "serialization_error";
    pub const SERVER_ERROR: &str = "server_error";
    pub const SESSION_BUSY: &str = "session_busy";
    pub const SKILL_CONTEXT_CHANGED: &str = "skill_context_changed";
    pub const SKILL_ERROR: &str = "skill_error";
    pub const STALE_APPROVAL: &str = "stale_approval";
    pub const STEERING_ERROR: &str = "steering_error";
    pub const STORAGE_ERROR: &str = "storage_error";
    pub const STORAGE_SENSITIVE_DATA: &str = "storage_sensitive_data";
    pub const TOOL_ABORTED: &str = "tool_aborted";
    pub const TOOL_ERROR: &str = "tool_error";
    pub const TOOL_TIMEOUT: &str = "tool_timeout";
    pub const TRANSIENT_CONNECTION: &str = "transient_connection";
    pub const UNAVAILABLE: &str = "unavailable";
    pub const UNKNOWN_TOOL: &str = "unknown_tool";
    pub const UNSUPPORTED_WORKER_SCHEMA: &str = "unsupported_worker_schema";
    pub const UNSUPPORTED_WORKER_TOOL: &str = "unsupported_worker_tool";
}

/// Persisted execution state; `Unknown` must never be treated as not executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Requested,
    WaitingConfirmation,
    Running,
    Succeeded,
    Failed,
    Denied,
    Unknown,
}

/// Stable error identifier and human-readable description; no credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
}

/// Runtime failure plus an optional provider Retry-After delay. The delay is
/// adapter metadata and is not part of the schema-v1 error object. When a
/// result cannot be saved after a side effect, the third field preserves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryError(
    pub ErrorInfo,
    pub Option<std::time::Duration>,
    pub Option<Box<ToolResult>>,
);
impl BoundaryError {
    /// Construct a boundary error without retry metadata or an observed side effect.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self(
            ErrorInfo {
                code: code.into(),
                message: message.into(),
            },
            None,
            None,
        )
    }

    /// Construct a boundary error from an existing [`ErrorInfo`].
    pub fn from_info(info: ErrorInfo) -> Self {
        Self(info, None, None)
    }

    /// Attach an optional retry delay suggested by the upstream provider.
    pub fn with_retry_after(mut self, delay: Option<std::time::Duration>) -> Self {
        self.1 = delay;
        self
    }

    /// Attach or replace an observed partial/completed tool result.
    pub fn with_observed_result(mut self, result: Option<ToolResult>) -> Self {
        self.2 = result.map(Box::new);
        self
    }

    pub fn set_code(&mut self, code: impl Into<String>) {
        self.0.code = code.into();
    }

    pub fn set_message(&mut self, message: impl Into<String>) {
        self.0.message = message.into();
    }

    pub fn set_observed_result(&mut self, result: Option<ToolResult>) {
        self.2 = result.map(Box::new);
    }

    pub fn info(&self) -> &ErrorInfo {
        &self.0
    }

    pub fn code(&self) -> &str {
        &self.0.code
    }

    pub fn message(&self) -> &str {
        &self.0.message
    }

    pub fn retry_after(&self) -> Option<std::time::Duration> {
        self.1
    }

    pub fn observed_result(&self) -> Option<&ToolResult> {
        self.2.as_deref()
    }

    pub fn has_observed_result(&self) -> bool {
        self.2.is_some()
    }

    pub fn into_observed_result(self) -> Option<Box<ToolResult>> {
        self.2
    }

    pub fn is_code(&self, code: &str) -> bool {
        self.0.code == code
    }

    pub fn is_storage_error(&self) -> bool {
        self.0.code == error_codes::STORAGE_ERROR || self.0.code.starts_with("storage_")
    }

    pub fn is_transient_model_error(&self) -> bool {
        matches!(
            self.0.code.as_str(),
            error_codes::TRANSIENT_CONNECTION
                | error_codes::RATE_LIMITED
                | error_codes::SERVER_ERROR
                | error_codes::MODEL_TIMEOUT
                | error_codes::MODEL_CONNECT_TIMEOUT
        )
    }
}
impl std::fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.0.code, self.0.message)
    }
}
impl std::error::Error for BoundaryError {}

/// A single run and its independently selected permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: RunId,
    pub session_id: SessionId,
    pub work_mode: WorkMode,
    pub permissions: PermissionMode,
}

/// Persistable confirmation request, not an authorization or reusable grant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub call_id: String,
    pub run_id: RunId,
    pub tool: String,
    pub operation_summary: String,
    pub arguments: Value,
    pub cwd: PathBuf,
    pub work_mode: WorkMode,
}

/// User-facing final status; CLI configuration failures also use `failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Completed,
    Failed,
    NeedsConfirmation,
    NeedsInput,
    LimitReached,
    Cancelled,
}

/// Token counts are optional; estimates must be labelled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    pub estimated: bool,
}

/// A verified created or modified target, not a guess about command side effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub path: PathBuf,
    pub operation: String,
}

/// Whether the final state was saved, and any persistence error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Persistence {
    pub saved: bool,
    pub error: Option<ErrorInfo>,
}

/// Schema-v1 final result. Missing values serialize as null, artifacts as an array.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub schema_version: u8,
    pub session_id: Option<SessionId>,
    pub run_id: Option<RunId>,
    pub status: RunStatus,
    pub stop_reason: Option<String>,
    pub text: Option<String>,
    pub error: Option<ErrorInfo>,
    pub usage: Option<Usage>,
    pub artifacts: Vec<Artifact>,
    pub persistence: Option<Persistence>,
}

/// Model requests for task execution and permission review remain distinct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelPurpose {
    Execution,
    Review,
    Compaction,
}

/// Source of a permission decision produced before user confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewSource {
    Rule,
    Model,
}

/// Source of a resolved permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalSource {
    User,
    Preauthorization,
}

/// Host-provided connection lifecycle. These records contain no endpoints or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTransport {
    Stdio,
    Http,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerState {
    Configured,
    Connecting,
    Connected,
    Stopped,
    NeedsAuth,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerStatus {
    pub server: String,
    pub transport: McpTransport,
    pub state: McpServerState,
    pub tools: usize,
}
pub const MAX_MCP_STATUS_SERVERS: usize = 16;

/// Schema-v1 event envelope. Sequence assignment and UTC RFC 3339 timestamps
/// are scheduler responsibilities; serialization alone does not enforce them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub schema_version: u8,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub seq: u64,
    pub timestamp: String,
    #[serde(flatten)]
    pub payload: EventPayload,
}

/// Event type and required data. Ordering and exactly one final event are runtime responsibilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventPayload {
    #[serde(rename = "session.created")]
    SessionCreated {
        work_mode: WorkMode,
        permissions: PermissionMode,
        workspace: PathBuf,
        profile: String,
    },
    #[serde(rename = "session.resumed")]
    SessionResumed {
        work_mode: WorkMode,
        permissions: PermissionMode,
        workspace: PathBuf,
        profile: String,
    },
    #[serde(rename = "run.started")]
    RunStarted {
        work_mode: WorkMode,
        permissions: PermissionMode,
        profile: String,
    },
    #[serde(rename = "mode.change_requested")]
    ModeChangeRequested { from: WorkMode, to: WorkMode },
    #[serde(rename = "mode.changed")]
    ModeChanged { from: WorkMode, to: WorkMode },
    #[serde(rename = "model.started")]
    ModelStarted {
        request_id: String,
        purpose: ModelPurpose,
        profile: String,
    },
    #[serde(rename = "model.text_delta")]
    ModelTextDelta { request_id: String, text: String },
    #[serde(rename = "model.finished")]
    ModelFinished {
        request_id: String,
        purpose: ModelPurpose,
        ok: bool,
        usage: Option<Usage>,
        error: Option<ErrorInfo>,
    },
    #[serde(rename = "tool.requested")]
    ToolRequested {
        call_id: String,
        name: String,
        args: Value,
    },
    #[serde(rename = "permission.reviewed")]
    PermissionReviewed {
        call_id: String,
        decision: String,
        source: ReviewSource,
        reason: String,
    },
    #[serde(rename = "permission.requested")]
    PermissionRequested {
        approval_id: String,
        call_id: String,
        operation_summary: String,
        work_mode: WorkMode,
    },
    #[serde(rename = "permission.resolved")]
    PermissionResolved {
        approval_id: String,
        call_id: String,
        decision: String,
        source: ApprovalSource,
    },
    #[serde(rename = "tool.started")]
    ToolStarted { call_id: String, name: String },
    #[serde(rename = "tool.finished")]
    ToolFinished {
        call_id: String,
        name: String,
        result: ToolResult,
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
    McpStatus { servers: Vec<McpServerStatus> },
    #[serde(rename = "run.finished")]
    RunFinished(RunResult),
}
