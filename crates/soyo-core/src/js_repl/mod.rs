//! Experimental opt-in read-only JavaScript orchestration surface (`F-21`).
//!
//! Per `D-082`/`D-083`: strictly disabled by default. Model-authored scripts
//! run inside a sandboxed `__soyo-js-worker` subprocess (the same binary,
//! QuickJS via `rquickjs`). Every tool call the script issues crosses a
//! bounded RPC back to the host, where the runtime re-dispatches it through
//! the exact deny/root/hooks/approval/budget/cancellation path used for
//! model-emitted calls. Only read-only tools are bridged; `exec`, writes,
//! managed/interactive controls and `js_repl` itself are denied before
//! dispatch. Intermediate results stay inside the worker; only the script's
//! final value becomes the outer call's `ToolResult`.

mod host;
mod protocol;
pub(crate) mod worker;

use crate::{
    contract::{BoundaryError, ErrorInfo, ToolCall, error_codes},
    registry::ToolEffect,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(crate) use host::ReplWorker;
pub use protocol::{HostMessage, MAX_WIRE_BYTES, WorkerInit, WorkerMessage};

/// Registered tool name exposed to the model when the feature is enabled.
pub const TOOL_NAME: &str = "js_repl";
/// Private argv[0] mode inside the same binary, never a model argument.
pub(crate) const WORKER_COMMAND: &str = "__soyo-js-worker";

/// Effective runtime limits for one enabled REPL. All values are bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplLimits {
    /// QuickJS heap cap inside the worker.
    pub memory_bytes: usize,
    /// QuickJS stack cap inside the worker.
    pub stack_bytes: usize,
    /// Maximum bridged tool calls per script run.
    pub max_calls: u32,
    /// Maximum accepted script source size.
    pub script_bytes: usize,
    /// Maximum single protocol message in either direction.
    pub message_bytes: usize,
    /// Default wall-clock timeout when the call omits `timeout_seconds`.
    pub default_timeout: Duration,
    /// Hard upper bound for `timeout_seconds`.
    pub max_timeout: Duration,
}
impl Default for ReplLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 64 * 1024 * 1024,
            stack_bytes: 1024 * 1024,
            max_calls: 64,
            script_bytes: 256 * 1024,
            message_bytes: 8 * 1024 * 1024,
            default_timeout: Duration::from_secs(120),
            max_timeout: Duration::from_secs(600),
        }
    }
}
impl ReplLimits {
    /// Reject zero or over-range limits before any worker is prepared.
    pub fn validate(&self) -> Result<(), BoundaryError> {
        let bytes = [
            self.memory_bytes,
            self.stack_bytes,
            self.script_bytes,
            self.message_bytes,
        ];
        if self.max_calls == 0
            || bytes.contains(&0)
            || self.memory_bytes > 1024 * 1024 * 1024
            || self.stack_bytes > 64 * 1024 * 1024
            || self.script_bytes > 4 * 1024 * 1024
            || self.message_bytes > protocol::MAX_WIRE_BYTES
            || self.default_timeout.is_zero()
            || self.max_timeout.is_zero()
            || self.default_timeout > self.max_timeout
            || self.max_timeout > Duration::from_secs(600)
        {
            return Err(failure(
                "invalid_config",
                "js_repl limits are outside their supported range",
            ));
        }
        Ok(())
    }
}

/// JavaScript engine selected for the sandboxed worker. `Quickjs` is the
/// embedded default; `Node` is the controlled external runtime retained as
/// the B2 extension point by D-083 and never discovered implicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplEngine {
    /// `__soyo-js-worker` same-binary subprocess with embedded QuickJS.
    Quickjs,
    /// External `node` binary spawned inside the same prepared sandbox. The
    /// program path is host-validated and mounted read-only.
    Node { program: std::path::PathBuf },
}
/// Fully resolved `js_repl` configuration: limits, engine and the extra
/// read-only filesystem exposure an external runtime needs inside the
/// sandbox. `read_files`/`read_roots` must be folded into the sandbox policy
/// before the prepared environment is created.
#[derive(Debug, Clone)]
pub struct JsReplConfig {
    pub limits: ReplLimits,
    pub engine: ReplEngine,
    pub read_files: Vec<std::path::PathBuf>,
    pub read_roots: Vec<std::path::PathBuf>,
}
impl JsReplConfig {
    /// Embedded-QuickJS configuration for embedders that set limits directly;
    /// no extra filesystem exposure is required.
    pub fn quickjs(limits: ReplLimits) -> Self {
        Self {
            limits,
            engine: ReplEngine::Quickjs,
            read_files: Vec::new(),
            read_roots: Vec::new(),
        }
    }
}
/// `[js_repl] engine = "…"` selection in host configuration.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JsEngine {
    #[default]
    Quickjs,
    Node,
}
/// `[js_repl.node]` external runtime pinning. `path` is required whenever
/// `engine = "node"`; `roots` exposes additional install directories a
/// non-self-contained Node build needs for its shared libraries.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JsNodeSettings {
    pub path: Option<std::path::PathBuf>,
    pub roots: Vec<std::path::PathBuf>,
}
/// Host configuration section `[js_repl]`; disabled unless `enabled = true`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct JsReplSettings {
    pub enabled: bool,
    pub engine: JsEngine,
    pub node: JsNodeSettings,
    pub max_calls: Option<u32>,
    pub memory_bytes: Option<u64>,
    pub stack_bytes: Option<u64>,
    pub script_bytes: Option<u64>,
    pub message_bytes: Option<u64>,
    pub default_timeout_seconds: Option<u64>,
    pub max_timeout_seconds: Option<u64>,
}
impl JsReplSettings {
    /// Validate and resolve the effective configuration. `Ok(None)` when
    /// disabled. Engine-specific pinning is fail-closed: the `node` section
    /// without `engine = "node"`, a missing/non-absolute binary path and any
    /// non-existent read root are configuration errors, never silent skips.
    pub fn resolve(&self) -> Result<Option<JsReplConfig>, BoundaryError> {
        if !self.enabled {
            return Ok(None);
        }
        let defaults = ReplLimits::default();
        let u64_to_usize = |value: Option<u64>, fallback: usize| {
            value
                .map(|value| usize::try_from(value).unwrap_or(usize::MAX))
                .unwrap_or(fallback)
        };
        let limits = ReplLimits {
            max_calls: self.max_calls.unwrap_or(defaults.max_calls),
            memory_bytes: u64_to_usize(self.memory_bytes, defaults.memory_bytes),
            stack_bytes: u64_to_usize(self.stack_bytes, defaults.stack_bytes),
            script_bytes: u64_to_usize(self.script_bytes, defaults.script_bytes),
            message_bytes: u64_to_usize(self.message_bytes, defaults.message_bytes),
            default_timeout: Duration::from_secs(
                self.default_timeout_seconds
                    .unwrap_or(defaults.default_timeout.as_secs()),
            ),
            max_timeout: Duration::from_secs(
                self.max_timeout_seconds
                    .unwrap_or(defaults.max_timeout.as_secs()),
            ),
        };
        limits.validate()?;
        let mut read_files = Vec::new();
        let mut read_roots = Vec::new();
        let engine = match self.engine {
            JsEngine::Quickjs => {
                if self.node.path.is_some() || !self.node.roots.is_empty() {
                    return Err(failure(
                        "invalid_config",
                        "[js_repl.node] settings require engine = \"node\"",
                    ));
                }
                ReplEngine::Quickjs
            }
            JsEngine::Node => {
                let path = self.node.path.as_ref().ok_or_else(|| {
                    failure(
                        "invalid_config",
                        "engine = \"node\" requires [js_repl.node] path",
                    )
                })?;
                if !path.is_absolute() {
                    return Err(failure(
                        "invalid_config",
                        "[js_repl.node] path must be an absolute path",
                    ));
                }
                let program = std::fs::canonicalize(path)
                    .map_err(|_| failure("invalid_config", "[js_repl.node] path does not exist"))?;
                if !program.is_file() {
                    return Err(failure(
                        "invalid_config",
                        "[js_repl.node] path is not a regular file",
                    ));
                }
                read_files.push(program.clone());
                for root in &self.node.roots {
                    if !root.is_absolute() {
                        return Err(failure(
                            "invalid_config",
                            "[js_repl.node] roots must be absolute paths",
                        ));
                    }
                    let root = std::fs::canonicalize(root).map_err(|_| {
                        failure("invalid_config", "[js_repl.node] roots must exist")
                    })?;
                    if !root.is_dir() {
                        return Err(failure(
                            "invalid_config",
                            "[js_repl.node] roots must be directories",
                        ));
                    }
                    read_roots.push(root);
                }
                ReplEngine::Node { program }
            }
        };
        Ok(Some(JsReplConfig {
            limits,
            engine,
            read_files,
            read_roots,
        }))
    }
}

/// Parsed and validated `js_repl` tool call arguments.
#[derive(Debug, Clone)]
pub struct ReplArgs {
    pub script: String,
    pub timeout: Duration,
}
/// Validate a call against the schema and limits. This also binds approvals:
/// the same normalization is reapplied when the permit is consumed.
pub fn parse_args(call: &ToolCall, limits: &ReplLimits) -> Result<ReplArgs, BoundaryError> {
    let args: serde_json::Value = serde_json::from_str(&call.arguments)
        .map_err(|_| failure(error_codes::INVALID_ARGS, "js_repl arguments must be JSON"))?;
    let map = args.as_object().ok_or_else(|| {
        failure(
            error_codes::INVALID_ARGS,
            "js_repl arguments must be an object",
        )
    })?;
    if map
        .keys()
        .any(|key| key != "script" && key != "timeout_seconds")
    {
        return Err(failure(
            error_codes::INVALID_ARGS,
            "js_repl accepts only script and timeout_seconds",
        ));
    }
    let script = map
        .get("script")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            failure(
                error_codes::INVALID_ARGS,
                "js_repl requires a script string",
            )
        })?;
    if script.is_empty() || script.len() > limits.script_bytes {
        return Err(failure(
            error_codes::INVALID_ARGS,
            "js_repl script is empty or exceeds the byte limit",
        ));
    }
    let seconds = map
        .get("timeout_seconds")
        .map(|value| {
            value.as_u64().ok_or_else(|| {
                failure(
                    error_codes::INVALID_ARGS,
                    "timeout_seconds must be a positive integer",
                )
            })
        })
        .transpose()?;
    let timeout = Duration::from_secs(seconds.unwrap_or(limits.default_timeout.as_secs()));
    if timeout.is_zero() || timeout > limits.max_timeout {
        return Err(failure(
            error_codes::INVALID_ARGS,
            "js_repl timeout is outside the configured range",
        ));
    }
    Ok(ReplArgs {
        script: script.to_owned(),
        timeout,
    })
}

/// Definition projected to the model when the feature is enabled. The
/// description documents the exact bridge contract; examples satisfy the
/// `DEV-093` schema/secret validation path.
pub fn definition() -> crate::boundary::ToolDefinition {
    crate::boundary::ToolDefinition {
        name: TOOL_NAME.into(),
        description: concat!(
            "Experimental: run a read-only JavaScript script inside an isolated worker. ",
            "Inside the script, `await soyo.callTool(name, args)` invokes allowed read-only tools; ",
            "every call is authorized individually against permission rules, so dynamically ",
            "computed arguments are still checked. Intermediate results stay inside the script; ",
            "only the final `return` value becomes this call's result. The script has no ",
            "filesystem, network, process, module or secret access. write/exec/interactive tools ",
            "and js_repl itself cannot be bridged."
        )
        .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "script": {
                    "type": "string",
                    "description": "JavaScript source evaluated as an async function body. Use `return` to produce the call result."
                },
                "timeout_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600,
                    "description": "Optional wall-clock timeout for the whole script run."
                }
            },
            "required": ["script"],
            "additionalProperties": false
        }),
        examples: vec![
            serde_json::json!({
                "script": "const hits = await soyo.callTool(\"search_text\", {\"pattern\": \"ToolDefinition\", \"path\": \"crates\"});\nreturn hits.ok ? hits.data : hits.error;"
            }),
            serde_json::json!({
                "script": "const files = await soyo.callTool(\"find_files\", {\"pattern\": \"*.md\", \"path\": \"docs\"});\nconst names = files.ok ? (files.data.files ?? []) : [];\nreturn {count: names.length, names: names.slice(0, 10)};",
                "timeout_seconds": 60
            }),
        ],
    }
}

/// Decide whether a script-issued call may enter the normal dispatch path.
/// Only read-only tools pass; `exec`, writes, managed/interactive controls,
/// `js_repl` recursion and anything classified as a mutation are denied
/// before authorization so a failed call still leaves durable intent and a
/// denied record.
pub(crate) fn allowed_sub_call(
    name: &str,
    descriptor: Option<&crate::registry::OperationDescriptor>,
) -> bool {
    if name == TOOL_NAME
        || name == "ask_user"
        || name.starts_with("background_")
        || name.starts_with("memory_")
        || name.starts_with("skill_")
        || name.starts_with("checkpoint_")
    {
        return false;
    }
    match crate::registry::builtin_effect(name) {
        // read_file/list_dir/find_files/search_text plus discovery and output
        // queries; todo_write is session metadata but mutates run state.
        Some(ToolEffect::FileRead | ToolEffect::WebRead) => true,
        Some(ToolEffect::SessionMetadata) => {
            matches!(
                name,
                "tool_search" | "tool_describe" | "output_read" | "output_search"
            )
        }
        Some(_) => false,
        // External adapters only bridge when the descriptor itself declares a
        // read-only effect; unknown external effects fail closed.
        None => matches!(
            descriptor.map(|descriptor| descriptor.effect),
            Some(ToolEffect::ExternalReadOnly | ToolEffect::WebRead)
        ),
    }
}

fn failure(code: &str, message: impl Into<String>) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{OperationDescriptor, ToolSource};
    use serde_json::json;

    fn call(arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "c1".into(),
            name: TOOL_NAME.into(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn settings_stay_disabled_by_default() {
        assert!(JsReplSettings::default().resolve().unwrap().is_none());
        let enabled = JsReplSettings {
            enabled: true,
            ..Default::default()
        };
        let resolved = enabled.resolve().unwrap().unwrap();
        assert_eq!(resolved.limits, ReplLimits::default());
        assert_eq!(resolved.engine, ReplEngine::Quickjs);
        assert!(resolved.read_files.is_empty() && resolved.read_roots.is_empty());
    }

    #[test]
    fn node_engine_requires_a_pinned_binary() {
        let enabled_node = |node: JsNodeSettings| JsReplSettings {
            enabled: true,
            engine: JsEngine::Node,
            node,
            ..Default::default()
        };
        assert!(enabled_node(JsNodeSettings::default()).resolve().is_err());
        assert!(
            enabled_node(JsNodeSettings {
                path: Some("relative/node".into()),
                ..Default::default()
            })
            .resolve()
            .is_err()
        );
        assert!(
            enabled_node(JsNodeSettings {
                path: Some("/definitely/missing/node".into()),
                ..Default::default()
            })
            .resolve()
            .is_err()
        );
        let with_binary = enabled_node(JsNodeSettings {
            path: Some(std::path::PathBuf::from("/bin/sh")),
            roots: vec!["/usr/lib".into()],
        });
        let resolved = with_binary.resolve().unwrap().unwrap();
        let ReplEngine::Node { program } = resolved.engine else {
            panic!("expected a node engine")
        };
        assert!(program.is_absolute() && program.is_file());
        assert_eq!(resolved.read_files, vec![program]);
        assert_eq!(resolved.read_roots.len(), 1);
        let misplaced = JsReplSettings {
            enabled: true,
            node: JsNodeSettings {
                path: Some("/bin/sh".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(misplaced.resolve().is_err());
    }

    #[test]
    fn limit_overrides_apply_and_validate() {
        let settings = JsReplSettings {
            enabled: true,
            max_calls: Some(4),
            memory_bytes: Some(8 * 1024 * 1024),
            default_timeout_seconds: Some(10),
            max_timeout_seconds: Some(60),
            ..Default::default()
        };
        let limits = settings.resolve().unwrap().unwrap().limits;
        assert_eq!(limits.max_calls, 4);
        assert_eq!(limits.memory_bytes, 8 * 1024 * 1024);
        assert_eq!(limits.default_timeout, Duration::from_secs(10));
        for bad in [
            JsReplSettings {
                enabled: true,
                max_calls: Some(0),
                ..Default::default()
            },
            JsReplSettings {
                enabled: true,
                memory_bytes: Some(2 * 1024 * 1024 * 1024),
                ..Default::default()
            },
            JsReplSettings {
                enabled: true,
                default_timeout_seconds: Some(120),
                max_timeout_seconds: Some(60),
                ..Default::default()
            },
        ] {
            assert!(bad.resolve().is_err());
        }
    }

    #[test]
    fn parse_args_accepts_valid_script() {
        let limits = ReplLimits::default();
        let parsed = parse_args(
            &call(json!({"script": "return 1;", "timeout_seconds": 30})),
            &limits,
        )
        .unwrap();
        assert_eq!(parsed.script, "return 1;");
        assert_eq!(parsed.timeout, Duration::from_secs(30));
        let parsed = parse_args(&call(json!({"script": "return 1;"})), &limits).unwrap();
        assert_eq!(parsed.timeout, limits.default_timeout);
    }

    #[test]
    fn parse_args_rejects_malformed_calls() {
        let limits = ReplLimits::default();
        for args in [
            json!({}),
            json!({"script": ""}),
            json!({"script": 42}),
            json!({"script": "x", "extra": true}),
            json!({"script": "x", "timeout_seconds": 0}),
            json!({"script": "x", "timeout_seconds": 10_000}),
            json!({"script": "x", "timeout_seconds": -1}),
            json!({"script": "x".repeat(limits.script_bytes + 1)}),
        ] {
            assert!(
                parse_args(&call(args.clone()), &limits).is_err(),
                "{args} should be rejected"
            );
        }
        let malformed = ToolCall {
            call_id: "c2".into(),
            name: TOOL_NAME.into(),
            arguments: "not json".into(),
        };
        assert!(parse_args(&malformed, &limits).is_err());
    }

    #[test]
    fn sub_call_allowlist_only_bridges_read_only_tools() {
        for name in [
            "read_file",
            "list_dir",
            "find_files",
            "search_text",
            "fetch_url",
            "web_search",
            "tool_search",
            "tool_describe",
            "output_read",
            "output_search",
        ] {
            assert!(allowed_sub_call(name, None), "{name} must be bridgeable");
        }
        for name in [
            "exec",
            "write_file",
            "edit_file",
            "apply_patch",
            "todo_write",
            "ask_user",
            "js_repl",
            "background_start",
            "background_status",
            "memory_update",
            "skill_load",
            "checkpoint_create",
            "browser_snapshot",
            "browser_open",
        ] {
            assert!(!allowed_sub_call(name, None), "{name} must not bridge");
        }
        assert!(!allowed_sub_call("mcp__x__y_deadbeef", None));
        let descriptor = OperationDescriptor {
            name: "mcp__srv__search_abc".into(),
            source: ToolSource::Mcp {
                server: "srv".into(),
                remote_name: "search".into(),
                revision: "1".into(),
            },
            effect: ToolEffect::ExternalReadOnly,
            fingerprint: "fp".into(),
        };
        assert!(allowed_sub_call(&descriptor.name, Some(&descriptor)));
        let mut denied = descriptor.clone();
        denied.effect = ToolEffect::ExternalUnknown;
        assert!(!allowed_sub_call(&denied.name, Some(&denied)));
    }
}
