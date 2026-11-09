use super::wire::OutgoingLimit;
use super::{McpLimits, error};
use crate::contract::BoundaryError;
use crate::credentials::{CredentialRef, CredentialScope, CredentialValue, FileCredentialStore};
use futures::{StreamExt, stream::BoxStream};
use reqwest_mcp::{
    Client,
    header::{HeaderName, HeaderValue},
};
use rmcp::model::{ClientJsonRpcMessage, ClientRequest, ServerJsonRpcMessage};
use rmcp::transport::auth::{AuthorizationManager, CredentialStore as _};
use rmcp::transport::streamable_http_client::{
    AuthRequiredError, HttpStatusError, InsufficientScopeError, StreamableHttpClient,
    StreamableHttpError, StreamableHttpPostResponse,
};
use sse_stream::{Error as SseError, Sse, SseStream};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::instrument::WithSubscriber;

type HttpError = StreamableHttpError<reqwest_mcp::Error>;

/// Single-send HTTP transport with early JSON and SSE byte limits.
/// OAuth tokens may refresh before a request. A 401 never resends the MCP request.
#[derive(Clone)]
pub struct McpHttpClient {
    client: Client,
    max_bytes: usize,
    authorization: Option<Arc<Mutex<AuthorizationManager>>>,
    authorized_endpoint: Option<String>,
    bearer_credential: Option<Arc<StoredBearer>>,
}
struct StoredBearer {
    store: FileCredentialStore,
    reference: CredentialRef,
    scope: CredentialScope,
}

#[derive(Debug)]
pub(super) struct AudienceMismatch;
impl std::fmt::Display for AudienceMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MCP request target differs from its authorized endpoint")
    }
}
impl std::error::Error for AudienceMismatch {}

impl std::fmt::Debug for McpHttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHttpClient")
            .field("max_bytes", &self.max_bytes)
            .field("has_authorization", &self.authorization.is_some())
            .field("has_bearer_reference", &self.bearer_credential.is_some())
            .finish()
    }
}

impl McpHttpClient {
    pub fn new(limits: &McpLimits) -> Result<Self, BoundaryError> {
        limits.validate()?;
        let client = Client::builder()
            .redirect(reqwest_mcp::redirect::Policy::none())
            .retry(reqwest_mcp::retry::never())
            .no_proxy()
            .connect_timeout(limits.connect_timeout)
            .build()
            .map_err(|_| error("mcp_transport", "MCP HTTP client setup failed"))?;
        Ok(Self {
            client,
            max_bytes: limits.max_wire_bytes,
            authorization: None,
            authorized_endpoint: None,
            bearer_credential: None,
        })
    }

    pub fn with_auth_manager(
        mut self,
        manager: AuthorizationManager,
        endpoint: &str,
    ) -> Result<Self, BoundaryError> {
        if self.bearer_credential.is_some() {
            return Err(error(
                "credential_scope",
                "MCP authentication sources conflict",
            ));
        }
        let url = reqwest_mcp::Url::parse(endpoint)
            .map_err(|_| error("credential_scope", "Authorized MCP endpoint is invalid"))?;
        if !matches!(url.scheme(), "https" | "http")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(error(
                "credential_scope",
                "Authorized MCP endpoint contains unsupported URL components",
            ));
        }
        self.authorization = Some(Arc::new(Mutex::new(manager)));
        self.authorized_endpoint = Some(url.to_string());
        Ok(self)
    }

    /// Resolve this exact endpoint-bound bearer reference before every HTTP
    /// operation, so file revocation affects an already connected client.
    pub fn with_stored_bearer(
        mut self,
        store: FileCredentialStore,
        reference: CredentialRef,
        scope: CredentialScope,
    ) -> Result<Self, BoundaryError> {
        let normalized =
            CredentialScope::for_mcp(&scope.server_id, &scope.audience, scope.scopes.clone())?;
        if normalized != scope || self.authorization.is_some() {
            return Err(error(
                "credential_scope",
                "MCP bearer reference binding is invalid or conflicts",
            ));
        }
        self.authorized_endpoint = Some(scope.audience.clone());
        self.bearer_credential = Some(Arc::new(StoredBearer {
            store,
            reference,
            scope,
        }));
        Ok(self)
    }

    /// Restore an existing public OAuth grant without launching interactive authorization.
    /// Both a missing issuer and an issuer change require a new explicit login.
    pub async fn with_oauth_store(
        self,
        endpoint: &str,
        store: crate::credentials::McpCredentialStore,
    ) -> Result<Self, BoundaryError> {
        self.restore_oauth_store(endpoint, store)
            .with_subscriber(tracing::Dispatch::none())
            .await
    }
    async fn restore_oauth_store(
        self,
        endpoint: &str,
        store: crate::credentials::McpCredentialStore,
    ) -> Result<Self, BoundaryError> {
        let expected_resource = store.oauth_resource().await?;
        let stored = store
            .load()
            .await
            .map_err(|_| error("needs_auth", "MCP stored OAuth credentials are unavailable"))?
            .filter(|credentials| credentials.token_response.is_some())
            .ok_or_else(|| error("needs_auth", "MCP requires an explicit OAuth login"))?;
        let issuer = stored
            .issuer
            .ok_or_else(|| error("needs_auth", "MCP OAuth grant has no bound issuer"))?;
        let mut manager = AuthorizationManager::new(endpoint)
            .await
            .map_err(|_| error("needs_auth", "MCP OAuth manager could not be prepared"))?;
        manager.set_credential_store(store);
        let resolution = manager
            .resolve_metadata()
            .await
            .map_err(|_| error("needs_auth", "MCP OAuth metadata could not be resolved"))?;
        crate::credentials::validate_oauth_metadata(&resolution.metadata, endpoint)?;
        if resolution.metadata.issuer.as_deref() != Some(issuer.as_str()) {
            return Err(error(
                "needs_auth",
                "MCP authorization issuer changed or is unavailable",
            ));
        }
        manager.set_metadata(resolution.metadata);
        manager
            .configure_client_id(&stored.client_id)
            .map_err(|_| error("needs_auth", "MCP OAuth client could not be restored"))?;
        let resource_probe = manager
            .get_authorization_url(&[])
            .await
            .map_err(|_| error("needs_auth", "MCP OAuth resource could not be validated"))?;
        if crate::credentials::authorization_resource(&resource_probe)? != expected_resource {
            return Err(error("needs_auth", "MCP canonical OAuth resource changed"));
        }
        if !manager
            .initialize_from_store()
            .await
            .map_err(|_| error("needs_auth", "MCP OAuth grant could not be restored"))?
        {
            return Err(error("needs_auth", "MCP requires a new OAuth login"));
        }
        self.with_auth_manager(manager, endpoint)
    }

    async fn token(
        &self,
        uri: &str,
        supplied: Option<String>,
    ) -> Result<Option<String>, HttpError> {
        if let Some(expected) = &self.authorized_endpoint
            && reqwest_mcp::Url::parse(uri)
                .ok()
                .as_ref()
                .map(reqwest_mcp::Url::as_str)
                != Some(expected.as_str())
        {
            return Err(StreamableHttpError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                AudienceMismatch,
            )));
        }
        if let Some(credential) = &self.bearer_credential {
            if supplied.is_some() {
                return Err(StreamableHttpError::UnexpectedServerResponse(
                    "conflicting credential sources".into(),
                ));
            }
            let resolved = credential
                .store
                .resolve(credential.reference, &credential.scope)
                .await
                .map_err(|_| {
                    StreamableHttpError::Auth(
                        rmcp::transport::auth::AuthError::AuthorizationRequired,
                    )
                })?;
            return match resolved.value {
                CredentialValue::Secret(value) => Ok(Some(value.expose().to_owned())),
                _ => Err(StreamableHttpError::Auth(
                    rmcp::transport::auth::AuthError::AuthorizationRequired,
                )),
            };
        }
        match (&self.authorization, supplied) {
            (Some(_), Some(_)) => Err(StreamableHttpError::UnexpectedServerResponse(
                "conflicting credential sources".into(),
            )),
            (Some(manager), None) => async { manager.lock().await.get_access_token().await }
                .with_subscriber(tracing::Dispatch::none())
                .await
                .map(Some)
                .map_err(StreamableHttpError::Auth),
            (None, supplied) => Ok(supplied),
        }
    }

    async fn send_post(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        max_event: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        let encoded = serde_json::to_vec(&message).map_err(StreamableHttpError::Deserialize)?;
        if encoded.len() > self.max_bytes {
            return Err(StreamableHttpError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                OutgoingLimit,
            )));
        }
        let token = self.token(uri.as_ref(), auth).await?;
        let mut request = self
            .client
            .post(uri.as_ref())
            .header("accept", "application/json, text/event-stream");
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let attached = session_id.is_some();
        if let Some(session) = session_id {
            request = request.header("Mcp-Session-Id", session.as_ref());
        }
        let response = request
            .header("content-type", "application/json")
            .body(encoded)
            .send()
            .await
            .map_err(StreamableHttpError::Client)?;
        let status = response.status();
        if matches!(status.as_u16(), 401 | 403)
            && let Some(challenge) = response.headers().get("www-authenticate")
        {
            let challenge = challenge
                .to_str()
                .map_err(|_| {
                    StreamableHttpError::UnexpectedServerResponse(
                        "invalid authorization challenge".into(),
                    )
                })?
                .to_owned();
            return Err(if status.as_u16() == 401 {
                StreamableHttpError::AuthRequired(AuthRequiredError::new(challenge))
            } else {
                StreamableHttpError::InsufficientScope(InsufficientScopeError::new(challenge, None))
            });
        }
        if matches!(status.as_u16(), 202 | 204) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        if status.as_u16() == 404 && attached {
            return Err(StreamableHttpError::SessionExpired);
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(String::from);
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if status.is_success() && mime.starts_with("text/event-stream") {
            return Ok(StreamableHttpPostResponse::Sse(
                sse(response, self.max_bytes.min(max_event)),
                session,
            ));
        }
        let notification = !matches!(message, ClientJsonRpcMessage::Request(_));
        if status.is_success() && notification && response.content_length() == Some(0) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let is_json = mime.starts_with("application/json");
        if status.is_success() && !is_json {
            return Err(StreamableHttpError::UnexpectedContentType(Some(mime)));
        }
        let max_body = if status.is_success() {
            self.max_bytes
        } else {
            self.max_bytes.min(64 * 1024)
        };
        let body = read_body(response, max_body).await?;
        let decoded = serde_json::from_slice::<ServerJsonRpcMessage>(&body);
        if !status.is_success() {
            // The Auto lifecycle must correlate a legacy discover rejection to its probe.
            if status.is_client_error()
                && !matches!(status.as_u16(), 401 | 403)
                && let ClientJsonRpcMessage::Request(request) = &message
                && matches!(request.request, ClientRequest::DiscoverRequest(_))
            {
                let error = match decoded {
                    Ok(ServerJsonRpcMessage::Error(error)) => error.error,
                    _ => rmcp::ErrorData::invalid_request("legacy discovery rejected", None),
                };
                return Ok(StreamableHttpPostResponse::Json(
                    ServerJsonRpcMessage::error(error, Some(request.id.clone())),
                    None,
                ));
            }
            if !matches!(status.as_u16(), 401 | 403)
                && is_json
                && let Ok(message @ ServerJsonRpcMessage::Error(_)) = decoded
            {
                return Ok(StreamableHttpPostResponse::Json(message, session));
            }
            return Err(StreamableHttpError::HttpStatus(HttpStatusError::new(
                status.as_u16(),
                None,
            )));
        }
        match decoded {
            Ok(message) => Ok(StreamableHttpPostResponse::Json(message, session)),
            Err(_) if notification => Ok(StreamableHttpPostResponse::Accepted),
            Err(_) => Err(StreamableHttpError::UnexpectedServerResponse(
                "invalid MCP JSON response".into(),
            )),
        }
    }
}

impl StreamableHttpClient for McpHttpClient {
    type Error = reqwest_mcp::Error;
    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.send_post(uri, message, session_id, auth, headers, self.max_bytes)
            .await
    }
    async fn post_message_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        max_event: usize,
    ) -> Result<StreamableHttpPostResponse, HttpError> {
        self.send_post(uri, message, session_id, auth, headers, max_event)
            .await
    }
    async fn delete_session(
        &self,
        uri: Arc<str>,
        session: Arc<str>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), HttpError> {
        let token = self.token(uri.as_ref(), auth).await?;
        self.client
            .delete_session(uri, session, token, headers)
            .await
    }
    async fn get_stream(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last_id: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, HttpError> {
        self.get_stream_with_max_sse_event_size(
            uri,
            session,
            last_id,
            auth,
            headers,
            self.max_bytes,
        )
        .await
    }
    async fn get_stream_with_max_sse_event_size(
        &self,
        uri: Arc<str>,
        session: Option<Arc<str>>,
        last_id: Option<String>,
        auth: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
        max_event: usize,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, HttpError> {
        let token = self.token(uri.as_ref(), auth).await?;
        self.client
            .get_stream_with_max_sse_event_size(
                uri,
                session,
                last_id,
                token,
                headers,
                self.max_bytes.min(max_event),
            )
            .await
    }
}

async fn read_body(
    response: reqwest_mcp::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, HttpError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(StreamableHttpError::ResponseBodyTooLarge {
            max_size: max_bytes,
        });
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(StreamableHttpError::Client)?;
        if chunk.len() > max_bytes.saturating_sub(body.len()) {
            return Err(StreamableHttpError::ResponseBodyTooLarge {
                max_size: max_bytes,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[derive(Default)]
struct FrameBudget {
    retained: usize,
    line: usize,
    after_cr: bool,
}
impl FrameBudget {
    fn observe(&mut self, chunk: &[u8], limit: usize) -> Result<(), std::io::Error> {
        for &byte in chunk {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            match byte {
                b'\r' | b'\n' => {
                    self.after_cr = byte == b'\r';
                    if self.line == 0 {
                        self.retained = 0;
                    } else {
                        self.retained = self.retained.saturating_add(self.line).saturating_add(1);
                    }
                    self.line = 0;
                }
                _ => self.line = self.line.saturating_add(1),
            }
            if self.retained.saturating_add(self.line) > limit {
                return Err(std::io::Error::other(
                    "MCP SSE event exceeds its byte limit",
                ));
            }
        }
        Ok(())
    }
}

fn sse(
    response: reqwest_mcp::Response,
    max_bytes: usize,
) -> BoxStream<'static, Result<Sse, SseError>> {
    let stream = futures::stream::try_unfold(
        (response.bytes_stream().boxed(), FrameBudget::default()),
        move |(mut stream, mut budget)| async move {
            let Some(chunk) = stream.next().await else {
                return Ok(None);
            };
            let chunk = chunk.map_err(|_| std::io::Error::other("MCP HTTP stream failed"))?;
            budget.observe(&chunk, max_bytes)?;
            Ok::<_, std::io::Error>(Some((chunk, (stream, budget))))
        },
    );
    SseStream::from_bytes_stream(stream).boxed()
}
