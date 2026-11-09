//! Adapter boundaries; implementations and orchestration are added in later tasks.

use crate::contract::{
    BoundaryError, Event, HistoryItem, ModelPurpose, Run, RunResult, Session, ToolCall, ToolResult,
    Usage,
};
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// A fallible adapter operation. It may perform I/O according to the adapter.
pub type Operation<'a, T> = Pin<Box<dyn Future<Output = Result<T, BoundaryError>> + Send + 'a>>;

/// Cooperative cancellation signal supplied by the runtime.
///
/// Adapters must stop or clean up their work when cancellation is observed and
/// return an error; dropping a future alone does not guarantee process cleanup.
pub trait Cancellation: Send + Sync {
    fn is_cancelled(&self) -> bool;

    /// Resolve when cancelled, including when cancellation happened earlier.
    /// This permits an in-flight adapter to wake without polling a busy loop.
    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

/// A model request containing ordered context and tool schemas.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub request_id: String,
    pub purpose: ModelPurpose,
    pub profile: String,
    pub instructions: String,
    /// Per-request system suffix. Keep it after reusable static instructions.
    pub dynamic_instructions: Option<String>,
    /// Provider output token limit; `None` uses the execution profile.
    pub max_output_tokens: Option<u32>,
    pub history: Vec<HistoryItem>,
    pub tools: Vec<ToolDefinition>,
}
impl ModelRequest {
    /// Combine system instructions for providers without content-block support.
    pub fn combined_instructions(&self) -> String {
        let mut text = self.instructions.clone();
        if let Some(suffix) = &self.dynamic_instructions {
            text.push_str(suffix);
        }
        text
    }
}

/// Model-facing tool declaration; execution must still validate arguments.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Streaming update. A completed turn retains ordered provider items and calls.
#[derive(Debug, Clone)]
pub enum ModelUpdate {
    TextDelta(String),
    /// Provider-returned plaintext thinking, separate from the visible answer.
    /// Signature, redacted and encrypted provider fields are never text deltas.
    ThinkingDelta {
        text: String,
    },
    /// A provider response event with no visible text. A later failure must
    /// not be retried after receiving any part of the response.
    Progress,
    Finished {
        items: Vec<HistoryItem>,
        usage: Option<Usage>,
    },
}

/// An in-flight model response; adapters report incomplete streams as errors.
pub trait ModelStream: Send {
    /// Read the next update; `None` means the stream ended, not that it succeeded.
    /// Cancellation and transport/protocol errors are returned as `BoundaryError`.
    fn next<'a>(&'a mut self, cancel: &'a dyn Cancellation) -> Operation<'a, Option<ModelUpdate>>;
}

/// Starts a provider request. Review calls must not be supplied executable tools.
pub trait Model: Send + Sync {
    /// Local counting of the exact serialized request, with no network I/O.
    fn request_counter(&self) -> Option<&dyn crate::context_builder::RequestCounter> {
        None
    }

    /// Verify that a persisted execution session still matches its provider,
    /// profile and model. Test doubles may use the default implementation.
    fn validate_session(&self, _session: &Session) -> Result<(), BoundaryError> {
        Ok(())
    }
    /// Start a request, possibly making network calls. Failures and cancellation
    /// return errors; retries and budgets are coordinated by the runtime.
    fn start<'a>(
        &'a self,
        request: &'a ModelRequest,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, Box<dyn ModelStream>>;
}

/// A local tool adapter. The scheduler handles mode, permission and persistence.
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    /// Host capture channel. Only trusted adapters can return sealed objects.
    /// The default preserves existing adapters and does not trust tool-body refs.
    fn execute_captured<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
        _outputs: Option<&'a crate::output_store::OutputStore>,
    ) -> Operation<'a, crate::output_store::CapturedToolResult> {
        Box::pin(async move {
            Ok(crate::output_store::CapturedToolResult {
                result: self.execute(call, context, cancel).await?,
                outputs: Vec::new(),
                capture_errors: Vec::new(),
            })
        })
    }

    /// Execute one complete call after validation and authorization. This can
    /// have side effects; callers must persist intent before dispatch, and must
    /// not automatically retry. Return cancellation or adapter failures as errors.
    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult>;
}

/// Execution context selected by the runtime, not by model-provided arguments.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolContext {
    pub run: Run,
    pub workspace: std::path::PathBuf,
}

/// Durable state transitions. A tool result must be paired with its original call ID.
#[derive(Debug, Clone)]
pub enum StoreChange {
    Session(Session),
    RunStarted(Run),
    History {
        session_id: crate::contract::SessionId,
        item: HistoryItem,
    },
    /// Record the source metadata supplied to a model request without its content.
    ProjectContext {
        run_id: crate::contract::RunId,
        record: crate::project_context::ContextRecord,
    },
    /// Durable runtime activity which may also be delivered as an event.
    Activity {
        run_id: crate::contract::RunId,
        payload: crate::contract::EventPayload,
    },
    /// Per-run counters, duration and provider-reported usage.
    RunMetrics {
        run_id: crate::contract::RunId,
        metrics: crate::metrics::RunMetrics,
    },
    ToolIntent {
        run_id: crate::contract::RunId,
        call: ToolCall,
    },
    ToolFinished {
        run_id: crate::contract::RunId,
        call_id: String,
        result: ToolResult,
    },
    /// Host-sealed output identities, committed with the matching tool result.
    OutputReferences {
        run_id: crate::contract::RunId,
        call_id: String,
        outputs: Vec<crate::output_store::OutputMetadata>,
    },
    /// Record waiting, denied, or unknown states without executing an operation.
    ToolState {
        run_id: crate::contract::RunId,
        call_id: String,
        state: crate::contract::ToolState,
    },
    /// Confirmation request only; decisions or reusable grants are not restored.
    ApprovalRequested(crate::contract::ApprovalRequest),
    RunFinished(RunResult),
    Event(Event),
    /// Persist an event from a sequence range reserved for a session writer.
    ReservedEvent(Event),
}

/// Storage boundary. The available in-memory implementation is not durable.
pub trait Store: Send + Sync {
    /// Trusted host output records for recovery. Tool bodies are not a registry.
    fn output_references(
        &self,
        _id: crate::contract::SessionId,
    ) -> Operation<'_, Vec<crate::output_store::OutputMetadata>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    /// Read the saved session and its ordered history. Returns `None` if absent;
    /// errors include inaccessible or invalid data. This never replays tools.
    fn load_session<'a>(&'a self, id: crate::contract::SessionId)
    -> Operation<'a, Option<Session>>;

    /// Commit all changes atomically or return an error. An interrupted commit
    /// may have an unknown outcome; callers must not assume it was rolled back.
    /// The runtime must stop scheduling on failure and must persist a tool's
    /// intent before dispatch and its result before subsequent dispatch.
    fn commit<'a>(&'a self, changes: &'a [StoreChange]) -> Operation<'a, ()>;
}

/// Receives a runtime event independently of any terminal or UI framework.
pub trait EventSink {
    /// Deliver an event in order; return an error if delivery fails. Persisted
    /// business events must already be committed before being acknowledged here.
    fn emit(&mut self, event: &Event) -> Result<(), BoundaryError>;
}
