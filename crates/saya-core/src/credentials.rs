//! Host-only credential references, a private local file backend, and an MCP OAuth bridge.
//! Values are resolved explicitly and never returned by metadata or Debug implementations.

mod file;
mod login;
mod oauth;

pub use file::FileCredentialStore;
pub(crate) use login::authorization_resource;
pub(crate) use login::validate_oauth_metadata;
pub use login::{McpOAuthLogin, OAuthLoginOptions};
pub use oauth::McpCredentialStore;

use crate::contract::{BoundaryError, ErrorInfo};
use rmcp::transport::auth::StoredCredentials;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;
use std::sync::RwLock;
use uuid::Uuid;

const MAX_SECRET_BYTES: usize = 64 * 1024;

fn error(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}

/// A stable opaque identifier. Revoking it is permanent, including across refreshes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CredentialRef(Uuid);

impl fmt::Display for CredentialRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
impl FromStr for CredentialRef {
    type Err = BoundaryError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| error("invalid_credential_ref", "Credential reference is invalid"))
    }
}

/// Exact server, resource audience, and requested-scope binding for one credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialScope {
    pub server_id: String,
    pub origin: String,
    pub audience: String,
    pub scopes: BTreeSet<String>,
}

impl CredentialScope {
    fn validate_oauth_resource(&self, resource: &str) -> Result<String, BoundaryError> {
        let resource = reqwest_mcp::Url::parse(resource)
            .map_err(|_| error("credential_scope", "OAuth resource is invalid"))?;
        let endpoint = reqwest_mcp::Url::parse(&self.audience)
            .map_err(|_| error("credential_scope", "Credential endpoint is invalid"))?;
        let parent_path = resource.path();
        let endpoint_path = endpoint.path();
        if resource.as_str().len() > 4096
            || resource.origin() != endpoint.origin()
            || !resource.username().is_empty()
            || resource.password().is_some()
            || resource.fragment().is_some()
            || !(parent_path == endpoint_path
                || (endpoint_path.starts_with(parent_path)
                    && (parent_path.ends_with('/')
                        || endpoint_path.as_bytes().get(parent_path.len()) == Some(&b'/'))))
        {
            return Err(error(
                "credential_scope",
                "OAuth resource differs from the configured endpoint origin or ancestor path",
            ));
        }
        Ok(resource.to_string())
    }
    pub fn for_mcp(
        server_id: impl Into<String>,
        resource: &str,
        scopes: BTreeSet<String>,
    ) -> Result<Self, BoundaryError> {
        let url = reqwest_mcp::Url::parse(resource)
            .map_err(|_| error("credential_scope", "Credential resource URL is invalid"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(error(
                "credential_scope",
                "Credential resource URL contains unsupported components",
            ));
        }
        let scope = Self {
            server_id: server_id.into(),
            origin: url.origin().ascii_serialization(),
            audience: url.to_string(),
            scopes,
        };
        scope.validate()?;
        Ok(scope)
    }

    fn validate(&self) -> Result<(), BoundaryError> {
        if !crate::registry::valid_server_id(&self.server_id)
            || self.audience.len() > 4096
            || self.scopes.len() > 128
            || self.scopes.iter().any(|scope| {
                scope.is_empty()
                    || scope.len() > 256
                    || scope
                        .chars()
                        .any(|character| character.is_control() || character.is_whitespace())
            })
        {
            return Err(error(
                "credential_scope",
                "Credential scope is invalid or exceeds limits",
            ));
        }
        let url = reqwest_mcp::Url::parse(&self.audience)
            .map_err(|_| error("credential_scope", "Credential audience URL is invalid"))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.origin().ascii_serialization() != self.origin
        {
            return Err(error(
                "credential_scope",
                "Credential origin and audience do not match",
            ));
        }
        Ok(())
    }
}

/// A resolved value. Exposure must be explicit at the final authorized injection boundary.
#[derive(Clone)]
pub struct SecretValue(String);
impl SecretValue {
    pub fn new(value: impl Into<String>) -> Result<Self, BoundaryError> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_SECRET_BYTES || value.contains('\0') {
            return Err(error(
                "invalid_credential",
                "Credential value is empty, oversized, or contains NUL",
            ));
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretValue([REDACTED])")
    }
}

pub enum CredentialValue {
    Secret(SecretValue),
    OAuth(Box<StoredCredentials>),
}
impl fmt::Debug for CredentialValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Secret(_) => "CredentialValue::Secret([REDACTED])",
            Self::OAuth(_) => "CredentialValue::OAuth([REDACTED])",
        })
    }
}

impl CredentialValue {
    pub fn oauth(credentials: StoredCredentials) -> Self {
        Self::OAuth(Box::new(credentials))
    }
    fn validate_scope(&self, scope: &CredentialScope) -> Result<(), BoundaryError> {
        let Self::OAuth(credentials) = self else {
            return Ok(());
        };
        if credentials
            .granted_scopes
            .iter()
            .any(|grant| !scope.scopes.contains(grant))
        {
            return Err(error(
                "credential_scope",
                "OAuth grant exceeds the approved credential scope",
            ));
        }
        if let Some(token) = &credentials.token_response {
            let encoded = serde_json::to_value(token)
                .map_err(|_| error("credential_scope", "OAuth scope cannot be decoded"))?;
            if let Some(token_scope) = encoded.get("scope") {
                let token_scope = token_scope
                    .as_str()
                    .ok_or_else(|| error("credential_scope", "OAuth scope must be a string"))?;
                if token_scope
                    .split_ascii_whitespace()
                    .any(|grant| !scope.scopes.contains(grant))
                {
                    return Err(error(
                        "credential_scope",
                        "OAuth token scope exceeds the approved credential scope",
                    ));
                }
            }
        }
        Ok(())
    }
    fn secrets(&self) -> Result<Vec<String>, BoundaryError> {
        match self {
            Self::Secret(secret) => Ok(vec![secret.expose().to_owned()]),
            Self::OAuth(credentials) => {
                let Some(response) = &credentials.token_response else {
                    return Ok(Vec::new());
                };
                if credentials.client_id.is_empty()
                    || credentials.client_id.len() > 8192
                    || credentials.client_id.chars().any(char::is_control)
                {
                    return Err(error(
                        "credential_scope",
                        "Active OAuth credentials require a valid bound client identifier",
                    ));
                }
                let issuer = credentials.issuer.as_deref().ok_or_else(|| {
                    error(
                        "credential_scope",
                        "Active OAuth credentials require a bound authorization issuer",
                    )
                })?;
                let url = reqwest_mcp::Url::parse(issuer)
                    .map_err(|_| error("credential_scope", "OAuth issuer URL is invalid"))?;
                let loopback = url.host_str().is_some_and(|host| {
                    host == "localhost"
                        || host == "[::1]"
                        || host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|address| address.is_loopback())
                });
                if url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                    || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
                {
                    return Err(error(
                        "credential_scope",
                        "OAuth issuer requires HTTPS or an explicit loopback test endpoint",
                    ));
                }
                let encoded = serde_json::to_value(response)
                    .map_err(|_| error("invalid_credential", "OAuth token cannot be encoded"))?;
                ["access_token", "refresh_token", "id_token", "client_secret"]
                    .into_iter()
                    .filter_map(|name| encoded.get(name).and_then(serde_json::Value::as_str))
                    .map(|value| SecretValue::new(value).map(|secret| secret.0))
                    .collect()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Secret,
    OAuth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialMetadata {
    pub reference: CredentialRef,
    pub scope: CredentialScope,
    pub kind: CredentialKind,
    pub revision: u64,
    pub revoked: bool,
    /// Canonical OAuth resource. The configured transport endpoint remains in scope.audience.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_resource: Option<String>,
}

pub struct ResolvedCredential {
    pub metadata: CredentialMetadata,
    pub value: CredentialValue,
}
impl fmt::Debug for ResolvedCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedCredential")
            .field("metadata", &self.metadata)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Shared bounded history of known secrets for runtime redaction and catalogue validation.
/// Old values remain registered after rotation or revocation for the host process lifetime.
#[derive(Default)]
pub struct SecretRegistry {
    values: RwLock<BTreeSet<String>>,
}
impl fmt::Debug for SecretRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretRegistry([REDACTED])")
    }
}
impl SecretRegistry {
    pub fn register(&self, values: &[String]) -> Result<(), BoundaryError> {
        let mut stored = self
            .values
            .write()
            .map_err(|_| error("credential_registry", "Secret registry lock failed"))?;
        let mut candidate = stored.clone();
        for value in values {
            candidate.insert(SecretValue::new(value.clone())?.0);
        }
        if candidate.len() > 4096
            || candidate.iter().map(String::len).sum::<usize>() > 4 * 1024 * 1024
        {
            return Err(error(
                "credential_registry_full",
                "Known-secret registry exceeds its configured capacity",
            ));
        }
        *stored = candidate;
        Ok(())
    }
    /// Explicit plaintext snapshot intended only for host-side redaction checks.
    pub fn snapshot(&self) -> Result<Vec<String>, BoundaryError> {
        self.values
            .read()
            .map(|values| values.iter().cloned().collect())
            .map_err(|_| error("credential_registry", "Secret registry lock failed"))
    }
}
