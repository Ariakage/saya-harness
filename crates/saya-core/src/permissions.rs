//! Permission rule evaluation. A decision does not itself execute a tool or
//! grant a persistent approval; dispatch must recheck the current context.

use crate::{
    boundary::ToolContext,
    contract::{BoundaryError, ErrorInfo, PermissionMode, ToolCall, WorkMode},
    tools::{
        exec::{ExecGate, ExecPlan},
        read::ReadGate,
        write::WriteGate,
    },
};
use globset::{GlobBuilder, GlobMatcher};
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

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

/// One of the rule or human/model decision paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Ask,
    Deny,
    ModelReview,
}
/// A decision together with its reason for display or audit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub decision: Decision,
    pub reason: &'static str,
}
fn decision(decision: Decision, reason: &'static str) -> PolicyDecision {
    PolicyDecision { decision, reason }
}

pub(crate) fn has_exec_environment_override(arguments: &serde_json::Value) -> bool {
    arguments
        .get("env")
        .is_some_and(|env| env.as_object().is_none_or(|values| !values.is_empty()))
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Effect {
    Allow,
    Ask,
    Deny,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleConfig {
    effect: Effect,
    tool: Option<String>,
    path_glob: Option<String>,
    executable: Option<PathBuf>,
    args_prefix: Option<Vec<String>>,
}
/// TOML permission fragment. Unknown fields and unsupported rules are errors.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionConfig {
    #[serde(default)]
    pub additional_roots: Vec<PathBuf>,
    #[serde(default)]
    rules: Vec<RuleConfig>,
}
impl PermissionConfig {
    /// Parse the content of the `[permissions]` table, not an entire config file.
    pub fn from_toml(text: &str) -> Result<Self, BoundaryError> {
        toml::from_str(text).map_err(|e| failure("invalid_config", e.to_string()))
    }
}
struct Rule {
    config: RuleConfig,
    path: Option<GlobMatcher>,
}

/// A verified operation, constructed from a tool name and resolved paths.
#[derive(Debug, Clone)]
pub struct Operation {
    tool: String,
    target: Target,
}
#[derive(Debug, Clone)]
enum Target {
    File(PathBuf),
    Exec {
        cwd: PathBuf,
        executable: PathBuf,
        args: Vec<String>,
        environment_digest: String,
        analysis: Arc<crate::command_analysis::CommandAnalysis>,
    },
    Input,
    BackgroundControl,
    Metadata,
    External(crate::registry::OperationDescriptor),
}
impl Operation {
    pub(crate) fn file_target(&self) -> Option<&Path> {
        match &self.target {
            Target::File(path) => Some(path),
            _ => None,
        }
    }
    /// Construct an external operation from a host-owned registry snapshot.
    pub fn external(
        descriptor: &crate::registry::OperationDescriptor,
    ) -> Result<Self, BoundaryError> {
        use crate::registry::{ToolEffect, ToolSource};
        if !crate::registry::valid_external_name(&descriptor.name)
            || !matches!(descriptor.source, ToolSource::Mcp { .. })
            || !matches!(
                descriptor.effect,
                ToolEffect::ExternalReadOnly | ToolEffect::ExternalUnknown
            )
        {
            return Err(failure(
                "invalid_args",
                "invalid external operation identity",
            ));
        }
        Ok(Self {
            tool: descriptor.name.clone(),
            target: Target::External(descriptor.clone()),
        })
    }
    /// Construct a file operation. Existing targets and existing parents of new
    /// targets are canonicalized. This reads filesystem metadata, not content.
    pub fn file(workspace: &Path, tool: &str, path: &Path) -> Result<Self, BoundaryError> {
        if !matches!(
            tool,
            "read_file" | "list_dir" | "find_files" | "search_text" | "write_file" | "apply_patch"
        ) {
            return Err(failure("invalid_args", "unknown file tool"));
        }
        Ok(Self {
            tool: tool.into(),
            target: Target::File(normalize_target(workspace, path)?),
        })
    }
    /// Construct a command operation from a previously resolved plan.
    pub fn exec(plan: &ExecPlan) -> Result<Self, BoundaryError> {
        Self::exec_with_environment_keys(plan, &[])
    }
    /// Include host-selected environment names in review facts. Actual values
    /// remain bound by the prepared digest and are never supplied to analysis.
    pub fn exec_with_environment_keys(
        plan: &ExecPlan,
        environment_keys: &[String],
    ) -> Result<Self, BoundaryError> {
        let cwd =
            std::fs::canonicalize(&plan.cwd).map_err(|e| failure("invalid_path", e.to_string()))?;
        let executable = plan
            .argv
            .first()
            .ok_or_else(|| failure("invalid_args", "empty argv"))?;
        let exe = std::fs::canonicalize(executable)
            .map_err(|e| failure("invalid_path", e.to_string()))?;
        if !Path::new(executable).is_absolute() || !cwd.is_dir() {
            return Err(failure(
                "invalid_args",
                "command must have absolute executable and directory cwd",
            ));
        }
        let executable = exe
            .to_str()
            .ok_or_else(|| failure("invalid_path", "executable path is not UTF-8"))?;
        let argv: Vec<String> = std::iter::once(executable.to_owned())
            .chain(plan.argv[1..].iter().cloned())
            .collect();
        let analysis = crate::command_analysis::analyze(
            crate::command_analysis::CommandInput {
                argv: &argv,
                cwd: &cwd,
                environment_digest: &plan.environment_digest,
                environment_keys,
            },
            crate::command_analysis::AnalysisLimits::default(),
        );
        Ok(Self {
            tool: "exec".into(),
            target: Target::Exec {
                cwd,
                executable: exe,
                args: plan.argv[1..].to_vec(),
                environment_digest: plan.environment_digest.clone(),
                analysis: Arc::new(analysis),
            },
        })
    }
    /// Tool name associated with this normalized operation.
    pub fn tool_name(&self) -> &str {
        &self.tool
    }

    /// Replace target fields in model-supplied arguments with normalized data.
    /// Other argument fields remain untrusted data and must not be instructions.
    pub fn normalized_arguments(
        &self,
        mut args: serde_json::Value,
    ) -> Result<serde_json::Value, BoundaryError> {
        let map = args
            .as_object_mut()
            .ok_or_else(|| failure("invalid_args", "tool arguments must be an object"))?;
        match &self.target {
            Target::File(path) => {
                map.insert(
                    "path".into(),
                    serde_json::to_value(path)
                        .map_err(|e| failure("invalid_path", e.to_string()))?,
                );
            }
            Target::Exec {
                cwd,
                executable,
                args,
                environment_digest,
                analysis,
            } => {
                let executable = executable
                    .to_str()
                    .ok_or_else(|| failure("invalid_path", "executable path is not UTF-8"))?;
                let argv: Vec<_> = std::iter::once(executable.to_owned())
                    .chain(args.iter().cloned())
                    .collect();
                map.insert(
                    "environment_digest".into(),
                    serde_json::Value::String(environment_digest.clone()),
                );
                map.insert(
                    "cwd".into(),
                    serde_json::to_value(cwd)
                        .map_err(|e| failure("invalid_path", e.to_string()))?,
                );
                map.insert(
                    "argv".into(),
                    serde_json::to_value(argv)
                        .map_err(|e| failure("invalid_args", e.to_string()))?,
                );
                // Binding excludes time-dependent parse progress. The complete
                // argv/cwd/environment identity is already included above.
                map.insert(
                    "__saya_command_analysis".into(),
                    serde_json::json!({
                        "schema_version": analysis.schema_version,
                        "analyzer_version": analysis.analyzer_version,
                        "limits": analysis.limits,
                    }),
                );
            }
            Target::External(descriptor) => {
                if map.contains_key("__saya_operation") {
                    return Err(failure("invalid_args", "reserved external operation field"));
                }
                map.insert(
                    "__saya_operation".into(),
                    serde_json::to_value(descriptor).map_err(|_| {
                        failure("invalid_args", "invalid external operation identity")
                    })?,
                );
            }
            Target::Input | Target::BackgroundControl | Target::Metadata => {}
        }
        args.sort_all_objects();
        Ok(args)
    }

    /// Add bounded static facts to the independent review's untrusted data.
    /// Parse completeness and risk facts never grant execution permission.
    pub fn review_arguments(
        &self,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, BoundaryError> {
        let mut args = self.normalized_arguments(args)?;
        if let Target::Exec { analysis, .. } = &self.target {
            args.as_object_mut()
                .ok_or_else(|| failure("invalid_args", "tool arguments must be an object"))?
                .insert(
                    "__saya_command_facts".into(),
                    serde_json::to_value(analysis.as_ref()).map_err(|_| {
                        failure("invalid_args", "command analysis cannot be encoded")
                    })?,
                );
        }
        Ok(args)
    }

    /// Return the normalized command identity when this is an exec operation.
    pub fn command_identity(&self) -> Option<(Vec<String>, PathBuf)> {
        match &self.target {
            Target::Exec {
                cwd,
                executable,
                args,
                ..
            } => {
                let executable = executable.to_str()?;
                Some((
                    std::iter::once(executable.to_owned())
                        .chain(args.iter().cloned())
                        .collect(),
                    cwd.clone(),
                ))
            }
            _ => None,
        }
    }

    /// Construct the user input control operation.
    pub fn ask_user() -> Self {
        Self {
            tool: "ask_user".into(),
            target: Target::Input,
        }
    }
    /// Construct a session metadata update with no workspace target.
    pub fn todo_write() -> Self {
        Self {
            tool: "todo_write".into(),
            target: Target::Metadata,
        }
    }
    /// Read retained output owned by the current session.
    pub fn output_query(tool: &str) -> Result<Self, BoundaryError> {
        if !matches!(tool, "output_read" | "output_search") {
            return Err(failure("invalid_args", "Unsupported output query"));
        }
        Ok(Self {
            tool: tool.into(),
            target: Target::Metadata,
        })
    }
    /// Construct a model-facing background status or cancellation operation.
    pub fn background_control(tool: &str) -> Result<Self, BoundaryError> {
        if !matches!(tool, "background_status" | "background_cancel") {
            return Err(failure("invalid_args", "unsupported background control"));
        }
        Ok(Self {
            tool: tool.into(),
            target: Target::BackgroundControl,
        })
    }
}
fn normalize_target(workspace: &Path, path: &Path) -> Result<PathBuf, BoundaryError> {
    let input = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    match std::fs::canonicalize(&input) {
        Ok(path) => Ok(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A dangling symlink is not a new file and cannot be normalized by
            // resolving only its parent.
            if std::fs::symlink_metadata(&input).is_ok() {
                return Err(failure("invalid_path", "target is a dangling symlink"));
            }
            let name = input
                .file_name()
                .ok_or_else(|| failure("invalid_path", "target has no filename"))?;
            let parent = input
                .parent()
                .ok_or_else(|| failure("invalid_path", "target has no parent"))?;
            Ok(std::fs::canonicalize(parent)
                .map_err(|e| failure("invalid_path", e.to_string()))?
                .join(name))
        }
        Err(e) => Err(failure("invalid_path", e.to_string())),
    }
}

fn normalize_glob(pattern: &str) -> Result<String, BoundaryError> {
    let mut literal = String::new();
    let mut component_start = 0;
    let mut suffix = "";
    let mut characters = pattern.char_indices();
    while let Some((offset, character)) = characters.next() {
        let escaped = character == '\\';
        let (offset, character) = if escaped {
            characters
                .next()
                .ok_or_else(|| failure("invalid_config", "unfinished glob escape"))?
        } else {
            (offset, character)
        };
        if !escaped && matches!(character, '*' | '?' | '[' | '{') {
            literal.truncate(literal.rfind('/').map_or(0, |index| index + 1));
            suffix = &pattern[component_start..];
            break;
        }
        literal.push(character);
        if character == '/' {
            component_start = offset + 1;
        }
    }
    if Path::new(pattern)
        .components()
        .chain(Path::new(&literal).components())
        .any(|component| component == std::path::Component::ParentDir)
    {
        return Err(failure(
            "invalid_config",
            "path_glob must not contain parent components",
        ));
    }
    let mut prefix = PathBuf::from(literal);
    let mut missing = Vec::new();
    let mut resolved = loop {
        match std::fs::canonicalize(&prefix) {
            Ok(path) => break path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&prefix).is_ok() {
                    return Err(failure(
                        "invalid_config",
                        "path_glob has a dangling symlink prefix",
                    ));
                }
                let name = prefix
                    .file_name()
                    .ok_or_else(|| failure("invalid_config", "invalid glob prefix"))?;
                missing.push(name.to_owned());
                prefix.pop();
            }
            Err(error) => return Err(failure("invalid_config", error.to_string())),
        }
    };
    for component in missing.into_iter().rev() {
        resolved.push(component);
    }
    let resolved = resolved
        .to_str()
        .ok_or_else(|| failure("invalid_config", "glob prefix must be UTF-8"))?;
    let escaped = globset::escape(resolved).replace('\\', "\\\\");
    Ok(if suffix.is_empty() {
        escaped
    } else {
        format!("{}/{}", escaped.trim_end_matches('/'), suffix)
    })
}

/// Immutable compiled rule set. Roots and rule paths are resolved before use.
pub struct Policy {
    workspace: PathBuf,
    roots: Vec<PathBuf>,
    rules: Vec<Rule>,
}
impl Policy {
    /// Compile explicit rules. The workspace and each additional root must exist.
    pub fn new(workspace: &Path, config: PermissionConfig) -> Result<Self, BoundaryError> {
        let workspace = std::fs::canonicalize(workspace)
            .map_err(|e| failure("invalid_config", e.to_string()))?;
        if !workspace.is_dir() {
            return Err(failure("invalid_config", "workspace must be a directory"));
        }
        let mut roots = vec![workspace.clone()];
        for root in config.additional_roots {
            let root = std::fs::canonicalize(root)
                .map_err(|e| failure("invalid_config", e.to_string()))?;
            if !root.is_dir() {
                return Err(failure(
                    "invalid_config",
                    "additional root must be a directory",
                ));
            }
            roots.push(root);
        }
        let mut rules = Vec::new();
        for mut rule in config.rules {
            if let Some(tool) = &rule.tool {
                if !matches!(
                    tool.as_str(),
                    "read_file"
                        | "list_dir"
                        | "find_files"
                        | "search_text"
                        | "write_file"
                        | "apply_patch"
                        | "exec"
                        | "ask_user"
                        | "background_status"
                        | "background_cancel"
                        | "todo_write"
                        | "output_read"
                        | "output_search"
                ) && !crate::registry::valid_external_name(tool)
                {
                    return Err(failure("invalid_config", "unknown rule tool"));
                }
                if tool != "exec" && (rule.executable.is_some() || rule.args_prefix.is_some()) {
                    return Err(failure(
                        "invalid_config",
                        "executable and args_prefix require exec",
                    ));
                }
            }
            if rule.executable.as_ref().is_some_and(|p| !p.is_absolute()) {
                return Err(failure(
                    "invalid_config",
                    "rule executable must be absolute",
                ));
            }
            if let Some(executable) = &mut rule.executable {
                *executable = std::fs::canonicalize(&*executable)
                    .map_err(|e| failure("invalid_config", e.to_string()))?;
            }
            let path = match &rule.path_glob {
                Some(pattern) if Path::new(pattern).is_absolute() => Some(
                    GlobBuilder::new(&normalize_glob(pattern)?)
                        .literal_separator(true)
                        .build()
                        .map_err(|e| failure("invalid_config", e.to_string()))?
                        .compile_matcher(),
                ),
                Some(_) => return Err(failure("invalid_config", "path_glob must be absolute")),
                None => None,
            };
            rules.push(Rule { config: rule, path });
        }
        Ok(Self {
            workspace,
            roots,
            rules,
        })
    }
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }
    /// Evaluate hard mode and root restrictions first, then deny > ask > allow.
    /// No model call, confirmation, mutation, or external I/O is performed here.
    pub fn evaluate(
        &self,
        mode: WorkMode,
        permissions: PermissionMode,
        op: &Operation,
    ) -> PolicyDecision {
        let read = matches!(
            op.tool.as_str(),
            "read_file" | "list_dir" | "find_files" | "search_text"
        ) || matches!(&op.target, Target::External(descriptor)
            if descriptor.effect == crate::registry::ToolEffect::ExternalReadOnly);
        let input = op.tool == "ask_user";
        let control = matches!(op.target, Target::BackgroundControl);
        let metadata = matches!(op.target, Target::Metadata);
        if mode == WorkMode::Plan && !read && !input && !control && !metadata {
            return decision(Decision::Deny, "plan_read_only");
        }
        let path = match &op.target {
            Target::File(path) => Some(path.as_path()),
            Target::Exec { cwd, .. } => Some(cwd.as_path()),
            Target::Input | Target::BackgroundControl | Target::Metadata | Target::External(_) => {
                None
            }
        };
        if permissions != PermissionMode::Bypass
            && path.is_some_and(|path| !self.roots.iter().any(|root| path.starts_with(root)))
        {
            return decision(Decision::Deny, "outside_roots");
        }
        let mut ask = false;
        let mut allow = false;
        for rule in &self.rules {
            if !rule.matches(op) {
                continue;
            }
            match rule.config.effect {
                Effect::Deny => return decision(Decision::Deny, "explicit_deny"),
                Effect::Ask => ask = true,
                Effect::Allow => allow = true,
            }
        }
        if permissions == PermissionMode::Bypass {
            return decision(Decision::Allow, "bypass");
        }
        if ask {
            return decision(Decision::Ask, "explicit_ask");
        }
        if permissions == PermissionMode::Manual {
            return decision(Decision::Ask, "manual_approval");
        }
        if allow
            || read
            || input
            || control
            || metadata
            || matches!(op.tool.as_str(), "write_file" | "apply_patch")
        {
            return decision(
                Decision::Allow,
                if allow {
                    "explicit_allow"
                } else {
                    "native_default"
                },
            );
        }
        decision(Decision::ModelReview, "unmatched_exec")
    }

    /// Evaluate the complete requested operation. Schema-v1 static allow rules
    /// do not authorize model-supplied environment overrides. Hard restrictions,
    /// explicit ask, manual confirmation and bypass retain their precedence.
    pub fn evaluate_with_arguments(
        &self,
        mode: WorkMode,
        permissions: PermissionMode,
        op: &Operation,
        arguments: &serde_json::Value,
    ) -> PolicyDecision {
        let result = self.evaluate(mode, permissions, op);
        if op.tool == "exec"
            && result.reason == "explicit_allow"
            && has_exec_environment_override(arguments)
        {
            return decision(Decision::ModelReview, "exec_environment_override");
        }
        result
    }
}
impl Rule {
    fn matches(&self, op: &Operation) -> bool {
        if self
            .config
            .tool
            .as_ref()
            .is_some_and(|tool| tool != &op.tool)
        {
            return false;
        }
        let path = match &op.target {
            Target::File(path) => Some(path.as_path()),
            Target::Exec { cwd, .. } => Some(cwd.as_path()),
            Target::Input | Target::BackgroundControl | Target::Metadata | Target::External(_) => {
                None
            }
        };
        if let Some(pattern) = &self.path
            && !path.is_some_and(|path| pattern.is_match(path))
        {
            return false;
        }
        if self.config.executable.is_some() || self.config.args_prefix.is_some() {
            let Target::Exec {
                executable, args, ..
            } = &op.target
            else {
                return false;
            };
            if self
                .config
                .executable
                .as_ref()
                .is_some_and(|wanted| wanted != executable)
            {
                return false;
            }
            if self
                .config
                .args_prefix
                .as_ref()
                .is_some_and(|prefix| !args.starts_with(prefix))
            {
                return false;
            }
        }
        true
    }
}

/// Fail-closed tool gate for decisions that require no user or model review.
/// `Ask` and `ModelReview` are intentionally not treated as permission.
pub struct PolicyGate {
    policy: Arc<Policy>,
}
impl PolicyGate {
    pub fn new(policy: Arc<Policy>) -> Self {
        Self { policy }
    }
}
impl WriteGate for PolicyGate {
    fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        normalized_target: &Path,
    ) -> Result<bool, BoundaryError> {
        if std::fs::canonicalize(&context.workspace)
            .map_err(|e| failure("invalid_path", e.to_string()))?
            != self.policy.workspace
        {
            return Err(failure(
                "invalid_workspace",
                "policy workspace differs from run",
            ));
        }
        let operation = Operation::file(&context.workspace, &call.name, normalized_target)?;
        Ok(self
            .policy
            .evaluate(context.run.work_mode, context.run.permissions, &operation)
            .decision
            == Decision::Allow)
    }
}
impl ReadGate for PolicyGate {
    fn authorize(
        &self,
        call: &ToolCall,
        context: &ToolContext,
        normalized_target: &Path,
    ) -> Result<bool, BoundaryError> {
        WriteGate::authorize(self, call, context, normalized_target)
    }
}
impl ExecGate for PolicyGate {
    fn authorize(
        &self,
        plan: &ExecPlan,
        call: &ToolCall,
        context: &ToolContext,
    ) -> Result<bool, BoundaryError> {
        if std::fs::canonicalize(&context.workspace)
            .map_err(|e| failure("invalid_path", e.to_string()))?
            != self.policy.workspace
        {
            return Err(failure(
                "invalid_workspace",
                "policy workspace differs from run",
            ));
        }
        let operation = Operation::exec(plan)?;
        let arguments = serde_json::from_str(&call.arguments)
            .map_err(|e| failure("invalid_args", e.to_string()))?;
        Ok(self
            .policy
            .evaluate_with_arguments(
                context.run.work_mode,
                context.run.permissions,
                &operation,
                &arguments,
            )
            .decision
            == Decision::Allow)
    }
}
