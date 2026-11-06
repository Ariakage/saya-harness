//! Strict, explicitly selected TOML configuration and environment resolution.
//! Loading configuration does not create files, call providers or execute tools.
use crate::{
    contract::{BoundaryError, ErrorInfo, PermissionMode, WorkMode},
    permissions::{PermissionConfig, Policy},
};
use reqwest::{
    Url,
    header::{HeaderName, HeaderValue},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    net::IpAddr,
    path::{Path, PathBuf},
};

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
impl Environment {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Protocol {
    #[serde(rename = "openai-chat-completions")]
    ChatCompletions,
    #[serde(rename = "openai-responses")]
    Responses,
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Auth {
    #[serde(rename = "bearer")]
    Bearer,
    #[serde(rename = "x-api-key")]
    ApiKey,
    #[serde(rename = "none")]
    None,
}
fn default_output_tokens() -> u32 {
    8192
}
fn default_review_tokens() -> u32 {
    1024
}
fn yes() -> bool {
    true
}
fn default_version() -> String {
    "2026-10-21".into()
}

/// Provider settings contain reference names, never API key values.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub protocol: Protocol,
    pub base_url: String,
    pub model: String,
    pub auth: Auth,
    pub api_key_env: Option<String>,
    pub context_window_tokens: u64,
    #[serde(default = "default_output_tokens")]
    pub max_output_tokens: u32,
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
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Defaults {
    profile: Option<String>,
    mode: WorkMode,
    permissions: PermissionMode,
    output: Output,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Review {
    pub profile: Option<String>,
    pub max_output_tokens: u32,
}
impl Default for Review {
    fn default() -> Self {
        Self {
            profile: None,
            max_output_tokens: default_review_tokens(),
        }
    }
}
#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Commands {
    pub inherit_env: BTreeSet<String>,
}

/// All B-15 limits; zero is rejected instead of disabling a limit.
#[derive(Deserialize)]
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
}
/// Effective settings; no model is implicitly selected. Secret values stay in Environment.
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
    env: Environment,
}
impl Config {
    /// Access a validated credential or header environment reference; do not log its return value.
    pub fn secret(&self, name: &str) -> Result<&str, BoundaryError> {
        self.env.reference(name)
    }
    pub fn secret_env_names(&self) -> BTreeSet<String> {
        self.profiles
            .values()
            .flat_map(|p| p.api_key_env.iter().chain(p.headers_env.values()))
            .cloned()
            .collect()
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
/// Load exactly one configuration file and validate every configured profile.
/// `process_cwd` resolves the default workspace; the function has no side effects.
pub fn load(
    overrides: Overrides,
    env: Environment,
    process_cwd: &Path,
) -> Result<Config, BoundaryError> {
    let path = config_path(&overrides, &env)?;
    let text = std::fs::read_to_string(&path)
        .map_err(|_| invalid("cannot read selected configuration file"))?;
    // Do not print parse errors: TOML diagnostics may contain credential-like source text.
    let doc: Document = toml::from_str(&text)
        .map_err(|_| invalid("invalid configuration syntax, type, enum or unknown key"))?;
    if doc.schema_version != 1 {
        return Err(invalid("unsupported configuration schema version"));
    }
    doc.limits.validate()?;
    if doc.review.max_output_tokens == 0 {
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
        .unwrap_or_else(|| process_cwd.to_path_buf());
    // Validate paths and rule semantics before execution, but do not expose
    // path, glob, or operating system diagnostics that might contain secrets.
    Policy::new(&cwd, doc.permissions.clone())
        .map_err(|_| invalid("invalid workspace, permission roots or permission rules"))?;
    let data_dir = match overrides.data_dir.or_else(|| env.path("SAYA_DATA_DIR")) {
        Some(p) => p,
        None => default_dir(&env, "XDG_DATA_HOME", ".local/share")?,
    };
    let state_dir = default_dir(&env, "XDG_STATE_HOME", ".local/state")?;
    let cache_dir = default_dir(&env, "XDG_CACHE_HOME", ".cache")?;
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
        env,
    })
}
