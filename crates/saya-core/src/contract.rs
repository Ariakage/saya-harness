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
    Review,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub ok: bool,
    pub data: Option<Value>,
    pub error: Option<ErrorInfo>,
    pub truncated: bool,
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

/// Runtime failures are returned across adapter boundaries, never treated as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundaryError(pub ErrorInfo);
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
    #[serde(rename = "run.finished")]
    RunFinished(RunResult),
}
