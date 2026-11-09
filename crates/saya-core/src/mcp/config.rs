//! Explicit server settings. Discovery never changes runtime policy or credential references.
use super::{McpHttpClient, McpLimits, error};
use crate::boundary::{Cancellation, Operation};
use crate::contract::BoundaryError;
use crate::credentials::CredentialRef;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpSettings {
    pub servers: BTreeMap<String, McpServerSettings>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerSettings {
    #[serde(default = "enabled")]
    pub enabled: bool,
    pub transport: McpTransportSettings,
    #[serde(default)]
    pub read_only_tools: BTreeSet<String>,
    #[serde(default)]
    pub limits: McpLimitSettings,
}
fn enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpTransportSettings {
    Stdio(StdioMcpSettings),
    Http(HttpMcpSettings),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StdioMcpSettings {
    pub argv: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    /// Destination environment name to captured host environment reference name.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpMcpSettings {
    pub endpoint: String,
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub bearer_env: Option<String>,
    #[serde(default)]
    pub headers_env: BTreeMap<String, String>,
    #[serde(default)]
    pub credential_ref: Option<CredentialRef>,
    #[serde(default)]
    pub scopes: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpLimitSettings {
    pub connect_timeout_seconds: u64,
    pub request_timeout_seconds: u64,
    pub max_wire_bytes: usize,
    pub max_pages: usize,
    pub max_tools: usize,
    pub max_catalog_bytes: usize,
    pub max_result_bytes: usize,
    pub max_content_blocks: usize,
}
impl Default for McpLimitSettings {
    fn default() -> Self {
        let limits = McpLimits::default();
        Self {
            connect_timeout_seconds: limits.connect_timeout.as_secs(),
            request_timeout_seconds: limits.request_timeout.as_secs(),
            max_wire_bytes: limits.max_wire_bytes,
            max_pages: limits.max_pages,
            max_tools: limits.max_tools,
            max_catalog_bytes: limits.max_catalog_bytes,
            max_result_bytes: limits.max_result_bytes,
            max_content_blocks: limits.max_content_blocks,
        }
    }
}
impl McpLimitSettings {
    pub fn resolve(&self) -> Result<McpLimits, BoundaryError> {
        let limits = McpLimits {
            connect_timeout: Duration::from_secs(self.connect_timeout_seconds),
            request_timeout: Duration::from_secs(self.request_timeout_seconds),
            max_wire_bytes: self.max_wire_bytes,
            max_pages: self.max_pages,
            max_tools: self.max_tools,
            max_catalog_bytes: self.max_catalog_bytes,
            max_result_bytes: self.max_result_bytes,
            max_content_blocks: self.max_content_blocks,
        };
        limits.validate()?;
        if limits.max_wire_bytes > 16 * 1024 * 1024
            || limits.max_catalog_bytes > 16 * 1024 * 1024
            || limits.max_result_bytes > 1024 * 1024
            || limits.max_tools > 256
            || limits.max_pages > 128
            || limits.max_content_blocks > 1024
            || self.connect_timeout_seconds > 120
            || self.request_timeout_seconds > 600
        {
            return Err(error(
                "invalid_config",
                "MCP limits exceed supported maxima",
            ));
        }
        Ok(limits)
    }
}
impl McpSettings {
    pub fn validate(&self) -> Result<(), BoundaryError> {
        if self.servers.len() > 16 {
            return Err(error("invalid_config", "MCP server count exceeds sixteen"));
        }
        for (server, settings) in &self.servers {
            if !crate::registry::valid_server_id(server) {
                return Err(error("invalid_config", "MCP server identifier is invalid"));
            }
            settings.limits.resolve()?;
            if settings.read_only_tools.len() > 256
                || settings
                    .read_only_tools
                    .iter()
                    .any(|name| !valid_tool_name(name))
            {
                return Err(error(
                    "invalid_config",
                    "MCP read-only tool names are invalid",
                ));
            }
            match &settings.transport {
                McpTransportSettings::Stdio(settings) => {
                    if settings.argv.is_empty()
                        || settings.argv[0].is_empty()
                        || settings.argv.len() > 256
                        || settings.argv.iter().map(String::len).sum::<usize>() > 64 * 1024
                        || settings.argv.iter().any(|argument| argument.contains('\0'))
                        || settings.env.len() > 64
                        || settings.env.iter().any(|(destination, source)| {
                            !valid_env_name(destination) || !valid_env_name(source)
                        })
                    {
                        return Err(error(
                            "invalid_config",
                            "MCP stdio argv or environment references are invalid",
                        ));
                    }
                }
                McpTransportSettings::Http(settings) => {
                    let mut header_names = BTreeSet::new();
                    let url = reqwest_mcp::Url::parse(&settings.endpoint)
                        .map_err(|_| error("invalid_config", "MCP HTTP endpoint is invalid"))?;
                    if settings.endpoint.len() > 4096
                        || url.host_str().is_none()
                        || !url.username().is_empty()
                        || url.password().is_some()
                        || url.query().is_some()
                        || url.fragment().is_some()
                        || !(url.scheme() == "https"
                            || (url.scheme() == "http" && settings.allow_insecure_http))
                        || (settings.bearer_env.is_some() && settings.credential_ref.is_some())
                        || settings
                            .bearer_env
                            .as_ref()
                            .is_some_and(|name| !valid_env_name(name))
                        || settings.headers_env.len() > 32
                        || settings.headers_env.iter().any(|(name, source)| {
                            reqwest_mcp::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                                || !valid_env_name(source)
                                || !header_names.insert(name.to_ascii_lowercase())
                                || matches!(
                                    name.to_ascii_lowercase().as_str(),
                                    "authorization"
                                        | "host"
                                        | "proxy-authorization"
                                        | "connection"
                                        | "content-length"
                                        | "content-type"
                                        | "accept"
                                        | "transfer-encoding"
                                )
                                || name.to_ascii_lowercase().starts_with("mcp-")
                        })
                        || settings.scopes.len() > 128
                        || settings.scopes.iter().any(|scope| {
                            scope.is_empty()
                                || scope.len() > 256
                                || scope.chars().any(|character| {
                                    character.is_control() || character.is_whitespace()
                                })
                        })
                    {
                        return Err(error(
                            "invalid_config",
                            "MCP HTTP settings contain invalid endpoint, authentication, or references",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}
pub(crate) fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

/// Host-only result. Neither credentials nor the authenticated client are serialized.
pub struct McpHttpAuthorization {
    pub client: McpHttpClient,
    pub bearer_token: Option<String>,
}

/// Applications resolve explicit credential references. A run never starts a login flow itself.
pub trait McpHttpAuthorizer: Send + Sync {
    fn authorize<'a>(
        &'a self,
        server: &'a str,
        settings: &'a HttpMcpSettings,
        client: McpHttpClient,
        cancel: &'a dyn Cancellation,
    ) -> Operation<'a, McpHttpAuthorization>;
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_servers_reject_unknown_fields_and_secret_literals() {
        let valid = r#"[servers.local]
read_only_tools=["fetch"]
[servers.local.transport]
type="stdio"
argv=["server","--stdio"]
env={SERVICE_TOKEN="SAYA_SERVICE_TOKEN"}
"#;
        let settings: McpSettings = toml::from_str(valid).unwrap();
        settings.validate().unwrap();
        assert!(
            toml::from_str::<McpSettings>(&valid.replace("argv=", "unknown=true\nargv=")).is_err()
        );
        let invalid: McpSettings =
            toml::from_str(&valid.replace("SAYA_SERVICE_TOKEN", "literal-secret-value")).unwrap();
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn http_headers_cannot_alias_by_case_and_scopes_reject_control_characters() {
        let mut settings: McpSettings = toml::from_str(
            r#"[servers.remote.transport]
type="http"
endpoint="https://example.invalid/mcp"
headers_env={"X-Service-Token"="TOKEN_ONE","x-service-token"="TOKEN_TWO"}
"#,
        )
        .unwrap();
        assert!(settings.validate().is_err());
        let McpTransportSettings::Http(http) =
            &mut settings.servers.get_mut("remote").unwrap().transport
        else {
            panic!("expected HTTP");
        };
        http.headers_env.clear();
        http.scopes.insert("read\u{7f}".into());
        assert!(settings.validate().is_err());
    }
}
