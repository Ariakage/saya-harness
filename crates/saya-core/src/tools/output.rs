//! Bounded, session-scoped retrieval of host-owned output references.
use crate::{
    boundary::{Cancellation, Operation, Tool, ToolContext, ToolDefinition},
    contract::{BoundaryError, ErrorInfo, ToolCall, ToolResult},
    output_store::{OutputCursor, OutputLimits, OutputStore, ReadOptions, SearchOptions},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

const PAGE_BYTES: usize = 8192;
const PAGE_LINES: usize = 200;
const SEARCH_MATCHES: usize = 50;
const SEARCH_CONTEXT: usize = 4;

fn failure(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

#[derive(Debug, Clone, Copy)]
pub enum OutputToolKind {
    Read,
    Search,
}

impl OutputToolKind {
    pub fn definition(self, limits: OutputLimits) -> ToolDefinition {
        let bytes = PAGE_BYTES.min(limits.max_page_bytes);
        let lines = PAGE_LINES.min(limits.max_page_lines);
        let matches = SEARCH_MATCHES.min(limits.max_search_matches);
        let cursor = json!({"type":"object", "properties":{
            "segment":{"type":"integer","minimum":0},
            "byte_offset":{"type":"integer","minimum":0},
            "view_sha256":{"type":"string","pattern":"^[0-9a-f]{64}$"}},
            "required":["segment","byte_offset","view_sha256"],"additionalProperties":false});
        let mut properties = json!({
            "output_ref":{"type":"string","minLength":1,"maxLength":160},
            "cursor":cursor,
            "max_bytes":{"type":"integer","minimum":1,"maximum":bytes}});
        let (name, description, required) = match self {
            Self::Read => {
                properties["max_lines"] = json!({"type":"integer","minimum":1,"maximum":lines});
                (
                    "output_read",
                    "Read a bounded retained output page from the current session. Check capture coverage and gaps.",
                    json!(["output_ref"]),
                )
            }
            Self::Search => {
                properties["pattern"] = json!({"type":"string","minLength":1,"maxLength":1024});
                properties["regex"] = json!({"type":"boolean"});
                properties["max_matches"] = json!({"type":"integer","minimum":1,"maximum":matches});
                properties["context_lines"] =
                    json!({"type":"integer","minimum":0,"maximum":SEARCH_CONTEXT});
                (
                    "output_search",
                    "Search retained output from the current session using literal text or a bounded regular expression. Results declare coverage.",
                    json!(["output_ref", "pattern"]),
                )
            }
        };
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            input_schema: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    output_ref: String,
    cursor: Option<OutputCursor>,
    max_bytes: Option<usize>,
    max_lines: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    output_ref: String,
    cursor: Option<OutputCursor>,
    pattern: String,
    #[serde(default)]
    regex: bool,
    max_bytes: Option<usize>,
    max_matches: Option<usize>,
    context_lines: Option<usize>,
}

enum ParsedCall {
    Read(String, ReadOptions),
    Search(String, SearchOptions),
}

fn parse(call: &ToolCall, limits: OutputLimits) -> Result<ParsedCall, BoundaryError> {
    if call.arguments.len() > 64 * 1024 {
        return Err(failure(
            "invalid_args",
            "output query arguments exceed their limit",
        ));
    }
    let parsed = match call.name.as_str() {
        "output_read" => {
            let args: ReadArgs = serde_json::from_str(&call.arguments)
                .map_err(|_| failure("invalid_args", "invalid output read arguments"))?;
            let options = ReadOptions {
                cursor: args.cursor,
                max_bytes: args
                    .max_bytes
                    .unwrap_or(PAGE_BYTES.min(limits.max_page_bytes)),
                max_lines: args.max_lines.unwrap_or(100.min(limits.max_page_lines)),
            };
            if options.max_bytes == 0
                || options.max_bytes > PAGE_BYTES.min(limits.max_page_bytes)
                || options.max_lines == 0
                || options.max_lines > PAGE_LINES.min(limits.max_page_lines)
            {
                return Err(failure("invalid_args", "output read exceeds page limits"));
            }
            ParsedCall::Read(args.output_ref, options)
        }
        "output_search" => {
            let args: SearchArgs = serde_json::from_str(&call.arguments)
                .map_err(|_| failure("invalid_args", "invalid output search arguments"))?;
            let options = SearchOptions {
                pattern: args.pattern,
                regex: args.regex,
                cursor: args.cursor,
                max_bytes: args
                    .max_bytes
                    .unwrap_or(PAGE_BYTES.min(limits.max_page_bytes)),
                max_matches: args
                    .max_matches
                    .unwrap_or(SEARCH_MATCHES.min(limits.max_search_matches)),
                context_lines: args.context_lines.unwrap_or(2),
            };
            if options.pattern.is_empty()
                || options.pattern.len() > 1024
                || options.max_bytes == 0
                || options.max_bytes > PAGE_BYTES.min(limits.max_page_bytes)
                || options.max_matches == 0
                || options.max_matches > SEARCH_MATCHES.min(limits.max_search_matches)
                || options.context_lines > SEARCH_CONTEXT
            {
                return Err(failure(
                    "invalid_args",
                    "output search exceeds query limits",
                ));
            }
            ParsedCall::Search(args.output_ref, options)
        }
        _ => return Err(failure("unknown_tool", "unsupported output query tool")),
    };
    let reference = match &parsed {
        ParsedCall::Read(reference, _) | ParsedCall::Search(reference, _) => reference,
    };
    if reference.is_empty()
        || reference.len() > 160
        || !reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return Err(failure(
            "invalid_args",
            "output reference has an invalid form",
        ));
    }
    Ok(parsed)
}

pub(crate) fn validate_call(call: &ToolCall, limits: OutputLimits) -> Result<(), BoundaryError> {
    parse(call, limits).map(|_| ())
}

struct StopOnDrop(Arc<AtomicBool>);
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Read-only adapter. The dispatcher authorizes each call and records its
/// intent/result; the store independently enforces session ownership.
pub struct OutputTool {
    store: Arc<OutputStore>,
    kind: OutputToolKind,
}
impl OutputTool {
    pub fn new(store: Arc<OutputStore>, kind: OutputToolKind) -> Self {
        Self { store, kind }
    }
}
impl Tool for OutputTool {
    fn definition(&self) -> ToolDefinition {
        self.kind.definition(self.store.limits())
    }

    fn execute<'a>(
        &'a self,
        call: &'a ToolCall,
        context: &'a ToolContext,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, ToolResult> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "output query cancelled"));
            }
            if call.name != self.definition().name {
                return Err(failure(
                    "invalid_args",
                    "output query tool identity differs",
                ));
            }
            let parsed = parse(call, self.store.limits())?;
            let store = self.store.clone();
            let session = context.run.session_id;
            let stop = StopOnDrop(Arc::new(AtomicBool::new(false)));
            let worker_stop = stop.0.clone();
            let mut task = tokio::task::spawn_blocking(move || {
                let value = match parsed {
                    ParsedCall::Read(reference, options) => serde_json::to_value(
                        store.read_cancellable(session, &reference, options, &worker_stop)?,
                    ),
                    ParsedCall::Search(reference, options) => serde_json::to_value(
                        store.search_cancellable(session, &reference, options, &worker_stop)?,
                    ),
                };
                value.map_err(|_| {
                    failure("serialization_error", "cannot encode output query result")
                })
            });
            let outcome: Result<Value, BoundaryError> = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    stop.0.store(true, Ordering::Relaxed);
                    let _ = task.await;
                    return Err(failure("cancelled", "output query cancelled"));
                }
                result = &mut task => result.map_err(|_| failure("tool_error", "output query worker failed"))?,
            };
            if cancel.is_cancelled() {
                return Err(failure("cancelled", "output query cancelled"));
            }
            Ok(match outcome {
                Ok(data) => {
                    let truncated = data
                        .get("next_cursor")
                        .is_some_and(|value| !value.is_null())
                        || data.pointer("/metadata/artifact_complete") == Some(&Value::Bool(false))
                        || data
                            .get("matches")
                            .and_then(Value::as_array)
                            .is_some_and(|matches| {
                                matches
                                    .iter()
                                    .any(|hit| hit.get("truncated") == Some(&Value::Bool(true)))
                            });
                    ToolResult {
                        ok: true,
                        truncated,
                        data: Some(data),
                        error: None,
                    }
                }
                Err(error) => ToolResult {
                    ok: false,
                    data: None,
                    error: Some(error.0),
                    truncated: false,
                },
            })
        })
    }
}
