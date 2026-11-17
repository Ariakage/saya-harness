//! Bounded static command facts for review and approval identity.
//!
//! This module neither executes programs nor reads files or environment values. The caller supplies
//! the exact prepared environment digest. Facts are untrusted data and can contain credentials in
//! ordinary arguments, so callers must use the existing redaction boundary before displaying or
//! persisting them. Structural completeness does not establish safety or executable identity.

use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tree_sitter::{Node, ParseOptions, Parser};

pub const ANALYSIS_SCHEMA_VERSION: u32 = 1;
pub const ANALYZER_VERSION: &str = "saya-command-analysis-v1/tree-sitter-0.27.1/bash-0.25.1";

/// Host-prepared input. Environment values stay outside the analyzer and its output.
pub struct CommandInput<'a> {
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub environment_digest: &'a str,
    pub environment_keys: &'a [String],
}

/// Limits may tighten the built-in ceilings. The deadline bounds cooperative parser/traversal work.
/// Retained-byte accounting bounds facts, not Tree-sitter's native allocator or OS scheduling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisLimits {
    pub max_input_bytes: usize,
    pub max_arguments: usize,
    pub max_script_bytes: usize,
    pub max_nodes: usize,
    pub max_facts: usize,
    pub max_depth: usize,
    pub max_wrapper_depth: usize,
    pub max_retained_bytes: usize,
    pub max_parser_callbacks: usize,
    pub deadline_ms: u64,
}

impl Default for AnalysisLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024,
            max_arguments: 1024,
            max_script_bytes: 16 * 1024,
            max_nodes: 8192,
            max_facts: 512,
            max_depth: 64,
            max_wrapper_depth: 8,
            max_retained_bytes: 1024 * 1024,
            max_parser_callbacks: 8192,
            deadline_ms: 50,
        }
    }
}

impl AnalysisLimits {
    fn bounded(self) -> Self {
        let ceiling = Self::default();
        Self {
            max_input_bytes: self.max_input_bytes.min(ceiling.max_input_bytes),
            max_arguments: self.max_arguments.min(ceiling.max_arguments),
            max_script_bytes: self.max_script_bytes.min(ceiling.max_script_bytes),
            max_nodes: self.max_nodes.min(ceiling.max_nodes),
            max_facts: self.max_facts.min(ceiling.max_facts),
            max_depth: self.max_depth.min(ceiling.max_depth),
            max_wrapper_depth: self.max_wrapper_depth.min(ceiling.max_wrapper_depth),
            max_retained_bytes: self.max_retained_bytes.min(ceiling.max_retained_bytes),
            max_parser_callbacks: self.max_parser_callbacks.min(ceiling.max_parser_callbacks),
            deadline_ms: self.deadline_ms.min(ceiling.deadline_ms),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalysisStatus {
    Complete,
    Incomplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncompleteReason {
    InvalidInput,
    InputBudgetExceeded,
    ScriptBudgetExceeded,
    NodeBudgetExceeded,
    FactBudgetExceeded,
    DepthBudgetExceeded,
    RetainedBudgetExceeded,
    ParserBudgetExceeded,
    DeadlineExceeded,
    Cancelled,
    ParserUnavailable,
    SyntaxError,
    UnsupportedDialect,
    UnsupportedShellOptions,
    UnsupportedWrapper,
    ExternalScript,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownEffect {
    HostExecutable,
    DynamicExpansion,
    InterpreterCode,
    ExternalScript,
    ShellState,
    FunctionDefinition,
    EnvironmentInfluence,
    UnsupportedSyntax,
    WrapperIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub script_id: usize,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentKind {
    Literal,
    Dynamic,
    ShellSource,
    AssignmentValueOmitted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentFact {
    pub kind: ArgumentKind,
    pub value: Option<String>,
    pub source: Option<SourceSpan>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WrapperKind {
    Env,
    Timeout,
    Command,
    Exec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandFact {
    pub id: usize,
    pub parent_command: Option<usize>,
    pub construct: Option<usize>,
    pub source: Option<SourceSpan>,
    pub executable: Option<String>,
    pub arguments: Vec<ArgumentFact>,
    pub wrapper: Option<WrapperKind>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellDialect {
    Bash,
    Posix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptFact {
    pub id: usize,
    pub parent_command: usize,
    /// Index in the command's arguments (excluding its executable).
    pub argument_index: usize,
    pub dialect: ShellDialect,
    pub sha256: String,
    pub source_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstructKind {
    Sequence,
    Pipeline,
    Conditional,
    And,
    Or,
    Background,
    Loop,
    Subshell,
    Function,
    CommandSubstitution,
    ProcessSubstitution,
    Expansion,
    Negation,
    Group,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstructFact {
    pub id: usize,
    pub parent: Option<usize>,
    pub kind: ConstructKind,
    pub source: SourceSpan,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedirectionFact {
    pub source: SourceSpan,
    pub construct: Option<usize>,
    pub operator: String,
    pub descriptor: Option<String>,
    /// Heredoc bodies are deliberately omitted. Targets may be dynamic.
    pub target: ArgumentFact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentChangeKind {
    Set,
    Unset,
    Clear,
    ChangeDirectory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentFact {
    pub command: Option<usize>,
    pub kind: EnvironmentChangeKind,
    /// A name only, never an assignment's value or a reference's resolved value.
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandAnalysis {
    pub schema_version: u32,
    pub analyzer_version: String,
    /// Absent when input validation or input budgets prevent complete identity calculation.
    pub input_digest: Option<String>,
    pub status: AnalysisStatus,
    pub effects_unknown: bool,
    pub reasons: Vec<IncompleteReason>,
    pub unknown_effects: Vec<UnknownEffect>,
    pub commands: Vec<CommandFact>,
    pub scripts: Vec<ScriptFact>,
    pub constructs: Vec<ConstructFact>,
    pub redirections: Vec<RedirectionFact>,
    pub environment: Vec<EnvironmentFact>,
    pub visited_nodes: usize,
    pub retained_bytes: usize,
    pub limits: AnalysisLimits,
}

/// Analyze argv as literal arguments and only parse source passed explicitly to a known shell.
/// No filesystem access or side effects occur. Partial facts accompany incomplete analysis.
pub fn analyze(input: CommandInput<'_>, limits: AnalysisLimits) -> CommandAnalysis {
    analyze_with_cancel(input, limits, &|| false)
}

/// Cancellation is checked before parsing and during bounded traversal/parser progress callbacks.
/// Cancellation, malformed input and exhausted budgets return conservative incomplete facts.
pub fn analyze_with_cancel(
    input: CommandInput<'_>,
    limits: AnalysisLimits,
    cancelled: &dyn Fn() -> bool,
) -> CommandAnalysis {
    let limits = limits.bounded();
    let mut analyzer = Analyzer {
        result: CommandAnalysis {
            schema_version: ANALYSIS_SCHEMA_VERSION,
            analyzer_version: ANALYZER_VERSION.into(),
            input_digest: None,
            status: AnalysisStatus::Complete,
            effects_unknown: false,
            reasons: Vec::new(),
            unknown_effects: Vec::new(),
            commands: Vec::new(),
            scripts: Vec::new(),
            constructs: Vec::new(),
            redirections: Vec::new(),
            environment: Vec::new(),
            visited_nodes: 0,
            retained_bytes: 0,
            limits,
        },
        started: Instant::now(),
        cancelled,
        halted: false,
        fact_count: 0,
        script_work_bytes: 0,
    };
    if analyzer.validate(&input) && analyzer.check() {
        analyzer.result.input_digest = Some(input_digest(&input, limits));
        for name in input.environment_keys {
            if matches!(name.as_str(), "BASH_ENV" | "ENV" | "ZDOTDIR" | "LD_PRELOAD")
                || name.starts_with("DYLD_")
            {
                analyzer.unknown(UnknownEffect::EnvironmentInfluence);
            }
        }
        let args = input
            .argv
            .iter()
            .map(|value| ArgumentFact {
                kind: ArgumentKind::Literal,
                value: Some(value.clone()),
                source: None,
            })
            .collect();
        analyzer.command(args, None, None, None, 0, &BTreeSet::new());
    }
    // Ancestor wrappers can forward a later shell's source argument. Retain its identity in
    // ScriptFact instead of duplicating the source (and embedded assignment values) in facts.
    let sources = analyzer
        .result
        .scripts
        .iter()
        .map(|script| (script.source_bytes, script.sha256.clone()))
        .collect::<BTreeSet<_>>();
    let lengths = sources
        .iter()
        .map(|(length, _)| *length)
        .collect::<BTreeSet<_>>();
    for command in &mut analyzer.result.commands {
        for argument in &mut command.arguments {
            if argument.value.as_ref().is_some_and(|value| {
                lengths.contains(&value.len())
                    && sources.contains(&(value.len(), hex_digest(value.as_bytes())))
            }) {
                argument.kind = ArgumentKind::ShellSource;
                argument.value = None;
            }
        }
    }
    analyzer.result
}

struct Analyzer<'a> {
    result: CommandAnalysis,
    started: Instant,
    cancelled: &'a dyn Fn() -> bool,
    halted: bool,
    fact_count: usize,
    script_work_bytes: usize,
}

impl Analyzer<'_> {
    fn incomplete(&mut self, reason: IncompleteReason) {
        self.result.status = AnalysisStatus::Incomplete;
        self.result.effects_unknown = true;
        if !self.result.reasons.contains(&reason) {
            self.result.reasons.push(reason);
        }
    }

    fn unknown(&mut self, effect: UnknownEffect) {
        self.result.effects_unknown = true;
        if !self.result.unknown_effects.contains(&effect) {
            self.result.unknown_effects.push(effect);
        }
    }

    fn stop(&mut self, reason: IncompleteReason) -> bool {
        self.incomplete(reason);
        self.halted = true;
        false
    }

    fn check(&mut self) -> bool {
        if self.halted {
            return false;
        }
        if (self.cancelled)() {
            return self.stop(IncompleteReason::Cancelled);
        }
        if self.started.elapsed() >= Duration::from_millis(self.result.limits.deadline_ms) {
            return self.stop(IncompleteReason::DeadlineExceeded);
        }
        true
    }

    fn charge(&mut self, payload_bytes: usize) -> bool {
        if !self.check() {
            return false;
        }
        if self.fact_count >= self.result.limits.max_facts {
            return self.stop(IncompleteReason::FactBudgetExceeded);
        }
        let bytes = payload_bytes.saturating_add(512);
        if bytes
            > self
                .result
                .limits
                .max_retained_bytes
                .saturating_sub(self.result.retained_bytes)
        {
            return self.stop(IncompleteReason::RetainedBudgetExceeded);
        }
        self.fact_count += 1;
        self.result.retained_bytes += bytes;
        true
    }

    fn validate(&mut self, input: &CommandInput<'_>) -> bool {
        if input.argv.len() > self.result.limits.max_arguments
            || input.environment_keys.len() > self.result.limits.max_arguments
        {
            return self.stop(IncompleteReason::InputBudgetExceeded);
        }
        let mut bytes = input
            .cwd
            .as_os_str()
            .len()
            .saturating_add(input.environment_digest.len());
        for value in input.argv.iter().chain(input.environment_keys) {
            bytes = bytes.saturating_add(value.len()).saturating_add(8);
            if bytes > self.result.limits.max_input_bytes {
                return self.stop(IncompleteReason::InputBudgetExceeded);
            }
        }
        if bytes > self.result.limits.max_input_bytes {
            return self.stop(IncompleteReason::InputBudgetExceeded);
        }
        let valid = input.argv.first().is_some_and(|s| !s.is_empty())
            && input.cwd.is_absolute()
            && !input.cwd.as_os_str().as_encoded_bytes().contains(&0)
            && input.argv.iter().all(|s| !s.contains('\0'))
            && input.environment_digest.len() == 64
            && input
                .environment_digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
            && input
                .environment_keys
                .iter()
                .all(|s| !s.is_empty() && !s.contains(['=', '\0']));
        if !valid {
            return self.stop(IncompleteReason::InvalidInput);
        }
        true
    }

    fn environment(
        &mut self,
        command: Option<usize>,
        kind: EnvironmentChangeKind,
        name: Option<String>,
    ) {
        if self.charge(name.as_ref().map_or(0, String::len)) {
            self.result.environment.push(EnvironmentFact {
                command,
                kind,
                name,
            });
        }
    }

    fn command(
        &mut self,
        mut argv: Vec<ArgumentFact>,
        parent: Option<usize>,
        construct: Option<usize>,
        source: Option<SourceSpan>,
        depth: usize,
        functions: &BTreeSet<String>,
    ) {
        if !self.check() {
            return;
        }
        if depth > self.result.limits.max_wrapper_depth {
            self.stop(IncompleteReason::DepthBudgetExceeded);
            return;
        }
        if argv.is_empty() {
            return;
        }
        let executable = argv.remove(0).value;
        let id = self.result.commands.len();
        let name = executable
            .as_deref()
            .and_then(|s| Path::new(s).file_name())
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let wrapper = match name {
            "env" => Some(WrapperKind::Env),
            "timeout" => Some(WrapperKind::Timeout),
            "command" => Some(WrapperKind::Command),
            "exec" => Some(WrapperKind::Exec),
            _ => None,
        };
        // Assignments may be forwarded through several wrappers. Omit their values even in
        // ancestor argv facts. The prepared input digest still binds every original byte.
        let mut stored = argv.clone();
        for argument in &mut stored {
            if let Some((name, _)) = argument.value.as_deref().and_then(assignment) {
                argument.kind = ArgumentKind::AssignmentValueOmitted;
                argument.value = Some(name.to_owned());
            }
        }
        let shell = match name {
            "bash" => Some(ShellDialect::Bash),
            "sh" | "dash" => Some(ShellDialect::Posix),
            _ => None,
        };
        let script = shell.map(|_| shell_source(&argv));
        if let Some(Ok(Some(index))) = script {
            stored[index].kind = ArgumentKind::ShellSource;
            stored[index].value = None;
        }
        let payload = executable.as_ref().map_or(0, String::len)
            + stored
                .iter()
                .map(|a| a.value.as_ref().map_or(0, String::len) + 128)
                .sum::<usize>();
        if !self.charge(payload) {
            return;
        }
        self.result.commands.push(CommandFact {
            id,
            parent_command: parent,
            construct,
            source,
            executable: executable.clone(),
            arguments: stored,
            wrapper,
        });
        if executable.is_none() {
            self.unknown(UnknownEffect::DynamicExpansion);
            return;
        }
        // A shell function may shadow even a familiar wrapper or interpreter name.
        if !executable.as_deref().is_some_and(|s| s.contains('/')) && functions.contains(name) {
            self.unknown(UnknownEffect::FunctionDefinition);
            return;
        }
        if let Some(dialect) = shell {
            self.unknown(UnknownEffect::WrapperIdentity);
            if argv.iter().filter_map(|a| a.value.as_deref()).any(|a| {
                matches!(a, "--login" | "--rcfile" | "--init-file")
                    || (a.starts_with('-') && !a.starts_with("--") && a.contains(['l', 'i']))
            }) {
                self.unknown(UnknownEffect::EnvironmentInfluence);
            }
            match script {
                Some(Ok(Some(index))) => {
                    if let Some(text) = argv[index].value.as_deref() {
                        self.script(text, dialect, id, index, depth + 1);
                    } else {
                        self.unknown(UnknownEffect::DynamicExpansion);
                    }
                }
                Some(Ok(None)) => {
                    self.unknown(UnknownEffect::ExternalScript);
                    self.incomplete(IncompleteReason::ExternalScript);
                }
                _ => self.incomplete(IncompleteReason::UnsupportedShellOptions),
            }
        } else if matches!(
            name,
            "zsh" | "ksh" | "fish" | "csh" | "tcsh" | "pwsh" | "powershell"
        ) {
            self.incomplete(IncompleteReason::UnsupportedDialect);
        } else if let Some(wrapper) = wrapper {
            self.unknown(UnknownEffect::WrapperIdentity);
            match self.wrapper(wrapper, &argv, id) {
                Ok(Some(index)) => self.command(
                    argv[index..].to_vec(),
                    Some(id),
                    construct,
                    None,
                    depth + 1,
                    functions,
                ),
                Ok(None) => {}
                Err(()) => self.incomplete(IncompleteReason::UnsupportedWrapper),
            }
        } else {
            self.unknown(UnknownEffect::HostExecutable);
            match name {
                "eval" | "source" | "." => {
                    self.unknown(UnknownEffect::ExternalScript);
                    self.incomplete(IncompleteReason::ExternalScript);
                }
                "python" | "python3" | "node" | "nodejs" | "perl" | "ruby" | "php" | "cargo"
                | "make" | "cmake" | "npm" | "npx" | "yarn" | "pnpm" => {
                    self.unknown(UnknownEffect::InterpreterCode);
                }
                "cd" | "alias" | "unalias" | "set" | "shopt" | "export" | "read" | "trap"
                | "enable" => {
                    self.unknown(UnknownEffect::ShellState);
                }
                _ => {}
            }
        }
    }

    fn wrapper(
        &mut self,
        wrapper: WrapperKind,
        args: &[ArgumentFact],
        id: usize,
    ) -> Result<Option<usize>, ()> {
        let mut index = 0;
        let mut options = true;
        while index < args.len() {
            let value = args[index].value.as_deref().ok_or(())?;
            if options && value == "--" {
                index += 1;
                if wrapper == WrapperKind::Env {
                    options = false;
                    continue;
                }
                break;
            }
            match wrapper {
                WrapperKind::Env => {
                    if let Some((name, _)) = assignment(value) {
                        options = false;
                        self.environment(Some(id), EnvironmentChangeKind::Set, Some(name.into()));
                    } else if !options {
                        return Ok(Some(index));
                    } else if matches!(value, "-i" | "--ignore-environment" | "-") {
                        self.environment(Some(id), EnvironmentChangeKind::Clear, None);
                    } else if matches!(value, "-u" | "--unset" | "-C" | "--chdir") {
                        let kind = if matches!(value, "-u" | "--unset") {
                            EnvironmentChangeKind::Unset
                        } else {
                            EnvironmentChangeKind::ChangeDirectory
                        };
                        index += 1;
                        let next = args.get(index).and_then(|a| a.value.as_deref()).ok_or(())?;
                        self.environment(
                            Some(id),
                            kind,
                            (kind == EnvironmentChangeKind::Unset).then(|| next.into()),
                        );
                    } else if let Some(name) = value
                        .strip_prefix("--unset=")
                        .or_else(|| value.strip_prefix("-u"))
                    {
                        if name.is_empty() {
                            return Err(());
                        }
                        self.environment(Some(id), EnvironmentChangeKind::Unset, Some(name.into()));
                    } else if value.starts_with("--chdir=") {
                        self.environment(Some(id), EnvironmentChangeKind::ChangeDirectory, None);
                    } else if value.starts_with('-') {
                        return Err(());
                    } else {
                        return Ok(Some(index));
                    }
                }
                WrapperKind::Timeout => {
                    if matches!(
                        value,
                        "--foreground" | "--preserve-status" | "-v" | "--verbose"
                    ) {
                    } else if matches!(value, "-k" | "--kill-after" | "-s" | "--signal") {
                        index += 1;
                        let option_value =
                            args.get(index).and_then(|a| a.value.as_deref()).ok_or(())?;
                        if option_value.is_empty()
                            || (matches!(value, "-k" | "--kill-after")
                                && !duration_argument(option_value))
                        {
                            return Err(());
                        }
                    } else if let Some(duration) = value
                        .strip_prefix("--kill-after=")
                        .or_else(|| value.strip_prefix("-k"))
                    {
                        if !duration_argument(duration) {
                            return Err(());
                        }
                    } else if let Some(signal) = value
                        .strip_prefix("--signal=")
                        .or_else(|| value.strip_prefix("-s"))
                    {
                        if signal.is_empty() {
                            return Err(());
                        }
                    } else if !value.starts_with('-') {
                        if !duration_argument(value) {
                            return Err(());
                        }
                        return (index + 1 < args.len())
                            .then_some(Some(index + 1))
                            .ok_or(());
                    } else {
                        return Err(());
                    }
                }
                WrapperKind::Command => {
                    if matches!(value, "-v" | "-V") {
                        return Ok(None);
                    }
                    if value != "-p" {
                        return if value.starts_with('-') {
                            Err(())
                        } else {
                            Ok(Some(index))
                        };
                    }
                }
                WrapperKind::Exec => {
                    if value == "-a" {
                        index += 1;
                        args.get(index).ok_or(())?;
                    } else if value == "-c" {
                        self.environment(Some(id), EnvironmentChangeKind::Clear, None);
                    } else if value != "-l" {
                        return if value.starts_with('-') {
                            Err(())
                        } else {
                            Ok(Some(index))
                        };
                    }
                }
            }
            index += 1;
        }
        if wrapper == WrapperKind::Timeout {
            let duration = args.get(index).and_then(|a| a.value.as_deref()).ok_or(())?;
            return if duration_argument(duration) && index + 1 < args.len() {
                Ok(Some(index + 1))
            } else {
                Err(())
            };
        }
        Ok((index < args.len()).then_some(index))
    }

    fn script(
        &mut self,
        source: &str,
        dialect: ShellDialect,
        command: usize,
        argument_index: usize,
        depth: usize,
    ) {
        if !self.check() {
            return;
        }
        if depth > self.result.limits.max_wrapper_depth {
            self.stop(IncompleteReason::DepthBudgetExceeded);
            return;
        }
        self.script_work_bytes = self.script_work_bytes.saturating_add(source.len());
        if source.len() > self.result.limits.max_script_bytes
            || self.script_work_bytes > self.result.limits.max_input_bytes
        {
            self.stop(IncompleteReason::ScriptBudgetExceeded);
            return;
        }
        if !self.charge(64) {
            return;
        }
        let script_id = self.result.scripts.len();
        self.result.scripts.push(ScriptFact {
            id: script_id,
            parent_command: command,
            argument_index,
            dialect,
            sha256: hex_digest(source.as_bytes()),
            source_bytes: source.len(),
        });
        if self.result.limits.max_parser_callbacks == 0 {
            self.stop(IncompleteReason::ParserBudgetExceeded);
            return;
        }
        let mut parser = Parser::new();
        if parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .is_err()
        {
            self.stop(IncompleteReason::ParserUnavailable);
            return;
        }
        let mut callbacks = 0usize;
        let mut interrupted = None;
        let mut progress = |_: &tree_sitter::ParseState| {
            callbacks += 1;
            let reason = if (self.cancelled)() {
                Some(IncompleteReason::Cancelled)
            } else if self.started.elapsed()
                >= Duration::from_millis(self.result.limits.deadline_ms)
            {
                Some(IncompleteReason::DeadlineExceeded)
            } else if callbacks > self.result.limits.max_parser_callbacks {
                Some(IncompleteReason::ParserBudgetExceeded)
            } else {
                None
            };
            if let Some(reason) = reason {
                interrupted = Some(reason);
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        let tree = parser.parse_with_options(
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        if let Some(reason) = interrupted {
            self.stop(reason);
            return;
        }
        let Some(tree) = tree else {
            self.stop(IncompleteReason::ParserUnavailable);
            return;
        };
        if !self.check() {
            return;
        }
        if tree.root_node().has_error() {
            self.incomplete(IncompleteReason::SyntaxError);
        }
        // Collect function names first so later or conditional definitions cannot hide wrappers.
        let mut functions = BTreeSet::new();
        let mut cursor = tree.walk();
        let mut scan_depth = 0;
        loop {
            let node = cursor.node();
            if !self.visit(scan_depth) {
                return;
            }
            if node.kind() == "function_definition"
                && let Some(name) = node
                    .child_by_field_name("name")
                    .and_then(|n| source.get(n.byte_range()))
            {
                functions.insert(name.to_owned());
            }
            if cursor.goto_first_child() {
                scan_depth += 1;
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return self.walk_script(
                        tree.root_node(),
                        source,
                        script_id,
                        command,
                        depth,
                        &functions,
                    );
                }
                scan_depth = scan_depth.saturating_sub(1);
            }
        }
    }

    fn visit(&mut self, depth: usize) -> bool {
        if !self.check() {
            return false;
        }
        if depth > self.result.limits.max_depth {
            return self.stop(IncompleteReason::DepthBudgetExceeded);
        }
        if self.result.visited_nodes >= self.result.limits.max_nodes {
            return self.stop(IncompleteReason::NodeBudgetExceeded);
        }
        self.result.visited_nodes += 1;
        true
    }

    fn walk_script(
        &mut self,
        root: Node<'_>,
        source: &str,
        script: usize,
        command: usize,
        wrapper_depth: usize,
        functions: &BTreeSet<String>,
    ) {
        let mut cursor = root.walk();
        let mut contexts = vec![None];
        loop {
            let node = cursor.node();
            if !self.visit(contexts.len() - 1) {
                return;
            }
            let parent = contexts.last().copied().flatten();
            let span = span(script, node);
            let kind = construct_kind(node.kind());
            let construct = if let Some(kind) = kind {
                self.construct(kind, parent, span.clone()).or(parent)
            } else {
                parent
            };
            if matches!(
                node.kind(),
                "simple_expansion"
                    | "expansion"
                    | "arithmetic_expansion"
                    | "command_substitution"
                    | "process_substitution"
                    | "brace_expression"
                    | "extglob_pattern"
                    | "ansi_c_string"
                    | "translated_string"
            ) {
                self.unknown(UnknownEffect::DynamicExpansion);
            }
            match node.kind() {
                "command" => {
                    let mut argv = Vec::new();
                    if let Some(name) = node.child_by_field_name("name") {
                        argv.push(argument(name, source, script));
                    }
                    let mut child_cursor = node.walk();
                    for child in node.children_by_field_name("argument", &mut child_cursor) {
                        argv.push(argument(child, source, script));
                    }
                    if argv.iter().any(|a| a.kind == ArgumentKind::Dynamic) {
                        self.unknown(UnknownEffect::DynamicExpansion);
                    }
                    self.command(
                        argv,
                        Some(command),
                        construct,
                        Some(span.clone()),
                        wrapper_depth,
                        functions,
                    );
                }
                "variable_assignment" => {
                    let name = node
                        .child_by_field_name("name")
                        .and_then(|n| source.get(n.byte_range()));
                    if let Some(name) = name.filter(|name| valid_name(name)) {
                        self.environment(
                            Some(command),
                            EnvironmentChangeKind::Set,
                            Some(name.into()),
                        );
                    } else {
                        self.unknown(UnknownEffect::DynamicExpansion);
                    }
                }
                "declaration_command" | "unset_command" => {
                    self.unknown(UnknownEffect::ShellState);
                }
                "function_definition" => self.unknown(UnknownEffect::FunctionDefinition),
                "file_redirect" | "heredoc_redirect" | "herestring_redirect" => {
                    self.redirect(node, source, script, construct);
                }
                "ERROR" => self.incomplete(IncompleteReason::SyntaxError),
                "&" => {
                    self.construct(ConstructKind::Background, construct, span.clone());
                }
                "&&" => {
                    self.construct(ConstructKind::And, construct, span.clone());
                }
                "||" => {
                    self.construct(ConstructKind::Or, construct, span.clone());
                }
                _ => {}
            }
            if node.is_missing() {
                self.incomplete(IncompleteReason::SyntaxError);
            }
            if cursor.goto_first_child() {
                contexts.push(construct);
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return;
                }
                contexts.pop();
            }
        }
    }

    fn construct(
        &mut self,
        kind: ConstructKind,
        parent: Option<usize>,
        source: SourceSpan,
    ) -> Option<usize> {
        if !self.charge(0) {
            return None;
        }
        let id = self.result.constructs.len();
        self.result.constructs.push(ConstructFact {
            id,
            parent,
            kind,
            source,
        });
        Some(id)
    }

    fn redirect(&mut self, node: Node<'_>, source: &str, script: usize, construct: Option<usize>) {
        let descriptor = node
            .child_by_field_name("descriptor")
            .and_then(|n| source.get(n.byte_range()))
            .map(str::to_owned);
        let mut operator = String::new();
        let mut target = None;
        let mut cursor = node.walk();
        for (child_index, child) in node.children(&mut cursor).enumerate() {
            if !child.is_named()
                && matches!(
                    child.kind(),
                    ">" | ">>"
                        | "<"
                        | "<>"
                        | ">|"
                        | ">&"
                        | "<&"
                        | "&>"
                        | "&>>"
                        | "<<"
                        | "<<-"
                        | "<<<"
                )
            {
                operator.push_str(child.kind());
            }
            if target.is_none()
                && (node.field_name_for_child(child_index as u32) == Some("destination")
                    || (node.kind() == "herestring_redirect"
                        && child.is_named()
                        && child.kind() != "file_descriptor")
                    || child.kind() == "heredoc_start")
            {
                target = Some(argument(child, source, script));
            }
        }
        let target = target.unwrap_or(ArgumentFact {
            kind: ArgumentKind::Dynamic,
            value: None,
            source: None,
        });
        if target.kind == ArgumentKind::Dynamic {
            self.unknown(UnknownEffect::DynamicExpansion);
        }
        let payload = operator.len()
            + descriptor.as_ref().map_or(0, String::len)
            + target.value.as_ref().map_or(0, String::len);
        if self.charge(payload) {
            self.result.redirections.push(RedirectionFact {
                source: span(script, node),
                construct,
                operator,
                descriptor,
                target,
            });
        }
    }
}

fn span(script_id: usize, node: Node<'_>) -> SourceSpan {
    SourceSpan {
        script_id,
        start_byte: node.start_byte(),
        end_byte: node.end_byte(),
    }
}

fn construct_kind(kind: &str) -> Option<ConstructKind> {
    match kind {
        "program" | "list" => Some(ConstructKind::Sequence),
        "pipeline" => Some(ConstructKind::Pipeline),
        "if_statement" | "elif_clause" | "else_clause" | "case_statement" | "case_item" => {
            Some(ConstructKind::Conditional)
        }
        "while_statement" | "for_statement" | "c_style_for_statement" => Some(ConstructKind::Loop),
        "subshell" => Some(ConstructKind::Subshell),
        "function_definition" => Some(ConstructKind::Function),
        "command_substitution" => Some(ConstructKind::CommandSubstitution),
        "process_substitution" => Some(ConstructKind::ProcessSubstitution),
        "expansion"
        | "simple_expansion"
        | "arithmetic_expansion"
        | "brace_expression"
        | "extglob_pattern" => Some(ConstructKind::Expansion),
        "negated_command" => Some(ConstructKind::Negation),
        "compound_statement" => Some(ConstructKind::Group),
        _ => None,
    }
}

fn argument(node: Node<'_>, source: &str, script: usize) -> ArgumentFact {
    let value = literal(node, source, 0);
    ArgumentFact {
        kind: if value.is_some() {
            ArgumentKind::Literal
        } else {
            ArgumentKind::Dynamic
        },
        value,
        source: Some(span(script, node)),
    }
}

fn literal(node: Node<'_>, source: &str, depth: usize) -> Option<String> {
    if depth > 64 || node.has_error() {
        return None;
    }
    let text = source.get(node.byte_range())?;
    match node.kind() {
        "command_name" | "concatenation" => {
            let mut result = String::new();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                result.push_str(&literal(child, source, depth + 1)?);
            }
            Some(result)
        }
        "raw_string" => text
            .strip_prefix('\'')?
            .strip_suffix('\'')
            .map(str::to_owned),
        "string" => decode_word(text.strip_prefix('"')?.strip_suffix('"')?, true),
        "word" | "number" | "heredoc_start" => decode_word(text, false),
        _ => None,
    }
}

fn decode_word(text: &str, quoted: bool) -> Option<String> {
    let mut result = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let next = chars.next()?;
            if next == '\n' {
                continue;
            }
            if quoted && !matches!(next, '$' | '`' | '"' | '\\') {
                result.push('\\');
            }
            result.push(next);
        } else if matches!(c, '$' | '`') || (!quoted && matches!(c, '*' | '?' | '[' | '{' | '~')) {
            return None;
        } else {
            result.push(c);
        }
    }
    Some(result)
}

fn shell_source(args: &[ArgumentFact]) -> Result<Option<usize>, ()> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].value.as_deref().ok_or(())?;
        if arg == "--" || !arg.starts_with('-') || arg == "-" {
            return Ok(None);
        }
        if matches!(arg, "-o" | "+o" | "-O" | "+O" | "--rcfile" | "--init-file") {
            index += 1;
            args.get(index).and_then(|a| a.value.as_deref()).ok_or(())?;
        } else if arg.starts_with("--") {
            if !matches!(
                arg,
                "--noprofile"
                    | "--norc"
                    | "--posix"
                    | "--restricted"
                    | "--verbose"
                    | "--login"
                    | "--noediting"
            ) {
                return Err(());
            }
        } else {
            let flags = arg.strip_prefix('-').ok_or(())?;
            if !flags
                .chars()
                .all(|c| "abefhkmnptuvxBCEHPTcilrsD".contains(c))
            {
                return Err(());
            }
            if flags.contains('c') {
                return args.get(index + 1).map(|_| Some(index + 1)).ok_or(());
            }
        }
        index += 1;
    }
    Ok(None)
}

fn assignment(value: &str) -> Option<(&str, &str)> {
    let (name, value) = value.split_once('=')?;
    valid_name(name).then_some((name, value))
}

fn valid_name(name: &str) -> bool {
    let mut chars = name.bytes();
    chars
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && chars.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn duration_argument(value: &str) -> bool {
    let number = value.strip_suffix(['s', 'm', 'h', 'd']).unwrap_or(value);
    number
        .parse::<f64>()
        .is_ok_and(|n| n.is_finite() && n >= 0.0)
}

fn input_digest(input: &CommandInput<'_>, limits: AnalysisLimits) -> String {
    let mut hash = Sha256::new();
    let mut part = |bytes: &[u8]| {
        hash.update((bytes.len() as u64).to_le_bytes());
        hash.update(bytes);
    };
    part(ANALYZER_VERSION.as_bytes());
    part(input.cwd.as_os_str().as_encoded_bytes());
    part(input.environment_digest.as_bytes());
    part(b"argv");
    part(&(input.argv.len() as u64).to_le_bytes());
    for value in input.argv {
        part(value.as_bytes());
    }
    part(b"environment_keys");
    let keys = input.environment_keys.iter().collect::<BTreeSet<_>>();
    part(&(keys.len() as u64).to_le_bytes());
    for name in keys {
        part(name.as_bytes());
    }
    for number in [
        limits.max_input_bytes,
        limits.max_arguments,
        limits.max_script_bytes,
        limits.max_nodes,
        limits.max_facts,
        limits.max_depth,
        limits.max_wrapper_depth,
        limits.max_retained_bytes,
        limits.max_parser_callbacks,
        limits.deadline_ms as usize,
    ] {
        part(&(number as u64).to_le_bytes());
    }
    encode_hex(&hash.finalize())
}

fn hex_digest(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[usize::from(byte >> 4)] as char);
        text.push(HEX[usize::from(byte & 15)] as char);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(argv: &[&str]) -> CommandAnalysis {
        let mut analysis = run_limits(argv, AnalysisLimits::default());
        // Structural fixtures can be descheduled during parsing. Each attempt
        // uses the production deadline. Explicit-limit fixtures call run_limits.
        for _ in 0..2 {
            if analysis.reasons != [IncompleteReason::DeadlineExceeded] {
                break;
            }
            analysis = run_limits(argv, AnalysisLimits::default());
        }
        analysis
    }

    fn run_limits(argv: &[&str], limits: AnalysisLimits) -> CommandAnalysis {
        let argv = argv.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        analyze(
            CommandInput {
                argv: &argv,
                cwd: Path::new("/workspace"),
                environment_digest: &"a".repeat(64),
                environment_keys: &[],
            },
            limits,
        )
    }

    fn shell(source: &str) -> CommandAnalysis {
        run(&["/bin/bash", "-c", source])
    }

    fn names(analysis: &CommandAnalysis) -> Vec<&str> {
        analysis
            .commands
            .iter()
            .filter_map(|c| c.executable.as_deref())
            .collect()
    }

    fn has(analysis: &CommandAnalysis, kind: ConstructKind) -> bool {
        analysis.constructs.iter().any(|c| c.kind == kind)
    }

    #[test]
    fn direct_argv_metacharacters_are_literal_and_never_shell_parsed() {
        let analysis = run(&[
            "/usr/bin/printf",
            "$(touch /tmp/file)",
            "a|b",
            ">target",
            "&",
        ]);
        assert_eq!(analysis.status, AnalysisStatus::Complete);
        assert!(analysis.scripts.is_empty());
        assert!(analysis.constructs.is_empty());
        assert_eq!(
            analysis.commands[0].arguments[0].value.as_deref(),
            Some("$(touch /tmp/file)")
        );
        assert!(analysis.effects_unknown);
    }

    #[test]
    fn actual_cst_distinguishes_quoted_and_structural_shell_operators() {
        let analysis = shell("printf '%s' 'a|b >target $(literal)' | cat >out 2>>err &");
        assert_eq!(analysis.status, AnalysisStatus::Complete, "{analysis:#?}");
        assert_eq!(names(&analysis), ["/bin/bash", "printf", "cat"]);
        assert!(has(&analysis, ConstructKind::Pipeline));
        assert!(has(&analysis, ConstructKind::Background));
        assert!(!has(&analysis, ConstructKind::CommandSubstitution));
        assert_eq!(analysis.redirections.len(), 2, "{analysis:#?}");
        assert_eq!(analysis.redirections[0].operator, ">");
        assert_eq!(
            analysis.redirections[0].target.value.as_deref(),
            Some("out")
        );
        assert_eq!(analysis.redirections[1].descriptor.as_deref(), Some("2"));
        assert_eq!(analysis.redirections[1].operator, ">>");
    }

    #[test]
    fn env_timeout_wrappers_preserve_chain_and_only_environment_names() {
        let analysis = run(&[
            "/usr/bin/env",
            "-i",
            "-u",
            "OLD",
            "TOKEN=synthetic-secret",
            "/usr/bin/timeout",
            "--signal=TERM",
            "-k",
            "1",
            "10s",
            "/bin/bash",
            "-c",
            "printf ok",
        ]);
        assert_eq!(analysis.status, AnalysisStatus::Complete, "{analysis:#?}");
        assert_eq!(
            names(&analysis),
            ["/usr/bin/env", "/usr/bin/timeout", "/bin/bash", "printf"]
        );
        assert_eq!(analysis.commands[1].parent_command, Some(0));
        assert_eq!(analysis.commands[2].parent_command, Some(1));
        assert!(
            analysis
                .environment
                .iter()
                .any(|f| f.kind == EnvironmentChangeKind::Clear)
        );
        assert!(
            analysis
                .environment
                .iter()
                .any(|f| f.name.as_deref() == Some("TOKEN"))
        );
        assert!(
            !serde_json::to_string(&analysis)
                .unwrap()
                .contains("synthetic-secret")
        );
    }

    #[test]
    fn command_lookup_is_not_misreported_as_execution() {
        let analysis = shell("command -v rm; command -p printf hi; exec -a renamed /bin/echo done");
        assert_eq!(analysis.status, AnalysisStatus::Complete, "{analysis:#?}");
        assert!(!names(&analysis).contains(&"rm"));
        assert!(names(&analysis).contains(&"printf"));
        assert!(names(&analysis).contains(&"/bin/echo"));
    }

    #[test]
    fn explicit_shell_options_select_the_actual_source_argument() {
        for argv in [
            vec!["bash", "-lc", "printf ok", "ignored"],
            vec![
                "bash",
                "--noprofile",
                "--norc",
                "-o",
                "pipefail",
                "-c",
                "printf ok",
            ],
            vec!["sh", "-ec", "printf ok"],
        ] {
            let analysis = run(&argv);
            assert_eq!(analysis.status, AnalysisStatus::Complete, "{analysis:#?}");
            assert!(names(&analysis).contains(&"printf"));
            assert_eq!(analysis.scripts.len(), 1);
        }
        let empty = run(&["bash", "-c", ""]);
        assert_eq!(empty.status, AnalysisStatus::Complete);
        assert_eq!(empty.scripts[0].source_bytes, 0);
    }

    #[test]
    fn unsupported_shell_and_wrapper_inputs_are_incomplete() {
        for argv in [
            vec!["fish", "-c", "echo hi"],
            vec!["bash", "--unknown", "-c", "echo hi"],
            vec!["env", "-S", "bash -c 'echo hi'"],
            vec!["timeout", "NaN", "echo", "hi"],
            vec!["timeout", "--bad", "1", "echo"],
            vec!["env", "-u"],
        ] {
            let analysis = run(&argv);
            assert_eq!(analysis.status, AnalysisStatus::Incomplete, "{analysis:#?}");
            assert!(analysis.effects_unknown);
            assert_eq!(analysis.commands.len(), 1);
        }
        for argv in [
            vec!["bash", "script.sh"],
            vec!["bash", "--", "-c", "echo hi"],
            vec!["sh", "-s"],
        ] {
            let analysis = run(&argv);
            assert!(analysis.reasons.contains(&IncompleteReason::ExternalScript));
        }
    }

    #[test]
    fn substitutions_globs_expansion_and_dynamic_executable_stay_unknown() {
        let analysis = shell(
            "$TOOL \"$VALUE\" *.rs $(printf sub) `echo old` <(cat in) $((x+1)); echo ~/{a,b}",
        );
        assert_eq!(analysis.status, AnalysisStatus::Complete, "{analysis:#?}");
        assert!(analysis.commands.iter().any(|c| c.executable.is_none()));
        assert!(has(&analysis, ConstructKind::CommandSubstitution));
        assert!(has(&analysis, ConstructKind::ProcessSubstitution));
        assert!(has(&analysis, ConstructKind::Expansion));
        assert!(names(&analysis).contains(&"printf"));
        assert!(
            analysis
                .unknown_effects
                .contains(&UnknownEffect::DynamicExpansion)
        );
    }

    #[test]
    fn structural_branches_loops_functions_and_shadowed_wrappers_are_retained() {
        let analysis = shell(
            "env() { printf local; }; if test -e in; then env bash -c 'rm file'; else for x in a b; do echo \"$x\"; done; fi; true && echo yes || echo no",
        );
        assert_eq!(analysis.status, AnalysisStatus::Complete, "{analysis:#?}");
        for kind in [
            ConstructKind::Function,
            ConstructKind::Conditional,
            ConstructKind::Loop,
            ConstructKind::And,
            ConstructKind::Or,
        ] {
            assert!(has(&analysis, kind));
        }
        assert!(!names(&analysis).contains(&"rm"));
        assert!(
            analysis
                .unknown_effects
                .contains(&UnknownEffect::FunctionDefinition)
        );
    }

    #[test]
    fn shell_assignments_omit_values_and_heredocs_respect_quoting() {
        let analysis = shell(
            "TOKEN=hidden printf ok; cat <<'EOF'\n$(not_a_command) $HIDDEN\nEOF\ncat <<EOF\n$(printf expanded)\nEOF\ncat <<<\"$VALUE\"",
        );
        assert_eq!(analysis.status, AnalysisStatus::Complete, "{analysis:#?}");
        assert_eq!(analysis.redirections.len(), 3, "{analysis:#?}");
        assert!(!names(&analysis).contains(&"not_a_command"));
        assert!(names(&analysis).contains(&"printf"));
        assert!(
            analysis
                .environment
                .iter()
                .any(|f| f.name.as_deref() == Some("TOKEN"))
        );
        assert!(!serde_json::to_string(&analysis).unwrap().contains("hidden"));
    }

    #[test]
    fn external_sources_and_other_language_code_remain_unknown() {
        let analysis = shell(
            "source ./project.sh; eval \"$CODE\"; python3 -c 'print(1)'; cargo build; cd /tmp",
        );
        assert_eq!(analysis.status, AnalysisStatus::Incomplete);
        for effect in [
            UnknownEffect::ExternalScript,
            UnknownEffect::InterpreterCode,
            UnknownEffect::ShellState,
        ] {
            assert!(analysis.unknown_effects.contains(&effect));
        }
    }

    #[test]
    fn syntax_errors_and_missing_nodes_never_become_complete() {
        for source in [
            "if true; then",
            "echo 'unterminated",
            "echo $(",
            "echo hi | | cat",
        ] {
            let analysis = shell(source);
            assert_eq!(
                analysis.status,
                AnalysisStatus::Incomplete,
                "source={source:?} {analysis:#?}"
            );
            assert!(analysis.reasons.contains(&IncompleteReason::SyntaxError));
        }
    }

    #[test]
    fn invalid_and_oversized_inputs_do_not_have_approval_digests() {
        for argv in [vec![], vec![""], vec!["echo", "bad\0argument"]] {
            let analysis = run(&argv);
            assert!(analysis.input_digest.is_none());
            assert_eq!(analysis.reasons, [IncompleteReason::InvalidInput]);
        }
        let huge = "a".repeat(100_000);
        let analysis = run(&["printf", &huge]);
        assert!(analysis.input_digest.is_none());
        assert_eq!(analysis.reasons, [IncompleteReason::InputBudgetExceeded]);
        assert!(analysis.commands.is_empty());
    }

    #[test]
    fn deterministic_fact_node_depth_script_and_memory_budgets_fail_closed() {
        let source = "echo hi; echo there; echo again";
        for (limits, reason) in [
            (
                AnalysisLimits {
                    max_facts: 1,
                    ..Default::default()
                },
                IncompleteReason::FactBudgetExceeded,
            ),
            (
                AnalysisLimits {
                    max_nodes: 2,
                    ..Default::default()
                },
                IncompleteReason::NodeBudgetExceeded,
            ),
            (
                AnalysisLimits {
                    max_depth: 1,
                    ..Default::default()
                },
                IncompleteReason::DepthBudgetExceeded,
            ),
            (
                AnalysisLimits {
                    max_script_bytes: 1,
                    ..Default::default()
                },
                IncompleteReason::ScriptBudgetExceeded,
            ),
            (
                AnalysisLimits {
                    max_parser_callbacks: 0,
                    ..Default::default()
                },
                IncompleteReason::ParserBudgetExceeded,
            ),
            (
                AnalysisLimits {
                    max_retained_bytes: 1,
                    ..Default::default()
                },
                IncompleteReason::RetainedBudgetExceeded,
            ),
            (
                AnalysisLimits {
                    deadline_ms: 0,
                    ..Default::default()
                },
                IncompleteReason::DeadlineExceeded,
            ),
        ] {
            let analysis = run_limits(&["bash", "-c", source], limits);
            assert_eq!(analysis.status, AnalysisStatus::Incomplete);
            assert!(
                analysis.reasons.contains(&reason),
                "{reason:?}: {analysis:#?}"
            );
        }
        let wrappers = ["env", "env", "env", "printf", "hi"];
        let analysis = run_limits(
            &wrappers,
            AnalysisLimits {
                max_wrapper_depth: 1,
                ..Default::default()
            },
        );
        assert!(
            analysis
                .reasons
                .contains(&IncompleteReason::DepthBudgetExceeded)
        );
    }

    #[test]
    fn cancellation_returns_partial_incomplete_facts_without_executing() {
        let argv = vec!["bash".to_owned(), "-c".to_owned(), "echo hi".to_owned()];
        let analysis = analyze_with_cancel(
            CommandInput {
                argv: &argv,
                cwd: Path::new("/workspace"),
                environment_digest: &"a".repeat(64),
                environment_keys: &[],
            },
            Default::default(),
            &|| true,
        );
        assert_eq!(analysis.reasons, [IncompleteReason::Cancelled]);
        assert!(analysis.commands.is_empty());
    }

    #[test]
    fn digest_binds_exact_argv_cwd_environment_and_budgets_and_is_repeatable() {
        let original = run(&["echo", "你好", "é"]);
        assert_eq!(original, run(&["echo", "你好", "é"]));
        assert_ne!(
            original.input_digest,
            run(&["echo", "你好", "e\u{301}"]).input_digest
        );
        assert_ne!(
            original.input_digest,
            run(&["echo", "你", "好é"]).input_digest
        );
        assert_ne!(
            original.input_digest,
            run_limits(
                &["echo", "你好", "é"],
                AnalysisLimits {
                    max_nodes: 100,
                    ..Default::default()
                }
            )
            .input_digest
        );
        let argv = vec!["echo".to_owned(), "你好".to_owned(), "é".to_owned()];
        for (cwd, digest) in [("/other", "a".repeat(64)), ("/workspace", "b".repeat(64))] {
            let other = analyze(
                CommandInput {
                    argv: &argv,
                    cwd: Path::new(cwd),
                    environment_digest: &digest,
                    environment_keys: &[],
                },
                Default::default(),
            );
            assert_ne!(original.input_digest, other.input_digest);
        }
    }

    #[test]
    fn unicode_source_spans_are_valid_byte_ranges() {
        let source = "printf '你好🙂' | cat >'输出.txt'";
        let analysis = shell(source);
        assert_eq!(analysis.status, AnalysisStatus::Complete);
        for command in &analysis.commands {
            if let Some(span) = &command.source {
                assert!(source.get(span.start_byte..span.end_byte).is_some());
            }
        }
        assert_eq!(
            analysis.redirections[0].target.value.as_deref(),
            Some("输出.txt")
        );
    }
    #[test]
    fn env_assignment_stops_options_but_separator_still_allows_assignments() {
        let after_separator = run(&["env", "--", "KEY=secret", "bash", "-c", "printf ok"]);
        assert_eq!(names(&after_separator), ["env", "bash", "printf"]);
        let options_stopped = run(&["env", "KEY=secret", "-u", "KEY", "bash", "-c", "printf no"]);
        assert_eq!(names(&options_stopped), ["env", "-u"]);
        assert!(options_stopped.scripts.is_empty());
    }

    #[test]
    fn ancestor_wrappers_never_duplicate_identified_scripts_or_assignment_values() {
        let analysis = run(&[
            "timeout",
            "1",
            "env",
            "KEY=private-value",
            "bash",
            "-c",
            "SECRET=private-value printf hi",
        ]);
        assert_eq!(analysis.status, AnalysisStatus::Complete);
        assert!(
            !serde_json::to_string(&analysis)
                .unwrap()
                .contains("private-value")
        );
        assert!(
            analysis
                .commands
                .iter()
                .flat_map(|c| &c.arguments)
                .any(|a| a.kind == ArgumentKind::ShellSource)
        );
    }

    #[test]
    fn environment_key_order_is_canonical_and_influence_is_explicit() {
        let argv = vec!["printf".to_owned()];
        let keys = vec!["ZDOTDIR".to_owned(), "PATH".to_owned()];
        let reordered = vec!["PATH".to_owned(), "ZDOTDIR".to_owned(), "PATH".to_owned()];
        let build = |keys: &[String]| {
            analyze(
                CommandInput {
                    argv: &argv,
                    cwd: Path::new("/workspace"),
                    environment_digest: &"a".repeat(64),
                    environment_keys: keys,
                },
                Default::default(),
            )
        };
        let a = build(&keys);
        let b = build(&reordered);
        assert_eq!(a.input_digest, b.input_digest);
        assert!(
            a.unknown_effects
                .contains(&UnknownEffect::EnvironmentInfluence)
        );
    }
}
