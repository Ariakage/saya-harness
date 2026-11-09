//! One interactive authorization-code login, with a private loopback callback.
use super::{
    CredentialRef, CredentialScope, CredentialValue, FileCredentialStore, SecretRegistry, error,
};
use crate::{boundary::Cancellation, contract::BoundaryError};
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationMetadata, CredentialStore, OAuthClientConfig,
    StoredCredentials,
};
use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::Instant,
};
use tracing::instrument::WithSubscriber;

#[derive(Debug, Clone)]
pub struct OAuthLoginOptions {
    pub interactive: bool,
    pub client_id: Option<String>,
    pub timeout: Duration,
    pub callback_port: u16,
}
impl Default for OAuthLoginOptions {
    fn default() -> Self {
        Self {
            interactive: false,
            client_id: None,
            timeout: Duration::from_secs(300),
            callback_port: 0,
        }
    }
}

/// Owns a single-use PKCE/state flow. Dropping it closes the callback listener.
/// Caller has already approved the exact `CredentialScope::scopes` set.
/// Login only creates a new opaque reference, with no configuration edits.
pub struct McpOAuthLogin {
    store: FileCredentialStore,
    scope: CredentialScope,
    manager: AuthorizationManager,
    transient: TransientStore,
    authorization_url: String,
    oauth_resource: String,
    redirect_uri: String,
    listener: TcpListener,
    deadline: Instant,
}
impl fmt::Debug for McpOAuthLogin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("McpOAuthLogin([PENDING])")
    }
}
#[derive(Clone)]
struct TransientStore {
    value: Arc<Mutex<Option<StoredCredentials>>>,
    scope: CredentialScope,
    secrets: Arc<SecretRegistry>,
}
fn store_error() -> AuthError {
    AuthError::CredentialStoreError("Interactive OAuth credential boundary failed".into())
}
#[async_trait::async_trait]
impl CredentialStore for TransientStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        Ok(self.value.lock().map_err(|_| store_error())?.clone())
    }
    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        let value = CredentialValue::oauth(credentials.clone());
        let secrets = value.secrets().map_err(|_| store_error())?;
        self.secrets.register(&secrets).map_err(|_| store_error())?;
        value
            .validate_scope(&self.scope)
            .map_err(|_| store_error())?;
        *self.value.lock().map_err(|_| store_error())? = Some(credentials);
        Ok(())
    }
    async fn clear(&self) -> Result<(), AuthError> {
        *self.value.lock().map_err(|_| store_error())? = None;
        Ok(())
    }
}
fn auth_error() -> BoundaryError {
    error(
        "needs_auth",
        "MCP OAuth authorization could not be completed",
    )
}
pub(crate) fn authorization_resource(input: &str) -> Result<String, BoundaryError> {
    let url = reqwest_mcp::Url::parse(input).map_err(|_| auth_error())?;
    let mut resources = url.query_pairs().filter(|(name, _)| name == "resource");
    let resource = resources.next().ok_or_else(auth_error)?.1.into_owned();
    if resources.next().is_some() {
        return Err(auth_error());
    }
    reqwest_mcp::Url::parse(&resource)
        .map(|url| url.to_string())
        .map_err(|_| auth_error())
}
fn loopback(url: &reqwest_mcp::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host == "[::1]"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    })
}
pub(crate) fn validate_oauth_metadata(
    metadata: &AuthorizationMetadata,
    audience: &str,
) -> Result<(), BoundaryError> {
    let resource = reqwest_mcp::Url::parse(audience).map_err(|_| auth_error())?;
    let issuer = metadata.issuer.as_deref().ok_or_else(auth_error)?;
    for endpoint in [
        Some(issuer),
        Some(metadata.authorization_endpoint.as_str()),
        Some(metadata.token_endpoint.as_str()),
        metadata.registration_endpoint.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let url = reqwest_mcp::Url::parse(endpoint).map_err(|_| auth_error())?;
        if url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !(url.scheme() == "https"
                || (url.scheme() == "http" && loopback(&url) && loopback(&resource)))
            || (endpoint == issuer && url.query().is_some())
        {
            return Err(error(
                "needs_auth",
                "MCP OAuth metadata contains an unsupported endpoint",
            ));
        }
    }
    Ok(())
}
async fn bounded<T>(
    cancel: &dyn Cancellation,
    deadline: Instant,
    operation: impl Future<Output = Result<T, BoundaryError>>,
) -> Result<T, BoundaryError> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(error("cancelled", "MCP OAuth login was cancelled")),
        _ = tokio::time::sleep_until(deadline) => Err(error("oauth_timeout", "MCP OAuth login expired")),
        result = operation => result,
    }
}
impl McpOAuthLogin {
    /// Discover the configured resource's issuer and create a bounded PKCE flow.
    /// The host confirms `scope.scopes` before calling. Noninteractive callers
    /// receive `needs_input`. Cancellation closes the listener. This performs
    /// metadata requests and may register a public OAuth client, but creates no
    /// credential record until a callback succeeds.
    pub async fn begin(
        store: FileCredentialStore,
        scope: CredentialScope,
        options: OAuthLoginOptions,
        cancel: &dyn Cancellation,
    ) -> Result<Self, BoundaryError> {
        if !options.interactive {
            return Err(error(
                "needs_input",
                "MCP OAuth login requires an interactive host user",
            ));
        }
        scope.validate()?;
        if options.timeout.is_zero()
            || options.timeout > Duration::from_secs(600)
            || options.client_id.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 8192 || id.chars().any(char::is_control)
            })
        {
            return Err(error(
                "invalid_config",
                "MCP OAuth login settings are invalid",
            ));
        }
        let deadline = Instant::now() + options.timeout;
        bounded(
            cancel,
            deadline,
            async move {
                let listener =
                    TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, options.callback_port))
                        .await
                        .map_err(|_| {
                            error("oauth_callback", "MCP loopback callback could not bind")
                        })?;
                let port = listener.local_addr().map_err(|_| auth_error())?.port();
                let redirect_uri = format!("http://127.0.0.1:{port}/oauth/callback");
                let transient = TransientStore {
                    value: Arc::new(Mutex::new(None)),
                    scope: scope.clone(),
                    secrets: store.secret_registry(),
                };
                let mut manager = AuthorizationManager::new(&scope.audience)
                    .await
                    .map_err(|_| auth_error())?;
                manager.set_credential_store(transient.clone());
                let resolution = manager.resolve_metadata().await.map_err(|_| auth_error())?;
                validate_oauth_metadata(&resolution.metadata, &scope.audience)?;
                manager.set_metadata(resolution.metadata);
                let scopes = scope.scopes.iter().map(String::as_str).collect::<Vec<_>>();
                if let Some(client_id) = options.client_id {
                    manager
                        .configure_client(
                            OAuthClientConfig::new(client_id, &redirect_uri)
                                .with_scopes(scope.scopes.iter().cloned().collect())
                                .with_application_type("native"),
                        )
                        .map_err(|_| auth_error())?;
                } else {
                    let client = manager
                        .register_client("Saya Harness", &redirect_uri, &scopes)
                        .await
                        .map_err(|_| auth_error())?;
                    if let Some(secret) = client.client_secret {
                        store.secret_registry().register(&[secret])?;
                        return Err(error(
                            "needs_auth",
                            "MCP native login requires a public OAuth client",
                        ));
                    }
                }
                // Call the manager directly so the SDK cannot add unapproved offline_access or default scopes.
                let authorization_url = manager
                    .get_authorization_url(&scopes)
                    .await
                    .map_err(|_| auth_error())?;
                let oauth_resource = authorization_resource(&authorization_url)?;
                scope.validate_oauth_resource(&oauth_resource)?;
                Ok(Self {
                    store,
                    scope,
                    manager,
                    transient,
                    authorization_url,
                    oauth_resource,
                    redirect_uri,
                    listener,
                    deadline,
                })
            }
            .with_subscriber(tracing::Dispatch::none()),
        )
        .await
    }
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }
    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }
    fn callback(&self, input: &str) -> Result<BTreeMap<String, String>, BoundaryError> {
        if input.len() > 16 * 1024 {
            return Err(auth_error());
        }
        let url = reqwest_mcp::Url::parse(input).map_err(|_| auth_error())?;
        let redirect = reqwest_mcp::Url::parse(&self.redirect_uri).map_err(|_| auth_error())?;
        if url.origin() != redirect.origin()
            || url.path() != redirect.path()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(auth_error());
        }
        let mut fields = BTreeMap::new();
        for (name, value) in url.query_pairs() {
            if !matches!(
                name.as_ref(),
                "code" | "state" | "iss" | "error" | "error_description" | "error_uri"
            ) || value.is_empty()
                || value.chars().any(char::is_control)
                || fields
                    .insert(name.into_owned(), value.into_owned())
                    .is_some()
            {
                return Err(auth_error());
            }
        }
        if fields.contains_key("error")
            || !fields.contains_key("code")
            || !fields.contains_key("state")
        {
            return Err(auth_error());
        }
        let authorization =
            reqwest_mcp::Url::parse(&self.authorization_url).map_err(|_| auth_error())?;
        let expected = authorization
            .query_pairs()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.into_owned());
        if fields.get("state") != expected.as_ref() {
            return Err(auth_error());
        }
        Ok(fields)
    }
    /// Consume a manually copied callback URL with exact destination, state,
    /// issuer and TTL validation. A successful exchange creates a fresh private
    /// credential reference. Token exchange observes cancellation. Once private
    /// persistence begins, its atomic write is awaited to a definite result.
    pub async fn complete_callback_url(
        self,
        input: &str,
        cancel: &dyn Cancellation,
    ) -> Result<CredentialRef, BoundaryError> {
        let fields = self.callback(input)?;
        self.complete(fields, cancel).await
    }
    async fn complete(
        self,
        fields: BTreeMap<String, String>,
        cancel: &dyn Cancellation,
    ) -> Result<CredentialRef, BoundaryError> {
        let credentials = bounded(cancel, self.deadline, async {
            let code = fields.get("code").ok_or_else(auth_error)?;
            let state = fields.get("state").ok_or_else(auth_error)?;
            self.store
                .secret_registry()
                .register(&[code.clone(), state.clone()])?;
            self.manager
                .exchange_code_for_token_with_issuer(
                    code,
                    state,
                    fields.get("iss").map(String::as_str),
                )
                .with_subscriber(tracing::Dispatch::none())
                .await
                .map_err(|_| auth_error())?;
            self.transient
                .load()
                .await
                .map_err(|_| auth_error())?
                .ok_or_else(auth_error)
        })
        .await?;
        // This local commit point is deliberately outside the cancellation
        // race. Dropping spawn_blocking would leave a possibly saved orphan ref.
        self.store
            .create_oauth(self.scope.clone(), credentials, self.oauth_resource.clone())
            .await
    }
    /// Consume this flow after one valid loopback HTTP callback. Malformed
    /// local requests receive HTTP 400 and remain bounded by the same TTL.
    /// Cancellation, expiry or token-exchange failure returns a static error.
    pub async fn wait_for_callback(
        self,
        cancel: &dyn Cancellation,
    ) -> Result<CredentialRef, BoundaryError> {
        let fields = bounded(cancel,self.deadline,async {
            loop {
                let (mut socket,peer) = self.listener.accept().await.map_err(|_| auth_error())?;
                if !peer.ip().is_loopback() { continue; }
                let request = tokio::time::timeout(Duration::from_secs(3),async {
                    let mut bytes = Vec::new();
                    let mut buffer = [0u8;1024];
                    while !bytes.ends_with(b"\r\n\r\n") {
                        let count = socket.read(&mut buffer).await.map_err(|_| auth_error())?;
                        if count == 0 || bytes.len()+count>16*1024 { return Err(auth_error()); }
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    let text = std::str::from_utf8(&bytes).map_err(|_| auth_error())?;
                    let mut words = text.lines().next().ok_or_else(auth_error)?.split_whitespace();
                    if words.next()!=Some("GET") { return Err(auth_error()); }
                    let path = words.next().ok_or_else(auth_error)?;
                    if !path.starts_with("/oauth/callback?") || words.next()!=Some("HTTP/1.1") || words.next().is_some() {return Err(auth_error());}
                    let callback = format!("{}{}",self.redirect_uri.trim_end_matches("/oauth/callback"),path);
                    self.callback(&callback)
                }).await;
                let Ok(Ok(fields)) = request else {
                    let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                    continue;
                };
                let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 53\r\nConnection: close\r\n\r\nAuthorization received. Return to the Saya terminal.\n").await;
                return Ok(fields);
            }
        }).await?;
        self.complete(fields, cancel).await
    }
}
