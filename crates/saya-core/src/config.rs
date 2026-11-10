//! Strict, explicitly selected TOML configuration and environment resolution.
//! Loading configuration does not create files, call providers or execute tools.
use crate::{
    contract::{BoundaryError, ErrorInfo, PermissionMode, WorkMode},
    mcp::config::{McpSettings, McpTransportSettings},
    permissions::{PermissionConfig, Policy},
};
use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    net::IpAddr,
    path::{Component, Path, PathBuf},
};

mod sandbox;
pub use sandbox::{SandboxNetworkSettings, SandboxSettings};
mod plugins;
pub use plugins::PluginSettings;

fn invalid(message: &'static str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: "invalid_config".into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// Captured environment, injectable for deterministic tests. Values are never formatted.
#[derive(Default, Clone)]
pub struct Environment(BTreeMap<OsString, OsString>);
impl std::fmt::Debug for Environment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Environment")
            .field("variable_count", &self.0.len())
            .finish_non_exhaustive()
    }
}
impl Environment {
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&OsString, &OsString)> {
        self.0.iter()
    }
    pub(crate) fn get_os(&self, name: &str) -> Option<&OsStr> {
        self.0.get(OsStr::new(name)).map(OsString::as_os_str)
    }
    pub fn capture() -> Self {
        Self(std::env::vars_os().collect())
    }
    pub fn from_pairs(pairs: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self(pairs.into_iter().collect())
    }
    fn path(&self, name: &str) -> Option<PathBuf> {
        self.0
            .get(OsStr::new(name))
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }
    fn value(&self, name: &str) -> Result<Option<&str>, BoundaryError> {
        self.0
            .get(OsStr::new(name))
            .map(|v| {
                v.to_str()
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| invalid("environment value is empty or not UTF-8"))
            })
            .transpose()
    }
    fn reference(&self, name: &str) -> Result<&str, BoundaryError> {
        if !valid_env_name(name) {
            return Err(invalid("invalid environment reference name"));
        }
        self.value(name)?
            .ok_or_else(|| invalid("required environment reference is missing"))
    }
}
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.bytes();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

fn launch_path(path: PathBuf, process_cwd: &Path) -> Result<PathBuf, BoundaryError> {
    let path = std::path::absolute(process_cwd.join(path))
        .map_err(|_| invalid("launch path cannot be resolved"))?;
    let mut resolved = PathBuf::new();
    for component in path.components() {
        if component == Component::ParentDir {
            match std::fs::canonicalize(&resolved) {
                Ok(directory) if directory.is_dir() => resolved = directory,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if std::fs::symlink_metadata(&resolved).is_ok() {
                        return Err(invalid("launch path contains a dangling symlink"));
                    }
                }
                _ => return Err(invalid("launch path parent is unavailable")),
            }
            resolved.pop();
        } else {
            resolved.push(component);
        }
    }
    Ok(resolved)
}

/// Command-line values. Unset values are resolved from the environment, file, then defaults.
#[derive(Default)]
pub struct Overrides {
    pub config: Option<PathBuf>,
    pub profile: Option<String>,
    pub mode: Option<WorkMode>,
    pub permissions: Option<PermissionMode>,
    pub cwd: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub output: Option<Output>,
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    #[default]
    Text,
    Json,
    Jsonl,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Protocol {
    #[serde(rename = "openai-chat-completions")]
    ChatCompletions,
    #[serde(rename = "openai-responses")]
    Responses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum Auth {
    #[serde(rename = "bearer")]
    Bearer,
    #[serde(rename = "x-api-key")]
    ApiKey,
    #[serde(rename = "none")]
    None,
}

/// An explicit provider effort request. Supported levels depend on the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}
impl ThinkingEffort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}
fn default_output_tokens() -> u32 {
    32000
}
fn yes() -> bool {
    true
}
fn default_version() -> String {
    "2026-10-21".into()
}

/// Provider settings contain reference names, never API key values.
#[derive(Clone, Deserialize, Serialize)]
#[serde(try_from = "RawProfile")]
pub struct Profile {
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    pub auth: Auth,
    pub api_key_env: Option<String>,
    pub context_window_tokens: u64,
    /// Resolved request target. Explicit configuration overrides the declared
    /// model default, which otherwise falls back to 32,000 tokens.
    pub max_output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<ProfileLimits>,
    /// Passed unchanged to the selected protocol. Omitted settings retain the
    /// legacy serialized profile and leave provider defaults unspecified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default)]
    pub allow_remote_http: bool,
    #[serde(default)]
    pub headers_env: BTreeMap<String, String>,
    #[serde(default)]
    pub strict_tools: bool,
    #[serde(default = "yes")]
    pub stream: bool,
    #[serde(default = "default_version")]
    pub api_version: String,
    #[serde(default)]
    pub legacy_max_tokens: bool,
    #[serde(default)]
    pub omit_stream_usage: bool,
    #[serde(default)]
    pub omit_reasoning_include: bool,
    /// Optional explicit rates; excluded from the provider identity fingerprint.
    #[serde(default, skip_serializing)]
    pub pricing: Option<Pricing>,
}
/// Model-declared defaults. An explicit request target takes precedence.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileLimits {
    pub output: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProfile {
    protocol: Protocol,
    base_url: String,
    model: String,
    auth: Auth,
    api_key_env: Option<String>,
    context_window_tokens: u64,
    max_output_tokens: Option<u32>,
    limit: Option<ProfileLimits>,
    thinking_effort: Option<ThinkingEffort>,
    #[serde(default)]
    allow_remote_http: bool,
    #[serde(default)]
    headers_env: BTreeMap<String, String>,
    #[serde(default)]
    strict_tools: bool,
    #[serde(default = "yes")]
    stream: bool,
    #[serde(default = "default_version")]
    api_version: String,
    #[serde(default)]
    legacy_max_tokens: bool,
    #[serde(default)]
    omit_stream_usage: bool,
    #[serde(default)]
    omit_reasoning_include: bool,
    pricing: Option<Pricing>,
}
impl TryFrom<RawProfile> for Profile {
    type Error = &'static str;

    fn try_from(raw: RawProfile) -> Result<Self, Self::Error> {
        if raw.max_output_tokens == Some(0)
            || raw.limit.as_ref().is_some_and(|limit| limit.output == 0)
        {
            return Err("profile output limits must be positive");
        }
        let max_output_tokens = raw
            .max_output_tokens
            .or_else(|| raw.limit.as_ref().map(|limit| limit.output))
            .unwrap_or_else(default_output_tokens);
        Ok(Self {
            protocol: raw.protocol,
            base_url: raw.base_url,
            model: raw.model,
            auth: raw.auth,
            api_key_env: raw.api_key_env,
            context_window_tokens: raw.context_window_tokens,
            max_output_tokens,
            limit: raw.limit,
            thinking_effort: raw.thinking_effort,
            allow_remote_http: raw.allow_remote_http,
            headers_env: raw.headers_env,
            strict_tools: raw.strict_tools,
            stream: raw.stream,
            api_version: raw.api_version,
            legacy_max_tokens: raw.legacy_max_tokens,
            omit_stream_usage: raw.omit_stream_usage,
            omit_reasoning_include: raw.omit_reasoning_include,
            pricing: raw.pricing,
        })
    }
}
/// Rates in millionths of the named currency per one million tokens.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pricing {
    pub currency: String,
    pub input_microunits_per_million: u64,
    pub output_microunits_per_million: u64,
}
impl Profile {
    /// Fingerprint profile settings and credential reference names, never credential values.
    pub fn fingerprint(&self) -> Result<String, BoundaryError> {
        let encoded = serde_json::to_vec(self).map_err(|_| invalid("cannot serialize profile"))?;
        Ok(Sha256::digest(encoded)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Defaults {
    profile: Option<String>,
    mode: WorkMode,
    permissions: PermissionMode,
    output: Output,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Review {
    pub profile: Option<String>,
    /// Omission inherits the actually selected review profile's target.
    pub max_output_tokens: Option<u32>,
}
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Commands {
    pub inherit_env: BTreeSet<String>,
}

/// All B-15 limits; zero is rejected instead of disabling a limit.
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub max_model_requests: usize,
    pub max_tool_calls: usize,
    pub max_background_running: usize,
    pub max_background_jobs: usize,
    pub active_run_timeout_seconds: u64,
    pub model_connect_timeout_seconds: u64,
    pub model_idle_timeout_seconds: u64,
    pub model_timeout_seconds: u64,
    pub command_timeout_seconds: u64,
    pub max_command_timeout_seconds: u64,
    pub read_return_max_bytes: usize,
    pub read_file_max_bytes: usize,
    pub write_max_bytes: usize,
    pub command_stdout_max_bytes: usize,
    pub command_stderr_max_bytes: usize,
    pub search_max_results: usize,
    pub search_return_max_bytes: usize,
    pub list_max_entries: usize,
    pub list_return_max_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_model_requests: 64,
            max_tool_calls: 256,
            max_background_running: 4,
            max_background_jobs: 256,
            active_run_timeout_seconds: 1800,
            model_connect_timeout_seconds: 10,
            model_idle_timeout_seconds: 60,
            model_timeout_seconds: 180,
            command_timeout_seconds: 120,
            max_command_timeout_seconds: 600,
            read_return_max_bytes: 262144,
            read_file_max_bytes: 16777216,
            write_max_bytes: 1048576,
            command_stdout_max_bytes: 65536,
            command_stderr_max_bytes: 65536,
            search_max_results: 200,
            search_return_max_bytes: 131072,
            list_max_entries: 1000,
            list_return_max_bytes: 131072,
        }
    }
}
impl Limits {
    fn validate(&self) -> Result<(), BoundaryError> {
        if [
            self.max_model_requests,
            self.max_tool_calls,
            self.max_background_running,
            self.max_background_jobs,
            self.read_return_max_bytes,
            self.read_file_max_bytes,
            self.write_max_bytes,
            self.command_stdout_max_bytes,
            self.command_stderr_max_bytes,
            self.search_max_results,
            self.search_return_max_bytes,
            self.list_max_entries,
            self.list_return_max_bytes,
        ]
        .contains(&0)
            || [
                self.active_run_timeout_seconds,
                self.model_connect_timeout_seconds,
                self.model_idle_timeout_seconds,
                self.model_timeout_seconds,
                self.command_timeout_seconds,
                self.max_command_timeout_seconds,
            ]
            .contains(&0)
            || self.command_timeout_seconds > self.max_command_timeout_seconds
        {
            return Err(invalid(
                "limits must be positive and the command timeout must not exceed its maximum",
            ));
        }
        Ok(())
    }
}
/// Private host-owned output retention. Existing command stream caps still apply.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputCaptureSettings {
    pub enabled: bool,
    pub limits: crate::output_store::OutputLimits,
}
impl Default for OutputCaptureSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            limits: crate::output_store::OutputLimits::default(),
        }
    }
}
/// Context policy is selected at launch and never restored as historical authority.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextSettings {
    pub auto_compact: bool,
    pub trigger_percent: u16,
    pub target_percent: u16,
    pub extra_safety_tokens: u64,
    /// Omission inherits the execution profile's resolved request target.
    pub summary_max_output_tokens: Option<u32>,
    pub max_compaction_calls: usize,
    pub compaction_timeout_seconds: u64,
}
impl Default for ContextSettings {
    fn default() -> Self {
        Self {
            auto_compact: true,
            trigger_percent: 80,
            target_percent: 55,
            extra_safety_tokens: 0,
            summary_max_output_tokens: None,
            max_compaction_calls: 32,
            compaction_timeout_seconds: 600,
        }
    }
}
impl ContextSettings {
    fn validate(&self) -> Result<(), BoundaryError> {
        if self.target_percent == 0
            || self.target_percent >= self.trigger_percent
            || self.trigger_percent > 100
            || self.summary_max_output_tokens == Some(0)
            || self.max_compaction_calls == 0
            || self.max_compaction_calls > 64
            || self.compaction_timeout_seconds == 0
            || self.compaction_timeout_seconds > 3600
        {
            return Err(invalid(
                "Context thresholds or compaction limits are invalid",
            ));
        }
        Ok(())
    }
    pub fn budget(&self, profile: &Profile) -> crate::context_builder::ContextBudget {
        crate::context_builder::ContextBudget {
            window_tokens: profile.context_window_tokens,
            output_reserve: u64::from(profile.max_output_tokens),
            safety_margin: profile
                .context_window_tokens
                .div_ceil(10)
                .saturating_add(1)
                .saturating_add(self.extra_safety_tokens),
            trigger_per_mille: self.trigger_percent * 10,
            target_per_mille: self.target_percent * 10,
        }
    }
}
/// Managed global memory is independent from explicitly supplied context files.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    pub read: bool,
    pub inject: bool,
    pub write: bool,
    pub root: Option<PathBuf>,
}
impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            read: true,
            inject: true,
            write: true,
            root: None,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    #[serde(default)]
    defaults: Defaults,
    #[serde(default)]
    profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    review: Review,
    #[serde(default)]
    permissions: PermissionConfig,
    #[serde(default)]
    commands: Commands,
    #[serde(default)]
    limits: Limits,
    #[serde(default)]
    sandbox: SandboxSettings,
    #[serde(default)]
    mcp: McpSettings,
    #[serde(default)]
    output_capture: OutputCaptureSettings,
    #[serde(default)]
    context: ContextSettings,
    #[serde(default)]
    memory: MemorySettings,
    #[serde(default)]
    plugins: PluginSettings,
}
/// Effective settings; no model is implicitly selected. Secret values stay in Environment.
#[derive(Clone)]
pub struct Config {
    pub config_path: PathBuf,
    pub cwd: PathBuf,
    pub data_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub profile: Option<String>,
    pub mode: WorkMode,
    pub permissions_mode: PermissionMode,
    pub output: Output,
    pub profiles: BTreeMap<String, Profile>,
    pub review: Review,
    pub permissions: PermissionConfig,
    pub commands: Commands,
    pub limits: Limits,
    pub sandbox: SandboxSettings,
    pub mcp: McpSettings,
    pub output_capture: OutputCaptureSettings,
    pub context: ContextSettings,
    pub memory: MemorySettings,
    pub plugins: PluginSettings,
    env: Environment,
}
impl Config {
    /// Captured HOME used for user-level discovery and default storage roots.
    pub fn home(&self) -> Option<PathBuf> {
        self.env.path("HOME")
    }
    pub fn memory_root(&self) -> Result<PathBuf, BoundaryError> {
        let root = match &self.memory.root {
            Some(root) => root.clone(),
            None => self
                .home()
                .ok_or_else(|| invalid("HOME is required for the default memory root"))?
                .join(".saya"),
        };
        if !root.is_absolute() {
            return Err(invalid("Memory root must be absolute"));
        }
        Ok(root)
    }
    /// Captured launch environment used by process and transport adapters.
    pub fn environment(&self) -> &Environment {
        &self.env
    }
    /// Access a validated credential or header environment reference; do not log its return value.
    pub fn secret(&self, name: &str) -> Result<&str, BoundaryError> {
        self.env.reference(name)
    }
    pub fn secret_env_names(&self) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = self
            .profiles
            .values()
            .flat_map(|p| p.api_key_env.iter().chain(p.headers_env.values()))
            .cloned()
            .collect();
        for server in self.mcp.servers.values().filter(|server| server.enabled) {
            match &server.transport {
                McpTransportSettings::Stdio(settings) => {
                    names.extend(settings.env.values().cloned())
                }
                McpTransportSettings::Http(settings) => names.extend(
                    settings
                        .bearer_env
                        .iter()
                        .chain(settings.headers_env.values())
                        .cloned(),
                ),
            }
        }
        if self.plugins.enabled {
            for slot in self.plugins.bindings.slots.values() {
                use crate::plugin_runtime::CredentialSlotBinding;
                let reference = match slot {
                    CredentialSlotBinding::Environment { reference, .. }
                    | CredentialSlotBinding::HttpBearer { reference, .. }
                    | CredentialSlotBinding::HttpHeader { reference, .. } => Some(reference),
                    CredentialSlotBinding::HttpCredential { .. } => None,
                };
                if let Some(reference) = reference
                    && self.env.value(reference).ok().flatten().is_some()
                {
                    names.insert(reference.clone());
                }
            }
        }
        names
    }
}
fn default_dir(env: &Environment, xdg: &str, fallback: &str) -> Result<PathBuf, BoundaryError> {
    if let Some(path) = env.path(xdg).filter(|p| p.is_absolute()) {
        return Ok(path.join("saya"));
    }
    let home = env
        .path("HOME")
        .filter(|p| p.is_absolute())
        .ok_or_else(|| invalid("HOME must be an absolute path when an XDG directory is not set"))?;
    Ok(home.join(fallback).join("saya"))
}
/// Select the sole configuration path without loading or creating it.
pub fn config_path(overrides: &Overrides, env: &Environment) -> Result<PathBuf, BoundaryError> {
    if let Some(path) = overrides.config.clone().or_else(|| env.path("SAYA_CONFIG")) {
        return Ok(path);
    }
    Ok(default_dir(env, "XDG_CONFIG_HOME", ".config")?.join("config.toml"))
}
fn parse_env<T: for<'de> Deserialize<'de>>(
    env: &Environment,
    name: &str,
) -> Result<Option<T>, BoundaryError> {
    env.value(name)?
        .map(|v| {
            T::deserialize(serde::de::value::StrDeserializer::<serde::de::value::Error>::new(v))
                .map_err(|_| invalid("invalid environment setting"))
        })
        .transpose()
}
fn validate_profile(p: &Profile, env: &Environment) -> Result<(), BoundaryError> {
    if p.model.trim().is_empty() || p.context_window_tokens == 0 || p.max_output_tokens == 0 {
        return Err(invalid("profile model and token limits are required"));
    }
    if p.pricing.as_ref().is_some_and(|pricing| {
        pricing.currency.len() != 3
            || !pricing
                .currency
                .bytes()
                .all(|byte| byte.is_ascii_uppercase())
    }) {
        return Err(invalid(
            "pricing currency must be a three-letter uppercase code",
        ));
    }
    let url = Url::parse(&p.base_url).map_err(|_| invalid("invalid API base URL"))?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "API base URL cannot contain credentials, query or fragment",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("API base URL requires a host"))?;
    let local = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_matches(['[', ']'])
            .parse::<IpAddr>()
            .is_ok_and(|v| v.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && (local || p.allow_remote_http)) {
        return Err(invalid(
            "API base URL requires HTTPS or explicit remote HTTP opt-in",
        ));
    }
    match (p.auth, &p.api_key_env) {
        (Auth::None, None) => {}
        (Auth::None, Some(_)) | (_, None) => {
            return Err(invalid(
                "authentication and API key reference are inconsistent",
            ));
        }
        (_, Some(name)) => {
            HeaderValue::from_str(env.reference(name)?)
                .map_err(|_| invalid("invalid authentication value"))?;
        }
    }
    if p.protocol == Protocol::AnthropicMessages
        && !matches!(p.api_version.as_str(), "2026-10-21" | "2023-06-01")
    {
        return Err(invalid("unsupported Anthropic API version"));
    }
    let mut names = BTreeSet::new();
    for (name, reference) in &p.headers_env {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| invalid("invalid HTTP header name"))?;
        if matches!(
            header.as_str(),
            "authorization"
                | "x-api-key"
                | "anthropic-version"
                | "anthropic-beta"
                | "host"
                | "content-length"
                | "transfer-encoding"
                | "connection"
                | "proxy-authorization"
                | "content-type"
                | "accept"
                | "cookie"
                | "set-cookie"
        ) || !names.insert(header.as_str().to_owned())
        {
            return Err(invalid("reserved or duplicate HTTP header"));
        }
        HeaderValue::from_str(env.reference(reference)?)
            .map_err(|_| invalid("invalid HTTP header value"))?;
    }
    Ok(())
}

fn validate_mcp(settings: &McpSettings, env: &Environment) -> Result<(), BoundaryError> {
    settings
        .validate()
        .map_err(|_| invalid("MCP server configuration is invalid"))?;
    for server in settings.servers.values().filter(|server| server.enabled) {
        match &server.transport {
            McpTransportSettings::Stdio(settings) => {
                for reference in settings.env.values() {
                    if env.reference(reference)?.contains('\0') {
                        return Err(invalid(
                            "MCP environment reference contains an invalid value",
                        ));
                    }
                }
            }
            McpTransportSettings::Http(settings) => {
                for reference in settings
                    .bearer_env
                    .iter()
                    .chain(settings.headers_env.values())
                {
                    HeaderValue::from_str(env.reference(reference)?).map_err(|_| {
                        invalid("MCP authentication reference contains an invalid value")
                    })?;
                }
            }
        }
    }
    Ok(())
}
/// Load exactly one configuration file and validate every configured profile.
/// `process_cwd` anchors relative launch paths; the function has no side effects.
pub fn load(
    overrides: Overrides,
    env: Environment,
    process_cwd: &Path,
) -> Result<Config, BoundaryError> {
    let selected_path = process_cwd.join(config_path(&overrides, &env)?);
    let text = std::fs::read_to_string(&selected_path)
        .map_err(|_| invalid("cannot read selected configuration file"))?;
    let path = launch_path(selected_path, process_cwd)?;
    // Do not print parse errors: TOML diagnostics may contain credential-like source text.
    let doc: Document = toml::from_str(&text)
        .map_err(|_| invalid("invalid configuration syntax, type, enum or unknown key"))?;
    if doc.schema_version != 1 {
        return Err(invalid("unsupported configuration schema version"));
    }
    doc.limits.validate()?;
    doc.sandbox.validate()?;
    validate_mcp(&doc.mcp, &env)?;
    doc.output_capture.limits.validate()?;
    doc.context.validate()?;
    doc.plugins.validate()?;
    if doc.review.max_output_tokens == Some(0) {
        return Err(invalid("review output limit must be positive"));
    }
    for (name, p) in &doc.profiles {
        if name.trim().is_empty() {
            return Err(invalid("profile name cannot be empty"));
        }
        validate_profile(p, &env)?;
    }
    if doc
        .review
        .profile
        .as_ref()
        .is_some_and(|p| !doc.profiles.contains_key(p))
    {
        return Err(invalid("review profile does not exist"));
    }
    if doc
        .defaults
        .profile
        .as_ref()
        .is_some_and(|p| !doc.profiles.contains_key(p))
    {
        return Err(invalid("default profile does not exist"));
    }
    if doc.commands.inherit_env.iter().any(|n| !valid_env_name(n)) {
        return Err(invalid("invalid inherited environment name"));
    }
    let profile = match overrides.profile {
        Some(p) => Some(p),
        None => env
            .value("SAYA_PROFILE")?
            .map(str::to_owned)
            .or(doc.defaults.profile),
    };
    if profile
        .as_ref()
        .is_some_and(|p| !doc.profiles.contains_key(p))
    {
        return Err(invalid("selected profile does not exist"));
    }
    let mode = match overrides.mode {
        Some(v) => v,
        None => parse_env(&env, "SAYA_MODE")?.unwrap_or(doc.defaults.mode),
    };
    let permissions_mode = match overrides.permissions {
        Some(v) => v,
        None => parse_env(&env, "SAYA_PERMISSIONS")?.unwrap_or(doc.defaults.permissions),
    };
    let cwd = overrides
        .cwd
        .or_else(|| env.path("SAYA_CWD"))
        .map(|path| process_cwd.join(path))
        .unwrap_or_else(|| process_cwd.to_path_buf());
    // Validate paths and rule semantics before execution, but do not expose
    // path, glob, or operating system diagnostics that might contain secrets.
    Policy::new(&cwd, doc.permissions.clone())
        .map_err(|_| invalid("invalid workspace, permission roots or permission rules"))?;
    let cwd = launch_path(cwd, process_cwd)?;
    let data_dir = launch_path(
        match overrides.data_dir.or_else(|| env.path("SAYA_DATA_DIR")) {
            Some(p) => p,
            None => default_dir(&env, "XDG_DATA_HOME", ".local/share")?,
        },
        process_cwd,
    )?;
    let state_dir = launch_path(
        default_dir(&env, "XDG_STATE_HOME", ".local/state")?,
        process_cwd,
    )?;
    let cache_dir = launch_path(default_dir(&env, "XDG_CACHE_HOME", ".cache")?, process_cwd)?;
    Ok(Config {
        config_path: path,
        cwd,
        data_dir,
        state_dir,
        cache_dir,
        profile,
        mode,
        permissions_mode,
        output: overrides.output.unwrap_or(doc.defaults.output),
        profiles: doc.profiles,
        review: doc.review,
        permissions: doc.permissions,
        commands: doc.commands,
        limits: doc.limits,
        sandbox: doc.sandbox,
        mcp: doc.mcp,
        output_capture: doc.output_capture,
        context: doc.context,
        memory: doc.memory,
        plugins: doc.plugins,
        env,
    })
}
