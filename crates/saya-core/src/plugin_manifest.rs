//! Bounded, offline validation of untrusted saya-plugin v1 declarations.
//!
//! Validation describes requested capabilities. The caller must separately check
//! host compatibility, select an immutable package identity and authorize use.
use crate::contract::{BoundaryError, ErrorInfo};
use jsonschema::{PatternOptions, Validator};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt, sync::OnceLock};

mod lock;
mod package;
pub use lock::{DependencyPin, LockedPackage, PackageSource, PluginLock, SourceKind, ToolAlias};
pub use package::{
    FileBudget, MAX_PACKAGE_BYTES, MAX_PACKAGE_ENTRIES, MAX_PACKAGE_FILE_BYTES, MAX_PACKAGE_FILES,
    PackageFile, TREE_DIGEST_ALGORITHM, VerifiedPackage, inspect_package,
};

pub const MANIFEST_NAME: &str = "saya-plugin.toml";
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
pub const MAX_PATH_BYTES: usize = 1024;
pub const MAX_PATH_DEPTH: usize = 16;
const MAX_JSON_NODES: usize = 16_384;
const MAX_JSON_DEPTH: usize = 24;
const SCHEMA: &str = include_str!("../../../docs/schemas/saya-plugin-v1.schema.json");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError {
    pub code: &'static str,
    pub message: &'static str,
}
impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for PluginError {}
impl From<PluginError> for BoundaryError {
    fn from(error: PluginError) -> Self {
        Self(
            ErrorInfo {
                code: error.code.into(),
                message: error.message.into(),
            },
            None,
            None,
        )
    }
}
type Result<T> = std::result::Result<T, PluginError>;
fn invalid(message: &'static str) -> PluginError {
    PluginError {
        code: "invalid_plugin",
        message,
    }
}
fn incompatible(message: &'static str) -> PluginError {
    PluginError {
        code: "incompatible_plugin",
        message,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub plugin: PluginMetadata,
    pub compatibility: Compatibility,
    pub capabilities: Capabilities,
    #[serde(default)]
    pub skills: Vec<SkillEntry>,
    #[serde(default)]
    pub resources: Vec<ResourceEntry>,
    #[serde(default)]
    pub mcp: Vec<McpEntry>,
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PluginMetadata {
    pub id: String,
    pub version: String,
    pub description: String,
    pub license: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license_file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Compatibility {
    pub requires_saya: String,
    pub requires_features: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    pub requested: Vec<String>,
    #[serde(default)]
    pub network_origins: Vec<String>,
    #[serde(default)]
    pub credential_slots: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SkillEntry {
    pub id: String,
    pub path: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceEntry {
    pub id: String,
    pub path: String,
    pub media_type: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", deny_unknown_fields)]
pub enum McpEntry {
    #[serde(rename = "stdio")]
    Stdio {
        id: String,
        program: String,
        args: Vec<String>,
        #[serde(default)]
        credential_slots: Vec<String>,
    },
    #[serde(rename = "streamable-http")]
    Http {
        id: String,
        url: String,
        #[serde(default)]
        credential_slots: Vec<String>,
    },
}
impl McpEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Stdio { id, .. } | Self::Http { id, .. } => id,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookEntry {
    pub id: String,
    pub event: String,
    pub kind: String,
    pub program: String,
    pub args: Vec<String>,
    pub timeout_ms: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Dependency {
    pub id: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityReport {
    pub version_matches: bool,
    pub missing_features: Vec<String>,
}
impl CompatibilityReport {
    pub fn is_compatible(&self) -> bool {
        self.version_matches && self.missing_features.is_empty()
    }
}

/// Parse a bounded TOML manifest using the bundled, offline v1 schema.
/// Errors never contain input values. This performs no file or network I/O.
pub fn parse_manifest(text: &str) -> Result<Manifest> {
    parse_manifest_with_secrets(text, &[])
}

/// As `parse_manifest`, also rejecting configured secrets before any metadata
/// is returned. Unknown secrets cannot be reliably inferred from arbitrary text.
pub fn parse_manifest_with_secrets(text: &str, secrets: &[String]) -> Result<Manifest> {
    if text.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("manifest exceeds its byte limit"));
    }
    if crate::redaction::contains_secret(text, secrets) {
        return Err(invalid("manifest contains a configured secret"));
    }
    let manifest: Manifest =
        toml::from_str(text).map_err(|_| invalid("manifest TOML or field types are invalid"))?;
    manifest.validate()?;
    Ok(manifest)
}

impl Manifest {
    /// Revalidate values deserialized or constructed by other callers.
    pub fn validate(&self) -> Result<()> {
        let value = serde_json::to_value(self).map_err(|_| invalid("cannot encode manifest"))?;
        validate_structure(&value)?;
        let version = Version::parse(&self.plugin.version)
            .map_err(|_| invalid("plugin version is not SemVer"))?;
        if version.to_string() != self.plugin.version {
            return Err(invalid("plugin version is not canonical SemVer"));
        }
        VersionReq::parse(&self.compatibility.requires_saya)
            .map_err(|_| invalid("host version range is invalid"))?;
        for value in [&self.plugin.description, &self.plugin.license] {
            if value.trim().is_empty() || value.chars().any(crate::text::is_terminal_control) {
                return Err(invalid("plugin metadata contains empty or unsafe text"));
            }
        }
        if let Some(path) = &self.plugin.license_file {
            validate_relative_path(path)?;
        }
        if let Some(homepage) = &self.plugin.homepage {
            let url = reqwest::Url::parse(homepage)
                .map_err(|_| invalid("plugin homepage is not a URI"))?;
            if !url.username().is_empty()
                || url.password().is_some()
                || homepage.chars().any(crate::text::is_terminal_control)
            {
                return Err(invalid("plugin homepage contains unsafe metadata"));
            }
        }
        if self
            .compatibility
            .requires_features
            .iter()
            .any(|feature| feature.len() > 128)
        {
            return Err(invalid("feature name exceeds its byte limit"));
        }
        unique(self.skills.iter().map(|entry| entry.id.as_str()))?;
        unique(self.resources.iter().map(|entry| entry.id.as_str()))?;
        unique(self.mcp.iter().map(McpEntry::id))?;
        unique(self.hooks.iter().map(|entry| entry.id.as_str()))?;
        unique(self.dependencies.iter().map(|entry| entry.id.as_str()))?;
        let mut origins = BTreeSet::new();
        for origin in &self.capabilities.network_origins {
            if !origins.insert(https_origin(origin, true)?) {
                return Err(invalid("duplicate normalized network origin"));
            }
        }
        if !origins.is_empty() {
            self.require_capability("network.outbound")?;
        }
        if !self.capabilities.credential_slots.is_empty() {
            self.require_capability("credential.use")?;
        }
        if self
            .capabilities
            .requested
            .iter()
            .any(|capability| capability.starts_with("browser."))
        {
            self.require_feature("browser-managed-v1")?;
        }
        for entry in &self.skills {
            validate_relative_path(&entry.path)?;
            validate_relative_path(&format!("{}/SKILL.md", entry.path))?;
            self.require_feature("skill-directory-v1")?;
        }
        for entry in &self.resources {
            validate_relative_path(&entry.path)?;
            if entry.media_type.trim().is_empty()
                || entry
                    .media_type
                    .chars()
                    .any(crate::text::is_terminal_control)
            {
                return Err(invalid("resource media type contains unsafe text"));
            }
        }
        for entry in &self.mcp {
            let slots = match entry {
                McpEntry::Stdio {
                    args,
                    credential_slots,
                    ..
                } => {
                    self.require_feature("mcp-stdio-v1")?;
                    self.require_capability("mcp.stdio")?;
                    self.require_capability("process.exec")?;
                    validate_args(args)?;
                    credential_slots
                }
                McpEntry::Http {
                    url,
                    credential_slots,
                    ..
                } => {
                    self.require_feature("mcp-http-v1")?;
                    self.require_capability("mcp.http")?;
                    self.require_capability("network.outbound")?;
                    if !origins.contains(&https_origin(url, false)?) {
                        return Err(invalid("MCP endpoint origin is undeclared"));
                    }
                    credential_slots
                }
            };
            if slots
                .iter()
                .any(|slot| !self.capabilities.credential_slots.contains(slot))
            {
                return Err(invalid("MCP credential slot is undeclared"));
            }
        }
        for hook in &self.hooks {
            self.require_feature("hook-events-v1")?;
            self.require_capability("process.exec")?;
            self.require_capability(if hook.kind == "check" {
                "hook.check"
            } else {
                "hook.observe"
            })?;
            validate_args(&hook.args)?;
        }
        for dependency in &self.dependencies {
            if dependency.id == self.plugin.id {
                return Err(invalid("plugin cannot depend on itself"));
            }
            VersionReq::parse(&dependency.version)
                .map_err(|_| invalid("dependency version range is invalid"))?;
        }
        Ok(())
    }

    pub fn compatibility_report(
        &self,
        host_version: &str,
        supported_features: &BTreeSet<String>,
    ) -> Result<CompatibilityReport> {
        self.validate()?;
        let host =
            Version::parse(host_version).map_err(|_| incompatible("host version is not SemVer"))?;
        let requirement = VersionReq::parse(&self.compatibility.requires_saya)
            .map_err(|_| invalid("host version range is invalid"))?;
        Ok(CompatibilityReport {
            version_matches: requirement.matches(&host),
            missing_features: self
                .compatibility
                .requires_features
                .iter()
                .filter(|feature| !supported_features.contains(*feature))
                .cloned()
                .collect(),
        })
    }

    /// Require the selected host contract. This never grants a requested capability.
    pub fn check_host(
        &self,
        host_version: &str,
        supported_features: &BTreeSet<String>,
    ) -> Result<()> {
        if !self
            .compatibility_report(host_version, supported_features)?
            .is_compatible()
        {
            return Err(incompatible(
                "host version or required feature is unavailable",
            ));
        }
        Ok(())
    }

    pub fn capability_fingerprint(&self) -> Result<String> {
        self.validate()?;
        let mut capabilities = self.capabilities.clone();
        capabilities.requested.sort();
        capabilities.credential_slots.sort();
        capabilities.network_origins = capabilities
            .network_origins
            .iter()
            .map(|origin| https_origin(origin, true))
            .collect::<Result<Vec<_>>>()?;
        capabilities.network_origins.sort();
        let mut features = self.compatibility.requires_features.clone();
        features.sort();
        let mut value = serde_json::json!({"capabilities":capabilities,"features":features});
        value.sort_all_objects();
        let bytes =
            serde_json::to_vec(&value).map_err(|_| invalid("cannot encode capability identity"))?;
        Ok(digest_hex(&bytes))
    }

    fn require_capability(&self, capability: &str) -> Result<()> {
        if !self
            .capabilities
            .requested
            .iter()
            .any(|requested| requested == capability)
        {
            return Err(invalid("entry requires an undeclared capability"));
        }
        Ok(())
    }
    fn require_feature(&self, feature: &str) -> Result<()> {
        if !self
            .compatibility
            .requires_features
            .iter()
            .any(|requested| requested == feature)
        {
            return Err(invalid("entry requires an undeclared host feature"));
        }
        Ok(())
    }
}

fn validate_structure(value: &serde_json::Value) -> Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_| invalid("cannot encode manifest"))?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(invalid("manifest exceeds its encoded byte limit"));
    }
    let mut pending = vec![(value, 0usize)];
    let mut nodes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        nodes += 1;
        if nodes > MAX_JSON_NODES || depth > MAX_JSON_DEPTH {
            return Err(invalid("manifest structure exceeds limits"));
        }
        match value {
            serde_json::Value::Object(values) => {
                pending.extend(values.values().map(|value| (value, depth + 1)))
            }
            serde_json::Value::Array(values) => {
                pending.extend(values.iter().map(|value| (value, depth + 1)))
            }
            _ => {}
        }
    }
    static VALIDATOR: OnceLock<std::result::Result<Validator, ()>> = OnceLock::new();
    let validator = VALIDATOR
        .get_or_init(|| {
            let schema: serde_json::Value = serde_json::from_str(SCHEMA).map_err(|_| ())?;
            jsonschema::options()
                .offline()
                .should_validate_formats(true)
                .with_pattern_options(
                    PatternOptions::fancy_regex()
                        .backtrack_limit(20_000)
                        .size_limit(256 * 1024),
                )
                .build(&schema)
                .map_err(|_| ())
        })
        .as_ref()
        .map_err(|_| invalid("bundled plugin schema is unavailable"))?;
    if !validator.is_valid(value) {
        return Err(invalid("manifest does not match schema v1"));
    }
    Ok(())
}

pub fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.starts_with('/')
        || path.contains(['\\', ':'])
        || path.chars().any(crate::text::is_terminal_control)
        || path.split('/').count() > MAX_PATH_DEPTH
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(invalid(
            "plugin path is not a normalized bounded relative path",
        ));
    }
    Ok(())
}
fn unique<'a>(values: impl Iterator<Item = &'a str>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(invalid("duplicate plugin entry identity"));
        }
    }
    Ok(())
}
fn validate_args(args: &[String]) -> Result<()> {
    for arg in args {
        if let Some(path) = arg.strip_prefix("@plugin/") {
            validate_relative_path(path)?;
        }
        let lower = arg.to_ascii_lowercase();
        if [
            "--api-key",
            "--api_key",
            "--token",
            "--password",
            "--secret",
        ]
        .iter()
        .any(|flag| {
            lower == *flag
                || lower
                    .strip_prefix(flag)
                    .is_some_and(|suffix| suffix.starts_with('='))
        }) || lower.contains("authorization:")
            || lower.contains("bearer ")
        {
            return Err(invalid("credentials must use declared symbolic slots"));
        }
    }
    Ok(())
}
fn https_origin(text: &str, origin_only: bool) -> Result<String> {
    let url = reqwest::Url::parse(text).map_err(|_| invalid("plugin HTTPS URL is invalid"))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || text.chars().any(crate::text::is_terminal_control)
        || (origin_only && url.path() != "/")
    {
        return Err(invalid(
            "plugin URL requires HTTPS without credentials, query or fragment",
        ));
    }
    Ok(url.origin().ascii_serialization())
}
fn digest_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests;
